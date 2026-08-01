//! install/device.rs — install/device 模块入口（v6.3 规范 5.1-5.4）
//!
//! 职责：设备 APO 查询子模块。
//! - endpoint：端点状态/名称查询（只读）
//! - format：WAVEFORMATEX 解析 + 通道掩码兜底（只读）
//! - slots：5 槽位读取 + 3 模式 + GUID 回退（只读）
//! - info：组合查询层（只读）
//!
//! 边界：不知道 pipeline/、config/。只负责设备 APO 的安装/卸载和设备查询。

pub mod endpoint;
pub mod format;
pub mod info;
pub mod slots;
