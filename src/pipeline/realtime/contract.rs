//! pipeline/realtime/contract.rs — 实时安全（RT-safety）契约（规范 4.5）
//!
//! 生产路径的接入点：`object/apo/process.rs::apo_process`（RT 线程入口，创建
//! `RtGuard`）与热重载线程的 `hot_reload_impl`（用 `rt_require_non_rt!` 断言
//! 自己不在 RT 上下文）。二者是**不同线程**并发运行，因此本模块的上下文状态
//! 必须是 **thread-local** 而非全局：全局标志会被 RT 线程置位，让并发的非 RT
//! 线程误判并抛出假的 RT-SAFETY 违例。

#[cfg(debug_assertions)]
use std::cell::Cell;

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
///
/// `Send + Sync` 上界（规范 4.7）：RT 类型要能在实时线程与构造线程之间传递/共享，
/// 缺少这两个 auto trait 的标记没有意义。
#[allow(dead_code)] // 规范 4.7 承诺的公开标记 trait：暂无仓内实现者，按规范保留
pub unsafe trait RtSafe: Send + Sync {}

/// 标记类型可以在实时路径中使用（值语义，无副作用）。
///
/// # Safety
///
/// 类型必须是 `Copy`，不包含任何非 `RtSafe` 的字段。
#[allow(dead_code)] // 规范 4.7 承诺的公开标记 trait：暂无仓内消费者，按规范保留
pub unsafe trait RtCopy: Copy {}

// SAFETY: f32 是 Copy、无堆分配、无 Drop、方法无副作用，满足 RtCopy 约束。
unsafe impl RtCopy for f32 {}
// SAFETY: f64 同上（Copy + 无副作用）。
unsafe impl RtCopy for f64 {}
// SAFETY: i8 同上（Copy + 无副作用）。
unsafe impl RtCopy for i8 {}
// SAFETY: i16 同上（Copy + 无副作用）。
unsafe impl RtCopy for i16 {}
// SAFETY: i32 同上（Copy + 无副作用）。
unsafe impl RtCopy for i32 {}
// SAFETY: i64 同上（Copy + 无副作用）。
unsafe impl RtCopy for i64 {}
// SAFETY: u8 同上（Copy + 无副作用）。
unsafe impl RtCopy for u8 {}
// SAFETY: u16 同上（Copy + 无副作用）。
unsafe impl RtCopy for u16 {}
// SAFETY: u32 同上（Copy + 无副作用）。
unsafe impl RtCopy for u32 {}
// SAFETY: u64 同上（Copy + 无副作用）。
unsafe impl RtCopy for u64 {}
// SAFETY: usize 同上（Copy + 无副作用）。
unsafe impl RtCopy for usize {}
// SAFETY: isize 同上（Copy + 无副作用）。
unsafe impl RtCopy for isize {}
// SAFETY: bool 同上（Copy + 无副作用）。
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

// 当前线程的 RT 上下文嵌套深度（仅 debug 模式使用）。
//
// 必须 thread-local：RT 线程与非 RT 线程（配置热重载 watcher）并发运行，
// 全局标志会互相污染。
//
// 用**深度计数**而非布尔量：嵌套守卫（如 RT 函数内部再取一次守卫）在
// 内层 Drop 时不能把外层仍在的 RT 上下文清掉。
#[cfg(debug_assertions)]
thread_local! {
    static RT_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// 进入实时处理上下文。
///
/// 可重入：每次调用使深度 +1，须与 `exit_rt_context` 一一配对。
#[inline(always)]
pub fn enter_rt_context() {
    #[cfg(debug_assertions)]
    RT_DEPTH.with(|d| d.set(d.get().saturating_add(1)));
}

/// 离开实时处理上下文。
///
/// 深度归零才算真正离开；内层守卫 Drop 不会清掉外层仍有效的上下文。
#[inline(always)]
pub fn exit_rt_context() {
    #[cfg(debug_assertions)]
    RT_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
}

/// 检查当前是否在 RT 上下文中（**仅当前线程**）。
///
/// Release 模式下始终返回 `false`（零开销）。
///
/// `dead_code` 只在 release 下出现：debug 下由 `RT_DEPTH` 的读写与断言宏使用；
/// release 下断言宏整体编译为空操作，本函数除被宏引用外暂无直接调用者。
/// 它是规范 4.7 承诺的公开 API，故保留。
#[allow(dead_code)] // 规范 4.7 承诺的公开 API：release 下断言宏编译为空，故暂无调用者
#[inline(always)]
pub fn is_rt_context() -> bool {
    #[cfg(debug_assertions)]
    {
        RT_DEPTH.with(|d| d.get() > 0)
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
#[allow(dead_code)] // 规范 4.7 承诺的公开 API：DSP 逐采样索引的 RT 安全替代，按规范保留
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
#[allow(dead_code)] // 规范 4.7 承诺的公开 API：DSP 逐采样索引的 RT 安全替代，按规范保留
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
    #[cfg(debug_assertions)]
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
    #[cfg(debug_assertions)]
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
            // 回归（缺陷 A）：内层守卫 Drop 后，外层仍在 ⇒ 必须仍处于 RT 上下文。
            // 原实现用布尔量 + 无计数，这里会假性退出，故该断言曾长期缺失。
            assert!(
                is_rt_context(),
                "内层守卫 Drop 不得清掉外层仍有效的 RT 上下文"
            );
        }
        assert!(!is_rt_context());
    }

    /// 回归（缺陷 B）：RT 上下文是**线程局部**的，不得泄漏到其它线程。
    ///
    /// 生产上 RT 线程与配置热重载线程并发；若用全局标志，watcher 线程会看到
    /// RT 线程置位的状态，从而在 `rt_require_non_rt!` 上抛假违例。
    #[test]
    #[cfg(debug_assertions)]
    fn rt_context_is_thread_local() {
        let _l = serial_lock();
        assert!(!is_rt_context());

        let guard = RtGuard::new();
        assert!(is_rt_context(), "本线程应处于 RT 上下文");

        // 另一个线程必须看不到本线程的 RT 状态。
        let seen_elsewhere = std::thread::spawn(is_rt_context).join().unwrap();
        drop(guard);

        assert!(
            !seen_elsewhere,
            "RT 上下文泄漏到其它线程（说明用了全局标志而非 thread-local）"
        );
        assert!(!is_rt_context());
    }

    /// 深度计数不得下溢（多余的 exit 调用应被饱和保护）。
    #[test]
    #[cfg(debug_assertions)]
    fn rt_depth_does_not_underflow() {
        let _l = serial_lock();
        assert!(!is_rt_context());
        exit_rt_context(); // 多余的退出调用
        exit_rt_context();
        assert!(!is_rt_context(), "多余的 exit 不得变成负数而误报在 RT 中");
        // 之后再正常进入仍应工作。
        let g = RtGuard::new();
        assert!(is_rt_context());
        drop(g);
        assert!(!is_rt_context());
    }

    #[test]
    #[cfg(debug_assertions)]
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
        // SAFETY: 索引 1 落在 data.len()=3 内，引用在本次调用内使用。
        let val = unsafe { rt_index(&data, 1) };
        assert_eq!(*val, 20.0);
    }

    #[test]
    fn rt_index_mut_writes_correctly() {
        let mut data = vec![10.0, 20.0, 30.0];
        // SAFETY: 索引 2 落在 data.len()=3 内；&mut data 保证独占访问。
        let val = unsafe { rt_index_mut(&mut data, 2) };
        *val = 99.0;
        assert_eq!(data[2], 99.0);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "out of bounds")]
    fn rt_index_panics_on_out_of_bounds_debug() {
        let data = vec![1.0, 2.0];
        // SAFETY: 刻意越界（5 >= len=2）——本用例就是要触发 debug 断言 panic，
        // 不产生任何解引用后的读写。
        unsafe {
            rt_index(&data, 5);
        }
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
        // RT 上下文跟踪是 debug-only 设施（release 下 is_rt_context 恒 false，零开销）：
        // 这里只断言 debug 下的标记行为，索引/平滑逻辑在两种 profile 下都验。
        #[cfg(debug_assertions)]
        assert!(is_rt_context());
        for i in 0..128 {
            // SAFETY: i ∈ 0..128 且 buffer.len()=128，索引恒在范围内。
            let sample = unsafe { rt_index_mut(&mut buffer, i) };
            *sample *= 0.5;
        }
        drop(_guard);
        #[cfg(debug_assertions)]
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
        fn init() {
            rt_require_non_rt!("init");
        }
        init();
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "RT-SAFETY VIOLATION")]
    fn macro_rt_require_non_rt_fails_in_rt() {
        let _l = serial_lock();
        fn init() {
            rt_require_non_rt!("init");
        }
        let _guard = RtGuard::new();
        init();
    }
}
