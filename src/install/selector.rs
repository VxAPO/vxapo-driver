//! install/selector.rs — 设备选择 + 安装/卸载入口聚合（规范 5.5）
//!
//! 仅声明 `operation` 子模块，不含业务逻辑。
//!
//! - `operation`：安装/卸载执行 + 事务回滚
//!
//! 交互式选择流程已由 CLI 自持（删除 `select` 死代码）。

pub mod operation;
