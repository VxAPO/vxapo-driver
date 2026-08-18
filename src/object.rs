//! object 模块入口（规范）。
//!
//! 边界：胶水层。Windows 加载 DLL 时创建的 COM 对象。允许依赖所有模块。

pub mod apo;
pub mod dll_exports;
pub mod factory;
pub mod ref_count;
pub mod vx_reg_props;
