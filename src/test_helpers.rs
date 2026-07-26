//! test_helpers.rs — 测试辅助工具
//!
//! 提供跨模块共享的测试基础设施，避免各模块重复定义。
//! 仅在 `#[cfg(test)]` 条件下编译，不进入 release 产物。
//!
//! 此模块为纯测试工具层，不依赖任何 Windows API，可在非 Windows 环境下使用。

/// 全局序列化锁。
///
/// 用于序列化依赖全局状态（注册表键、原子计数器、RT 标志等）的测试，
/// 防止并行执行时互相污染。
///
/// # 使用
///
/// ```ignore
/// use crate::utils::test_helpers::serial_lock;
///
/// #[test]
/// fn my_test() {
///     let _l = serial_lock();
///     // ... 测试代码 ...
/// }
/// ```
///
/// # Poisoned 处理
///
/// 如果持有锁的测试 panic（`should_panic` 测试），mutex 会被 poisoned。
/// `serial_lock` 通过 `unwrap_or_else(|e| e.into_inner())` 自动恢复，
/// 后续测试不会因此 panic。
pub fn serial_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}