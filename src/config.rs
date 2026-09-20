//! config 模块入口（规范）。
//!
//! 边界：不知道 install/、object/。只负责解析配置文件，构建 Filter 链。
//! 不直接操作 Chain。

pub mod error;
pub mod model;
pub mod parser;
pub mod watcher;
