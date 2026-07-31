//! install 模块入口（v6.2 规范）。
//!
//! 边界：不知道 pipeline/、config/。只负责设备 APO 的安装/卸载和设备查询。
//!
//! 结构：
//! - `device`：设备查询子模块（只读，endpoint/format/slots/info）
//! - `selector`：设备选择 + 安装/卸载/回滚（原 install+rollback 合并）
//! - `audiodg`：audiodg 进程刷新

pub mod audiodg;
pub mod device;
pub mod selector;