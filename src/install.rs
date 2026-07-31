//! install 模块入口（v6.2 规范）。
//!
//! 边界：不知道 pipeline/、config/。只负责设备 APO 的安装/卸载和设备查询。

// ── v6.2 子模块（已迁移，第八步语义重构） ──
pub mod device;

// ── 待第八步迁移的子模块 ──
// pub mod selector;
// pub mod audiodg;
// pub mod install;
// pub mod rollback;