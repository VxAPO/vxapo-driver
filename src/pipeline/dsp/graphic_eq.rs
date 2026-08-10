//! dsp/graphic_eq.rs — 图形均衡器（EqualizerAPO 兼容的卷积实现）
//!
//! 语法示例（EqualizerAPO 兼容）：
//! `GraphicEQ: 25 0; 40 0; 63 0; 100 0; 160 0; ... 16000 0`
//!
//! 实现方式与 EqualizerAPO 对齐（GraphicEQFilter.cpp）：
//! 1. 在节点之间按对数频率线性插值增益，生成目标频响；
//! 2. 用最小相位（cepstrum）把频响转成 FIR；
//! 3. 通过分区 FFT 卷积执行（复用 `ConvolutionFilter`，RT 零分配）。
//!
//! 旧实现把每个频点当作 Q=1.414 的 peaking biquad 级联，相邻负增益互相叠加，
//! 实测全负曲线会被额外压低 6~9 dB（中心 -11~-12 dB 而非 -3 dB）——已废弃。

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

use crate::pipeline::dsp::convolution::ConvolutionFilter;
use crate::pipeline::dsp::filter::Filter;
use crate::pipeline::dsp::math::{MAX_GRAPHIC_EQ_BANDS, clamp_gain_db, db_to_linear, warn_rate_limited};

/// 生成的 FIR 长度（EqualizerAPO 用 16384）。
/// 这里用 1024 点最小相位 FIR + 分块 FFT 卷积（块 128，v9.5）：
/// - 频响与旧直接 FIR 完全一致，但单实例 CPU 约降 3~4 倍——多路音频流
///   （每路一个 PreMix 实例）不再吃满 audiodg，声音设置页卡顿随之缓解；
/// - 分块带来 128 采样隐藏延迟（≈2.7ms@48k），与既有策略一致不上报引擎。
const GRAPHIC_EQ_IR_LEN: usize = 1024;
/// 频响幅值下限，避免 log(0)。
const GRAPHIC_EQ_MIN_MAG: f32 = 1e-5;

/// 单段 EQ 参数。
#[derive(Debug, Clone, Copy)]
pub struct EqBand {
    /// 中心频率（Hz）。
    pub frequency: f32,
    /// 增益（dB）。
    pub gain_db: f32,
}

/// 图形均衡器。
#[derive(Debug)]
pub struct GraphicEqFilter {
    /// 各段参数（含 0 dB 段，用于 band_count / 调试）。
    bands: Vec<EqBand>,
    /// 本滤波器作用的平面通道槽位（`Channel:` 选择，空 = 顺序 0..N）。
    channel_indices: Vec<usize>,
    /// 卷积执行器（IR 在 initialize 时生成）。
    conv: ConvolutionFilter,
}

impl GraphicEqFilter {
    /// 创建图形均衡器。
    pub fn new(bands: Vec<EqBand>) -> Self {
        Self {
            bands,
            channel_indices: Vec::new(),
            conv: ConvolutionFilter::with_ir_direct(Vec::new(), 0.0),
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

        let needs_filter = self
            .bands
            .iter()
            .any(|b| b.gain_db.abs() >= 0.05);
        let ir = if needs_filter {
            build_graphic_eq_ir(&self.bands, sample_rate, GRAPHIC_EQ_IR_LEN)
        } else {
            Vec::new()
        };

        let mut conv = ConvolutionFilter::with_ir(ir, 0.0);
        conv.set_channel_indices(&self.channel_indices);
        conv.initialize(sample_rate, channel_names);
        self.conv = conv;
        None
    }

    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        self.conv.process(samples, frame_count);
    }

    fn latency(&self) -> u32 {
        self.conv.latency()
    }

    fn set_channel_indices(&mut self, indices: &[usize]) {
        self.channel_indices = indices.to_vec();
        self.conv.set_channel_indices(indices);
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// FIR 生成（对数频率插值 + 最小相位 cepstrum）
// ══════════════════════════════════════════════════════════════════════════════

/// 按频率排序节点副本。
fn sorted_bands(bands: &[EqBand]) -> Vec<EqBand> {
    let mut v = bands.to_vec();
    v.sort_by(|a, b| a.frequency.total_cmp(&b.frequency));
    v
}

/// 对数频率线性插值：`EqualizerAPO GainIterator` 同语义。
///
/// 低于首个节点 / 高于末个节点时，保持端点增益（频带外平坦）。
fn gain_at(freq: f32, nodes: &[EqBand]) -> f32 {
    if nodes.is_empty() {
        return 0.0;
    }
    if freq <= nodes[0].frequency {
        return nodes[0].gain_db;
    }
    let last = nodes.len() - 1;
    if freq >= nodes[last].frequency {
        return nodes[last].gain_db;
    }

    for i in 0..last {
        let f0 = nodes[i].frequency;
        let f1 = nodes[i + 1].frequency;
        if freq >= f0 && freq <= f1 {
            if f1 <= f0 {
                return nodes[i].gain_db;
            }
            let t = (freq.ln() - f0.ln()) / (f1.ln() - f0.ln());
            return nodes[i].gain_db + t * (nodes[i + 1].gain_db - nodes[i].gain_db);
        }
    }
    nodes[last].gain_db
}

/// 从 EQ 节点生成最小相位 FIR。
///
/// 算法（EqualizerAPO GraphicEQFilter::mps 对齐）：
/// 频响幅值 → log → IFFT 到倒谱 → 因果折叠 → FFT → exp → IFFT。
fn build_graphic_eq_ir(bands: &[EqBand], sample_rate: u32, n: usize) -> Vec<f32> {
    let nodes = sorted_bands(bands);
    if nodes.is_empty() {
        return Vec::new();
    }

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n);
    let ifft = planner.plan_fft_inverse(n);
    let scratch_len = fft
        .get_inplace_scratch_len()
        .max(ifft.get_inplace_scratch_len());
    let mut scratch = vec![Complex::new(0.0, 0.0); scratch_len];

    // 1. 频响幅值（log 域，对称填充）。
    let mut spectrum = vec![Complex::new(0.0, 0.0); n];
    for k in 0..=n / 2 {
        let freq = k as f32 * sample_rate as f32 / n as f32;
        let db = gain_at(freq, &nodes);
        let mag = db_to_linear(db).max(GRAPHIC_EQ_MIN_MAG);
        spectrum[k] = Complex::new(mag.ln(), 0.0);
    }
    for k in 1..n / 2 {
        spectrum[n - k] = spectrum[k];
    }

    // 2. IFFT → 倒谱，并归一化（rustfft 的 IFFT 未缩放）。
    ifft.process_with_scratch(&mut spectrum, &mut scratch);
    let inv_n = 1.0 / n as f32;
    for v in spectrum.iter_mut() {
        v.re *= inv_n;
        v.im = 0.0;
    }

    // 3. 最小相位倒谱：n>N/2 置 0，0<n<N/2 加倍。
    let mut cep_min = vec![Complex::new(0.0, 0.0); n];
    cep_min[0] = spectrum[0];
    for k in 1..n / 2 {
        cep_min[k] = Complex::new(spectrum[k].re * 2.0, 0.0);
    }
    // cep_min[n/2] 保持 0；k > n/2 保持 0。

    // 4. FFT → 最小相位复频谱，exp 恢复幅值并产生相位。
    fft.process_with_scratch(&mut cep_min, &mut scratch);
    for v in cep_min.iter_mut() {
        let e = v.re.exp();
        let re = e * v.im.cos();
        let im = e * v.im.sin();
        *v = Complex::new(re, im);
    }

    // 5. IFFT → 时域 IR，归一化 + 平滑窗（EqualizerAPO 同款 raised-cosine）。
    ifft.process_with_scratch(&mut cep_min, &mut scratch);
    let mut ir = Vec::with_capacity(n);
    for (i, v) in cep_min.iter().enumerate() {
        let x = v.re * inv_n;
        let factor = 0.5 * (1.0 + (std::f32::consts::PI * i as f32 / n as f32).cos());
        ir.push(x * factor);
    }
    ir
}

// ══════════════════════════════════════════════════════════════════════════════
// 解析
// ══════════════════════════════════════════════════════════════════════════════

/// 解析 `GraphicEQ:` 参数字符串。
///
/// 格式：`freq1 gain1; freq2 gain2; ...`
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

    fn stereo_names() -> Vec<String> {
        vec!["L".into(), "R".into()]
    }

    /// 从 FIR 计算指定频率的幅值响应（dB）。
    fn ir_response_db(ir: &[f32], freq: f32, sample_rate: u32) -> f32 {
        let w = 2.0 * std::f32::consts::PI * freq / sample_rate as f32;
        let mut re = 0.0f32;
        let mut im = 0.0f32;
        for (i, &v) in ir.iter().enumerate() {
            let t = w * i as f32;
            re += v * t.cos();
            im -= v * t.sin();
        }
        20.0 * (re.hypot(im)).log10()
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

    // ── gain_at 插值 ─────────────────────────────────────────────────────────

    #[test]
    fn gain_at_interpolates_log_frequency() {
        let nodes = vec![
            EqBand { frequency: 100.0, gain_db: 0.0 },
            EqBand { frequency: 1000.0, gain_db: -10.0 },
        ];
        // 几何中点 ≈316 Hz → -5 dB。
        let g = gain_at(316.227766, &nodes);
        assert!((g - (-5.0)).abs() < 0.1, "g={g}");
    }

    #[test]
    fn gain_at_clamps_outside_range() {
        let nodes = vec![
            EqBand { frequency: 100.0, gain_db: -2.0 },
            EqBand { frequency: 1000.0, gain_db: 3.0 },
        ];
        assert_eq!(gain_at(10.0, &nodes), -2.0);
        assert_eq!(gain_at(10000.0, &nodes), 3.0);
    }

    // ── GraphicEqFilter 行为 ────────────────────────────────────────────────

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
        assert_eq!(filter.latency(), 0);

        let mut samples = vec![vec![1.0f32; 64]; 2];
        filter.process(&mut samples, 64);
        for ch in &samples {
            for &v in ch {
                assert!((v - 1.0).abs() < 1e-4, "flat EQ should passthrough");
            }
        }
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
    fn latency_is_partition_block_size() {
        let bands = vec![EqBand { frequency: 1000.0, gain_db: 3.0 }];
        let mut filter = GraphicEqFilter::new(bands);
        filter.initialize(48000, &stereo_names());
        assert_eq!(
            filter.latency(),
            crate::pipeline::dsp::math::CONVOLUTION_PARTITION_SIZE as u32
        );
    }

    #[test]
    fn all_negative_bands_do_not_stack_excessively() {
        // 31 段 1/3 倍频程全 -3 dB：卷积实现的实际中心响应应接近 -3 dB，
        // 而不是旧 biquad 级联的 -11~-12 dB。
        let bands: Vec<EqBand> = (0..31)
            .map(|i| EqBand {
                frequency: 20.0 * 2.0_f32.powf(i as f32 / 3.0),
                gain_db: -3.0,
            })
            .collect();
        let ir = build_graphic_eq_ir(&bands, 48000, GRAPHIC_EQ_IR_LEN);
        let db = ir_response_db(&ir, 1000.0, 48000);
        assert!(
            db > -5.0 && db < -1.0,
            "all -3 dB bands should stay near -3 dB, got {db:.2} dB"
        );
    }


    #[test]
    fn cut_at_1000_changes_440_sine() {
        let bands = parse_graphic_eq_params("1000.4 -24").unwrap();
        let mut filter = GraphicEqFilter::new(bands);
        filter.initialize(48000, &stereo_names());

        let mut samples = vec![vec![0.0f32; 4800]; 2];
        for f in 0..4800 {
            samples[0][f] = (2.0 * std::f32::consts::PI * 440.0 * f as f32 / 48000.0).sin();
        }
        let before = samples[0].clone();
        filter.process(&mut samples, 4800);

        let mut max_diff = 0.0f32;
        for f in 2000..4800 {
            max_diff = max_diff.max((samples[0][f] - before[f]).abs());
        }
        assert!(
            max_diff > 0.001,
            "GraphicEQ -24dB @1kHz should affect 440Hz sine, max_diff={max_diff}"
        );
    }

    #[test]
    fn channel_selection_routes_to_selected_slot() {
        let bands = parse_graphic_eq_params("1000 6").unwrap();
        let mut filter = GraphicEqFilter::new(bands);
        filter.set_channel_indices(&[1]);
        filter.initialize(48000, &vec!["R".to_owned()]);

        let mut samples = vec![vec![0.5f32; 1024], vec![0.5f32; 1024]];
        let l_orig = samples[0].clone();
        filter.process(&mut samples, 1024);

        assert_eq!(samples[0], l_orig);
        assert!(samples[1].iter().any(|&v| v != 0.5));
    }
}
