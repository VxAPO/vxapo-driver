//! pipeline/dsp.rs — DSP 算法模块入口（v6.3 规范）
//!
//! 职责：Filter trait、工厂注册表、过渡混合。
//!
//! 滤波器与效果器（biquad/peq/gain/aural/reverb 等）平铺在 `pipeline/dsp/`
//! 子模块；工厂实现与注册集中在 `pipeline/dsp/factory.rs`。

pub mod aural;
pub mod biquad;
pub mod factory;
pub mod fir;
pub mod filter;
pub mod gain;
pub mod loudness;
pub mod math;
pub mod maximizer;
pub mod model;
pub mod peq_hybrid;
pub mod reverb;
pub mod transition;
pub mod wide;

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

#[cfg(test)]
pub(crate) use test_support::{test_ctx, test_loader};
