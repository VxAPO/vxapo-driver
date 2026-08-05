//! config/commands/loudness.rs — Loudness: 命令（响度补偿开关）
//!
//! 语法：`Loudness: on|off`（默认 on）。
//!
//! APP 未来通过该接口控制补偿；`off` = 无补偿（LoudnessCorrection 直通）。
//! 命令本身不产生滤波器，但会产出 spec 指纹，保证热重载能感知开关变化。

use crate::config::error::ConfigError;
use crate::config::parser::ParseContext;

/// 处理 `Loudness: on|off`。
pub fn handle(value: &str, ctx: &mut ParseContext) -> Result<(), ConfigError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "1" => {
            ctx.dsp_ctx.loudness_enabled.set(true);
            Ok(())
        }
        "off" | "false" | "0" => {
            ctx.dsp_ctx.loudness_enabled.set(false);
            Ok(())
        }
        _ => Err(ConfigError::SyntaxError {
            file: ctx.current_file.display().to_string(),
            line: ctx.line_number,
            message: "Loudness: 需要 on 或 off".to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parser::{ParseContext, ParseStage};
    use crate::pipeline::dsp::filter::{DeviceType, DspContext, ProcessingStage};
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
            device_type: DeviceType::Render,
            stage: ProcessingStage::None,
            variables: HashMap::new(),
            loudness_enabled: std::cell::Cell::new(true),
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
    fn on_sets_enabled_true() {
        let mut ctx = test_ctx();
        ctx.dsp_ctx.loudness_enabled.set(false);
        handle("on", &mut ctx).unwrap();
        assert!(ctx.dsp_ctx.loudness_enabled.get());
    }

    #[test]
    fn off_sets_enabled_false() {
        let mut ctx = test_ctx();
        handle("off", &mut ctx).unwrap();
        assert!(!ctx.dsp_ctx.loudness_enabled.get());
    }

    #[test]
    fn invalid_value_errors() {
        let mut ctx = test_ctx();
        assert!(handle("maybe", &mut ctx).is_err());
    }
}
