//! telemetry/logger.rs — 无锁环形日志，实时路径零堆分配（v6.3 规范 9.1）

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
pub struct Logger {
    ring: RingBuffer<LogEntry>,
}

impl Logger {
    /// 创建日志器（指定容量）。
    pub fn new(capacity: usize) -> Self {
        Self {
            ring: RingBuffer::new(capacity),
        }
    }

    /// 实时安全：无堆分配，无阻塞。消息超过 255 字节截断。
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_and_drain_roundtrip() {
        let logger = Logger::new(16);
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
        let logger = Logger::new(16);
        let long = "x".repeat(300);
        logger.log(LogLevel::Warning, &long);
        logger.drain(|_level, msg| {
            assert_eq!(msg.len(), 255);
        });
    }
}