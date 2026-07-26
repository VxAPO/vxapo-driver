//! instance/ref_count.rs — 活跃实例原子计数（Note 2）
//!
//! `INST_COUNT` 原子计数器，追踪当前存活的 APO COM 对象实例数。
//! 与 `com/factory.rs` 中的 `LOCK_COUNT`（客户端显式锁定）配合使用。
//!
//! `DllCanUnloadNow` 判定条件：`INST_COUNT == 0 && LOCK_COUNT == 0` 时返回 `S_OK`。
//!
//! 计数规则：
//! - `ClassFactory::CreateInstance` 成功后 +1
//! - APO 对象 `Release` 引用计数归零、析构时 -1
//! - 初始值为 0
//!
//! 此模块仅提供原子计数器操作，不包含引用计数实现逻辑。

use std::sync::atomic::{AtomicU32, Ordering};

/// 全局活跃 APO 实例计数。
///
/// - 原子操作保证线程安全
/// - `SeqCst` 顺序确保与 `LOCK_COUNT` 的读取一致
/// - 非实时路径操作，无实时安全约束
static INST_COUNT: AtomicU32 = AtomicU32::new(0);

// ══════════════════════════════════════════════════════════════════════════════
// 计数操作
// ══════════════════════════════════════════════════════════════════════════════

/// 创建实例时递增计数。
///
/// 调用时机：`ClassFactory::CreateInstance` 成功返回前。
///
/// 返回递增后的值（用于调试日志）。
pub fn increment() -> u32 {
    INST_COUNT.fetch_add(1, Ordering::SeqCst) + 1
}

/// 实例析构时递减计数。
///
/// 调用时机：APO 对象的最后一个 `Release` 调用、引用计数归零后。
///
/// 返回递减后的值（用于调试日志）。
///
/// # Safety 契约
///
/// 每次 `increment` 必须有且仅有一次对应的 `decrement`。
/// 不得将计数减到负数——这表示 `increment` / `decrement` 配对出错。
pub fn decrement() -> u32 {
    let prev = INST_COUNT.fetch_sub(1, Ordering::SeqCst);
    prev - 1
}

/// 读取当前活跃实例数。
///
/// 用于 `DllCanUnloadNow` 判断（与 `LOCK_COUNT` 联合检查）。
pub fn get() -> u32 {
    INST_COUNT.load(Ordering::SeqCst)
}

/// 检查是否没有活跃实例。
///
/// `DllCanUnloadNow` 中：`is_zero() && lock_count::is_zero()` → 返回 `S_OK`。
pub fn is_zero() -> bool {
    get() == 0
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试辅助（仅 test profile）
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
/// 测试专用：重置计数器到 0。
///
/// **仅用于测试**——生产代码不得调用。
pub fn reset_for_test() {
    INST_COUNT.store(0, Ordering::SeqCst);
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    use crate::utils::test_helpers::serial_lock;

    #[test]
    fn initial_state() {
        let _l = serial_lock();
        reset_for_test();
        assert_eq!(get(), 0);
        assert!(is_zero());
    }

    #[test]
    fn single_increment() {
        let _l = serial_lock();
        reset_for_test();
        let count = increment();
        assert_eq!(count, 1);
        assert_eq!(get(), 1);
        assert!(!is_zero());
    }

    #[test]
    fn increment_decrement_cycle() {
        let _l = serial_lock();
        reset_for_test();
        assert_eq!(increment(), 1);
        assert_eq!(increment(), 2);
        assert_eq!(increment(), 3);
        assert_eq!(get(), 3);

        assert_eq!(decrement(), 2);
        assert_eq!(decrement(), 1);
        assert_eq!(decrement(), 0);
        assert!(is_zero());
    }

    #[test]
    fn multiple_cycles() {
        let _l = serial_lock();
        reset_for_test();
        for _ in 0..100 {
            increment();
        }
        assert_eq!(get(), 100);
        for _ in 0..100 {
            decrement();
        }
        assert!(is_zero());
    }

    #[test]
    fn concurrent_increments() {
        let _l = serial_lock();
        reset_for_test();
        let handles: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| {
                    for _ in 0..1000 {
                        increment();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(get(), 8000);

        // 清理
        for _ in 0..8000 {
            decrement();
        }
        assert!(is_zero());
    }

    #[test]
    fn concurrent_decrements() {
        let _l = serial_lock();
        reset_for_test();
        for _ in 0..8000 {
            increment();
        }

        let handles: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| {
                    for _ in 0..1000 {
                        decrement();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert!(is_zero());
    }

    #[test]
    fn mixed_concurrent() {
        let _l = serial_lock();
        reset_for_test();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));

        let mut handles = Vec::new();

        // 8 个线程各 increment 500
        for _ in 0..8 {
            let b = barrier.clone();
            handles.push(std::thread::spawn(move || {
                b.wait();
                for _ in 0..500 {
                    increment();
                }
            }));
        }

        // 先 join increment，再 decrement
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(get(), 4000);

        let handles: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| {
                    for _ in 0..500 {
                        decrement();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert!(is_zero());
    }

    #[test]
    fn return_value_reflects_count() {
        let _l = serial_lock();
        reset_for_test();
        assert_eq!(increment(), 1);
        assert_eq!(increment(), 2);
        assert_eq!(decrement(), 1);
        assert_eq!(decrement(), 0);
    }
}