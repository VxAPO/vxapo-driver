//! dsp/filters/gain.rs — 增益滤波器（含内部平滑插值）
//!
//! 实现 `Preamp:` 命令。
//!
//! 平滑过渡（Note 15）：
//! 增益变化时使用线性插值避免 click/pop。
//! 平滑长度默认 128 采样（约 2.7ms @ 48kHz）。
//!
//! `process` 方法遵守 RT-safety 约束（Note 12）。

use crate::dsp::filter::Filter;

/// 默认平滑长度（采样数）。
const DEFAULT_SMOOTHING_SAMPLES: usize = 128;

/// 增益滤波器。
///
/// 支持：
/// - 固定增益（dB → linear）
/// - 线性插值平滑过渡
#[derive(Debug)]
pub struct GainFilter {
    /// 目标增益（线性因子）。
    target_gain: f32,
    /// 当前增益（线性因子，平滑插值中）。
    current_gain: f32,
    /// 增益步进（每采样增量）。
    gain_step: f32,
    /// 剩余平滑采样数。
    smoothing_remaining: usize,
    /// 平滑总长度。
    smoothing_length: usize,
}

impl GainFilter {
    /// 创建增益滤波器。
    ///
    /// - `gain_db`：增益（dB）
    /// - `sample_rate`：采样率（初始化时设置）
    pub fn new(gain_db: f32) -> Self {
        let linear = db_to_linear(gain_db);
        Self {
            target_gain: linear,
            current_gain: linear,
            gain_step: 0.0,
            smoothing_remaining: 0,
            smoothing_length: DEFAULT_SMOOTHING_SAMPLES,
        }
    }

    /// 设置新增益（dB），触发平滑过渡。
    pub fn set_gain_db(&mut self, gain_db: f32) {
        self.set_gain_linear(db_to_linear(gain_db));
    }

    /// 设置新增益（线性），触发平滑过渡。
    pub fn set_gain_linear(&mut self, target: f32) {
        self.target_gain = target;
        let diff = target - self.current_gain;
        self.gain_step = diff / self.smoothing_length as f32;
        self.smoothing_remaining = self.smoothing_length;
    }

    /// 当前增益（线性）。
    pub fn current_gain_linear(&self) -> f32 {
        self.current_gain
    }

    /// 当前增益（dB）。
    pub fn current_gain_db(&self) -> f32 {
        linear_to_db(self.current_gain)
    }
}

impl Filter for GainFilter {
    fn initialize(&mut self, _sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        let _ = channel_names;
        None
    }

    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        for f in 0..frame_count {
            // 平滑插值
            if self.smoothing_remaining > 0 {
                self.current_gain += self.gain_step;
                self.smoothing_remaining -= 1;
                if self.smoothing_remaining == 0 {
                    self.current_gain = self.target_gain;
                }
            }

            let gain = self.current_gain;
            for ch in samples.iter_mut() {
                ch[f] *= gain;
            }
        }
    }
}

/// dB 转线性因子。
pub fn db_to_linear(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

/// 线性因子转 dB。
pub fn linear_to_db(linear: f32) -> f32 {
    if linear <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * linear.log10()
    }
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

    // ── GainFilter 基础 ─────────────────────────────────────────────────────

    #[test]
    fn unity_gain_passthrough() {
        let mut filter = GainFilter::new(0.0); // 0 dB = unity
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
        // smoothing_length = 128, but we set gain at creation so
        // current_gain already equals target → no smoothing
        filter.initialize(48000, &stereo_names());

        let mut samples = vec![vec![1.0, 2.0], vec![0.5, 1.0]];
        filter.process(&mut samples, 2);

        assert!((samples[0][0] - 2.0).abs() < 0.01);
        assert!((samples[0][1] - 4.0).abs() < 0.02);
    }

    #[test]
    fn gain_minus_inf_is_silent() {
        let mut filter = GainFilter::new(-100.0); // ≈ -inf dB
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
        let mut filter = GainFilter::new(0.0); // unity
        filter.initialize(48000, &stereo_names());

        // 设置新增益 → 触发平滑
        filter.set_gain_db(6.0);

        // 处理几帧，输出应在过渡中
        let mut samples = vec![vec![1.0; 200]; 2];
        filter.process(&mut samples, 200);

        // 前 128 帧是过渡区
        // 过渡完成后应接近 2.0
        let final_val = samples[0][199];
        assert!((final_val - db_to_linear(6.0)).abs() < 0.01);
    }

    #[test]
    fn gain_change_no_click() {
        let mut filter = GainFilter::new(0.0);
        filter.initialize(48000, &stereo_names());

        filter.set_gain_linear(0.0); // 突变到静音

        // 过渡期间不应有突变
        let mut samples = vec![vec![1.0; 200]; 2];
        filter.process(&mut samples, 200);

        // 检查相邻采样差值不超过步进大小
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
    fn latency_zero() {
        let filter = GainFilter::new(0.0);
        assert_eq!(filter.latency(), 0);
    }
}