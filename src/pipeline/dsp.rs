//! pipeline/dsp.rs — DSP 算法模块入口（v6.2 规范）
//!
//! 职责：Filter trait、工厂注册表、过渡混合。
//!
//! 具体滤波器（biquad/peq/gain 等）迁移到 `pipeline/dsp/`。

pub mod biquad;
pub mod convolution;
pub mod copy;
pub mod delay;
pub mod factory;
pub mod filter;
pub mod gain;
pub mod graphic_eq;
pub mod hp_lp;
pub mod loudness;
pub mod peq;
pub mod transition;
pub mod vst;

use crate::pipeline::dsp::copy::{parse_copy_ops, CopyFilter};
use crate::pipeline::dsp::factory::FilterRegistry;
use crate::pipeline::dsp::filter::{
    ConfigLoader, DspContext, FilterCreateResult, FilterFactory,
};

// ══════════════════════════════════════════════════════════════════════════════
// 内置工厂：Gain（Preamp: 命令）
// ══════════════════════════════════════════════════════════════════════════════

/// `Preamp:` 命令工厂 → GainFilter。
///
/// 语法：`Preamp: -6.0 dB`（可选 "dB" 后缀）。
#[derive(Debug)]
pub struct PreampFactory;

impl FilterFactory for PreampFactory {
    fn create_filter(
        &self,
        params: &str,
        _ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        let trimmed = params.trim();
        // 移除可选的 "dB" 后缀。
        let num = trimmed
            .strip_suffix("dB")
            .map(str::trim)
            .unwrap_or(trimmed);
        match num.parse::<f32>() {
            Ok(db) => FilterCreateResult::Filter(Box::new(gain::GainFilter::new(db))),
            Err(_) => FilterCreateResult::NoMatch,
        }
    }

    fn command_name(&self) -> &str {
        "Preamp"
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 内置工厂：Copy（Copy: 命令）
// ══════════════════════════════════════════════════════════════════════════════

/// `Copy:` 命令工厂 → CopyFilter。
///
/// 语法：`Copy: L2=L R2=R`
#[derive(Debug)]
pub struct CopyFactory;

impl FilterFactory for CopyFactory {
    fn create_filter(
        &self,
        params: &str,
        ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        match parse_copy_ops(params, &ctx.channel_names) {
            Some(ops) => FilterCreateResult::Filter(Box::new(CopyFilter::new(ops))),
            None => FilterCreateResult::NoMatch,
        }
    }

    fn command_name(&self) -> &str {
        "Copy"
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 内置工厂注册
// ══════════════════════════════════════════════════════════════════════════════

/// 注册所有内置 Filter 工厂到 FilterRegistry（v6.2 规范 4.10）。
///
/// 当前已注册：
/// - `Preamp:` → GainFilter
/// - `Copy:` → CopyFilter
///
/// 待注册（TODO 下一批）：Delay / GraphicEQ / Filter(PK/LP/HP/...) / Convolution / VST / Loudness。
pub fn register_builtin_filters(registry: &mut FilterRegistry) {
    registry.register(Box::new(PreampFactory));
    registry.register(Box::new(CopyFactory));
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::dsp::filter::{DeviceType, ProcessingStage};
    use std::collections::HashMap;

    fn test_ctx() -> DspContext {
        DspContext {
            sample_rate: 48000,
            channel_count: 2,
            channel_mask: 0x3,
            channel_names: vec!["L".into(), "R".into()],
            max_frame_count: 480,
            bits_per_sample: 32,
            device_type: DeviceType::Render,
            stage: ProcessingStage::None,
            variables: HashMap::new(),
        }
    }

    struct NullLoader;
    impl ConfigLoader for NullLoader {
        fn load_config(&self, _path: &str, _ctx: &DspContext) -> Vec<Box<dyn Filter>> {
            vec![]
        }
    }

    #[test]
    fn preamp_factory_parses_db() {
        let factory = PreampFactory;
        let result = factory.create_filter("-6.0 dB", &test_ctx(), &NullLoader);
        assert!(matches!(result, FilterCreateResult::Filter(_)));
    }

    #[test]
    fn preamp_factory_invalid_no_match() {
        let factory = PreampFactory;
        let result = factory.create_filter("oops", &test_ctx(), &NullLoader);
        assert!(matches!(result, FilterCreateResult::NoMatch));
    }

    #[test]
    fn copy_factory_parses_mapping() {
        let factory = CopyFactory;
        let result = factory.create_filter("L=R", &test_ctx(), &NullLoader);
        assert!(matches!(result, FilterCreateResult::Filter(_)));
    }

    #[test]
    fn copy_factory_invalid_no_match() {
        let factory = CopyFactory;
        let result = factory.create_filter("X=Y", &test_ctx(), &NullLoader);
        assert!(matches!(result, FilterCreateResult::NoMatch));
    }

    #[test]
    fn register_registers_known_factories() {
        let mut registry = FilterRegistry::new();
        register_builtin_filters(&mut registry);
        let names = registry.factory_names();
        assert!(names.contains(&"Preamp"));
        assert!(names.contains(&"Copy"));
    }
}