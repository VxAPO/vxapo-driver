//! telemetry/logger.rs — 无锁环形日志，实时路径零堆分配（v6.2 规范 9.1）
//!
//! 职责：提供实时安全的有界日志写入。RT 线程调用 `log` 时无堆分配、无阻塞。
//!
//! 引用来源：
//! - `crate::pipeline::realtime::ring::RingBuffer`
//!
//! 导出给：`pipeline/`、`object/`。
//!
//! 注意：当前 `pipeline/realtime/ring.rs` 为编译期容量版本（`RingBuffer<T, N>`）。
//! 第八步 pipeline 核心重写时将按规范 v6.2 切换为运行时容量 API，此文件同步调整。

use crate::pipeline::realtime::ring::RingBuffer;

/// 日志级别。
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warning,
    Error,
    RtViolation,
}

/// 环形日志条目（定长，RT 安全）。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LogEntry {
    pub level: LogLevel,
    pub len: u8,
    pub msg: [u8; 256],
}

impl Default for LogEntry {
    fn default() -> Self {
        Self {
            level: LogLevel::Debug,
            len: 0,
            msg: [0u8; 256],
        }
    }
}

/// 无锁环形日志器。
///
/// RT 线程调用 `log`，非 RT 线程通过 `drain` 批量消费。
pub struct Logger {
    ring: RingBuffer<LogEntry, 1024>,
}

impl Logger {
    /// 创建日志器（1024 条容量）。
    pub fn new(_capacity: usize) -> Self {
        Self {
            ring: RingBuffer::new(),
        }
    }

    /// 实时安全：无堆分配，无阻塞。
    ///
    /// 消息超过 255 字节时静默截断。
    pub fn log(&self, level: LogLevel, msg: &str) {
        let bytes = msg.as_bytes();
        let len = bytes.len().min(255);
        let mut entry = LogEntry {
            level,
            len: len as u8,
            msg: [0u8; 256],
        };
        entry.msg[..len].copy_from_slice(&bytes[..len]);
        let _ = self.ring.push(entry);
    }

    /// 非实时：批量读取日志。
    pub fn drain<F>(&self, mut f: F)
    where
        F: FnMut(LogLevel, &str),
    {
        while let Some(entry) = self.ring.pop() {
            let len = entry.len as usize;
            let msg = core::str::from_utf8(&entry.msg[..len]).unwrap_or("<invalid utf8>");
            f(entry.level, msg);
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_and_drain_roundtrip() {
        let logger = Logger::new(1024);
        logger.log(LogLevel::Info, "hello");
        logger.log(LogLevel::Error, "world");

        let mut collected = Vec::new();
        logger.drain(|level, msg| collected.push((level, msg.to_owned())));

        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0], (LogLevel::Info, "hello".to_owned()));
        assert_eq!(collected[1], (LogLevel::Error, "world".to_owned()));
    }

    #[test]
    fn long_message_truncated_to_255() {
        let logger = Logger::new(1024);
        let long = "x".repeat(300);
        logger.log(LogLevel::Warning, &long);

        logger.drain(|_level, msg| {
            assert_eq!(msg.len(), 255);
        });
    }

    #[test]
    fn empty_log_drains_nothing() {
        let logger = Logger::new(1024);
        let mut count = 0;
        logger.drain(|_, _| count += 1);
        assert_eq!(count, 0);
    }

    #[test]
    fn rt_violation_level_roundtrip() {
        let logger = Logger::new(1024);
        logger.log(LogLevel::RtViolation, "rt violation in process");
        logger.drain(|level, msg| {
            assert_eq!(level, LogLevel::RtViolation);
            assert_eq!(msg, "rt violation in process");
        });
    }

    #[test]
    fn log_level_values() {
        assert_eq!(LogLevel::Debug as u8, 0);
        assert_eq!(LogLevel::Info as u8, 1);
        assert_eq!(LogLevel::Warning as u8, 2);
        assert_eq!(LogLevel::Error as u8, 3);
        assert_eq!(LogLevel::RtViolation as u8, 4);
    }
}