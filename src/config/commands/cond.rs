//! config/commands/cond.rs — If:/ElseIf:/Else:/EndIf: 条件分支系统（v6.3 规范 6.5）

use std::collections::HashMap;

use crate::config::error::ConfigError;
use crate::config::parser::{ParseContext, ParseStage};

// ══════════════════════════════════════════════════════════════════════════════
// 类型
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

/// 条件栈。
pub type CondStack = Vec<CondState>;

/// 变量存储类型。
pub type Variables = HashMap<String, f64>;

// ══════════════════════════════════════════════════════════════════════════════
// 公开 API
// ══════════════════════════════════════════════════════════════════════════════

/// 检查当前是否在跳过状态（栈中任何一层 executing == false）。
pub fn is_skipping(stack: &[CondState]) -> bool {
    stack.iter().any(|s| !s.executing)
}

/// 处理 If: 命令。外层已跳过时不评估条件，直接压入 false。
pub fn handle_if(value: &str, ctx: &mut ParseContext) -> Result<(), ConfigError> {
    if is_skipping(&ctx.cond_stack) {
        // 外层正在跳过：不评估，直接压入 false 层。
        ctx.cond_stack.push(CondState {
            executing: false,
            had_true: false,
        });
        return Ok(());
    }

    let cond = eval_condition(value, ctx)?;
    ctx.cond_stack.push(CondState {
        executing: cond,
        had_true: cond,
    });
    Ok(())
}

/// 处理 ElseIf: 命令。已有 true 分支时跳过。
pub fn handle_elseif(value: &str, ctx: &mut ParseContext) -> Result<(), ConfigError> {
    // 外层是否在跳过（不含栈顶本身）。
    let outer_skipping = ctx.cond_stack.len() > 1
        && is_skipping(&ctx.cond_stack[..ctx.cond_stack.len() - 1]);

    let Some(top) = ctx.cond_stack.last_mut() else {
        return Err(syntax(ctx, "ElseIf without matching If"));
    };

    // 已有 true 分支 → 本层不再执行，不评估。
    if top.had_true {
        top.executing = false;
        return Ok(());
    }

    // 外层跳过 → 本层也不执行。
    if outer_skipping {
        top.executing = false;
        return Ok(());
    }

    // 评估条件（需要不可变借用 ctx，先释放可变借用）。
    let cond = eval_condition(value, ctx)?;
    let top = ctx.cond_stack.last_mut().unwrap();
    top.executing = cond;
    top.had_true = cond;
    Ok(())
}

/// 处理 Else: 命令。已有 true 分支时 executing = false。
pub fn handle_else(ctx: &mut ParseContext) -> Result<(), ConfigError> {
    // 先计算外层是否跳过（不借用栈顶）。
    let outer_skipping = ctx.cond_stack.len() > 1
        && is_skipping(&ctx.cond_stack[..ctx.cond_stack.len() - 1]);

    let Some(top) = ctx.cond_stack.last_mut() else {
        return Err(syntax(ctx, "Else without matching If"));
    };

    if top.had_true {
        top.executing = false;
    } else {
        // 外层跳过 → 本层也不执行。
        top.executing = !outer_skipping;
    }
    Ok(())
}

/// 处理 EndIf: 命令。弹出栈顶。
pub fn handle_endif(ctx: &mut ParseContext) -> Result<(), ConfigError> {
    if ctx.cond_stack.pop().is_none() {
        return Err(syntax(ctx, "EndIf without matching If"));
    }
    Ok(())
}

// ══════════════════════════════════════════════════════════════════════════════
// 条件求值
// ══════════════════════════════════════════════════════════════════════════════

/// 求值条件表达式。
///
/// 支持：true/false 字面量、取反（!）、比较运算、
/// device_type == render/capture、stage == premix/postmix/capture、
/// 单变量（非零为 true）。
pub fn eval_condition(expr: &str, ctx: &ParseContext) -> Result<bool, ConfigError> {
    let trimmed = expr.trim();
    if trimmed.is_empty() {
        return Err(syntax(ctx, "empty condition"));
    }

    // 字面量
    match trimmed {
        "true" => return Ok(true),
        "false" => return Ok(false),
        _ => {}
    }

    // 取反
    if let Some(rest) = trimmed.strip_prefix('!') {
        return Ok(!eval_condition(rest, ctx)?);
    }

    // 括号包裹
    if trimmed.starts_with('(') && trimmed.ends_with(')') {
        return eval_condition(&trimmed[1..trimmed.len() - 1], ctx);
    }

    // 比较运算
    for op in ["==", "!=", ">=", "<=", ">", "<"] {
        if let Some((lhs, rhs)) = split_once_op(trimmed, op) {
            let lhs = lhs.trim();
            let rhs = rhs.trim();
            return match op {
                "==" => Ok(compare_value(lhs, rhs, ctx)?),
                "!=" => Ok(!compare_value(lhs, rhs, ctx)?),
                ">=" | "<=" | ">" | "<" => {
                    let l = variable_or_number(lhs, ctx)?;
                    let r = variable_or_number(rhs, ctx)?;
                    Ok(match op {
                        ">=" => l >= r,
                        "<=" => l <= r,
                        ">" => l > r,
                        _ => l < r,
                    })
                }
                _ => unreachable!(),
            };
        }
    }

    // 单变量（非零为 true）
    if let Ok(v) = variable_or_number(trimmed, ctx) {
        return Ok(v != 0.0);
    }

    Err(syntax(ctx, &format!("cannot evaluate condition '{trimmed}'")))
}

// ══════════════════════════════════════════════════════════════════════════════
// 内部辅助
// ══════════════════════════════════════════════════════════════════════════════

/// 比较两侧值（支持 device_type / stage 字符串比较和数值比较）。
fn compare_value(lhs: &str, rhs: &str, ctx: &ParseContext) -> Result<bool, ConfigError> {
    // 特殊：device_type == render/capture
    if lhs == "device_type" {
        let target = rhs.to_ascii_lowercase();
        let actual = if ctx.is_capture { "capture" } else { "render" };
        return Ok(actual == target);
    }

    // 特殊：stage == premix/postmix/capture
    if lhs == "stage" {
        let target = rhs.to_ascii_lowercase();
        let actual = match ctx.stage {
            ParseStage::PreMix => "premix",
            ParseStage::PostMix => "postmix",
            ParseStage::Capture => "capture",
            ParseStage::None => "none",
        };
        return Ok(actual == target);
    }

    // 数值比较
    let l = variable_or_number(lhs, ctx)?;
    let r = variable_or_number(rhs, ctx)?;
    Ok(l == r)
}

/// 解析变量或数值字面量。
fn variable_or_number(s: &str, ctx: &ParseContext) -> Result<f64, ConfigError> {
    let s = s.trim();
    // 变量引用
    if let Some(v) = ctx.variables.get(s) {
        return Ok(*v);
    }
    // 内置常量
    match s {
        "pi" => return Ok(std::f64::consts::PI),
        "e" => return Ok(std::f64::consts::E),
        _ => {}
    }
    // 数值字面量
    s.parse::<f64>().map_err(|_| {
        syntax(ctx, &format!("cannot resolve value '{s}'"))
    })
}

/// 在字符串中查找运算符并分割（跳过括号内的运算符）。
fn split_once_op<'a>(s: &'a str, op: &str) -> Option<(&'a str, &'a str)> {
    let mut depth = 0usize;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + op.len() <= bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if depth == 0 && &s[i..i + op.len()] == op {
            return Some((&s[..i], &s[i + op.len()..]));
        }
        i += 1;
    }
    None
}

/// 构造语法错误。
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
    use crate::pipeline::dsp::filter::DspContext;
    use crate::pipeline::dsp::factory::FilterRegistry;
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
            rt_marker: std::marker::PhantomData,
        }));
        let registry: &'static FilterRegistry = Box::leak(Box::new(FilterRegistry::new()));
        let specs: &'static mut Vec<String> = Box::leak(Box::new(Vec::new()));
        ParseContext {
            filters,
            specs,
            registry,
            dsp_ctx: dsp,
            stage: ParseStage::None,
            is_capture: false,
            current_file: Path::new("<test>").to_path_buf(),
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
    fn is_skipping_empty() {
        assert!(!is_skipping(&Vec::new()));
    }

    #[test]
    fn cond_literals() {
        let ctx = test_ctx();
        assert!(eval_condition("true", &ctx).unwrap());
        assert!(!eval_condition("false", &ctx).unwrap());
        assert!(eval_condition("!false", &ctx).unwrap());
    }

    #[test]
    fn cond_comparisons() {
        let mut ctx = test_ctx();
        ctx.variables.insert("gain".into(), -3.0);
        assert!(eval_condition("gain == -3.0", &ctx).unwrap());
        assert!(eval_condition("gain != 0", &ctx).unwrap());
        assert!(eval_condition("gain < 0", &ctx).unwrap());
        assert!(eval_condition("gain >= -3.0", &ctx).unwrap());
    }

    #[test]
    fn cond_device_type() {
        let ctx = test_ctx();
        assert!(eval_condition("device_type == render", &ctx).unwrap());
        assert!(!eval_condition("device_type == capture", &ctx).unwrap());
    }

    #[test]
    fn cond_stage() {
        let mut ctx = test_ctx();
        ctx.stage = ParseStage::PreMix;
        assert!(eval_condition("stage == premix", &ctx).unwrap());
        assert!(!eval_condition("stage == postmix", &ctx).unwrap());
    }

    #[test]
    fn cond_variable_single() {
        let mut ctx = test_ctx();
        ctx.variables.insert("enabled".into(), 1.0);
        assert!(eval_condition("enabled", &ctx).unwrap());
        ctx.variables.insert("disabled".into(), 0.0);
        assert!(!eval_condition("disabled", &ctx).unwrap());
    }

    #[test]
    fn if_else_flow() {
        let mut ctx = test_ctx();
        handle_if("true", &mut ctx).unwrap();
        assert!(!is_skipping(&ctx.cond_stack));
        handle_else(&mut ctx).unwrap();
        assert!(is_skipping(&ctx.cond_stack));
        handle_endif(&mut ctx).unwrap();
        assert!(ctx.cond_stack.is_empty());
    }

    #[test]
    fn if_false_skips() {
        let mut ctx = test_ctx();
        handle_if("false", &mut ctx).unwrap();
        assert!(is_skipping(&ctx.cond_stack));
        handle_endif(&mut ctx).unwrap();
        assert!(ctx.cond_stack.is_empty());
    }

    #[test]
    fn elseif_after_true_skipped() {
        let mut ctx = test_ctx();
        handle_if("true", &mut ctx).unwrap();
        handle_elseif("true", &mut ctx).unwrap();
        assert!(is_skipping(&ctx.cond_stack));
    }

    #[test]
    fn unmatched_endif_errors() {
        let mut ctx = test_ctx();
        assert!(handle_endif(&mut ctx).is_err());
    }
}