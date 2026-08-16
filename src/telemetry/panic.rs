//! telemetry/panic.rs — panic hook 安装（v6.3 规范 9.2）
//!
//! 职责：捕获 panic 信息写入 logger（RtViolation 级别），然后 `abort()`。
//!
//! 引用来源：
//! - `crate::telemetry::logger::Logger`
//!
//! 导出给：`object/dll_exports.rs`。

use std::sync::Once;

use crate::telemetry::logger::{LogLevel, Logger};

static INSTALL: Once = Once::new();

/// 安装全局 panic hook。
///
/// `logger` 必须是 `'static`——panic hook 是全局的，必须在程序整个生命周期内有效。
///
/// panic hook 行为：
/// 1. 捕获 panic 信息
/// 2. 写入 logger（RtViolation 级别）
/// 3. `abort()`
pub fn install_panic_hook(logger: &'static Logger) {
    // 幂等：DllGetClassObject 可能为 PreMix/PostMix 各调一次，只安装首个 hook。
    INSTALL.call_once(|| {
        std::panic::set_hook(Box::new(move |info| {
            let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
                format!("panic: {}", s)
            } else if let Some(s) = info.payload().downcast_ref::<String>() {
                format!("panic: {}", s)
            } else {
                format!("panic: {}", info)
            };
            logger.log(LogLevel::RtViolation, &msg);
            std::process::abort();
        }));
    });
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
//
// 由于 install_panic_hook 会 abort 进程，无法直接测试 hook 触发。
// 仅做一个 get_or_init 相关的不触发测试。
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    #[test]
    fn logger_static_can_be_created() {
        static LOGGER: OnceLock<Logger> = OnceLock::new();
        let logger = LOGGER.get_or_init(|| Logger::new(1024));
        logger.log(LogLevel::Info, "test logger");
        let mut count = 0;
        logger.drain(|_, _| count += 1);
        assert_eq!(count, 1);
    }
}