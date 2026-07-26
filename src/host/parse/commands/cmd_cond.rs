//! host/parse/commands/cmd_cond.rs — If: / ElseIf: / Else: / EndIf: 条件分支
//!
//! 语法：
//! ```text
//! If: <condition>
//!   ... (condition true lines)
//! ElseIf: <condition>
//!   ... (if previous was false, check this)
//! Else:
//!   ... (all conditions were false)
//! EndIf:
//! ```
//!
//! 条件表达式：
//! - `variable == value` / `!=` / `>` / `<` / `>=` / `<=`
//! - `variable`（非零为 true）
//! - `!variable`（零为 true）
//!
//! 嵌套支持：If/EndIf 可嵌套，内部 If 在外层跳过时不计数。

use std::collections::HashMap;

use crate::host::parse::parser::{ConfigError, ProcessingStage};

// ══════════════════════════════════════════════════════════════════════════════
// 条件栈（嵌入 ParseContext）
// ══════════════════════════════════════════════════════════════════════════════

/// 单层条件状态。
#[derive(Debug, Clone)]
pub struct CondState {
    /// 本层是否正在执行（条件为 true 且外层也允许执行）。
    pub executing: bool,
    /// 本层是否已经有过一个 true 分支（If 或 ElseIf 命中）。
    /// 一旦为 true，后续 ElseIf / Else 跳过。
    pub had_true: bool,
}

impl CondState {
    /// 新建一层条件，初始状态由 If 条件决定。
    pub fn new(executing: bool) -> Self {
        Self {
            executing,
            had_true: executing,
        }
    }
}

/// 条件栈。
pub type CondStack = Vec<CondState>;

/// 检查当前是否在跳过状态（外层有 false 分支）。
///
/// 遍历栈：如果任何一层 `executing == false`，当前行应跳过。
pub fn is_skipping(stack: &CondStack) -> bool {
    stack.iter().any(|s| !s.executing)
}

/// 处理 `If:` 命令。
pub fn handle_if(
    value: &str,
    vars: &HashMap<String, f64>,
    stage: ProcessingStage,
    is_capture: bool,
    stack: &mut CondStack,
    file: &str,
    line: usize,
) -> Result<(), ConfigError> {
    let outer_skipping = is_skipping(stack);

    if outer_skipping {
        // 外层已跳过——压入一个 false 状态，不评估条件
        stack.push(CondState::new(false));
        return Ok(());
    }

    let result = eval_condition(value, vars, stage, is_capture, file, line)?;
    stack.push(CondState::new(result));

    log::debug!("{}:{}: If: {} = {}", file, line, value, result);
    Ok(())
}

/// 处理 `ElseIf:` 命令。
pub fn handle_elseif(
    value: &str,
    vars: &HashMap<String, f64>,
    stage: ProcessingStage,
    is_capture: bool,
    stack: &mut CondStack,
    file: &str,
    line: usize,
) -> Result<(), ConfigError> {
    let outer_skipping = is_skipping_with_outer(stack);

    let state = stack.last_mut().ok_or_else(|| ConfigError::SyntaxError {
        file: file.to_owned(),
        line,
        message: "ElseIf without matching If".into(),
    })?;

    if outer_skipping {
        // 外层跳过中，不评估
        state.executing = false;
        return Ok(());
    }

    if state.had_true {
        // 已有 true 分支，跳过
        state.executing = false;
    } else {
        let result = eval_condition(value, vars, stage, is_capture, file, line)?;
        state.executing = result;
        if result {
            state.had_true = true;
        }
    }

    log::debug!("{}:{}: ElseIf: {} = {}", file, line, value, state.executing);
    Ok(())
}

/// 处理 `Else:` 命令。
pub fn handle_else(
    stack: &mut CondStack,
    file: &str,
    line: usize,
) -> Result<(), ConfigError> {
    let outer_skipping = is_skipping_with_outer(stack);

    let state = stack.last_mut().ok_or_else(|| ConfigError::SyntaxError {
        file: file.to_owned(),
        line,
        message: "Else without matching If".into(),
    })?;

    if outer_skipping {
        state.executing = false;
    } else {
        state.executing = !state.had_true;
    }

    Ok(())
}

/// 处理 `EndIf:` 命令。
pub fn handle_endif(
    stack: &mut CondStack,
    file: &str,
    line: usize,
) -> Result<(), ConfigError> {
    if stack.pop().is_none() {
        return Err(ConfigError::SyntaxError {
            file: file.to_owned(),
            line,
            message: "EndIf without matching If".into(),
        });
    }
    Ok(())
}

// ══════════════════════════════════════════════════════════════════════════════
// 条件求值
// ══════════════════════════════════════════════════════════════════════════════

/// 求值条件表达式。
///
/// 支持：
/// - `variable`（非零为 true）
/// - `!variable`（零为 true）
/// - `variable op value`（比较运算）
/// - `true` / `false` 字面量
/// - `device_type == render` / `capture`
/// - `stage == premix` / `postmix` / `capture`
fn eval_condition(
    expr: &str,
    vars: &HashMap<String, f64>,
    stage: ProcessingStage,
    is_capture: bool,
    file: &str,
    line: usize,
) -> Result<bool, ConfigError> {
    let expr = expr.trim();

    // 字面量
    if expr.eq_ignore_ascii_case("true") {
        return Ok(true);
    }
    if expr.eq_ignore_ascii_case("false") {
        return Ok(false);
    }

    // 取反
    if let Some(inner) = expr.strip_prefix('!') {
        let inner = inner.trim();
        return eval_condition(inner, vars, stage, is_capture, file, line)
            .map(|v| !v);
    }

    // 比较运算符
    for op in &["!=", ">=", "<=", "==", ">", "<"] {
        if let Some(pos) = find_operator(expr, op) {
            let left = expr[..pos].trim();
            let right = expr[pos + op.len()..].trim();
            return eval_comparison(left, op, right, vars, stage, is_capture, file, line);
        }
    }

    // 单变量：非零为 true
    let value = resolve_variable(expr, vars, stage, is_capture, file, line)?;
    Ok(value != 0.0)
}

/// 查找运算符位置（排除字符串开头的 `!`）。
fn find_operator(expr: &str, op: &str) -> Option<usize> {
    // 从位置 1 开始找，避免匹配前缀 `!`
    let start = if op == "!=" { 0 } else { 1 };
    expr[start..].find(op).map(|p| p + start)
}

/// 解析比较表达式。
fn eval_comparison(
    left: &str,
    op: &str,
    right: &str,
    vars: &HashMap<String, f64>,
    stage: ProcessingStage,
    is_capture: bool,
    file: &str,
    line: usize,
) -> Result<bool, ConfigError> {
    // 特殊字段比较
    if left.eq_ignore_ascii_case("device_type") {
        let expected = right.to_lowercase();
        let actual_is_capture = expected == "capture";
        return match op {
            "==" => Ok(is_capture == actual_is_capture),
            "!=" => Ok(is_capture != actual_is_capture),
            _ => Err(ConfigError::SyntaxError {
                file: file.to_owned(),
                line,
                message: format!("operator '{}' not supported for device_type", op),
            }),
        };
    }

    if left.eq_ignore_ascii_case("stage") {
        let expected = match right.to_lowercase().as_str() {
            "premix" | "pre-mix" | "pre_mix" => ProcessingStage::PreMix,
            "postmix" | "post-mix" | "post_mix" => ProcessingStage::PostMix,
            "capture" => ProcessingStage::Capture,
            _ => {
                return Err(ConfigError::SyntaxError {
                    file: file.to_owned(),
                    line,
                    message: format!("unknown stage '{}'", right),
                });
            }
        };
        return match op {
            "==" => Ok(stage == expected),
            "!=" => Ok(stage != expected),
            _ => Err(ConfigError::SyntaxError {
                file: file.to_owned(),
                line,
                message: format!("operator '{}' not supported for stage", op),
            }),
        };
    }

    // 数值比较
    let l = resolve_variable(left, vars, stage, is_capture, file, line)?;
    let r = resolve_value(right, vars, stage, is_capture, file, line)?;

    match op {
        "==" => Ok((l - r).abs() < 1e-10),
        "!=" => Ok((l - r).abs() >= 1e-10),
        ">" => Ok(l > r),
        "<" => Ok(l < r),
        ">=" => Ok(l >= r),
        "<=" => Ok(l <= r),
        _ => unreachable!(),
    }
}

/// 解析右侧值：常量或变量。
fn resolve_value(
    s: &str,
    vars: &HashMap<String, f64>,
    stage: ProcessingStage,
    is_capture: bool,
    file: &str,
    line: usize,
) -> Result<f64, ConfigError> {
    // 尝试作为数字解析
    if let Ok(n) = s.parse::<f64>() {
        return Ok(n);
    }
    // 作为变量查找
    resolve_variable(s, vars, stage, is_capture, file, line)
}

/// 解析变量值。
fn resolve_variable(
    name: &str,
    vars: &HashMap<String, f64>,
    _stage: ProcessingStage,
    _is_capture: bool,
    file: &str,
    line: usize,
) -> Result<f64, ConfigError> {
    if let Some(&v) = vars.get(name) {
        return Ok(v);
    }

    Err(ConfigError::SyntaxError {
        file: file.to_owned(),
        line,
        message: format!("undefined variable '{}'", name),
    })
}

/// 检查是否在跳过状态（含外层）。
///
/// 和 `is_skipping` 相同，但语义上用于 ElseIf/Else 判断。
fn is_skipping_with_outer(stack: &CondStack) -> bool {
    // 外层有任何 false → 整体跳过
    if stack.len() >= 2 {
        let outer = &stack[..stack.len() - 1];
        if outer.iter().any(|s| !s.executing) {
            return true;
        }
    }
    false
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn vars_with(key: &str, val: f64) -> HashMap<String, f64> {
        let mut m = HashMap::new();
        m.insert(key.to_owned(), val);
        m
    }

    // ── If / EndIf 基础 ─────────────────────────────────────────────────────

    #[test]
    fn if_true() {
        let v = vars_with("x", 1.0);
        let mut stack = CondStack::new();
        handle_if("x", &v, ProcessingStage::None, false, &mut stack, "t", 1).unwrap();
        assert!(stack.last().unwrap().executing);
        assert!(!is_skipping(&stack));
        handle_endif(&mut stack, "t", 2).unwrap();
        assert!(stack.is_empty());
    }

    #[test]
    fn if_false() {
        let v = vars_with("x", 0.0);
        let mut stack = CondStack::new();
        handle_if("x", &v, ProcessingStage::None, false, &mut stack, "t", 1).unwrap();
        assert!(!stack.last().unwrap().executing);
        assert!(is_skipping(&stack));
        handle_endif(&mut stack, "t", 2).unwrap();
    }

    #[test]
    fn if_comparison_true() {
        let v = vars_with("x", 5.0);
        let mut stack = CondStack::new();
        handle_if("x > 3", &v, ProcessingStage::None, false, &mut stack, "t", 1).unwrap();
        assert!(stack.last().unwrap().executing);
        handle_endif(&mut stack, "t", 2).unwrap();
    }

    #[test]
    fn if_comparison_false() {
        let v = vars_with("x", 2.0);
        let mut stack = CondStack::new();
        handle_if("x > 3", &v, ProcessingStage::None, false, &mut stack, "t", 1).unwrap();
        assert!(!stack.last().unwrap().executing);
        handle_endif(&mut stack, "t", 2).unwrap();
    }

    #[test]
    fn if_not() {
        let v = vars_with("x", 0.0);
        let mut stack = CondStack::new();
        handle_if("!x", &v, ProcessingStage::None, false, &mut stack, "t", 1).unwrap();
        assert!(stack.last().unwrap().executing);
        handle_endif(&mut stack, "t", 2).unwrap();
    }

    // ── ElseIf ──────────────────────────────────────────────────────────────

    #[test]
    fn elseif_first_true() {
        let v = vars_with("x", 1.0);
        let mut stack = CondStack::new();
        handle_if("x", &v, ProcessingStage::None, false, &mut stack, "t", 1).unwrap();
        assert!(stack.last().unwrap().executing);

        // ElseIf: x == 1 → 但已有 true 分支，应跳过
        handle_elseif("x == 1", &v, ProcessingStage::None, false, &mut stack, "t", 2).unwrap();
        assert!(!stack.last().unwrap().executing);
        handle_endif(&mut stack, "t", 3).unwrap();
    }

    #[test]
    fn elseif_first_false_second_true() {
        let v = vars_with("x", 2.0);
        let mut stack = CondStack::new();
        handle_if("x > 5", &v, ProcessingStage::None, false, &mut stack, "t", 1).unwrap();
        assert!(!stack.last().unwrap().executing);

        handle_elseif("x > 1", &v, ProcessingStage::None, false, &mut stack, "t", 2).unwrap();
        assert!(stack.last().unwrap().executing);
        handle_endif(&mut stack, "t", 3).unwrap();
    }

    // ── Else ────────────────────────────────────────────────────────────────

    #[test]
    fn else_after_false() {
        let v = vars_with("x", 0.0);
        let mut stack = CondStack::new();
        handle_if("x", &v, ProcessingStage::None, false, &mut stack, "t", 1).unwrap();
        handle_else(&mut stack, "t", 2).unwrap();
        assert!(stack.last().unwrap().executing);
        handle_endif(&mut stack, "t", 3).unwrap();
    }

    #[test]
    fn else_after_true() {
        let v = vars_with("x", 1.0);
        let mut stack = CondStack::new();
        handle_if("x", &v, ProcessingStage::None, false, &mut stack, "t", 1).unwrap();
        handle_else(&mut stack, "t", 2).unwrap();
        assert!(!stack.last().unwrap().executing);
        handle_endif(&mut stack, "t", 3).unwrap();
    }

    // ── 嵌套 ────────────────────────────────────────────────────────────────

    #[test]
    fn nested_if() {
        let mut v = HashMap::new();
        v.insert("x".to_owned(), 1.0);
        v.insert("y".to_owned(), 2.0);
        let mut stack = CondStack::new();

        handle_if("x", &v, ProcessingStage::None, false, &mut stack, "t", 1).unwrap();
        assert!(!is_skipping(&stack));

        handle_if("y > 1", &v, ProcessingStage::None, false, &mut stack, "t", 2).unwrap();
        assert!(!is_skipping(&stack)); // both true

        handle_endif(&mut stack, "t", 3).unwrap();
        assert!(!is_skipping(&stack)); // outer still true

        handle_endif(&mut stack, "t", 4).unwrap();
        assert!(stack.is_empty());
    }

    #[test]
    fn nested_outer_false_skips_inner() {
        let mut v = HashMap::new();
        v.insert("x".to_owned(), 0.0);
        v.insert("y".to_owned(), 100.0);
        let mut stack = CondStack::new();

        handle_if("x", &v, ProcessingStage::None, false, &mut stack, "t", 1).unwrap();
        assert!(is_skipping(&stack));

        // 内层 If: 即使 y 为 true，外层 false → 内层也 false
        handle_if("y > 1", &v, ProcessingStage::None, false, &mut stack, "t", 2).unwrap();
        assert!(is_skipping(&stack));
        assert!(!stack.last().unwrap().executing);

        handle_endif(&mut stack, "t", 3).unwrap();
        handle_endif(&mut stack, "t", 4).unwrap();
    }

    // ── 阶段/设备比较 ──────────────────────────────────────────────────────

    #[test]
    fn if_stage_premix() {
        let v = HashMap::new();
        let mut stack = CondStack::new();
        handle_if("stage == premix", &v, ProcessingStage::PreMix, false, &mut stack, "t", 1).unwrap();
        assert!(stack.last().unwrap().executing);
        handle_endif(&mut stack, "t", 2).unwrap();
    }

    #[test]
    fn if_stage_not_premix() {
        let v = HashMap::new();
        let mut stack = CondStack::new();
        handle_if("stage == premix", &v, ProcessingStage::PostMix, false, &mut stack, "t", 1).unwrap();
        assert!(!stack.last().unwrap().executing);
        handle_endif(&mut stack, "t", 2).unwrap();
    }

    #[test]
    fn if_device_capture() {
        let v = HashMap::new();
        let mut stack = CondStack::new();
        handle_if("device_type == capture", &v, ProcessingStage::None, true, &mut stack, "t", 1).unwrap();
        assert!(stack.last().unwrap().executing);
        handle_endif(&mut stack, "t", 2).unwrap();
    }

    // ── 错误处理 ────────────────────────────────────────────────────────────

    #[test]
    fn endif_without_if() {
        let mut stack = CondStack::new();
        let result = handle_endif(&mut stack, "t", 1);
        assert!(result.is_err());
    }

    #[test]
    fn else_without_if() {
        let mut stack = CondStack::new();
        let result = handle_else(&mut stack, "t", 1);
        assert!(result.is_err());
    }

    #[test]
    fn undefined_variable() {
        let v = HashMap::new();
        let mut stack = CondStack::new();
        let result = handle_if("x", &v, ProcessingStage::None, false, &mut stack, "t", 1);
        assert!(result.is_err());
    }
}