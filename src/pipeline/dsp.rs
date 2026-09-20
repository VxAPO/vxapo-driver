//! pipeline/dsp.rs — DSP 算法模块入口（规范）
//!
//! 职责：Filter trait、工厂注册表、过渡混合。
//!
//! 滤波器与效果器（biquad/peq/gain/aural/reverb 等）平铺在 `pipeline/dsp/`
//! 子模块；工厂实现与注册集中在 `pipeline/dsp/factory.rs`。

pub mod aural;
pub mod biquad;
pub mod compressor;
pub mod factory;
pub mod fir;
pub mod filter;
pub mod gain;
pub mod loudness;
pub mod math;
pub mod model;
pub mod peq_hybrid;
pub mod reverb;
pub mod specs;
pub mod transition;
pub mod wide;
