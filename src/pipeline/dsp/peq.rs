//! dsp/filters/peq.rs — 参量均衡器（级联 biquad）
//!
//! 实现 `IIR:` 命令中的 Peaking 类型。
//!
//! 语法示例（EqualizerAPO 兼容）：
//! - `IIR, PK, Fc = 1000, Gain = 3, Q = 1.41`
//!
//! 多段 PEQ 通过在配置文件中多次使用 `IIR, PK, ...` 实现，
//! 每段产生一个独立的 `PeakingFilter` 实例。
//!
//! 内部使用 `BiquadFilter`（Direct Form II Transposed）。

use crate::pipeline::dsp::filter::Filter;
use crate::pipeline::dsp::biquad::{BiquadCoeffs, BiquadFilter, BiquadStructure, BiquadType, compute_coeffs};

/// 单段参量均衡器。
#[derive(Debug)]
pub struct PeakingFilter {
    inner: BiquadFilter,
    fc: f32,
    gain_db: f32,
    q: f32,
}

impl PeakingFilter {
    /// 创建参量均衡器。
    ///
    /// - `fc`：中心频率（Hz）
    /// - `gain_db`：增益（dB）
    /// - `q`：品质因数
    pub fn new(fc: f32, gain_db: f32, q: f32) -> Self {
        Self {
            inner: BiquadFilter::new(
                BiquadCoeffs::BYPASS,
                BiquadStructure::DirectFormIITransposed,
            ),
            fc,
            gain_db,
            q,
        }
    }
}

impl Filter for PeakingFilter {
    fn initialize(&mut self, sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        let coeffs = compute_coeffs(BiquadType::Peaking, self.fc, self.gain_db, self.q, sample_rate);
        self.inner.set_coeffs(coeffs);
        self.inner.initialize(sample_rate, channel_names)
    }

    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        self.inner.process(samples, frame_count);
    }

    fn latency(&self) -> u32 {
        0
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

    #[test]
    fn zero_gain_passthrough() {
        let mut filter = PeakingFilter::new(1000.0, 0.0, 1.0);
        filter.initialize(48000, &stereo_names());

        let mut samples = vec![
            vec![0.5, -0.3, 0.8, -0.1, 0.0],
            vec![0.1, -0.2, 0.3, -0.4, 0.5],
        ];
        let input0 = samples[0].clone();
        filter.process(&mut samples, 5);

        for f in 0..5 {
            assert!(
                (samples[0][f] - input0[f]).abs() < 0.01,
                "frame {}: got {} expected {}",
                f, samples[0][f], input0[f]
            );
        }
    }

    #[test]
    fn peaking_boost_dc_stays() {
        // DC 不应被 peaking 影响（远离中心频率）
        let mut filter = PeakingFilter::new(5000.0, 12.0, 1.0);
        filter.initialize(48000, &stereo_names());

        let mut samples = vec![vec![1.0f32; 200]; 2];
        filter.process(&mut samples, 200);

        // DC 输出应接近 1.0
        assert!(
            (samples[0][199] - 1.0).abs() < 0.05,
            "DC should pass through peaking, got {}",
            samples[0][199]
        );
    }

    #[test]
    fn silence_stays_silent() {
        let mut filter = PeakingFilter::new(1000.0, 6.0, 2.0);
        filter.initialize(48000, &stereo_names());

        let mut samples = vec![vec![0.0f32; 100]; 2];
        filter.process(&mut samples, 100);

        for f in 0..100 {
            assert!(samples[0][f].abs() < 1e-10);
        }
    }

    #[test]
    fn latency_zero() {
        let filter = PeakingFilter::new(1000.0, 3.0, 1.0);
        assert_eq!(filter.latency(), 0);
    }

    #[test]
    fn multiple_peq_stages() {
        // 两段级联：低频 boost + 高频 cut
        let mut low = PeakingFilter::new(200.0, 6.0, 1.0);
        let mut high = PeakingFilter::new(8000.0, -3.0, 1.0);
        low.initialize(48000, &stereo_names());
        high.initialize(48000, &stereo_names());

        let mut samples = vec![vec![0.0f32; 200]; 2];
        samples[0][0] = 1.0; // 脉冲

        low.process(&mut samples, 200);
        high.process(&mut samples, 200);

        // 级联后信号应存在（非零）
        let energy: f32 = samples[0].iter().map(|x| x * x).sum();
        assert!(energy > 0.01, "cascaded PEQ should produce output");
    }
}