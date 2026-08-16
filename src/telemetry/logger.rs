//! telemetry/logger.rs — 无锁环形日志，实时路径零堆分配（v6.3 规范 9.1）

use std::sync::OnceLock;

use crate::pipeline::realtime::ring::RingBuffer;

/// 进程级日志器（DllGetClassObject 首次调用时惰性初始化）。
pub static LOGGER: OnceLock<Logger> = OnceLock::new();

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

    /// 惰性初始化进程级日志器并安装到 `log` crate。
    ///
    /// 首次调用发生在 `DllGetClassObject`（Loader Lock 之外），不在 DllMain。
    /// 重复调用安全：`set_logger` 失败仅忽略（已安装时）。
    pub fn install() -> &'static Logger {
        let logger = LOGGER.get_or_init(|| Logger::new(4096));
        let _ = log::set_logger(logger);
        log::set_max_level(log::LevelFilter::Debug);
        logger
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

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        // 安装时全局 max level 已设为 Debug；这里对标准级别放行。
        metadata.level() <= log::Level::Debug
    }

    fn log(&self, record: &log::Record<'_>) {
        let level = match record.level() {
            log::Level::Error => LogLevel::Error,
            log::Level::Warn => LogLevel::Warning,
            log::Level::Info => LogLevel::Info,
            log::Level::Debug | log::Level::Trace => LogLevel::Debug,
        };
        // `log` 宏不进入 RT 热路径（RT 诊断走 record_rt_call 定长环形），此处允许分配。
        self.log(level, &format!("{}: {}", record.target(), record.args()));
    }

    fn flush(&self) {}
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