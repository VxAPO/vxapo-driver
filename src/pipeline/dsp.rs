//! pipeline/dsp.rs — DSP 算法模块入口（v6.3 规范）
//!
//! 职责：Filter trait、工厂注册表、过渡混合。
//!
//! 具体滤波器（biquad/peq/gain 等）在 `pipeline/dsp/` 子模块。
//! 工厂实现与注册集中在 `pipeline/dsp/factory.rs`。

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

/// 注册所有内置 Filter 工厂到 FilterRegistry（v6.3 规范 4.10）。
///
/// 实现位于 `factory::register_builtin_filters`——这里是重导出，
/// 供 `config/commands.rs` 的 `register_all_commands` 调用。
pub use factory::register_builtin_filters;