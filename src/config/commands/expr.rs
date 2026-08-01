//! config/commands/expr.rs — Eval: 命令 + 表达式求值（v6.2 规范 6.7）
//!
//! 语法：
//! - `Eval: gain = -3.0` — 常量赋值
//! - `Eval: x = 1 + 2 * 3` — 算术运算
//! - `Eval: y = db_to_linear(-6)` — 函数调用
//! - `Eval: z = x + 1` — 变量引用

use crate::config::error::ConfigError;
use crate::config::parser::ParseContext;

/// 处理 Eval: 命令。
///
/// 格式：`variable = expression`，将结果存入 ctx.variables。
pub fn handle(value: &str, ctx: &mut ParseContext) -> Result<(), ConfigError> {
    let trimmed = value.trim();
    let Some((name, expr)) = trimmed.split_once('=') else {
        return Err(syntax(ctx, "Eval: requires 'variable = expression'"));
    };

    let name = name.trim();
    if name.is_empty() || !is_valid_identifier(name) {
        return Err(syntax(ctx, &format!("invalid variable name '{name}'")));
    }

    let result = eval_expression(expr.trim(), ctx)?;
    ctx.variables.insert(name.to_owned(), result);
    Ok(())
}

/// 求值算术表达式。
///
/// 支持：+ - * /（标准优先级）、括号、函数调用。
pub fn eval_expression(expr: &str, ctx: &ParseContext) -> Result<f64, ConfigError> {
    let mut tokens = tokenize(expr, ctx)?;
    let value = parse_add_sub(&mut tokens, ctx)?;
    if !tokens.is_empty() {
        return Err(syntax(ctx, "unexpected trailing tokens in expression"));
    }
    Ok(value)
}

// ══════════════════════════════════════════════════════════════════════════════
// Tokenizer
// ══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Var(String),
    Fn(String),
    Op(char),
    LParen,
    RParen,
    Comma,
}

fn tokenize(s: &str, ctx: &ParseContext) -> Result<Vec<Tok>, ConfigError> {
    let chars: Vec<char> = s.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        match chars[i] {
            ' ' | '\t' => i += 1,
            '+' | '-' | '*' | '/' => {
                tokens.push(Tok::Op(chars[i]));
                i += 1;
            }
            '(' => {
                tokens.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(Tok::RParen);
                i += 1;
            }
            ',' => {
                tokens.push(Tok::Comma);
                i += 1;
            }
            '.' | '0'..='9' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                let num_str: String = chars[start..i].iter().collect();
                let v: f64 = num_str
                    .parse()
                    .map_err(|_| syntax(ctx, &format!("invalid number '{num_str}'")))?;
                tokens.push(Tok::Num(v));
            }
            'a'..='z' | 'A'..='Z' | '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let ident: String = chars[start..i].iter().collect();
                let is_fn = i < chars.len() && chars[i] == '(' && !is_builtin_const(&ident);
                if is_fn {
                    tokens.push(Tok::Fn(ident));
                } else {
                    tokens.push(Tok::Var(ident));
                }
            }
            c => {
                return Err(syntax(ctx, &format!("unexpected character '{c}'")));
            }
        }
    }
    Ok(tokens)
}

fn is_builtin_const(name: &str) -> bool {
    matches!(name, "pi" | "e")
}

// ══════════════════════════════════════════════════════════════════════════════
// 递归下降解析
// ══════════════════════════════════════════════════════════════════════════════

fn parse_add_sub(tokens: &mut Vec<Tok>, ctx: &ParseContext) -> Result<f64, ConfigError> {
    let mut left = parse_mul_div(tokens, ctx)?;
    while let Some(Tok::Op(c)) = tokens.first().cloned() {
        match c {
            '+' => {
                tokens.remove(0);
                left += parse_mul_div(tokens, ctx)?;
            }
            '-' => {
                tokens.remove(0);
                left -= parse_mul_div(tokens, ctx)?;
            }
            _ => break,
        }
    }
    Ok(left)
}

fn parse_mul_div(tokens: &mut Vec<Tok>, ctx: &ParseContext) -> Result<f64, ConfigError> {
    let mut left = parse_unary(tokens, ctx)?;
    while let Some(Tok::Op(c)) = tokens.first().cloned() {
        match c {
            '*' => {
                tokens.remove(0);
                left *= parse_unary(tokens, ctx)?;
            }
            '/' => {
                tokens.remove(0);
                let right = parse_unary(tokens, ctx)?;
                if right == 0.0 {
                    return Err(syntax(ctx, "division by zero"));
                }
                left /= right;
            }
            _ => break,
        }
    }
    Ok(left)
}

fn parse_unary(tokens: &mut Vec<Tok>, ctx: &ParseContext) -> Result<f64, ConfigError> {
    match tokens.first() {
        Some(Tok::Op('-')) => {
            tokens.remove(0);
            Ok(-parse_unary(tokens, ctx)?)
        }
        Some(Tok::Op('+')) => {
            tokens.remove(0);
            parse_unary(tokens, ctx)
        }
        _ => parse_primary(tokens, ctx),
    }
}

fn parse_primary(tokens: &mut Vec<Tok>, ctx: &ParseContext) -> Result<f64, ConfigError> {
    let Some(tok) = tokens.first().cloned() else {
        return Err(syntax(ctx, "unexpected end of expression"));
    };

    match tok {
        Tok::Num(v) => {
            tokens.remove(0);
            Ok(v)
        }
        Tok::Var(name) => {
            tokens.remove(0);
            match name.as_str() {
                "pi" => Ok(std::f64::consts::PI),
                "e" => Ok(std::f64::consts::E),
                _ => ctx
                    .variables
                    .get(&name)
                    .copied()
                    .ok_or_else(|| syntax(ctx, &format!("unknown variable '{name}'"))),
            }
        }
        Tok::Fn(name) => {
            tokens.remove(0);
            if tokens.first() != Some(&Tok::LParen) {
                return Err(syntax(ctx, &format!("expected '(' after function '{name}'")));
            }
            tokens.remove(0);
            let mut args = Vec::new();
            if tokens.first() != Some(&Tok::RParen) {
                loop {
                    args.push(parse_add_sub(tokens, ctx)?);
                    match tokens.first() {
                        Some(Tok::Comma) => {
                            tokens.remove(0);
                        }
                        Some(Tok::RParen) => break,
                        _ => return Err(syntax(ctx, "expected ',' or ')' in function call")),
                    }
                }
            }
            if tokens.first() != Some(&Tok::RParen) {
                return Err(syntax(ctx, &format!("expected ')' after function '{name}'")));
            }
            tokens.remove(0);
            call_function(&name, &args, ctx)
        }
        Tok::LParen => {
            tokens.remove(0);
            let v = parse_add_sub(tokens, ctx)?;
            if tokens.first() != Some(&Tok::RParen) {
                return Err(syntax(ctx, "expected ')'"));
            }
            tokens.remove(0);
            Ok(v)
        }
        _ => Err(syntax(ctx, "unexpected token in expression")),
    }
}

fn call_function(name: &str, args: &[f64], ctx: &ParseContext) -> Result<f64, ConfigError> {
    let bad_args = |expected: usize| {
        syntax(
            ctx,
            &format!("function '{name}' expects {expected} argument(s), got {}", args.len()),
        )
    };

    match name {
        "db_to_linear" => {
            if args.len() != 1 {
                return Err(bad_args(1));
            }
            Ok(10f64.powf(args[0] / 20.0))
        }
        "linear_to_db" => {
            if args.len() != 1 {
                return Err(bad_args(1));
            }
            if args[0] <= 0.0 {
                return Err(syntax(ctx, "linear_to_db: value must be > 0"));
            }
            Ok(20.0 * args[0].log10())
        }
        "abs" => {
            if args.len() != 1 {
                return Err(bad_args(1));
            }
            Ok(args[0].abs())
        }
        "sqrt" => {
            if args.len() != 1 {
                return Err(bad_args(1));
            }
            Ok(args[0].sqrt())
        }
        "sin" => {
            if args.len() != 1 {
                return Err(bad_args(1));
            }
            Ok(args[0].sin())
        }
        "cos" => {
            if args.len() != 1 {
                return Err(bad_args(1));
            }
            Ok(args[0].cos())
        }
        "tan" => {
            if args.len() != 1 {
                return Err(bad_args(1));
            }
            Ok(args[0].tan())
        }
        "log" | "ln" => {
            if args.len() != 1 {
                return Err(bad_args(1));
            }
            Ok(args[0].ln())
        }
        "log10" => {
            if args.len() != 1 {
                return Err(bad_args(1));
            }
            Ok(args[0].log10())
        }
        "floor" => {
            if args.len() != 1 {
                return Err(bad_args(1));
            }
            Ok(args[0].floor())
        }
        "ceil" => {
            if args.len() != 1 {
                return Err(bad_args(1));
            }
            Ok(args[0].ceil())
        }
        "round" => {
            if args.len() != 1 {
                return Err(bad_args(1));
            }
            Ok(args[0].round())
        }
        _ => Err(syntax(ctx, &format!("unknown function '{name}'"))),
    }
}

fn is_valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn syntax(ctx: &ParseContext, message: &str) -> ConfigError {
    ConfigError::SyntaxError {
        file: ctx.current_file.display().to_string(),
        line: ctx.line_number,
        message: message.to_owned(),
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parser::{ParseContext, ParseStage};
    use crate::pipeline::dsp::filter::DspContext;
    use crate::pipeline::dsp::factory::FilterRegistry;
    use std::collections::HashMap;
    use std::path::Path;

    fn test_ctx() -> ParseContext<'static> {
        let filters: &'static mut Vec<Box<dyn crate::pipeline::dsp::filter::Filter>> =
            Box::leak(Box::new(Vec::new()));
        let dsp: &'static DspContext = Box::leak(Box::new(DspContext {
            sample_rate: 48000,
            channel_count: 2,
            channel_mask: 0x3,
            channel_names: vec!["L".into(), "R".into()],
            max_frame_count: 480,
            bits_per_sample: 32,
            device_type: crate::pipeline::dsp::filter::DeviceType::Render,
            stage: crate::pipeline::dsp::filter::ProcessingStage::None,
            variables: HashMap::new(),
        }));
        let registry: &'static FilterRegistry = Box::leak(Box::new(FilterRegistry::new()));
        ParseContext {
            filters,
            registry,
            dsp_ctx: dsp,
            stage: ParseStage::None,
            is_capture: false,
            current_file: Path::new("<test>"),
            line_number: 1,
            abort_file: false,
            cond_stack: Vec::new(),
            variables: HashMap::new(),
            include_depth: 0,
            current_channels: vec!["L".into(), "R".into()],
            all_channels: vec!["L".into(), "R".into()],
            current_device: None,
        }
    }

    #[test]
    fn eval_arithmetic() {
        let ctx = test_ctx();
        assert_eq!(eval_expression("3 + 4", &ctx).unwrap(), 7.0);
        assert_eq!(eval_expression("2 + 3 * 4", &ctx).unwrap(), 14.0);
        assert_eq!(eval_expression("(2 + 3) * 4", &ctx).unwrap(), 20.0);
        assert_eq!(eval_expression("-5 + 3", &ctx).unwrap(), -2.0);
    }

    #[test]
    fn eval_variables_and_consts() {
        let mut ctx = test_ctx();
        ctx.variables.insert("gain".into(), -3.0);
        assert_eq!(eval_expression("gain + 1", &ctx).unwrap(), -2.0);
        assert_eq!(eval_expression("pi", &ctx).unwrap(), std::f64::consts::PI);
    }

    #[test]
    fn eval_functions() {
        let ctx = test_ctx();
        assert_eq!(eval_expression("db_to_linear(-6)", &ctx).unwrap(), 10f64.powf(-0.3));
        assert_eq!(eval_expression("abs(-5)", &ctx).unwrap(), 5.0);
        assert_eq!(eval_expression("sqrt(16)", &ctx).unwrap(), 4.0);
        assert_eq!(eval_expression("round(2.6)", &ctx).unwrap(), 3.0);
    }

    #[test]
    fn eval_errors() {
        let ctx = test_ctx();
        assert!(eval_expression("1 / 0", &ctx).is_err());
        assert!(eval_expression("unknown_var", &ctx).is_err());
        assert!(eval_expression("1 +", &ctx).is_err());
    }

    #[test]
    fn handle_assignment() {
        let mut ctx = test_ctx();
        handle("gain = db_to_linear(-6)", &mut ctx).unwrap();
        assert!((ctx.variables["gain"] - 10f64.powf(-0.3)).abs() < 1e-9);
    }

    #[test]
    fn invalid_identifier() {
        let mut ctx = test_ctx();
        assert!(handle("1bad = 5", &mut ctx).is_err());
        assert!(handle("= 5", &mut ctx).is_err());
    }
}