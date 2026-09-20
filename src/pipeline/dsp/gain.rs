//! pipeline/dsp/gain.rs — 增益滤波器（含内部平滑插值）
//!
//! 实现 `Preamp:` 命令。
//!
//! 平滑过渡：
//! 增益变化时使用**比例平滑**（等效对数域）避免 click/pop；带到达步数与跳转阈值。
//!
//! 数值护栏（P0）：
//! - dB 经 `math::clamp_gain_db`（[-120, +48]），线性因子保证有限；
//! - 非有限目标直接忽略；
//! - `process` 遵守 RT-safety 约束，零分配。

// GAIN_DB_MAX / linear_to_db 仅被 cfg(test) 下的用例与辅助函数使用。
#[cfg(test)]
use crate::pipeline::dsp::math::{GAIN_DB_MAX, linear_to_db as math_linear_to_db};

use crate::pipeline::dsp::filter::Filter;
use crate::pipeline::dsp::math::{
    GAIN_SMOOTH_RERATE, GAIN_SMOOTH_STEPS_DEFAULT, GAIN_SNAP_THRESHOLD,
    MAX_GAIN_STEP_RATIO, db_to_linear as math_db_to_linear,
};

// ── 参数模型（随实现；聚合见 `dsp::model` 的 re-export）────────────────────

/// 全局增益。
#[derive(Debug, Clone, Copy)]
pub struct PreampParams {
    pub gain_db: f32,
}

/// 增益滤波器。
///
/// 支持：
/// - 固定增益（dB → linear）
/// - 比例平滑过渡（无 click，极端跳变有界）
#[derive(Debug)]
pub struct GainFilter {
    /// 目标增益（线性因子）。
    target_gain: f32,
    /// 当前增益（线性因子，平滑插值中）。
    current_gain: f32,
    /// 每采样比例步进（重算间隔内复用）。
    ratio: f32,
    /// 平滑步数计数。
    step_counter: u32,
    /// 目标步数（到达时间保证；受 `MAX_GAIN_STEP_RATIO` 上限约束）。
    steps_to_reach: u32,
    /// 本滤波器作用的平面通道槽位（`Channel:` 选择，空 = 顺序 0..N）。
    channel_indices: Vec<usize>,
}

impl GainFilter {
    /// 创建增益滤波器。
    ///
    /// - `gain_db`：增益（dB），经 clamp 保证有限
    pub fn new(gain_db: f32) -> Self {
        let linear = math_db_to_linear(gain_db);
        Self {
            target_gain: linear,
            current_gain: linear,
            ratio: 1.0,
            step_counter: 0,
            steps_to_reach: GAIN_SMOOTH_STEPS_DEFAULT,
            channel_indices: Vec::new(),
        }
    }

    /// 设置新增益（dB），触发平滑过渡。
#[cfg(test)]
    pub fn set_gain_db(&mut self, gain_db: f32) {
        self.set_gain_linear(math_db_to_linear(gain_db));
    }

    /// 设置新增益（线性），触发平滑过渡。
#[cfg(test)]
    pub fn set_gain_linear(&mut self, target: f32) {
        if !target.is_finite() {
            return; // 非有限目标：忽略，保持当前值。
        }
        let target = target.clamp(0.0, math_db_to_linear(GAIN_DB_MAX));
        self.target_gain = target;
        self.ratio = 1.0;
        self.step_counter = 0;
        self.steps_to_reach = GAIN_SMOOTH_STEPS_DEFAULT;
    }

    /// 当前增益（线性）。
#[cfg(test)]
    pub fn current_gain_linear(&self) -> f32 {
        self.current_gain
    }

    /// 当前增益（dB）。
#[cfg(test)]
    #[allow(dead_code)] // 死簇：仅被已死的调用链引用，删除需整链评估
    pub fn current_gain_db(&self) -> f32 {
        math_linear_to_db(self.current_gain)
    }

    /// 比例平滑推进一采样。
    ///
    /// - 足够接近目标 → 直接跳转（`GAIN_SNAP_THRESHOLD` 相对误差）；
    /// - 每 `GAIN_SMOOTH_RERATE` 采样重算一次理想比例，并 clamp 到
    ///   `[1-MAX_GAIN_STEP_RATIO, 1+MAX_GAIN_STEP_RATIO]`，避免每采样 powf；
    /// - 从极小值（≈0）恢复时先给一个可数的起点。
    #[inline]
    fn advance(&mut self) {
        if (self.current_gain - self.target_gain).abs()
            < GAIN_SNAP_THRESHOLD * self.target_gain.abs().max(1.0)
        {
            self.current_gain = self.target_gain;
            return;
        }

        if self.current_gain.abs() < GAIN_SNAP_THRESHOLD {
            self.current_gain = self.target_gain * 1e-4;
        }

        if self.step_counter.is_multiple_of(GAIN_SMOOTH_RERATE) {
            // remaining 下限 = GAIN_SMOOTH_RERATE：避免 counter 超过目标步数后
            // “单步到位”控制器被 ±5% clamp 来回振荡，改为 ≥32 步的收缩控制器。
            let remaining = (self.steps_to_reach.saturating_sub(self.step_counter))
                .max(GAIN_SMOOTH_RERATE);
            let ideal = (self.target_gain / self.current_gain).powf(1.0 / remaining as f32);
            self.ratio = ideal.clamp(1.0 - MAX_GAIN_STEP_RATIO, 1.0 + MAX_GAIN_STEP_RATIO);
        }
        self.current_gain *= self.ratio;
        self.step_counter += 1;
    }
}

impl Filter for GainFilter {
    fn initialize(&mut self, _sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        if self.channel_indices.is_empty() {
            self.channel_indices = (0..channel_names.len()).collect();
        }
        None
    }

    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        for f in 0..frame_count {
            if self.current_gain != self.target_gain {
                self.advance();
            }

            let gain = self.current_gain;
            for &ch in &self.channel_indices {
                if let Some(buf) = samples.get_mut(ch) {
                    buf[f] *= gain;
                }
            }
        }
    }

    fn set_channel_indices(&mut self, indices: &[usize]) {
        self.channel_indices = indices.to_vec();
    }
}

/// dB 转线性因子（薄转发到 `math::db_to_linear`，保持既有公开 API）。
#[cfg(test)]
pub fn db_to_linear(db: f32) -> f32 {
    math_db_to_linear(db)
}

/// 线性因子转 dB（薄转发到 `math::linear_to_db`）。
#[cfg(test)]
pub fn linear_to_db(linear: f32) -> f32 {
    math_linear_to_db(linear)
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo_names() -> Vec<String> {
        vec!["L".into(), "R".into()]
    }

    // ── dB ↔ linear 转换 ────────────────────────────────────────────────────

    #[test]
    fn zero_db_is_unity() {
        assert!((db_to_linear(0.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn neg_6db_is_half() {
        let linear = db_to_linear(-6.0206);
        assert!((linear - 0.5).abs() < 0.001);
    }

    #[test]
    fn plus_6db_is_double() {
        let linear = db_to_linear(6.0206);
        assert!((linear - 2.0).abs() < 0.01);
    }

    #[test]
    fn linear_to_db_roundtrip() {
        let db = 3.5;
        let linear = db_to_linear(db);
        let back = linear_to_db(linear);
        assert!((back - db).abs() < 1e-5);
    }

    #[test]
    fn linear_to_db_zero() {
        assert_eq!(linear_to_db(0.0), f32::NEG_INFINITY);
    }

    #[test]
    fn linear_to_db_negative() {
        assert_eq!(linear_to_db(-1.0), f32::NEG_INFINITY);
    }

    #[test]
    fn extreme_db_clamped_finite() {
        let hi = GainFilter::new(1000.0);
        assert!(hi.current_gain_linear().is_finite());
        assert!((hi.current_gain_linear() - db_to_linear(GAIN_DB_MAX)).abs() < 1e-3);

        let lo = GainFilter::new(-1000.0);
        assert!(lo.current_gain_linear().is_finite());
    }

    // ── GainFilter 基础 ─────────────────────────────────────────────────────

    #[test]
    fn unity_gain_passthrough() {
        let mut filter = GainFilter::new(0.0);
        filter.initialize(48000, &stereo_names());

        let mut samples = vec![
            vec![1.0, 2.0, 3.0, 4.0],
            vec![0.5, 1.0, 1.5, 2.0],
        ];
        filter.process(&mut samples, 4);

        assert_eq!(samples[0], vec![1.0, 2.0, 3.0, 4.0]);
        assert_eq!(samples[1], vec![0.5, 1.0, 1.5, 2.0]);
    }

    #[test]
    fn gain_plus_6db_doubles() {
        let mut filter = GainFilter::new(6.0206);
        filter.initialize(48000, &stereo_names());

        let mut samples = vec![vec![1.0, 2.0], vec![0.5, 1.0]];
        filter.process(&mut samples, 2);

        assert!((samples[0][0] - 2.0).abs() < 0.01);
        assert!((samples[0][1] - 4.0).abs() < 0.02);
    }

    #[test]
    fn gain_minus_inf_is_silent() {
        let mut filter = GainFilter::new(-100.0); // clamp 到 -120 dB ≈ 1e-6
        filter.initialize(48000, &stereo_names());

        let mut samples = vec![vec![1.0; 10]; 2];
        filter.process(&mut samples, 10);

        for f in 0..10 {
            assert!(samples[0][f].abs() < 1e-4);
        }
    }

    // ── 平滑过渡 ────────────────────────────────────────────────────────────

    #[test]
    fn gain_change_smooths() {
        let mut filter = GainFilter::new(0.0);
        filter.initialize(48000, &stereo_names());

        filter.set_gain_db(6.0);

        let mut samples = vec![vec![1.0; 200]; 2];
        filter.process(&mut samples, 200);

        let final_val = samples[0][199];
        assert!((final_val - db_to_linear(6.0)).abs() < 0.01);
    }

    #[test]
    fn gain_change_no_click() {
        let mut filter = GainFilter::new(0.0);
        filter.initialize(48000, &stereo_names());

        filter.set_gain_linear(0.0); // 突变到静音

        let mut samples = vec![vec![1.0; 200]; 2];
        filter.process(&mut samples, 200);

        for f in 1..200 {
            let diff = (samples[0][f] - samples[0][f - 1]).abs();
            assert!(
                diff < 0.1,
                "click detected at frame {}: diff = {}",
                f, diff
            );
        }
    }

    #[test]
    fn extreme_jump_smooths_without_click() {
        let mut filter = GainFilter::new(0.0);
        filter.initialize(48000, &stereo_names());

        // +1000 dB → clamp 到 +48 dB（线性 ≈ 251）。
        filter.set_gain_db(1000.0);

        let mut samples = vec![vec![0.25; 600]; 2];
        filter.process(&mut samples, 600);

        // 输出全程有限，峰值有界（无 inf / 爆音）。
        for ch in samples.iter() {
            for &v in ch.iter() {
                assert!(v.is_finite());
                assert!(v < 300.0, "peak too high: {v}");
            }
        }
        // 600 采样后应到达目标（113 步 ≈ 到达，远小于 600）。
        let target = db_to_linear(GAIN_DB_MAX);
        assert!((samples[0][599] - target * 0.25).abs() < 0.01 * target);
    }

    #[test]
    fn deep_cut_recovers_within_bounded_steps() {
        let mut filter = GainFilter::new(-120.0); // 1e-6
        filter.initialize(48000, &stereo_names());

        filter.set_gain_db(0.0); // 恢复到 1.0

        let mut samples = vec![vec![1.0; 400]; 2];
        filter.process(&mut samples, 400);

        // 120 dB 恢复需要 ≈ 283 步（×1.05/步），400 采样内必须到位。
        assert!(
            (samples[0][399] - 1.0).abs() < 0.01,
            "deep cut recovery too slow, got {}",
            samples[0][399]
        );
    }

    #[test]
    fn non_finite_target_ignored() {
        let mut filter = GainFilter::new(0.0);
        filter.initialize(48000, &stereo_names());

        filter.set_gain_linear(f32::NAN);
        filter.set_gain_linear(f32::INFINITY);
        assert_eq!(filter.current_gain_linear(), 1.0);
        assert_eq!(filter.target_gain, 1.0);
    }

    #[test]
    fn latency_zero() {
        let filter = GainFilter::new(0.0);
        assert_eq!(filter.latency(), 0);
    }
}
