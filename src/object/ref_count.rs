//! object/ref_count.rs — INST_COUNT 原子计数（v6.3 规范 7.4）

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
