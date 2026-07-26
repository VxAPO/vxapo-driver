//! vxapo-driver/src/lib.rs — 库根模块
//!
//! VxAPO：Windows 音频处理对象（APO）驱动，提供系统级音频 DSP 处理能力。
//!
//! 模块结构：
//!
//! - [`dsp`]：DSP 算法模块。
//! - [`host`]：主机模块。
//! - [`pipeline`]：处理管道模块。
//! - [`sys`]：系统模块。
//! - [`utils`]：工具模块。
//! - [`test_helpers`]：测试辅助模块。
//!
// ======================== Crate 级配置 ========================

// 禁止不安全代码的文档缺失（强制要求 SAFETY 注释）
#![deny(clippy::undocumented_unsafe_blocks)]

// Windows COM 接口沿用 PascalCase / SCREAMING_SNAKE_CASE 命名
#![allow(non_camel_case_types, non_snake_case)]

// ======================== 模块声明 ========================

mod dsp;
mod host;
mod pipeline;
mod sys;
mod utils;

#[cfg(test)]
mod test_helpers;

