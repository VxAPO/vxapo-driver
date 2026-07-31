//! pipeline/dsp.rs — DSP 算法模块入口（v6.2 规范）
//!
//! 职责：Filter trait、工厂注册表、过渡混合。
//!
//! 具体滤波器（biquad/peq/gain 等）迁移到 `pipeline/dsp/` 已有文件，
//! 但尚未与新版 Filter trait / DspContext 完全对齐。待第八步后续批次完成。
//!
//! register_builtin_filters 将在滤波器 API 对齐后注册（见 config/commands.rs 的 register_all_commands）。

pub mod factory;
pub mod filter;
pub mod transition;

/// 注册所有内置 Filter 工厂到 FilterRegistry（v6.2 规范 4.10）。
///
/// TODO(v6.2): 滤波器迁移完成后，在 config/commands.rs 中通过
/// register_all_commands 调用此函数注册内置 DSP 工厂。
pub fn register_builtin_filters(_registry: &mut factory::FilterRegistry) {
    // 待滤波器 API 与新版 Filter trait 对齐后实现
}