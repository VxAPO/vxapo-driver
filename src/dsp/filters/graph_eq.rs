//! dsp/filters/graph_eq.rs — 图形均衡器（多段 biquad）
//!
//! 实现 `GraphicEQ:` 命令。
//!
//! 语法示例（EqualizerAPO 兼容）：
//! `GraphicEQ: 25 0; 40 0; 63 0; 100 0; 160 0; ... 16000 0`
//!
//! 每段使用一个 Peaking 类型 biquad 滤波器，
//! 中心频率由 ISO 标准 1/3 倍频程确定。
//!
//! 所有 biquad 级联处理（串联）。
//!
//! `process` 方法遵守 RT-safety 约束（Note 12）。
//! 所有 biquad 实例在 `initialize` 时创建。

use crate::dsp::filter::Filter;
use super::biquad::{BiquadFilter, BiquadStructure, BiquadType, compute_coeffs};

/// 单段 EQ 参数。
#[derive(Debug, Clone, Copy)]
pub struct EqBand {
    /// 中心频率（Hz）。
    pub frequency: f32,
    /// 增益（dB）。
    pub gain_db: f32,
}

/// 图形均衡器。
///
/// 多段级联 biquad peaking 滤波器。
#[derive(Debug)]
pub struct GraphicEqFilter {
    /// 各段参数。
    bands: Vec<EqBand>,
    /// 各段 biquad 滤波器（initialize 时创建）。
    biquads: Vec<BiquadFilter>,
}

impl GraphicEqFilter {
    /// 创建图形均衡器。
    ///
    /// - `bands`：各段参数列表
    pub fn new(bands: Vec<EqBand>) -> Self {
        Self {
            bands,
            biquads: Vec::new(),
        }
    }

    /// 段数。
    pub fn band_count(&self) -> usize {
        self.bands.len()
    }
}

impl Filter for GraphicEqFilter {
    fn initialize(&mut self, sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        self.biquads.clear();

        for band in &self.bands {
            let coeffs = compute_coeffs(
                BiquadType::Peaking,
                band.frequency,
                band.gain_db,
                1.414, // Q ≈ sqrt(2)，标准 1/3 倍频程
                sample_rate,
            );
            let mut bq = BiquadFilter::new(coeffs, BiquadStructure::DirectFormIITransposed);
            bq.initialize(sample_rate, channel_names);
            self.biquads.push(bq);
        }

        None
    }

    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        // 级联处理：每段依次处理
        for bq in self.biquads.iter_mut() {
            bq.process(samples, frame_count);
        }
    }

    fn latency(&self) -> u32 {
        0
    }
}

/// 解析 `GraphicEQ:` 参数字符串。
///
/// 格式：`freq1 gain1; freq2 gain2; ...`
///
/// 分号分隔各段，空格分隔频率和增益。
pub fn parse_graphic_eq_params(params: &str) -> Option<Vec<EqBand>> {
    let mut bands = Vec::new();

    for segment in params.split(';') {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }

        let parts: Vec<&str> = segment.split_whitespace().collect();
        if parts.len() != 2 {
            return None;
        }

        let frequency = parts[0].parse::<f32>().ok()?;
        let gain_db = parts[1].parse::<f32>().ok()?;

        if frequency <= 0.0 || !frequency.is_finite() || !gain_db.is_finite() {
            return None;
        }

        bands.push(EqBand { frequency, gain_db });
    }

    if bands.is_empty() {
        None
    } else {
        Some(bands)
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

    // ── parse_graphic_eq_params ──────────────────────────────────────────────

    #[test]
    fn parse_valid() {
        let bands = parse_graphic_eq_params("25 0; 40 -3; 63 6").unwrap();
        assert_eq!(bands.len(), 3);
        assert_eq!(bands[0].frequency, 25.0);
        assert_eq!(bands[0].gain_db, 0.0);
        assert_eq!(bands[1].gain_db, -3.0);
    }

    #[test]
    fn parse_single_band() {
        let bands = parse_graphic_eq_params("1000 3.5").unwrap();
        assert_eq!(bands.len(), 1);
    }

    #[test]
    fn parse_empty_returns_none() {
        assert!(parse_graphic_eq_params("").is_none());
        assert!(parse_graphic_eq_params("  ").is_none());
    }

    #[test]
    fn parse_invalid_format() {
        assert!(parse_graphic_eq_params("abc def").is_none());
        assert!(parse_graphic_eq_params("1000").is_none());
        assert!(parse_graphic_eq_params("1000 abc def").is_none());
    }

    #[test]
    fn parse_negative_frequency() {
        assert!(parse_graphic_eq_params("-100 0").is_none());
    }

    // ── GraphicEqFilter ─────────────────────────────────────────────────────

    #[test]
    fn flat_eq_passthrough() {
        // 所有段增益 0 → 应接近直通
        let bands: Vec<EqBand> = (0..10)
            .map(|i| EqBand {
                frequency: 31.25 * 2.0_f32.powf(i as f32 / 3.0),
                gain_db: 0.0,
            })
            .collect();

        let mut filter = GraphicEqFilter::new(bands);
        filter.initialize(48000, &stereo_names());

        let mut samples = vec![vec![0.0f32; 200]; 2];
        samples[0][0] = 1.0; // 脉冲
        let energy_before: f32 = samples[0].iter().map(|x| x * x).sum();

        filter.process(&mut samples, 200);
        let energy_after: f32 = samples[0].iter().map(|x| x * x).sum();

        // 能量应大致保持（0dB EQ 不改变总能量）
        assert!(
            (energy_after - energy_before).abs() / energy_before.max(1e-10) < 0.5,
            "flat EQ energy: before={} after={}",
            energy_before,
            energy_after
        );
    }

    #[test]
    fn silence_stays_silent() {
        let bands = vec![
            EqBand { frequency: 100.0, gain_db: 6.0 },
            EqBand { frequency: 1000.0, gain_db: -3.0 },
            EqBand { frequency: 10000.0, gain_db: 6.0 },
        ];
        let mut filter = GraphicEqFilter::new(bands);
        filter.initialize(48000, &stereo_names());

        let mut samples = vec![vec![0.0f32; 100]; 2];
        filter.process(&mut samples, 100);

        for f in 0..100 {
            assert!(samples[0][f].abs() < 1e-10);
        }
    }

    #[test]
    fn band_count() {
        let bands = vec![
            EqBand { frequency: 100.0, gain_db: 0.0 },
            EqBand { frequency: 1000.0, gain_db: 0.0 },
        ];
        let filter = GraphicEqFilter::new(bands);
        assert_eq!(filter.band_count(), 2);
    }

    #[test]
    fn latency_zero() {
        let bands = vec![EqBand { frequency: 1000.0, gain_db: 0.0 }];
        let filter = GraphicEqFilter::new(bands);
        assert_eq!(filter.latency(), 0);
    }

    #[test]
    fn multi_channel() {
        let names = vec!["L".into(), "R".into(), "C".into()];
        let bands = vec![
            EqBand { frequency: 500.0, gain_db: 3.0 },
            EqBand { frequency: 5000.0, gain_db: -3.0 },
        ];
        let mut filter = GraphicEqFilter::new(bands);
        filter.initialize(48000, &names);

        let mut samples = vec![vec![1.0f32; 200]; 3];
        filter.process(&mut samples, 200);

        for ch in 0..3 {
            let energy: f32 = samples[ch].iter().map(|x| x * x).sum();
            assert!(energy > 0.0, "channel {} should have output", ch);
        }
    }
}