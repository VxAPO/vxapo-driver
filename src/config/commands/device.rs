//! config/commands/device.rs — Device: 命令（v6.3 规范 6.6）
//!
//! 语法：
//! - `Device: <path>` — 绑定配置到指定设备路径
//! - `Device: AbortFile` — 终止当前文件解析（不终止 Include 的父文件）
//!
//! 语义：`AbortFile` 设置 `ctx.abort_file = true` 并返回 `Err(ConfigError::AbortFile)`。
//! 设备路径匹配由调用方在解析前通过 `DspContext.device_type` 预处理。

use crate::config::error::ConfigError;
use crate::config::parser::ParseContext;

/// Device: 命令的 AbortFile 关键字。
pub const ABORT_FILE: &str = "AbortFile";

/// 处理 Device: 命令。
///
/// - `value == "AbortFile"`：设置 `abort_file` 并返回 `AbortFile` 错误。
/// - 其他值：设备路径匹配（记录到上下文，由上层设备过滤）。
pub fn handle(value: &str, ctx: &mut ParseContext) -> Result<(), ConfigError> {
    let trimmed = value.trim();

    if trimmed.eq_ignore_ascii_case(ABORT_FILE) {
        ctx.abort_file = true;
        return Err(ConfigError::AbortFile);
    }

    if trimmed.is_empty() {
        return Err(syntax(ctx, "Device: requires a device path or AbortFile"));
    }

    // 非 AbortFile 的设备路径：记录到上下文（后续批次扩展）。
    ctx.current_device = Some(trimmed.to_owned());
    Ok(())
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
            rt_marker: std::marker::PhantomData,
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
    fn abort_file_sets_flag_and_errors() {
        let mut ctx = test_ctx();
        let result = handle(ABORT_FILE, &mut ctx);
        assert!(result.is_err());
        assert!(ctx.abort_file);
        assert!(matches!(result, Err(ConfigError::AbortFile)));
    }

    #[test]
    fn device_path_sets_current_device() {
        let mut ctx = test_ctx();
        handle("PCI\\VEN_8086", &mut ctx).unwrap();
        assert_eq!(ctx.current_device.as_deref(), Some("PCI\\VEN_8086"));
    }

    #[test]
    fn empty_value_errors() {
        let mut ctx = test_ctx();
        assert!(handle("", &mut ctx).is_err());
    }
}
