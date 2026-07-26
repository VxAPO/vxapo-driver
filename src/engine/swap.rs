//! engine/swap.rs — 配置热重载交换机制（Note 19）
//!
//! 三指针架构与信号量协调：
//!
//! ```text
//! Builder Thread                    Realtime Thread
//! ─────────────                     ───────────────
//! build new Chain                   check pending_swap()
//! ↓                                 ↓
//! store as next_chain               try_lock next_chain
//! ↓                                 ↓
//! set pending = true                begin smoothing transition
//! ↓                                 ↓
//! (wait for ack)                    blend old + new outputs
//!                                   ↓
//!                                   swap: prev ← current ← next
//!                                   ↓
//!                                   set pending = false (ack)
//! ```
//!
//! 信号量初始值 1，最大值 1（Note 19）。
//! 暴露 `has_pending_swap()` 和 `current_chain()` 接口供实时线程查询。
//!
//! 此模块的加载线程侧允许堆分配与 I/O，实时线程侧仅操作原子变量与信号量。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::engine::chain::Chain;
use crate::engine::transition::SmoothingProvider;

// ══════════════════════════════════════════════════════════════════════════════
// SwapController — 三指针配置交换控制器
// ══════════════════════════════════════════════════════════════════════════════

/// 配置交换控制器。
///
/// 管理三个 `Chain` 指针和过渡混合状态。
/// 非实时路径（builder 线程）调用 `submit_new_chain`，
/// 实时路径（`pipeline.rs`）调用 `check_swap` / `advance_transition`。
pub struct SwapController {
    /// 当前活跃的过滤器链。
    ///
    /// 实时线程每帧使用此链处理音频。
    /// `None` 表示尚未加载任何配置（passthrough 模式）。
    current_chain: Option<Chain>,

    /// 新构建的过滤器链（等待激活）。
    ///
    /// builder 线程通过 `Arc<Mutex>` 写入，实时线程通过 `try_lock` 读取。
    /// `try_lock` 永不阻塞，满足实时安全（Note 12）。
    next_chain: Arc<Mutex<Option<Chain>>>,

    /// 旧配置（过渡期间用于混合）。
    ///
    /// 过渡完成后被 drop。
    previous_chain: Option<Chain>,

    /// 过渡混合状态机。
    smoothing: SmoothingProvider,

    /// 是否有待处理的配置交换。
    ///
    /// 原子标志，实时线程每帧检查。
    pending: Arc<AtomicBool>,

    /// 过渡期间需要同时使用新旧两套链。
    /// 此标志为 `true` 时，pipeline 对新旧链分别处理后混合。
    transitioning: bool,
}

impl SwapController {
    /// 创建新的交换控制器。
    ///
    /// - `smoothing_length`：过渡帧数（如 48kHz 下 2400 = 50ms）
    pub fn new(smoothing_length: u32) -> Self {
        Self {
            current_chain: None,
            next_chain: Arc::new(Mutex::new(None)),
            previous_chain: None,
            smoothing: SmoothingProvider::new(smoothing_length),
            pending: Arc::new(AtomicBool::new(false)),
            transitioning: false,
        }
    }

    // ── Builder 线程接口（非实时路径） ──────────────────────────────────────

    /// 获取 next_chain 的共享引用，供 builder 线程写入。
    ///
    /// ```ignore
    /// // builder 线程
    /// let new_chain = build_chain(config_path, ctx);
    /// let next = swap.next_chain_handle();
    /// *next.lock().unwrap() = Some(new_chain);
    /// swap.notify_new_chain();
    /// ```
    pub fn next_chain_handle(&self) -> Arc<Mutex<Option<Chain>>> {
        self.next_chain.clone()
    }

    /// 通知实时线程有新配置可用。
    ///
    /// builder 线程将新 Chain 写入 `next_chain` 后调用。
    /// 设置 `pending` 标志，实时线程下一次 `check_swap` 时检测到。
    pub fn notify_new_chain(&self) {
        self.pending.store(true, Ordering::Release);
    }

    /// 直接提交新配置（便捷方法，合并 lock + write + notify）。
    pub fn submit_new_chain(&self, chain: Chain) {
        if let Ok(mut guard) = self.next_chain.lock() {
            *guard = Some(chain);
        }
        self.pending.store(true, Ordering::Release);
    }

    // ── 实时线程接口 ────────────────────────────────────────────────────────

    /// 检查是否有待处理的配置交换。
    ///
    /// 实时线程每帧调用。纯原子读取，无锁。
    pub fn has_pending_swap(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }

    /// 尝试获取新配置并开始过渡。
    ///
    /// 实时线程调用。使用 `try_lock`（非阻塞）读取 next_chain。
    ///
    /// 返回 `true` 如果成功获取新配置并开始过渡。
    pub fn check_swap(&mut self) -> bool {
        if !self.has_pending_swap() {
            return false;
        }

        // try_lock 不阻塞——满足 RT 安全（Note 12）
        let new_chain = match self.next_chain.try_lock() {
            Ok(mut guard) => guard.take(),
            Err(_) => return false, // 锁被占用，下一帧重试
        };

        let new_chain = match new_chain {
            Some(c) => c,
            None => {
                // pending 为 true 但 chain 为 None——竞争条件，清除 pending
                self.pending.store(false, Ordering::Release);
                return false;
            }
        };

        // 开始过渡
        if self.current_chain.is_some() {
            // 有旧配置：保存为 previous，开始混合过渡
            self.previous_chain = self.current_chain.take();
            self.transitioning = true;
            self.smoothing.begin();
        }

        self.current_chain = Some(new_chain);
        self.pending.store(false, Ordering::Release);

        true
    }

    /// 推进过渡混合，返回当前混合因子。
    ///
    /// 返回 `Some(factor)` 如果过渡进行中，`None` 如果已完成或未激活。
    ///
    /// 实时线程每帧调用。
    pub fn advance_transition(&mut self) -> Option<f32> {
        let factor = self.smoothing.advance()?;

        if !self.smoothing.is_active() && self.transitioning {
            // 过渡完成，丢弃旧配置
            self.previous_chain = None;
            self.transitioning = false;
        }

        Some(factor)
    }

    /// 是否正在过渡中。
    pub fn is_transitioning(&self) -> bool {
        self.transitioning
    }

    // ── 链访问 ──────────────────────────────────────────────────────────────

    /// 当前活跃过滤器链（只读引用）。
    ///
    /// `pipeline.rs` 使用此链处理音频。
    /// `None` 表示尚未加载配置——passthrough 模式。
    pub fn current_chain(&self) -> Option<&Chain> {
        self.current_chain.as_ref()
    }

    /// 当前活跃过滤器链（可变引用）。
    pub fn current_chain_mut(&mut self) -> Option<&mut Chain> {
        self.current_chain.as_mut()
    }

    /// 旧配置过滤器链（过渡期间只读）。
    ///
    /// 过渡混合时需要同时访问新旧两套链。
    pub fn previous_chain(&self) -> Option<&Chain> {
        self.previous_chain.as_ref()
    }

    /// 是否有活跃配置。
    pub fn has_chain(&self) -> bool {
        self.current_chain.is_some()
    }

    // ── 状态查询 ────────────────────────────────────────────────────────────

    /// 获取当前配置的通道数。
    pub fn channel_count(&self) -> usize {
        self.current_chain
            .as_ref()
            .map(|c| c.real_channel_count())
            .unwrap_or(0)
    }

    /// 获取当前配置的过滤器数量。
    pub fn filter_count(&self) -> usize {
        self.current_chain
            .as_ref()
            .map(|c| c.filter_count())
            .unwrap_or(0)
    }

    /// 重置到初始状态。
    pub fn reset(&mut self) {
        self.current_chain = None;
        self.previous_chain = None;
        self.transitioning = false;
        self.smoothing.reset();
        self.pending.store(false, Ordering::Release);
        if let Ok(mut guard) = self.next_chain.lock() {
            *guard = None;
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo_names() -> Vec<String> {
        vec!["L".to_owned(), "R".to_owned()]
    }

    // ── 初始状态 ────────────────────────────────────────────────────────────

    #[test]
    fn new_controller_has_no_chain() {
        let ctrl = SwapController::new(480);
        assert!(!ctrl.has_chain());
        assert!(ctrl.current_chain().is_none());
        assert!(ctrl.previous_chain().is_none());
        assert!(!ctrl.has_pending_swap());
        assert!(!ctrl.is_transitioning());
        assert_eq!(ctrl.channel_count(), 0);
        assert_eq!(ctrl.filter_count(), 0);
    }

    // ── 首次配置（无过渡） ──────────────────────────────────────────────────

    #[test]
    fn first_chain_no_transition() {
        let mut ctrl = SwapController::new(480);

        let chain = Chain::new(2, 480, stereo_names());
        ctrl.submit_new_chain(chain);
        assert!(ctrl.has_pending_swap());

        let swapped = ctrl.check_swap();
        assert!(swapped);
        assert!(ctrl.has_chain());
        assert!(!ctrl.has_pending_swap());
        // 首次加载无过渡
        assert!(!ctrl.is_transitioning());
        assert!(ctrl.previous_chain().is_none());
        assert_eq!(ctrl.channel_count(), 2);
    }

    // ── 配置切换（触发过渡） ────────────────────────────────────────────────

    #[test]
    fn second_chain_triggers_transition() {
        let mut ctrl = SwapController::new(100);

        // 第一次配置
        ctrl.submit_new_chain(Chain::new(2, 480, stereo_names()));
        ctrl.check_swap();
        assert!(!ctrl.is_transitioning());

        // 第二次配置
        ctrl.submit_new_chain(Chain::new(6, 480, vec![
            "L".into(), "R".into(), "C".into(),
            "LFE".into(), "RL".into(), "RR".into(),
        ]));
        assert!(ctrl.has_pending_swap());

        let swapped = ctrl.check_swap();
        assert!(swapped);
        assert!(ctrl.is_transitioning());
        assert!(ctrl.previous_chain().is_some());
        assert_eq!(ctrl.channel_count(), 6); // 新配置的通道数
    }

    // ── 过渡推进 ────────────────────────────────────────────────────────────

    #[test]
    fn transition_advances_and_completes() {
        let mut ctrl = SwapController::new(10);

        // 首次配置
        ctrl.submit_new_chain(Chain::new(2, 480, stereo_names()));
        ctrl.check_swap();

        // 第二次配置
        ctrl.submit_new_chain(Chain::new(2, 480, stereo_names()));
        ctrl.check_swap();
        assert!(ctrl.is_transitioning());

        // 推进过渡
        for _ in 0..10 {
            let factor = ctrl.advance_transition();
            assert!(factor.is_some());
        }

        // 第 11 次：过渡完成
        let factor = ctrl.advance_transition().unwrap();
        assert!((factor - 1.0).abs() < 1e-6);

        // 过渡结束后旧链被释放
        assert!(!ctrl.is_transitioning());
        assert!(ctrl.previous_chain().is_none());
    }

    #[test]
    fn advance_returns_none_when_not_transitioning() {
        let mut ctrl = SwapController::new(480);
        assert!(ctrl.advance_transition().is_none());
    }

    // ── try_lock 竞争 ───────────────────────────────────────────────────────

    #[test]
    fn check_swap_returns_false_when_no_pending() {
        let mut ctrl = SwapController::new(480);
        assert!(!ctrl.check_swap());
    }

    #[test]
    fn check_swap_returns_false_when_next_is_none() {
        let mut ctrl = SwapController::new(480);
        // 手动设置 pending 但不写入 chain
        ctrl.pending.store(true, Ordering::Release);
        assert!(!ctrl.check_swap());
        // pending 被清除
        assert!(!ctrl.has_pending_swap());
    }

    // ── 线程安全：builder + RT 线程 ─────────────────────────────────────────

    #[test]
    fn concurrent_submit_and_check() {
        let mut ctrl = SwapController::new(10);
        let next_handle = ctrl.next_chain_handle();
        let pending = ctrl.pending.clone();

        // builder 线程
        let builder = std::thread::spawn(move || {
            let chain = Chain::new(2, 480, vec!["L".into(), "R".into()]);
            *next_handle.lock().unwrap() = Some(chain);
            pending.store(true, Ordering::Release);
        });

        builder.join().unwrap();

        // RT 线程（主线程模拟）
        let swapped = ctrl.check_swap();
        assert!(swapped);
        assert!(ctrl.has_chain());
    }

    // ── Reset ───────────────────────────────────────────────────────────────

    #[test]
    fn reset_clears_everything() {
        let mut ctrl = SwapController::new(100);
        ctrl.submit_new_chain(Chain::new(2, 480, stereo_names()));
        ctrl.check_swap();
        ctrl.submit_new_chain(Chain::new(2, 480, stereo_names()));
        ctrl.check_swap();

        ctrl.reset();
        assert!(!ctrl.has_chain());
        assert!(!ctrl.has_pending_swap());
        assert!(!ctrl.is_transitioning());
        assert!(ctrl.current_chain().is_none());
        assert!(ctrl.previous_chain().is_none());
    }

    // ── 多次切换 ────────────────────────────────────────────────────────────

    #[test]
    fn multiple_swaps() {
        let mut ctrl = SwapController::new(10);

        for _i in 0..5 {
            ctrl.submit_new_chain(Chain::new(2, 480, stereo_names()));
            ctrl.check_swap();

            // 如果在过渡中，推进到完成
            while ctrl.is_transitioning() {
                ctrl.advance_transition();
            }
        }

        assert!(ctrl.has_chain());
        assert!(!ctrl.is_transitioning());
    }

    // ── 通道数查询 ──────────────────────────────────────────────────────────

    #[test]
    fn channel_count_updates_on_swap() {
        let mut ctrl = SwapController::new(10);
        assert_eq!(ctrl.channel_count(), 0);

        ctrl.submit_new_chain(Chain::new(6, 480, vec![
            "L".into(), "R".into(), "C".into(),
            "LFE".into(), "RL".into(), "RR".into(),
        ]));
        ctrl.check_swap();
        assert_eq!(ctrl.channel_count(), 6);
    }
}