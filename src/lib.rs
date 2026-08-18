//! vxapo-driver/src/lib.rs — 库根模块
//!
//! VxAPO：Windows 音频处理对象（APO）驱动，提供系统级音频 DSP 处理能力。
//!
//! 模块结构（规范）：
//!
//! - [`sys`]：FFI 层（COM/注册表/音频定义）
//! - [`pipeline`]：音频处理管道（context/buffer/interleave/chain/process/dsp）
//! - [`install`]：设备 APO 安装/卸载
//! - [`config`]：配置文件解析
//! - [`object`]：胶水层（ApoObject COM 对象）
//! - [`telemetry`]：无锁日志 + panic hook
//! - [`utils`]：工具层

// ======================== Crate 级配置 ========================

// 禁止不安全代码的文档缺失（强制要求 SAFETY 注释）
#![deny(clippy::undocumented_unsafe_blocks)]

// Windows COM 接口沿用 PascalCase / SCREAMING_SNAKE_CASE 命名
#![allow(non_camel_case_types, non_snake_case)]

// ======================== 模块声明（规范） ========================

pub mod config;
pub mod install;
pub mod object;
pub mod pipeline;
pub mod sys;
pub mod telemetry;
pub mod utils;
