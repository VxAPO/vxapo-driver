//! dsp/filters/hp_lp.rs — 高通/低通滤波器（基于 biquad 系数生成）
//!
//! 实现 `IIR:` 命令中的 HighPass / LowPass 类型。
//!
//! 内部使用 `BiquadFilter`（Direct Form II Transposed）。

use crate::dsp::filter::Filter;
use super::biquad::{BiquadCoeffs, BiquadFilter, BiquadStructure, BiquadType, compute_coeffs};

/// 高通/低通滤波器。
#[derive(Debug)]
pub struct HighLowPassFilter {
    inner: BiquadFilter,
    filter_type: BiquadType,
    fc: f32,
    q: f32,
}

impl HighLowPassFilter {
    /// 创建高通滤波器。
    pub fn high_pass(fc: f32, q: f32) -> Self {
        Self::new(BiquadType::HighPass, fc, q)
    }

    /// 创建低通滤波器。
    pub fn low_pass(fc: f32, q: f32) -> Self {
        Self::new(BiquadType::LowPass, fc, q)
    }

    /// 从类型和参数创建。
    ///
    /// `filter_type` 必须是 `HighPass` 或 `LowPass`。
    pub fn new(filter_type: BiquadType, fc: f32, q: f32) -> Self {
        Self {
            inner: BiquadFilter::new(
                BiquadCoeffs::BYPASS,
                BiquadStructure::DirectFormIITransposed,
            ),
            filter_type,
            fc,
            q,
        }
    }
}

impl Filter for HighLowPassFilter {
    fn initialize(&mut self, sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        let coeffs = compute_coeffs(self.filter_type, self.fc, 0.0, self.q, sample_rate);
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
    fn lowpass_silence_stays_silent() {
        let mut filter = HighLowPassFilter::low_pass(1000.0, 0.707);
        filter.initialize(48000, &stereo_names());

        let mut samples = vec![vec![0.0f32; 100]; 2];
        filter.process(&mut samples, 100);

        for f in 0..100 {
            assert!(samples[0][f].abs() < 1e-10);
        }
    }

    #[test]
    fn highpass_silence_stays_silent() {
        let mut filter = HighLowPassFilter::high_pass(100.0, 0.707);
        filter.initialize(48000, &stereo_names());

        let mut samples = vec![vec![0.0f32; 100]; 2];
        filter.process(&mut samples, 100);

        for f in 0..100 {
            assert!(samples[0][f].abs() < 1e-10);
        }
    }

    #[test]
    fn lowpass_removes_dc() {
        // DC（0 Hz）应完整通过低通
        let mut filter = HighLowPassFilter::low_pass(1000.0, 0.707);
        filter.initialize(48000, &stereo_names());

        let mut samples = vec![vec![1.0f32; 200]; 2];
        filter.process(&mut samples, 200);

        // 稳态后输出应接近 1.0（DC 通过低通）
        assert!(
            samples[0][199] > 0.8,
            "DC should pass through LP, got {}",
            samples[0][199]
        );
    }

    #[test]
    fn highpass_removes_dc() {
        let mut filter = HighLowPassFilter::high_pass(100.0, 0.707);
        filter.initialize(48000, &stereo_names());

        let mut samples = vec![vec![1.0f32; 500]; 2];
        filter.process(&mut samples, 500);

        // 稳态后 DC 应被衰减
        assert!(
            samples[0][499].abs() < 0.1,
            "DC should be rejected by HP, got {}",
            samples[0][499]
        );
    }

    #[test]
    fn multi_channel() {
        let names = vec!["L".into(), "R".into(), "C".into()];
        let mut filter = HighLowPassFilter::low_pass(2000.0, 0.707);
        filter.initialize(48000, &names);

        let mut samples = vec![vec![1.0f32; 200]; 3];
        filter.process(&mut samples, 200);

        // 三个通道都应处理
        for ch in 0..3 {
            assert!(samples[ch][199].abs() > 0.0);
        }
    }

    #[test]
    fn latency_zero() {
        let filter = HighLowPassFilter::low_pass(1000.0, 0.707);
        assert_eq!(filter.latency(), 0);
    }
}