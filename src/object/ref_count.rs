//! object/ref_count.rs — INST_COUNT 原子计数（v6.3 规范 7.4）

use std::sync::atomic::{AtomicU32, Ordering};

/// 全局活跃 APO 实例计数。
static INST_COUNT: AtomicU32 = AtomicU32::new(0);

/// 创建实例时递增。返回递增后的值。
pub fn increment() -> u32 {
    INST_COUNT.fetch_add(1, Ordering::SeqCst) + 1
}

/// 实例析构时递减。返回递减后的值。
pub fn decrement() -> u32 {
    let prev = INST_COUNT.fetch_sub(1, Ordering::SeqCst);
    prev - 1
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