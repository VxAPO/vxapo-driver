//! host/parse/commands/cmd_expr.rs — Eval: 命令
//!
//! 语法：`Eval: variable = expression`
//!
//! 支持的表达式：
//! - 常量赋值：`Eval: gain = 0.5`
//! - 算术运算：`Eval: x = 1 + 2 * 3`
//! - 变量引用：`Eval: y = x + 1`
//! - 数学函数：`Eval: z = db_to_linear(-6)`
//!
//! 变量存储在 `ParseContext` 中，后续过滤器参数可引用。
//!
//! Phase 7：基础四则运算 + 变量引用 + db_to_linear / linear_to_db。
//! Phase 8+：扩展函数库。

use std::collections::HashMap;

use crate::host::parse::parser::{ConfigError};

// ══════════════════════════════════════════════════════════════════════════════
// 变量存储（附加到 ParseContext 的外部 HashMap）
// ══════════════════════════════════════════════════════════════════════════════

/// 全局变量存储（在 parser.rs 中创建，传递给 cmd_expr）。
pub type Variables = HashMap<String, f64>;

// ══════════════════════════════════════════════════════════════════════════════
// 表达式求值
// ══════════════════════════════════════════════════════════════════════════════

/// 处理 `Eval:` 命令。
///
/// 格式：`variable = expression`
pub fn handle(value: &str, vars: &mut Variables, file: &str, line: usize) -> Result<(), ConfigError> {
    // 分割 variable = expression
    let eq_pos = value.find('=').ok_or_else(|| ConfigError::SyntaxError {
        file: file.to_owned(),
        line,
        message: "Eval: requires 'variable = expression' format".into(),
    })?;

    let var_name = value[..eq_pos].trim();
    let expr_str = value[eq_pos + 1..].trim();

    if var_name.is_empty() {
        return Err(ConfigError::SyntaxError {
            file: file.to_owned(),
            line,
            message: "Eval: missing variable name".into(),
        });
    }

    if expr_str.is_empty() {
        return Err(ConfigError::SyntaxError {
            file: file.to_owned(),
            line,
            message: "Eval: missing expression".into(),
        });
    }

    let result = eval_expression(expr_str, vars, file, line)?;
    vars.insert(var_name.to_owned(), result);

    log::debug!("{}:{}: {} = {}", file, line, var_name, result);
    Ok(())
}

// ══════════════════════════════════════════════════════════════════════════════
// 简易递归下降解析器
// ══════════════════════════════════════════════════════════════════════════════

/// 表达式求值（支持 +, -, *, /, 函数调用, 变量引用, 常量）。
///
/// 运算符优先级：* /  >  + -（标准数学优先级）
fn eval_expression(
    expr: &str,
    vars: &Variables,
    file: &str,
    line: usize,
) -> Result<f64, ConfigError> {
    let mut parser = ExprParser::new(expr, vars, file, line);
    let result = parser.parse_additive()?;
    if !parser.remaining().is_empty() {
        return Err(ConfigError::SyntaxError {
            file: file.to_owned(),
            line,
            message: format!("unexpected trailing characters: '{}'", parser.remaining()),
        });
    }
    Ok(result)
}

struct ExprParser<'a> {
    input: &'a str,
    pos: usize,
    vars: &'a Variables,
    file: &'a str,
    line: usize,
}

impl<'a> ExprParser<'a> {
    fn new(input: &'a str, vars: &'a Variables, file: &'a str, line: usize) -> Self {
        Self { input, pos: 0, vars, file, line }
    }

    fn remaining(&self) -> &'a str {
        self.input[self.pos..].trim_start()
    }

    fn skip_whitespace(&mut self) {
        while self.pos < self.input.len()
            && self.input.as_bytes()[self.pos] == b' '
        {
            self.pos += 1;
        }
    }

    fn peek(&mut self) -> Option<char> {
        self.skip_whitespace();
        self.input[self.pos..].chars().next()
    }

    fn consume(&mut self, ch: char) -> bool {
        self.skip_whitespace();
        if self.input[self.pos..].starts_with(ch) {
            self.pos += ch.len_utf8();
            true
        } else {
            false
        }
    }

    /// additive = multiplicative (('+' | '-') multiplicative)*
    fn parse_additive(&mut self) -> Result<f64, ConfigError> {
        let mut value = self.parse_multiplicative()?;
        loop {
            if self.consume('+') {
                value += self.parse_multiplicative()?;
            } else if self.consume('-') {
                value -= self.parse_multiplicative()?;
            } else {
                break;
            }
        }
        Ok(value)
    }

    /// multiplicative = unary (('*' | '/') unary)*
    fn parse_multiplicative(&mut self) -> Result<f64, ConfigError> {
        let mut value = self.parse_unary()?;
        loop {
            if self.consume('*') {
                value *= self.parse_unary()?;
            } else if self.consume('/') {
                let divisor = self.parse_unary()?;
                if divisor == 0.0 {
                    return Err(ConfigError::SyntaxError {
                        file: self.file.to_owned(),
                        line: self.line,
                        message: "division by zero".into(),
                    });
                }
                value /= divisor;
            } else {
                break;
            }
        }
        Ok(value)
    }

    /// unary = ('+' | '-')? primary
    fn parse_unary(&mut self) -> Result<f64, ConfigError> {
        if self.consume('-') {
            Ok(-self.parse_primary()?)
        } else if self.consume('+') {
            self.parse_primary()
        } else {
            self.parse_primary()
        }
    }

    /// primary = NUMBER | VARIABLE | FUNCTION '(' expr ')' | '(' expr ')'
    fn parse_primary(&mut self) -> Result<f64, ConfigError> {
        self.skip_whitespace();

        // 括号
        if self.consume('(') {
            let value = self.parse_additive()?;
            if !self.consume(')') {
                return Err(ConfigError::SyntaxError {
                    file: self.file.to_owned(),
                    line: self.line,
                    message: "missing closing ')'".into(),
                });
            }
            return Ok(value);
        }

        // 数字
        if let Some(ch) = self.peek() {
            if ch.is_ascii_digit() || ch == '.' {
                return self.parse_number();
            }
        }

        // 函数或变量
        if let Some(ch) = self.peek() {
            if ch.is_ascii_alphabetic() || ch == '_' {
                return self.parse_identifier_or_function();
            }
        }

        Err(ConfigError::SyntaxError {
            file: self.file.to_owned(),
            line: self.line,
            message: format!("unexpected character at '{}'", self.remaining()),
        })
    }

    fn parse_number(&mut self) -> Result<f64, ConfigError> {
        self.skip_whitespace();
        let start = self.pos;
        while self.pos < self.input.len() {
            let ch = self.input.as_bytes()[self.pos];
            if ch.is_ascii_digit() || ch == b'.' || ch == b'e' || ch == b'E'
                || (ch == b'-' && self.pos > start
                    && matches!(self.input.as_bytes()[self.pos - 1], b'e' | b'E'))
            {
                self.pos += 1;
            } else {
                break;
            }
        }
        let s = &self.input[start..self.pos];
        s.parse::<f64>().map_err(|_| ConfigError::SyntaxError {
            file: self.file.to_owned(),
            line: self.line,
            message: format!("invalid number '{}'", s),
        })
    }

    fn parse_identifier_or_function(&mut self) -> Result<f64, ConfigError> {
        self.skip_whitespace();
        let start = self.pos;
        while self.pos < self.input.len() {
            let ch = self.input.as_bytes()[self.pos];
            if ch.is_ascii_alphanumeric() || ch == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let name = &self.input[start..self.pos];

        // 检查是否为函数调用
        if self.consume('(') {
            let arg = self.parse_additive()?;
            if !self.consume(')') {
                return Err(ConfigError::SyntaxError {
                    file: self.file.to_owned(),
                    line: self.line,
                    message: format!("missing closing ')' for function '{}'", name),
                });
            }
            return call_function(name, arg);
        }

        // 内置常量
        match name {
            "pi" | "PI" => return Ok(std::f64::consts::PI),
            "e" | "E" => return Ok(std::f64::consts::E),
            _ => {}
        }

        // 变量查找
        if let Some(&value) = self.vars.get(name) {
            return Ok(value);
        }

        Err(ConfigError::SyntaxError {
            file: self.file.to_owned(),
            line: self.line,
            message: format!("undefined variable '{}'", name),
        })
    }
}

/// 调用内置数学函数。
fn call_function(name: &str, arg: f64) -> Result<f64, ConfigError> {
    match name {
        "abs" => Ok(arg.abs()),
        "sqrt" => {
            if arg < 0.0 {
                Err(ConfigError::SyntaxError {
                    file: String::new(),
                    line: 0,
                    message: format!("sqrt of negative number {}", arg),
                })
            } else {
                Ok(arg.sqrt())
            }
        }
        "sin" => Ok(arg.sin()),
        "cos" => Ok(arg.cos()),
        "tan" => Ok(arg.tan()),
        "log" | "ln" => {
            if arg <= 0.0 {
                Err(ConfigError::SyntaxError {
                    file: String::new(),
                    line: 0,
                    message: format!("log of non-positive number {}", arg),
                })
            } else {
                Ok(arg.ln())
            }
        }
        "log10" => {
            if arg <= 0.0 {
                Err(ConfigError::SyntaxError {
                    file: String::new(),
                    line: 0,
                    message: format!("log10 of non-positive number {}", arg),
                })
            } else {
                Ok(arg.log10())
            }
        }
        "floor" => Ok(arg.floor()),
        "ceil" => Ok(arg.ceil()),
        "round" => Ok(arg.round()),
        // dB 转换（音频常用）
        "db_to_linear" => Ok(10.0_f64.powf(arg / 20.0)),
        "linear_to_db" => {
            if arg <= 0.0 {
                Err(ConfigError::SyntaxError {
                    file: String::new(),
                    line: 0,
                    message: format!("linear_to_db of non-positive {}", arg),
                })
            } else {
                Ok(20.0 * arg.log10())
            }
        }
        _ => Err(ConfigError::SyntaxError {
            file: String::new(),
            line: 0,
            message: format!("unknown function '{}'", name),
        }),
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> Variables {
        Variables::new()
    }

    // ── 常量赋值 ────────────────────────────────────────────────────────────

    #[test]
    fn eval_integer() {
        let mut v = vars();
        handle("x = 42", &mut v, "test.txt", 1).unwrap();
        assert_eq!(v["x"], 42.0);
    }

    #[test]
    fn eval_float() {
        let mut v = vars();
        handle("gain = 0.5", &mut v, "test.txt", 1).unwrap();
        assert_eq!(v["gain"], 0.5);
    }

    #[test]
    fn eval_negative() {
        let mut v = vars();
        handle("x = -3.14", &mut v, "test.txt", 1).unwrap();
        assert!((v["x"] - (-3.14)).abs() < 1e-10);
    }

    // ── 算术运算 ────────────────────────────────────────────────────────────

    #[test]
    fn eval_addition() {
        let mut v = vars();
        handle("x = 1 + 2", &mut v, "test.txt", 1).unwrap();
        assert_eq!(v["x"], 3.0);
    }

    #[test]
    fn eval_precedence() {
        let mut v = vars();
        handle("x = 1 + 2 * 3", &mut v, "test.txt", 1).unwrap();
        assert_eq!(v["x"], 7.0); // 1 + (2*3) = 7, not (1+2)*3 = 9
    }

    #[test]
    fn eval_parentheses() {
        let mut v = vars();
        handle("x = (1 + 2) * 3", &mut v, "test.txt", 1).unwrap();
        assert_eq!(v["x"], 9.0);
    }

    #[test]
    fn eval_complex() {
        let mut v = vars();
        handle("x = (2 + 3) * (4 - 1) / 5", &mut v, "test.txt", 1).unwrap();
        assert_eq!(v["x"], 3.0); // 5 * 3 / 5 = 3
    }

    // ── 变量引用 ────────────────────────────────────────────────────────────

    #[test]
    fn eval_variable_ref() {
        let mut v = vars();
        handle("x = 10", &mut v, "test.txt", 1).unwrap();
        handle("y = x + 5", &mut v, "test.txt", 2).unwrap();
        assert_eq!(v["y"], 15.0);
    }

    #[test]
    fn eval_undefined_variable() {
        let mut v = vars();
        let result = handle("y = x + 1", &mut v, "test.txt", 1);
        assert!(result.is_err());
    }

    // ── 函数调用 ────────────────────────────────────────────────────────────

    #[test]
    fn eval_db_to_linear() {
        let mut v = vars();
        handle("x = db_to_linear(0)", &mut v, "test.txt", 1).unwrap();
        assert!((v["x"] - 1.0).abs() < 1e-10); // 0 dB = 1.0 linear
    }

    #[test]
    fn eval_db_to_linear_neg6() {
        let mut v = vars();
        handle("x = db_to_linear(-6)", &mut v, "test.txt", 1).unwrap();
        // -6 dB ≈ 0.501
        assert!((v["x"] - 0.5012).abs() < 0.001);
    }

    #[test]
    fn eval_linear_to_db() {
        let mut v = vars();
        handle("x = linear_to_db(1)", &mut v, "test.txt", 1).unwrap();
        assert!((v["x"]).abs() < 1e-10); // 1.0 linear = 0 dB
    }

    #[test]
    fn eval_sqrt() {
        let mut v = vars();
        handle("x = sqrt(16)", &mut v, "test.txt", 1).unwrap();
        assert_eq!(v["x"], 4.0);
    }

    #[test]
    fn eval_abs() {
        let mut v = vars();
        handle("x = abs(-42)", &mut v, "test.txt", 1).unwrap();
        assert_eq!(v["x"], 42.0);
    }

    // ── 常量 ────────────────────────────────────────────────────────────────

    #[test]
    fn eval_pi() {
        let mut v = vars();
        handle("x = pi", &mut v, "test.txt", 1).unwrap();
        assert!((v["x"] - std::f64::consts::PI).abs() < 1e-10);
    }

    // ── 错误处理 ────────────────────────────────────────────────────────────

    #[test]
    fn eval_no_equals() {
        let mut v = vars();
        let result = handle("x 42", &mut v, "test.txt", 1);
        assert!(result.is_err());
    }

    #[test]
    fn eval_empty_var() {
        let mut v = vars();
        let result = handle(" = 42", &mut v, "test.txt", 1);
        assert!(result.is_err());
    }

    #[test]
    fn eval_empty_expr() {
        let mut v = vars();
        let result = handle("x = ", &mut v, "test.txt", 1);
        assert!(result.is_err());
    }

    #[test]
    fn eval_division_by_zero() {
        let mut v = vars();
        let result = handle("x = 1 / 0", &mut v, "test.txt", 1);
        assert!(result.is_err());
    }

    #[test]
    fn eval_unknown_function() {
        let mut v = vars();
        let result = handle("x = foobar(1)", &mut v, "test.txt", 1);
        assert!(result.is_err());
    }

    #[test]
    fn eval_overwrite_variable() {
        let mut v = vars();
        handle("x = 1", &mut v, "test.txt", 1).unwrap();
        handle("x = 2", &mut v, "test.txt", 2).unwrap();
        assert_eq!(v["x"], 2.0);
    }
}