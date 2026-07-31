//! pipeline/realtime.rs — 实时安全基础设施（v6.2 规范）
//!
//! 提供 SPSC 无锁环形缓冲（ring.rs），供 telemetry/logger.rs 使用。

pub mod ring;