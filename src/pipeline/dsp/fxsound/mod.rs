//! pipeline/dsp/fxsound — FxSound 效果器移植（AGPL 来源）
//!
//! 移植自 FxSound（AGPL-3.0-or-later）：
//! - `Auralp.c` → Aural Enhancer
//! - `Maxi16.c` → Maximizer
//! - `Wide32.c` → Wide（立体声加宽 / Surround）
//! - Reverb → Dattorro 板式混响（v9.3 起按论文独立实现，不再来自 FxSound）
//!
//! 每个模块提供：
//! - 参数结构体与 EAPO 风格 `Key Value` 解析；
//! - `Filter` 实现（initialize 预分配、process RT 零分配）；
//! - 对应 `FilterFactory`，供 `pipeline/dsp/factory.rs` 注册。

pub mod aural;
pub mod maximizer;
pub mod reverb;
pub mod wide;

#[cfg(test)]
pub(crate) use test_support::{test_ctx, test_loader};

#[cfg(test)]
mod test_support {
    use crate::pipeline::dsp::filter::{ConfigLoader, DeviceType, DspContext, Filter, ProcessingStage};
    use std::collections::HashMap;

    pub(crate) fn test_ctx() -> DspContext {
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
            loudness_enabled: std::cell::Cell::new(true),
            rt_marker: std::marker::PhantomData,
        }
    }

    pub(crate) struct TestLoader;
    impl ConfigLoader for TestLoader {
        fn load_config(&self, _path: &str, _ctx: &DspContext) -> Vec<Box<dyn Filter>> {
            Vec::new()
        }
    }

    pub(crate) fn test_loader() -> TestLoader {
        TestLoader
    }
}
