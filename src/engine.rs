//! 实时处理引擎
//!
//! 本模块下的处理函数运行在多媒体实时线程上。
//! 禁止堆分配、互斥锁、I/O、panic（Note 12）。

pub mod rt_contract;
pub mod context;
pub mod pipeline;
pub mod filter;
pub mod registry;
pub mod chain;
pub mod parser;
pub mod swap;
pub mod watcher;
pub mod channel;
pub mod deinterleave;
pub mod buffer;
pub mod transition;
pub mod commands;