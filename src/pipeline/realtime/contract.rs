//! pipeline/realtime/contract.rs — 实时安全（RT-safety）契约（规范 4.5）
// 本模块是 RT-safety 契约设施（守卫/标记 trait/线程局部状态）：生产路径经宏展开引用，仓内直接消费者少。
#![allow(dead_code, unused_imports)]


#[cfg(debug_assertions)]
use std::sync::atomic::{AtomicBool, Ordering};

// ══════════════════════════════════════════════════════════════════════════════
// RtSafe marker trait
// ══════════════════════════════════════════════════════════════════════════════

/// 标记类型的所有方法满足 RT-safety 约束。
///
/// # Safety
///
/// 实现者必须保证类型的所有 `&self` / `&mut self` 方法：
/// - 不进行堆分配（包括隐式分配，如 `Vec::clone(）`)
/// - 不获取任何锁（`Mutex`、`RwLock`、`std::sync::Once`）
/// - 不执行 I/O（文件、网络、注册表）
/// - 不 panic（无 `unwrap()`、`expect(）`、越界索引)
/// - 不调用任何非 `RtSafe` 标记的方法
///
/// `initialize` / `new` 等构造方法不受此约束——它们在非实时路径中调用。
pub unsafe trait RtSafe {}

/// 标记类型可以在实时路径中使用（值语义，无副作用）。
///
/// # Safety
///
/// 类型必须是 `Copy`，不包含任何非 `RtSafe` 的字段。
pub unsafe trait RtCopy: Copy {}

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
// RealtimeContext — RT 编译期见证
// ══════════════════════════════════════════════════════════════════════════════

/// RT 编译期见证标记（零尺寸）。
///
/// 无字段、无用户可达构造函数。RT harness（pipeline/process.rs 的 RT 内部函数）
/// 在实时路径创建后按引用传递。其出现在调用栈中即为设计原则：
/// **编译期能解决的问题，绝不拖到运行时**。
///
/// `DspContext::rt_marker`（`PhantomData<RealtimeContext>`）将"此配置服务于 RT"的
/// 语义前移到编译期；逐步将 `rt_assert_in_rt!` 运行时断言升级为编译期见证。
pub struct RealtimeContext {
    _private: (),
}

impl RealtimeContext {
    /// 供 RT harness 内部创建（仅 pipeline 内部可见）。
    pub(crate) fn new() -> Self {
        Self { _private: () }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// RT 上下文跟踪（Debug 模式）
// ══════════════════════════════════════════════════════════════════════════════

/// 全局 RT 上下文标志（仅 debug 模式使用）。
#[cfg(debug_assertions)]
static RT_ACTIVE: AtomicBool = AtomicBool::new(false);

/// 进入实时处理上下文。
#[inline(always)]
pub fn enter_rt_context() {
    #[cfg(debug_assertions)]
    {
        RT_ACTIVE.store(true, Ordering::SeqCst);
    }
}

/// 离开实时处理上下文。
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
/// Release 模式下编译为空操作。
#[macro_export]
macro_rules! rt_assert_not_in_rt {
    () => {
        rt_assert_not_in_rt!("operation not allowed in realtime context")
    };
    ($msg:expr) => {
        #[cfg(debug_assertions)]
        {
            if $crate::pipeline::realtime::contract::is_rt_context() {
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
/// Release 模式下编译为空操作。
#[macro_export]
macro_rules! rt_assert_in_rt {
    () => {
        rt_assert_in_rt!("expected to be in realtime context")
    };
    ($msg:expr) => {
        #[cfg(debug_assertions)]
        {
            if !$crate::pipeline::realtime::contract::is_rt_context() {
                panic!("RT-SAFETY ASSERTION: {}", $msg);
            }
        }
    };
}

/// 标记函数为仅限非实时路径调用。
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
/// # Safety
///
/// `index` 必须在 `[0, slice.len())` 范围内。
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
    use std::sync::Mutex;

    /// 串行化测试（全局 RT 标志是单个静态变量）。
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn serial_lock() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

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
