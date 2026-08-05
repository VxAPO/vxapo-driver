//! dsp/filters/graph_eq.rs — 图形均衡器（多段 biquad）
//!
//! 实现 `GraphicEQ:` 命令。
//!
//! 语法示例（EqualizerAPO 兼容）：
//! `GraphicEQ: 25 0; 40 0; 63 0; 100 0; 160 0; ... 16000 0`
//!
//! 每段使用一个 Peaking 类型 biquad 滤波器，中心频率由 ISO 标准 1/3 倍频程确定，
//! 所有 biquad 级联处理（串联）。
//!
//! 性能（M3）：有效段数固定后，用**宏递归展开**（tt-list）生成直线依赖链，
//! 展开期保证、不依赖 LLVM 循环展开启发式；`process_sample` 内联为 FMA 链。
//!
//! `process` 方法遵守 RT-safety 约束（Note 12），零分配。

use crate::pipeline::dsp::filter::Filter;
use crate::pipeline::dsp::biquad::{BiquadCoeffs, BiquadState, BiquadType, compute_coeffs};
use crate::pipeline::dsp::math::{MAX_GRAPHIC_EQ_BANDS, clamp_gain_db, warn_rate_limited};

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
/// 多段级联 biquad peaking 滤波器，按有效段数 dispatch 到宏展开的直线级联。
#[derive(Debug)]
pub struct GraphicEqFilter {
    /// 各段参数（含 0 dB 段，用于 band_count / 调试）。
    bands: Vec<EqBand>,
    /// 有效段系数（|gain| ≥ 0.05，≤ MAX_GRAPHIC_EQ_BANDS）。
    coeffs: Vec<BiquadCoeffs>,
    /// 每选中通道一段独立状态：`states[channel][band]`。
    states: Vec<Vec<BiquadState>>,
    /// 本滤波器作用的平面通道槽位（`Channel:` 选择，空 = 顺序 0..N）。
    channel_indices: Vec<usize>,
}

impl GraphicEqFilter {
    /// 创建图形均衡器。
    ///
    /// - `bands`：各段参数列表
    pub fn new(bands: Vec<EqBand>) -> Self {
        Self {
            bands,
            coeffs: Vec::new(),
            states: Vec::new(),
            channel_indices: Vec::new(),
        }
    }

    /// 段数。
    pub fn band_count(&self) -> usize {
        self.bands.len()
    }
}

impl Filter for GraphicEqFilter {
    fn initialize(&mut self, sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        if self.channel_indices.is_empty() {
            self.channel_indices = (0..channel_names.len()).collect();
        }

        // 0 dB 段 H(z)=1，直接跳过；段数上限 31（P0/性能）。
        self.coeffs.clear();
        for band in &self.bands {
            if band.gain_db.abs() < 0.05 {
                continue;
            }
            if self.coeffs.len() >= MAX_GRAPHIC_EQ_BANDS {
                warn_rate_limited(
                    "graphic_eq_bands",
                    "GraphicEQ 段数超过 31，超出部分忽略",
                );
                break;
            }
            let coeffs = compute_coeffs(
                BiquadType::Peaking,
                band.frequency,
                band.gain_db,
                1.414, // Q ≈ sqrt(2)，标准 1/3 倍频程
                sample_rate,
            );
            self.coeffs.push(coeffs);
        }

        let num_ch = self.channel_indices.len().max(1);
        self.states = vec![vec![BiquadState::new(); self.coeffs.len()]; num_ch];
        None
    }

    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        let n_bands = self.coeffs.len();
        if n_bands == 0 {
            return;
        }
        let num_ch = self.channel_indices.len().min(self.states.len());

        for k in 0..num_ch {
            let slot = self.channel_indices[k];
            if slot >= samples.len() {
                continue;
            }
            let coeffs = &self.coeffs;
            let states = &mut self.states[k];
            for f in 0..frame_count {
                samples[slot][f] =
                    process_cascade_dynamic(n_bands, coeffs, states, samples[slot][f]);
            }
        }
    }

    fn latency(&self) -> u32 {
        0
    }

    fn set_channel_indices(&mut self, indices: &[usize]) {
        self.channel_indices = indices.to_vec();
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 宏递归展开级联（M3）
// ══════════════════════════════════════════════════════════════════════════════

/// 递归展开：每步处理一个固定索引段，输出喂给下一段。
///
/// 展开期生成纯直线依赖链：无循环计数器、无变量索引、无分支。
macro_rules! cascade_body {
    ($coeffs:ident, $states:ident, $input:expr, ) => { $input };
    ($coeffs:ident, $states:ident, $input:expr, $head:tt $($tail:tt)*) => {{
        let x = $states[$head].process_sample(&$coeffs[$head], $input);
        cascade_body!($coeffs, $states, x, $($tail)*)
    }};
}

/// 按有效段数 dispatch 到宏展开直线级联（N=0..31）。
///
/// `_` 分支为防御性回退循环（正常路径不会命中——initialize 已把段数 cap 到 31）。
fn process_cascade_dynamic(
    n: usize,
    coeffs: &[BiquadCoeffs],
    states: &mut [BiquadState],
    input: f32,
) -> f32 {
    match n {
        0 => input,
        1 => cascade_body!(coeffs, states, input, 0),
        2 => cascade_body!(coeffs, states, input, 0 1),
        3 => cascade_body!(coeffs, states, input, 0 1 2),
        4 => cascade_body!(coeffs, states, input, 0 1 2 3),
        5 => cascade_body!(coeffs, states, input, 0 1 2 3 4),
        6 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5),
        7 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5 6),
        8 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5 6 7),
        9 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5 6 7 8),
        10 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5 6 7 8 9),
        11 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5 6 7 8 9 10),
        12 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5 6 7 8 9 10 11),
        13 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5 6 7 8 9 10 11 12),
        14 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5 6 7 8 9 10 11 12 13),
        15 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14),
        16 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15),
        17 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16),
        18 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17),
        19 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18),
        20 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19),
        21 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20),
        22 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21),
        23 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22),
        24 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23),
        25 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24),
        26 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25),
        27 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26),
        28 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27),
        29 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28),
        30 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29),
        31 => cascade_body!(coeffs, states, input, 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30),
        _ => {
            // 防御回退（正常不命中：initialize 已 cap 到 31）。
            let mut x = input;
            for b in 0..n {
                x = states[b].process_sample(&coeffs[b], x);
            }
            x
        }
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

        if bands.len() >= MAX_GRAPHIC_EQ_BANDS {
            warn_rate_limited(
                "graphic_eq_bands",
                "GraphicEQ 段数超过 31，超出部分忽略",
            );
            break;
        }

        let gain_db = clamp_gain_db(gain_db);
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
    use crate::pipeline::dsp::biquad::{BiquadFilter, BiquadStructure};

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

    #[test]
    fn parse_extreme_gain_clamped() {
        let bands = parse_graphic_eq_params("1000 1000; 200 -1000").unwrap();
        assert_eq!(bands[0].gain_db, 48.0);
        assert_eq!(bands[1].gain_db, -120.0);
    }

    // ── GraphicEqFilter ─────────────────────────────────────────────────────

    #[test]
    fn flat_eq_passthrough() {
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

    #[test]
    fn cut_at_1000_changes_440_sine() {
        let bands = parse_graphic_eq_params("1000.4 -24").unwrap();
        let mut filter = GraphicEqFilter::new(bands);
        filter.initialize(48000, &stereo_names());

        let mut samples = vec![vec![0.0f32; 480]; 2];
        for f in 0..480 {
            samples[0][f] = (2.0 * std::f32::consts::PI * 440.0 * f as f32 / 48000.0).sin();
        }
        let before = samples[0].clone();
        filter.process(&mut samples, 480);

        let mut max_diff = 0.0f32;
        for f in 0..480 {
            max_diff = max_diff.max((samples[0][f] - before[f]).abs());
        }
        assert!(
            max_diff > 0.001,
            "GraphicEQ -24dB @1kHz should affect 440Hz sine, max_diff={max_diff}"
        );
    }

    #[test]
    fn channel_selection_routes_to_selected_slot() {
        // `Channel: R`：只处理槽位 1，槽位 0 逐位不变。
        let bands = parse_graphic_eq_params("1000 6").unwrap();
        let mut filter = GraphicEqFilter::new(bands);
        filter.set_channel_indices(&[1]);
        filter.initialize(48000, &vec!["R".to_owned()]);

        let mut samples = vec![vec![0.5f32; 64], vec![0.5f32; 64]];
        let l_orig = samples[0].clone();
        filter.process(&mut samples, 64);

        assert_eq!(samples[0], l_orig);
        assert!(samples[1].iter().any(|&v| v != 0.5));
    }

    #[test]
    fn cascade_matches_reference_biquad_chain() {
        // 宏展开级联必须与逐段 BiquadFilter 串联（相同系数/状态数学）一致。
        let bands = parse_graphic_eq_params("100 3; 250 -2; 500 6; 1000 -4; 2000 2; 4000 -1; 8000 3")
            .unwrap();
        let len = 512;
        let input: Vec<f32> = (0..len).map(|i| ((i as f32 * 0.03).sin() * 0.4) as f32).collect();

        let mut filter = GraphicEqFilter::new(bands.clone());
        filter.initialize(48000, &vec!["L".to_owned()]);
        let mut samples = vec![input.clone()];
        filter.process(&mut samples, len);

        // 参考：7 个独立 BiquadFilter 串联（同样跳过 0 dB，系数一致）。
        let mut ref_chain: Vec<BiquadFilter> = Vec::new();
        for band in bands.iter().filter(|b| b.gain_db.abs() >= 0.05) {
            let coeffs = compute_coeffs(
                BiquadType::Peaking,
                band.frequency,
                band.gain_db,
                1.414,
                48000,
            );
            let mut bq = BiquadFilter::new(coeffs, BiquadStructure::DirectFormIITransposed);
            bq.initialize(48000, &vec!["L".to_owned()]);
            ref_chain.push(bq);
        }
        let mut reference = vec![input.clone()];
        for bq in ref_chain.iter_mut() {
            bq.process(&mut reference, len);
        }

        for f in 0..len {
            assert!(
                (samples[0][f] - reference[0][f]).abs() < 1e-9,
                "cascade mismatch at {f}: macro={} ref={}",
                samples[0][f],
                reference[0][f]
            );
        }
    }
}
