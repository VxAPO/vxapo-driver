//! config/commands.rs — 命令工厂入口（v6.3 规范 6.3）
//!
//! 注册所有命令工厂到 FilterRegistry。
//!
//! 工厂优先级：`config/` 注册的工厂排在 `pipeline/dsp/` 工厂之前
//! （`Device:` / `If:` 等需优先匹配）。

pub mod channel;
pub mod cond;
pub mod copy;
pub mod delay;
pub mod device;
pub mod expr;
pub mod filter;
pub mod graphic;
pub mod include;
pub mod preamp;
pub mod rew;
pub mod stage;

use crate::pipeline::dsp::factory::FilterRegistry;

/// 注册所有命令工厂和内置 DSP 过滤器工厂。
///
/// DSP 工厂由 pipeline/dsp 统一注册（完全下沉）。
pub fn register_all_commands(registry: &mut FilterRegistry) {
    // DSP 工厂（pipeline/dsp.rs 的 register_builtin_filters：Preamp/Copy 已注册）
    crate::pipeline::dsp::register_builtin_filters(registry);

    // 注：纯配置语义工厂（Device/If/Stage/Channel/Eval/Include/GraphicEQ/Delay/Filter/REW）
    // 由 parser.rs 直接按命令名分发（handle_* 函数），无需注册到 FilterRegistry。
}
