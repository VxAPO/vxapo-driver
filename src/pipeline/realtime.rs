//! pipeline/realtime.rs — 实时安全基础设施（v6.3 规范）
//!
//! 提供 RT-safety 契约（contract.rs）与 SPSC 无锁环形缓冲（ring.rs）。

pub mod contract;
pub mod ring;
