//! config 模块入口（v6.2 规范）。
//!
//! 边界：不知道 install/、object/。只负责解析配置文件，构建 Filter 链。
//! 不直接操作 Chain。

pub mod commands;
pub mod error;
pub mod parser;
pub mod watcher;

/// 配置解析专用错误类型（重导出自 error.rs，保持兼容）。
pub use error::ConfigError;