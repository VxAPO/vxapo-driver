//! telemetry 模块入口（规范）。
//!
//! 职责：无锁环形日志 + panic hook，实时路径零堆分配。

pub mod logger;
pub mod panic;
