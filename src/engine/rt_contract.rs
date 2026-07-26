//! engine/rt_contract.rs — 实时安全（RT-safety）契约（Note 12/58）
//!
//! `engine/` 模块中的处理函数运行在 Windows 多媒体实时线程上。
//! 任何违反 RT-safety 的操作都可能导致音频卡顿（glitch）甚至进程死锁。
//!
//! # RT-safety 规则
//!
//! 在实时路径（`Filter::process`、`pipeline.rs` 的 `process`、`chain.rs` 的遍历）中：
//!
//! 1. 禁止堆分配：不得调用 `Vec::push`、`Vec::clone`、`.to_vec()`、`.to_string()`、
//!    `format!()`、`Box::new()`、`HashMap::insert()`、`String::new()`（Note 58）
//! 2. 禁止互斥锁：不得调用 `Mutex::lock()`、`RwLock::read()`、`RwLock::write()`
//! 3. 禁止 I/O：不得进行文件读写、网络操作、注册表访问
//! 4. 禁止 panic：不得使用 `unwrap()`、`expect()`、`assert!()`、索引越界（Note 57）
//! 5. 所有缓冲区在初始化时预分配：实时路径只操作已分配的内存
//! 6. 混合函数使用裸指针：避免借用检查器在热路径中引入额外开销
//!
//! # 实现策略
//!
//! - 编译期：`RtSafe` marker trait 通过类型系统标记 RT 安全的类型
//! - Debug 运行时：`RtGuard` 在 debug 模式下跟踪 RT 上下文，捕获违规行为
//! - Release 运行时：零开销，所有 debug 检查由 `cfg(debug_assertions)` 消除
//!
//! 此模块为纯 trait 与 marker 类型定义，不包含运行时处理逻辑。

use std::sync::atomic::{AtomicBool, Ordering};

// ══════════════════════════════════════════════════════════════════════════════
// RtSafe marker trait
// ══════════════════════════════════════════════════════════════════════════════

/// 标记类型的所有方法满足 RT-safety 约束。
///
/// # Safety
///
/// 实现者必须保证类型的所有 `&self` / `&mut self` 方法：
/// - 不进行堆分配（包括隐式分配，如 `Vec::clone()`）
/// - 不获取任何锁（`Mutex`、`RwLock`、`std::sync::Once`）
/// - 不执行 I/O（文件、网络、注册表）
/// - 不 panic（无 `unwrap()`、`expect()`、越界索引）
/// - 不调用任何非 `RtSafe` 标记的方法
///
/// `initialize` / `new` 等构造方法不受此约束——它们在非实时路径中调用。
///
/// 违反上述保证将导致音频卡顿或进程死锁，属于未定义行为（逻辑层面）。
pub unsafe trait RtSafe {}

/// 标记类型可以在实时路径中使用（值语义，无副作用）。
///
/// 适用于：`f32`、`usize`、`bool`、`[f32; N]` 等 POD 类型。
///
/// # Safety
///
/// 类型必须是 `Copy`，不包含任何非 `RtSafe` 的字段。
pub unsafe trait RtCopy: Copy {}

// 基础数值类型全部标记为 RtCopy
unsafe impl RtCopy for f32 {}
unsafe impl RtCopy for f64 {}
unsafe impl RtCopy for i8 {}
unsafe impl RtCopy for i16 {}
unsafe impl RtCopy for i32 {}
unsafe impl RtCopy for i64 {}
unsafe impl RtCopy for u8 {}
unsafe impl RtCopy for u16 {}
unsafe impl RtCopy for u32 {}
unsafe impl RtCopy for u64 {}
unsafe impl RtCopy for usize {}
unsafe impl RtCopy for isize {}
unsafe impl RtCopy for bool {}

// ══════════════════════════════════════════════════════════════════════════════
// RT 上下文跟踪（Debug 模式）
// ══════════════════════════════════════════════════════════════════════════════

/// 全局 RT 上下文标志（仅 debug 模式使用）。
///
/// 当 `RT_ACTIVE` 为 `true` 时，表示当前线程正在执行实时处理。
/// `rt_assert_*` 宏检查此标志来捕获违规。
#[cfg(debug_assertions)]
static RT_ACTIVE: AtomicBool = AtomicBool::new(false);

/// 进入实时处理上下文。
///
/// 由 `pipeline.rs` 的 `process` 入口调用。
/// 仅 debug 模式有效。
#[inline(always)]
pub fn enter_rt_context() {
    #[cfg(debug_assertions)]
    {
        RT_ACTIVE.store(true, Ordering::SeqCst);
    }
}

/// 离开实时处理上下文。
///
/// 由 `pipeline.rs` 的 `process` 出口调用。
/// 仅 debug 模式有效。
#[inline(always)]
pub fn exit_rt_context() {
    #[cfg(debug_assertions)]
    {
        RT_ACTIVE.store(false, Ordering::SeqCst);
    }
}

/// 检查当前是否在 RT 上下文中。
///
/// Release 模式下始终返回 `false`（零开销）。
#[inline(always)]
pub fn is_rt_context() -> bool {
    #[cfg(debug_assertions)]
    {
        RT_ACTIVE.load(Ordering::SeqCst)
    }
    #[cfg(not(debug_assertions))]
    {
        false
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// RtGuard — RAII 实时上下文守卫
// ══════════════════════════════════════════════════════════════════════════════

/// RAII 实时上下文守卫。
///
/// 创建时进入 RT 上下文，Drop 时退出。
/// 用于 `pipeline.rs` 的 `process` 方法包裹整个处理流程。
///
/// ```ignore
/// fn process(&mut self, ...) {
///     let _guard = RtGuard::new();
///     // 所有实时处理代码在此作用域内
/// }
/// ```
///
/// Debug 模式下设置全局标志，release 模式下零开销。
pub struct RtGuard {
    _private: (),
}

impl RtGuard {
    /// 创建守卫，进入 RT 上下文。
    #[inline(always)]
    pub fn new() -> Self {
        enter_rt_context();
        Self { _private: () }
    }
}

impl Drop for RtGuard {
    #[inline(always)]
    fn drop(&mut self) {
        exit_rt_context();
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// Debug 断言宏
// ══════════════════════════════════════════════════════════════════════════════

/// 断言当前不在实时上下文中。
///
/// 用于标记禁止在 RT 路径中调用的函数（如堆分配、I/O）。
/// Release 模式下编译为空操作。
///
/// # 用法
///
/// ```ignore
/// fn allocate_buffer(size: usize) -> Vec<f32> {
///     rt_assert_not_in_rt!("allocate_buffer performs heap allocation");
///     vec![0.0; size]
/// }
/// ```
#[macro_export]
macro_rules! rt_assert_not_in_rt {
    () => {
        rt_assert_not_in_rt!("operation not allowed in realtime context")
    };
    ($msg:expr) => {
        #[cfg(debug_assertions)]
        {
            if $crate::engine::rt_contract::is_rt_context() {
                panic!(
                    "RT-SAFETY VIOLATION: {} called in realtime context. \
                     This will cause audio glitches or deadlocks. \
                     See Note 12/58.",
                    $msg
                );
            }
        }
    };
}

/// 断言当前在实时上下文中。
///
/// 用于确认某段代码确实运行在 RT 路径（如裸指针操作）。
/// Release 模式下编译为空操作。
#[macro_export]
macro_rules! rt_assert_in_rt {
    () => {
        rt_assert_in_rt!("expected to be in realtime context")
    };
    ($msg:expr) => {
        #[cfg(debug_assertions)]
        {
            if !$crate::engine::rt_contract::is_rt_context() {
                panic!("RT-SAFETY ASSERTION: {}", $msg);
            }
        }
    };
}

/// 标记函数为仅限非实时路径调用。
///
/// 在函数入口处检查 RT 上下文。用于 `initialize`、`load_config` 等。
#[macro_export]
macro_rules! rt_require_non_rt {
    ($fn_name:expr) => {
        $crate::rt_assert_not_in_rt!(concat!($fn_name, " (non-RT only)"));
    };
}

// ══════════════════════════════════════════════════════════════════════════════
// 辅助：RT 安全的 slice 操作
// ══════════════════════════════════════════════════════════════════════════════

/// RT 安全的 slice 索引——debug 模式检查边界，release 模式使用 `get_unchecked`。
///
/// 等效于 `slice[index]`，但 debug 模式下用 `debug_assert!` 而非 panic。
///
/// # Safety
///
/// `index` 必须在 `[0, slice.len())` 范围内。
/// 实现者保证此约束成立。
#[inline(always)]
pub unsafe fn rt_index<T>(slice: &[T], index: usize) -> &T {
    debug_assert!(
        index < slice.len(),
        "RT-SAFETY: slice index {} out of bounds (len {})",
        index,
        slice.len()
    );
    slice.get_unchecked(index)
}

/// RT 安全的可变 slice 索引。
///
/// # Safety
///
/// `index` 必须在 `[0, slice.len())` 范围内。
#[inline(always)]
pub unsafe fn rt_index_mut<T>(slice: &mut [T], index: usize) -> &mut T {
    debug_assert!(
        index < slice.len(),
        "RT-SAFETY: slice index {} out of bounds (len {})",
        index,
        slice.len()
    );
    slice.get_unchecked_mut(index)
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    use crate::utils::test_helpers::serial_lock;

    // ── RtGuard 基础 ────────────────────────────────────────────────────────

    #[test]
    fn rt_guard_sets_context() {
        let _l = serial_lock();
        assert!(!is_rt_context());
        {
            let _guard = RtGuard::new();
            assert!(is_rt_context());
        }
        assert!(!is_rt_context());
    }

    #[test]
    fn rt_guard_nested() {
        let _l = serial_lock();
        assert!(!is_rt_context());
        {
            let _g1 = RtGuard::new();
            assert!(is_rt_context());
            {
                let _g2 = RtGuard::new();
                assert!(is_rt_context());
            }
        }
        assert!(!is_rt_context());
    }

    #[test]
    fn rt_guard_drop_restores() {
        let _l = serial_lock();
        let g = RtGuard::new();
        assert!(is_rt_context());
        drop(g);
        assert!(!is_rt_context());
    }

    // ── rt_assert_not_in_rt ─────────────────────────────────────────────────

    #[test]
    fn assert_not_in_rt_passes_when_not_in_rt() {
        let _l = serial_lock();
        assert!(!is_rt_context());
        rt_assert_not_in_rt!("test");
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "RT-SAFETY VIOLATION")]
    fn assert_not_in_rt_panics_in_rt() {
        let _l = serial_lock();
        let _guard = RtGuard::new();
        rt_assert_not_in_rt!("test should panic");
    }

    // ── rt_assert_in_rt ─────────────────────────────────────────────────────

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "RT-SAFETY ASSERTION")]
    fn assert_in_rt_panics_when_not_in_rt() {
        let _l = serial_lock();
        assert!(!is_rt_context());
        rt_assert_in_rt!("test should panic");
    }

    #[test]
    fn assert_in_rt_passes_when_in_rt() {
        let _l = serial_lock();
        let _guard = RtGuard::new();
        rt_assert_in_rt!("test");
    }

    // ── rt_index（不碰全局状态，不需要锁） ──────────────────────────────────

    #[test]
    fn rt_index_reads_correctly() {
        let data = vec![10.0, 20.0, 30.0];
        let val = unsafe { rt_index(&data, 1) };
        assert_eq!(*val, 20.0);
    }

    #[test]
    fn rt_index_mut_writes_correctly() {
        let mut data = vec![10.0, 20.0, 30.0];
        let val = unsafe { rt_index_mut(&mut data, 2) };
        *val = 99.0;
        assert_eq!(data[2], 99.0);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "out of bounds")]
    fn rt_index_panics_on_out_of_bounds_debug() {
        let data = vec![1.0, 2.0];
        unsafe { rt_index(&data, 5); }
    }

    // ── RtCopy（不碰全局状态） ──────────────────────────────────────────────

    #[test]
    fn rt_copy_implementations() {
        fn assert_rt_copy<T: RtCopy>() {}
        assert_rt_copy::<f32>();
        assert_rt_copy::<f64>();
        assert_rt_copy::<i16>();
        assert_rt_copy::<u32>();
        assert_rt_copy::<usize>();
        assert_rt_copy::<bool>();
    }

    // ── 模拟 RT 处理流程 ────────────────────────────────────────────────────

    #[test]
    fn simulate_rt_process_flow() {
        let _l = serial_lock();
        let mut buffer = vec![0.0f32; 128];
        let _guard = RtGuard::new();
        assert!(is_rt_context());
        for i in 0..128 {
            let sample = unsafe { rt_index_mut(&mut buffer, i) };
            *sample *= 0.5;
        }
        drop(_guard);
        assert!(!is_rt_context());
        assert!(buffer.iter().all(|&v| v == 0.0));
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "RT-SAFETY VIOLATION")]
    fn rt_context_rejects_allocation() {
        let _l = serial_lock();
        fn allocate_something() {
            rt_assert_not_in_rt!("allocate_something");
            let _v = vec![0.0f32; 128];
        }
        let _guard = RtGuard::new();
        allocate_something();
    }

    #[test]
    fn non_rt_context_allows_allocation() {
        let _l = serial_lock();
        fn allocate_something() {
            rt_assert_not_in_rt!("allocate_something");
            let _v = vec![0.0f32; 128];
        }
        assert!(!is_rt_context());
        allocate_something();
    }

    // ── 宏展开测试 ──────────────────────────────────────────────────────────

    #[test]
    fn macro_rt_require_non_rt_passes() {
        let _l = serial_lock();
        fn init() { rt_require_non_rt!("init"); }
        init();
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "RT-SAFETY VIOLATION")]
    fn macro_rt_require_non_rt_fails_in_rt() {
        let _l = serial_lock();
        fn init() { rt_require_non_rt!("init"); }
        let _guard = RtGuard::new();
        init();
    }
}