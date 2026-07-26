//! vxapo-driver/src/lib.rs — 库根模块
//!
//! VxAPO：Windows 音频处理对象（APO）驱动，提供系统级音频 DSP 处理能力。
//!
//! 模块层次（对应 Phase 1–9）：
//! - `com/`：COM 基础设施（ABI 定义、vtable、ClassFactory、注册属性）
//! - `instance/`：APO 实例生命周期（对象创建、初始化、格式协商、实时处理入口）
//! - `engine/`：实时处理引擎（过滤器链、缓冲区管理、配置解析、通道映射）
//! - `dsp/`：数字信号处理滤波器（纯 Rust，独立可测试）
//! - `device/`：设备查询层（端点枚举、槽位管理、格式解析）
//! - `installation/`：DLL 注册与安装（导出函数、注册表写入、权限提升、备份回滚）
//! - `realtime/`：实时安全基础设施（无锁环形缓冲区等）
//! - `telemetry/`：可观测性（无锁日志、panic 防御）
//! - `utils/`：共享工具（错误类型、对齐分配、注册表只读操作）

// ======================== Crate 级配置 ========================

// 禁止不安全代码的文档缺失（强制要求 SAFETY 注释）
#![deny(clippy::undocumented_unsafe_blocks)]

// Windows COM 接口沿用 PascalCase / SCREAMING_SNAKE_CASE 命名
#![allow(non_camel_case_types, non_snake_case)]

// ======================== 模块声明 ========================

// 顶层模块（对应 src/ 下的 .rs 文件）
pub mod utils;
pub mod com;
pub mod instance;
pub mod engine;
pub mod dsp;
pub mod realtime;
pub mod device;
pub mod installation;
pub mod telemetry;
