//! object/ref_count.rs — INST_COUNT 原子计数（规范 7.4）

use std::sync::atomic::{AtomicU32, Ordering};

/// 全局活跃 APO 实例计数。
static INST_COUNT: AtomicU32 = AtomicU32::new(0);

/// 创建实例时递增。返回递增后的值。
pub fn increment() -> u32 {
    INST_COUNT.fetch_add(1, Ordering::SeqCst) + 1
}

/// 实例析构时递减。返回递减后的值。
///
/// 使用 saturating_sub：INST_COUNT 仅用于统计（DllCanUnloadNow），
/// 生产路径由 COM 引用计数保证不会重复 drop；测试并行时各测试
/// 的 `reset_for_test()` 会交错清零全局计数，若此时仍有 ApoObject
/// 存活 drop，裸 `prev - 1` 会在 prev=0 时 u32 下溢 panic（flaky）。
pub fn decrement() -> u32 {
    let mut prev = INST_COUNT.load(Ordering::SeqCst);
    loop {
        if prev == 0 {
            // 并行测试 reset 交错时保护：计数已为 0，不再下溢为 u32::MAX。
            return 0;
        }
        match INST_COUNT.compare_exchange(prev, prev - 1, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return prev - 1,
            Err(actual) => prev = actual,
        }
    }
}

/// 读取当前活跃实例数。
pub fn get() -> u32 {
    INST_COUNT.load(Ordering::SeqCst)
}

/// 检查是否没有活跃实例。
pub fn is_zero() -> bool {
    get() == 0
}

/// 进程级全局状态的**测试串行化锁**。
///
/// `INST_COUNT`（本模块）与 `LOCK_COUNT`（`object/factory.rs`）都是进程级全局量，
/// 而被**两个不同测试模块**（`object/dll_exports.rs`、`object/factory.rs`）的用例
/// 读写；这些用例各自先 `reset_for_test()` 再断言精确值。并行执行时一个用例的 reset
/// 会清掉另一个刚刚建立的计数，产生低频假失败（`left == right` 不匹配）。
///
/// 因此锁必须放在**共享位置**（本模块）而非各测试模块内——放在各自模块里，
/// 跨模块的用例之间依然会互相干扰。
///
/// 用法：任何读写 `INST_COUNT` / `LOCK_COUNT` 的测试，第一条语句取此锁。
#[cfg(test)]
pub(crate) static GLOBAL_STATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 取得全局状态锁；前序用例 panic 污染锁时沿用内部值继续（与仓库其余测试惯例一致）。
#[cfg(test)]
pub(crate) fn serial_lock() -> std::sync::MutexGuard<'static, ()> {
    GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
pub fn reset_for_test() {
    INST_COUNT.store(0, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn increment_decrement_cycle() {
        reset_for_test();
        assert_eq!(increment(), 1);
        assert_eq!(increment(), 2);
        assert_eq!(decrement(), 1);
        assert_eq!(decrement(), 0);
        assert!(is_zero());
    }
}
