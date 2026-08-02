//! config/commands/rew.rs — REW 导出格式（v6.3 规范 6.15，v7.4 修订）
//!
//! 语法：`Filter 1: ON PK Fc 50,0 Hz Gain -10,0 dB Q 2,50`
//!
//! 语义：解析 REW Room EQ V5 格式行（含逗号小数点），转换为标准参数后通过
//! `registry.try_create("PK", ...)` 创建。v7.4 修订：`Filter N:` 前缀由
//! `config/parser.rs` 分发层剥离（`split_command_value`），本 handle 直接收
//! 冒号后的值（`ON PK ...`）。
//!
//! REW 使用逗号作为小数点分隔符，需先转换为标准格式。

use crate::config::error::ConfigError;
use crate::config::parser::ParseContext;
use crate::pipeline::dsp::factory::OutcomeKind;

/// 处理 REW 格式参数。
///
/// 入参为 `split_command_value` 剥离后的冒号后值（`ON TYPE 参数...`）。
/// 例：`Filter 1: ON PK Fc 50,0 Hz Gain -10,0 dB Q 2,50` → 参数
/// `ON PK Fc 50,0 Hz Gain -10,0 dB Q 2,50`。
pub fn handle(value: &str, ctx: &mut ParseContext) -> Result<(), ConfigError> {
    let trimmed = value.trim();

    // 解析 ON/OFF 开关。
    let (enabled, params) = if let Some(r) = trimmed.strip_prefix("ON").map(str::trim_start) {
        (true, r)
    } else if let Some(r) = trimmed.strip_prefix("OFF").map(str::trim_start) {
        (false, r)
    } else {
        // 省略开关：默认 ON。
        (true, trimmed)
    };

    // OFF：创建 PassthroughFilter（与 6.14 Filter: OFF 语义一致）。
    if !enabled {
        ctx.filters.push(Box::new(crate::pipeline::dsp::filter::PassthroughFilter));
        return Ok(());
    }

    if params.is_empty() {
        return Err(syntax(ctx, "REW: missing filter parameters"));
    }

    // 转换逗号小数点为标准句点（50,0 → 50.0）。
    let normalized = params.replace(',', ".");

    let outcome = ctx
        .registry
        .try_create(&normalized, ctx.dsp_ctx, &crate::config::parser::NullConfigLoader);

    match outcome.result {
        OutcomeKind::FilterAdded(f) => {
            ctx.filters.push(f);
            Ok(())
        }
        OutcomeKind::MatchedNoFilter => Ok(()),
        OutcomeKind::Aborted => {
            ctx.abort_file = true;
            Ok(())
        }
        OutcomeKind::Unmatched => Err(syntax(
            ctx,
            &format!("REW: cannot parse '{normalized}'"),
        )),
    }
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
    use crate::config::parser::{ParseContext, ParseStage};
    use crate::pipeline::dsp::filter::DspContext;
    use crate::pipeline::dsp::factory::{FilterRegistry, register_builtin_filters};
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
            rt_marker: std::marker::PhantomData,
        }));
        // 需要注册 DSP 工厂才能 try_create 成功。
        let mut reg = FilterRegistry::new();
        register_builtin_filters(&mut reg);
        let registry: &'static FilterRegistry = Box::leak(Box::new(reg));
        ParseContext {
            filters,
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
    fn comma_decimal_normalization() {
        let original = "PK Fc 50,0 Hz Gain -10,0 dB Q 2,50";
        let normalized = original.replace(',', ".");
        assert_eq!(normalized, "PK Fc 50.0 Hz Gain -10.0 dB Q 2.50");
    }

    #[test]
    fn handle_parses_on_pk() {
        let mut ctx = test_ctx();
        handle("ON PK Fc 50,0 Hz Gain -10,0 dB Q 2,50", &mut ctx).unwrap();
        assert_eq!(ctx.filters.len(), 1);
    }

    #[test]
    fn handle_off_creates_passthrough() {
        let mut ctx = test_ctx();
        handle("OFF", &mut ctx).unwrap();
        assert_eq!(ctx.filters.len(), 1);
        assert!(format!("{:?}", ctx.filters[0]).contains("PassthroughFilter"));
    }

    #[test]
    fn handle_empty_errors() {
        let mut ctx = test_ctx();
        assert!(handle("", &mut ctx).is_err());
    }
}
