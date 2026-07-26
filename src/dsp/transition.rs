//! engine/transition.rs — 过渡混合（Note 21）
//!
//! 配置热重载时，旧配置（`current_chain`）与新配置（`next_chain`）之间
//! 需要平滑过渡，避免音频断裂（pop/click）。
//!
//! 使用升余弦（raised cosine）混合因子：
//!
//! ```text
//! raised_cosine(counter, length) = 0.5 * (1.0 - cos(PI * counter / length))
//! ```
//!
//! - `counter = 0` → factor = 0.0（100% 旧配置）
//! - `counter = length` → factor = 1.0（100% 新配置）
//! - 中间过程平滑过渡，无突变
//!
//! 混合函数使用裸指针签名，避免借用检查器在热路径中引入开销（Note 21）。
//! `counter >= length` 时直接返回 1.0，避免浮点精度问题。
//!
//! 此模块运行在实时音频线程中，纯数值计算，无堆分配（Note 12）。

// ══════════════════════════════════════════════════════════════════════════════
// 升余弦混合因子
// ══════════════════════════════════════════════════════════════════════════════

/// 计算升余弦混合因子。
///
/// ```text
/// factor = 0.5 * (1.0 - cos(PI * counter / length))
/// ```
///
/// - `counter`：当前过渡帧计数（0..=length）
/// - `length`：过渡总帧数
///
/// 返回值：0.0（旧配置）→ 1.0（新配置）。
///
/// `counter >= length` 时直接返回 1.0（避免浮点精度问题，Note 21）。
///
/// # 实时安全
///
/// 纯数值计算，无分配。
#[inline(always)]
pub fn raised_cosine(counter: u32, length: u32) -> f32 {
    if length == 0 {
        return 1.0;
    }
    if counter >= length {
        return 1.0;
    }
    let ratio = counter as f32 / length as f32;
    0.5 * (1.0 - (std::f32::consts::PI * ratio).cos())
}

// ══════════════════════════════════════════════════════════════════════════════
// 混合函数（Note 21：裸指针签名）
// ══════════════════════════════════════════════════════════════════════════════

/// 对两个平面缓冲区进行线性混合（逐采样）。
///
/// ```text
/// output[f] = old[f] * (1.0 - factor) + new[f] * factor
/// ```
///
/// # 参数
///
/// - `old_samples`：旧配置输出（长度 = `frame_count`）
/// - `new_samples`：新配置输出（长度 = `frame_count`）
/// - `output`：混合结果写入目标（长度 ≥ `frame_count`）
/// - `frame_count`：本帧采样数
/// - `factor`：混合因子（0.0 = 100% old，1.0 = 100% new）
///
/// # 实时安全
///
/// 纯数值计算，无分配。
///
/// # Safety
///
/// 所有指针必须有效且指向至少 `frame_count` 个 `f32` 元素。
/// 调用方保证不发生越界访问。
#[inline]
pub unsafe fn mix_buffers(
    old_samples: *const f32,
    new_samples: *const f32,
    output: *mut f32,
    frame_count: usize,
    factor: f32,
) {
    let inv_factor = 1.0 - factor;

    for f in 0..frame_count {
        let old = *old_samples.add(f);
        let new = *new_samples.add(f);
        *output.add(f) = old * inv_factor + new * factor;
    }
}

/// 对整个平面缓冲区（多通道）进行过渡混合。
///
/// 逐通道调用 `mix_buffers`。
///
/// # 实时安全
///
/// 纯数值计算，无分配。
pub fn mix_plane_buffers(
    old_buffers: &[Vec<f32>],
    new_buffers: &[Vec<f32>],
    output: &mut [Vec<f32>],
    frame_count: usize,
    factor: f32,
) {
    let channels = old_buffers.len().min(new_buffers.len()).min(output.len());
    let inv_factor = 1.0 - factor;

    for c in 0..channels {
        for f in 0..frame_count {
            output[c][f] = old_buffers[c][f] * inv_factor + new_buffers[c][f] * factor;
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// SmoothingProvider — 过渡状态管理
// ══════════════════════════════════════════════════════════════════════════════

/// 过渡混合状态。
///
/// 管理新旧配置之间的平滑过渡：
///
/// ```text
/// Idle → (nextConfig 到达) → Smoothing → (过渡完成) → Idle
/// ```
///
/// `pipeline.rs` 在每帧处理时调用 `advance` 获取当前混合因子。
pub struct SmoothingProvider {
    /// 过渡总帧数。
    length: u32,
    /// 当前过渡帧计数。
    counter: u32,
    /// 是否正在过渡中。
    active: bool,
}

impl SmoothingProvider {
    /// 创建新的过渡提供者。
    ///
    /// - `length`：过渡帧数。常见值 = 采样率 / 10（100ms 过渡）。
    pub fn new(length: u32) -> Self {
        Self {
            length,
            counter: 0,
            active: false,
        }
    }

    /// 开始过渡。
    ///
    /// 由 `pipeline.rs` 在检测到 `nextConfig` 可用时调用。
    pub fn begin(&mut self) {
        self.counter = 0;
        self.active = true;
    }

    /// 推进一帧，返回当前混合因子。
    ///
    /// - 返回 `Some(factor)`：过渡进行中
    /// - 返回 `None`：过渡完成或未激活
    ///
    /// 每帧调用一次。`factor` 从 0.0（旧）平滑过渡到 1.0（新）。
    pub fn advance(&mut self) -> Option<f32> {
        if !self.active {
            return None;
        }

        let factor = raised_cosine(self.counter, self.length);
        self.counter += 1;

        if self.counter > self.length {
            self.active = false;
            return Some(1.0);
        }

        Some(factor)
    }

    /// 是否正在过渡中。
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// 过渡总帧数。
    pub fn length(&self) -> u32 {
        self.length
    }

    /// 当前帧计数。
    pub fn counter(&self) -> u32 {
        self.counter
    }

    /// 进度百分比（0.0..=1.0）。
    pub fn progress(&self) -> f32 {
        if self.length == 0 {
            return 1.0;
        }
        (self.counter as f32 / self.length as f32).min(1.0)
    }

    /// 重置到空闲状态。
    pub fn reset(&mut self) {
        self.counter = 0;
        self.active = false;
    }

    /// 更改过渡长度（配置变更时可能需要调整）。
    pub fn set_length(&mut self, length: u32) {
        self.length = length;
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 常用过渡长度预设
// ══════════════════════════════════════════════════════════════════════════════

/// 根据采样率计算默认过渡帧数（约 50ms）。
pub fn default_smoothing_length(sample_rate: u32) -> u32 {
    sample_rate / 20 // 50ms
}

/// 根据采样率计算短过渡帧数（约 10ms）。
pub fn short_smoothing_length(sample_rate: u32) -> u32 {
    sample_rate / 100 // 10ms
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── raised_cosine ───────────────────────────────────────────────────────

    #[test]
    fn raised_cosine_start() {
        let f = raised_cosine(0, 100);
        assert!((f - 0.0).abs() < 1e-6, "expected 0.0, got {f}");
    }

    #[test]
    fn raised_cosine_end() {
        let f = raised_cosine(100, 100);
        assert!((f - 1.0).abs() < 1e-6, "expected 1.0, got {f}");
    }

    #[test]
    fn raised_cosine_midpoint() {
        let f = raised_cosine(50, 100);
        assert!((f - 0.5).abs() < 1e-5, "expected 0.5, got {f}");
    }

    #[test]
    fn raised_cosine_quarter() {
        // cos(PI * 0.25) = cos(45°) = √2/2 ≈ 0.7071
        // 0.5 * (1 - 0.7071) ≈ 0.1464
        let f = raised_cosine(25, 100);
        assert!((f - 0.1464).abs() < 0.001, "expected ~0.1464, got {f}");
    }

    #[test]
    fn raised_cosine_three_quarter() {
        // cos(PI * 0.75) = cos(135°) = -√2/2 ≈ -0.7071
        // 0.5 * (1 - (-0.7071)) ≈ 0.8536
        let f = raised_cosine(75, 100);
        assert!((f - 0.8536).abs() < 0.001, "expected ~0.8536, got {f}");
    }

    #[test]
    fn raised_cosine_beyond_length() {
        let f = raised_cosine(200, 100);
        assert_eq!(f, 1.0);
    }

    #[test]
    fn raised_cosine_zero_length() {
        let f = raised_cosine(0, 0);
        assert_eq!(f, 1.0);
    }

    #[test]
    fn raised_cosine_monotonically_increasing() {
        let length = 480u32;
        let mut prev = 0.0f32;
        for counter in 1..=length {
            let f = raised_cosine(counter, length);
            assert!(f >= prev, "not monotonic at counter={counter}: {f} < {prev}");
            prev = f;
        }
    }

    #[test]
    fn raised_cosine_symmetry() {
        // f(x) + f(length - x) = 1.0
        let length = 100u32;
        for counter in 0..=length {
            let f1 = raised_cosine(counter, length);
            let f2 = raised_cosine(length - counter, length);
            assert!(
                (f1 + f2 - 1.0).abs() < 1e-6,
                "symmetry broken at counter={counter}: {f1} + {f2} = {}",
                f1 + f2
            );
        }
    }

    // ── mix_buffers（裸指针） ────────────────────────────────────────────────

    #[test]
    fn mix_buffers_factor_zero() {
        let old = vec![1.0f32, 2.0, 3.0, 4.0];
        let new = vec![10.0f32, 20.0, 30.0, 40.0];
        let mut output = vec![0.0f32; 4];

        unsafe {
            mix_buffers(old.as_ptr(), new.as_ptr(), output.as_mut_ptr(), 4, 0.0);
        }

        // factor=0.0 → 100% old
        assert_eq!(output, old);
    }

    #[test]
    fn mix_buffers_factor_one() {
        let old = vec![1.0f32, 2.0, 3.0, 4.0];
        let new = vec![10.0f32, 20.0, 30.0, 40.0];
        let mut output = vec![0.0f32; 4];

        unsafe {
            mix_buffers(old.as_ptr(), new.as_ptr(), output.as_mut_ptr(), 4, 1.0);
        }

        // factor=1.0 → 100% new
        assert_eq!(output, new);
    }

    #[test]
    fn mix_buffers_factor_half() {
        let old = vec![0.0f32, 0.0, 0.0, 0.0];
        let new = vec![2.0f32, 4.0, 6.0, 8.0];
        let mut output = vec![0.0f32; 4];

        unsafe {
            mix_buffers(old.as_ptr(), new.as_ptr(), output.as_mut_ptr(), 4, 0.5);
        }

        // factor=0.5 → 50% of each
        assert_eq!(output, vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn mix_buffers_partial_frame() {
        let old = vec![1.0f32; 8];
        let new = vec![3.0f32; 8];
        let mut output = vec![0.0f32; 8];

        unsafe {
            mix_buffers(old.as_ptr(), new.as_ptr(), output.as_mut_ptr(), 4, 0.5);
        }

        // 只混合前 4 帧
        assert_eq!(output[0], 2.0);
        assert_eq!(output[1], 2.0);
        assert_eq!(output[2], 2.0);
        assert_eq!(output[3], 2.0);
        assert_eq!(output[4], 0.0); // 未混合
    }

    // ── mix_plane_buffers ───────────────────────────────────────────────────

    #[test]
    fn mix_plane_buffers_stereo() {
        let old = vec![vec![0.0f32; 4], vec![0.0f32; 4]];
        let new = vec![vec![2.0f32; 4], vec![4.0f32; 4]];
        let mut output = vec![vec![0.0f32; 4], vec![0.0f32; 4]];

        mix_plane_buffers(&old, &new, &mut output, 4, 0.5);
        assert_eq!(output[0], vec![1.0, 1.0, 1.0, 1.0]);
        assert_eq!(output[1], vec![2.0, 2.0, 2.0, 2.0]);
    }

    #[test]
    fn mix_plane_buffers_zero_factor() {
        let old = vec![vec![5.0f32; 4]];
        let new = vec![vec![10.0f32; 4]];
        let mut output = vec![vec![0.0f32; 4]];

        mix_plane_buffers(&old, &new, &mut output, 4, 0.0);
        assert_eq!(output[0], vec![5.0, 5.0, 5.0, 5.0]);
    }

    #[test]
    fn mix_plane_buffers_one_factor() {
        let old = vec![vec![5.0f32; 4]];
        let new = vec![vec![10.0f32; 4]];
        let mut output = vec![vec![0.0f32; 4]];

        mix_plane_buffers(&old, &new, &mut output, 4, 1.0);
        assert_eq!(output[0], vec![10.0, 10.0, 10.0, 10.0]);
    }

    // ── SmoothingProvider ───────────────────────────────────────────────────

    #[test]
    fn smoothing_new_not_active() {
        let sp = SmoothingProvider::new(480);
        assert!(!sp.is_active());
        assert_eq!(sp.length(), 480);
        assert_eq!(sp.counter(), 0);
    }

    #[test]
    fn smoothing_begin_activate() {
        let mut sp = SmoothingProvider::new(480);
        sp.begin();
        assert!(sp.is_active());
        assert_eq!(sp.counter(), 0);
    }

    #[test]
    fn smoothing_advance_returns_factor() {
        let mut sp = SmoothingProvider::new(100);
        sp.begin();

        let f0 = sp.advance().unwrap();
        assert!((f0 - 0.0).abs() < 1e-6);
        assert_eq!(sp.counter(), 1);

        let f50 = sp.advance().unwrap();
        // counter=1, length=100, not at midpoint yet
        assert!(f50 > 0.0);
    }

    #[test]
    fn smoothing_completes() {
        let mut sp = SmoothingProvider::new(10);
        sp.begin();

        let mut last_factor = 0.0;
        for _ in 0..10 {
            let f = sp.advance().unwrap();
            assert!(f >= last_factor, "not monotonic: {f} < {last_factor}");
            last_factor = f;
        }

        // 第 11 次：counter=10 > length=10 → 返回 1.0 并停止
        let f = sp.advance().unwrap();
        assert!((f - 1.0).abs() < 1e-6);
        assert!(!sp.is_active());
    }

    #[test]
    fn smoothing_advance_returns_none_when_inactive() {
        let mut sp = SmoothingProvider::new(100);
        assert!(sp.advance().is_none());
    }

    #[test]
    fn smoothing_progress() {
        let mut sp = SmoothingProvider::new(100);
        sp.begin();

        assert_eq!(sp.progress(), 0.0);

        for _ in 0..50 {
            sp.advance();
        }
        assert!((sp.progress() - 0.5).abs() < 0.01);
    }

    #[test]
    fn smoothing_reset() {
        let mut sp = SmoothingProvider::new(100);
        sp.begin();
        sp.advance();
        sp.advance();

        sp.reset();
        assert!(!sp.is_active());
        assert_eq!(sp.counter(), 0);
    }

    #[test]
    fn smoothing_set_length() {
        let mut sp = SmoothingProvider::new(100);
        sp.set_length(200);
        assert_eq!(sp.length(), 200);
    }

    // ── 完整过渡流程模拟 ────────────────────────────────────────────────────

    #[test]
    fn simulate_config_transition() {
        let frames = 100usize;
        let mut sp = SmoothingProvider::new(frames as u32);

        // 旧配置输出
        let old_output = vec![vec![1.0f32; frames], vec![1.0f32; frames]];
        // 新配置输出
        let new_output = vec![vec![2.0f32; frames], vec![2.0f32; frames]];
        let mut mixed = vec![vec![0.0f32; frames], vec![0.0f32; frames]];

        // 开始过渡
        sp.begin();

        for _ in 0..frames {
            let factor = sp.advance().unwrap();
            mix_plane_buffers(&old_output, &new_output, &mut mixed, frames, factor);
        }

        // 过渡结束后最后一帧 factor ≈ 1.0（旧→新）
        // 但因为 advance 在 counter > length 时才返回 1.0 并停止，
        // 最后一次有效 factor 是 counter=100 时的值
        // 所以最终 mixed 应该接近 new_output
        for c in 0..2 {
            for f in 0..frames {
                assert!(
                    (mixed[c][f] - 2.0).abs() < 0.01,
                    "expected ~2.0 at [{c}][{f}], got {}",
                    mixed[c][f]
                );
            }
        }
    }

    #[test]
    fn simulate_transition_no_discontinuity() {
        let length = 480u32;
        let mut sp = SmoothingProvider::new(length);
        sp.begin();

        let mut prev_factor = 0.0f32;
        let max_step = 0.05; // 最大允许单步跳变

        for _ in 0..length {
            let factor = sp.advance().unwrap();
            let step = (factor - prev_factor).abs();
            assert!(
                step <= max_step,
                "discontinuity: factor jumped from {prev_factor} to {factor} (step={step})"
            );
            prev_factor = factor;
        }
    }

    // ── 预设 ────────────────────────────────────────────────────────────────

    #[test]
    fn default_smoothing_length_48k() {
        assert_eq!(default_smoothing_length(48000), 2400); // 50ms
    }

    #[test]
    fn default_smoothing_length_441k() {
        assert_eq!(default_smoothing_length(44100), 2205); // 50ms
    }

    #[test]
    fn short_smoothing_length_48k() {
        assert_eq!(short_smoothing_length(48000), 480); // 10ms
    }

    #[test]
    fn short_smoothing_length_96k() {
        assert_eq!(short_smoothing_length(96000), 960); // 10ms
    }
}