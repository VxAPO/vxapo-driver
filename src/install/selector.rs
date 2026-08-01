//! install/selector.rs — 设备选择 + 安装/卸载入口聚合（v6.4 规范 5.5）
//!
//! 仅声明 `select` / `operation` 两个子模块，不含业务逻辑。
//!
//! - `select`：设备选择交互（枚举、列出、选择、调度安装/卸载）
//! - `operation`：安装/卸载执行 + 事务回滚

pub mod operation;
pub mod select;