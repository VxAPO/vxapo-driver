//! pipeline/realtime.rs — 实时安全基础设施（规范）
//!
//! 提供 RT-safety 契约（contract.rs）。
//! SPSC 无锁环形缓冲位于 `utils/ring.rs`（telemetry 与 pipeline 共用，不下沉到 pipeline）。

pub mod contract;
