//! dsp/filters/loudness.rs — ISO 226 等响曲线
//!
//! 实现 `LoudnessCorrection:` 命令。
//!
//! 根据 ISO 226:2003 标准，根据播放音量（phon）自动调整频率响应，
//! 模拟人耳在不同声压级下的感知差异。
//!
//! 实现方式：
//! 1. 预计算 ISO 226 曲线（在 `initialize` 中，非实时路径）
//! 2. 用多段 biquad（GraphicEq）拟合等响曲线
//! 3. `process` 时通过 biquad 级联执行 EQ 调整
//!
//! 当前为占位实现，直接使用 GraphicEq 内部。
//! Phase 8+ 补全 ISO 226 查表 + 曲线拟合。

use crate::dsp::filter::Filter;
use super::biquad::{BiquadFilter, BiquadStructure, BiquadType, compute_coeffs};

/// 等响曲线滤波器。
#[derive(Debug)]
pub struct LoudnessFilter {
    /// 目标声压级（phon）。
    phon: f32,
    /// 参考声压级（phon），曲线在此处为平直。
    reference_phon: f32,
    /// 各频段 biquad。
    biquads: Vec<BiquadFilter>,
}

/// ISO 226 标准 1/3 倍频程中心频率。
const ISO_FREQUENCIES: [f32; 29] = [
    20.0, 25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0,
    200.0, 250.0, 315.0, 400.0, 500.0, 630.0, 800.0, 1000.0, 1250.0,
    1600.0, 2000.0, 2500.0, 3150.0, 4000.0, 5000.0, 6300.0, 8000.0,
    10000.0, 12500.0,
];

impl LoudnessFilter {
    /// 创建等响曲线滤波器。
    ///
    /// - `phon`：目标声压级（phon），典型值 40–80
    /// - `reference_phon`：参考声压级，曲线在此处为平直（默认 80 phon）
    pub fn new(phon: f32, reference_phon: f32) -> Self {
        Self {
            phon,
            reference_phon,
            biquads: Vec::new(),
        }
    }
}

impl Filter for LoudnessFilter {
    fn initialize(&mut self, sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        self.biquads.clear();

        let diff = self.phon - self.reference_phon;
        if diff.abs() < 0.1 {
            // 参考级别：不需要调整
            return None;
        }

        // 简化 ISO 226 曲线拟合：
        // 低频 boost/cut + 高频微调
        // 完整 ISO 226 查表在 Phase 8+ 补全
        for (i, &freq) in ISO_FREQUENCIES.iter().enumerate() {
            let gain = iso_226_approx(freq, diff);
            if gain.abs() > 0.05 {
                let coeffs = compute_coeffs(
                    BiquadType::Peaking,
                    freq,
                    gain,
                    1.414, // Q ≈ sqrt(2)，1/3 倍频程
                    sample_rate,
                );
                let mut bq = BiquadFilter::new(coeffs, BiquadStructure::DirectFormIITransposed);
                bq.initialize(sample_rate, channel_names);
                self.biquads.push(bq);
            }
        }

        None
    }

    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        for bq in self.biquads.iter_mut() {
            bq.process(samples, frame_count);
        }
    }

    fn latency(&self) -> u32 {
        0
    }
}

/// 简化的 ISO 226 曲线近似。
///
/// 返回该频率在给定 phon 差值下的增益调整量（dB）。
///
/// 这是一个粗糙的近似，完整实现需要 ISO 226 查表 + 插值。
fn iso_226_approx(freq: f32, phon_diff: f32) -> f32 {
    // 低频需要更多补偿，高频需要少量补偿
    // 基于等响曲线的一般特征
    let freq_factor = if freq < 200.0 {
        // 低频：补偿量随频率降低而增大
        let normalized = (200.0 / freq).log10(); // 0 at 200Hz, ~1 at 20Hz
        normalized * 0.8
    } else if freq > 6000.0 {
        // 高频：少量补偿
        let normalized = (freq / 6000.0).log10();
        normalized * 0.3
    } else {
        // 中频：补偿量小
        0.05
    };

    phon_diff * freq_factor
}

/// 解析 `LoudnessCorrection:` 参数。
///
/// 格式：`phon [reference_phon]`
pub fn parse_loudness_params(params: &str) -> Option<(f32, f32)> {
    let parts: Vec<&str> = params.split_whitespace().collect();
    if parts.is_empty() {
        return None;
    }

    let phon = parts[0].parse::<f32>().ok()?;
    let reference = if parts.len() >= 2 {
        parts[1].parse::<f32>().unwrap_or(80.0)
    } else {
        80.0
    };

    Some((phon, reference))
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

    // ── parse_loudness_params ────────────────────────────────────────────────

    #[test]
    fn parse_phon_only() {
        let (phon, ref_phon) = parse_loudness_params("40").unwrap();
        assert_eq!(phon, 40.0);
        assert_eq!(ref_phon, 80.0);
    }

    #[test]
    fn parse_both() {
        let (phon, ref_phon) = parse_loudness_params("60 80").unwrap();
        assert_eq!(phon, 60.0);
        assert_eq!(ref_phon, 80.0);
    }

    #[test]
    fn parse_empty() {
        assert!(parse_loudness_params("").is_none());
    }

    // ── LoudnessFilter ──────────────────────────────────────────────────────

    #[test]
    fn reference_phon_passthrough() {
        let mut filter = LoudnessFilter::new(80.0, 80.0);
        filter.initialize(48000, &stereo_names());

        // 参考级别：无 biquad 创建
        assert!(filter.biquads.is_empty());

        let mut samples = vec![vec![1.0, 2.0, 3.0], vec![0.5, 1.0, 1.5]];
        let input = samples.clone();
        filter.process(&mut samples, 3);

        assert_eq!(samples, input);
    }

    #[test]
    fn low_phon_creates_eq() {
        let mut filter = LoudnessFilter::new(40.0, 80.0);
        filter.initialize(48000, &stereo_names());

        // 40 phon vs 80 phon reference → 应创建低频 boost 段
        assert!(!filter.biquads.is_empty());
    }

    #[test]
    fn silence_stays_silent() {
        let mut filter = LoudnessFilter::new(40.0, 80.0);
        filter.initialize(48000, &stereo_names());

        let mut samples = vec![vec![0.0f32; 200]; 2];
        filter.process(&mut samples, 200);

        for f in 0..200 {
            assert!(samples[0][f].abs() < 1e-10);
        }
    }

    #[test]
    fn latency_zero() {
        let filter = LoudnessFilter::new(40.0, 80.0);
        assert_eq!(filter.latency(), 0);
    }
}