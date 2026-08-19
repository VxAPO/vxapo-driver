//! Wide（立体声加宽器）
//!
//! 设计：
//! - **200 Hz 线性相位 FIR 分频**（1024 点 Hamming 窗低通 + 互补高通）：
//!   低频支路 = LP FIR 输出，高频支路 = 延迟对齐原信号 − LP 输出，
//!   两路**完美重建**（无 IIR 相位旋转 / 群延迟差），低频原样不散；
//!   延迟 = 511 采样（与 GraphicEQ 1024 点 FIR 同级）；
//! - 高频支路做 **M/S 宽度处理**：`side = (HL-HR)/2` 按
//!   `1 + 2.3·Intensity^0.6` 放大（甜点 0.5 → ≈2.52×，满档 → ≈3.3×），
//! 中央按 `1 - 0.10·Intensity^0.6` 补偿（斜率较 降低，缓解中频能量不足）；
//! - 高频支路 **tanh 软限幅**：`headroom_db = 0.2 + 0.8·(1-Intensity)`，
//!   `out = tanh(out·10^(-headroom_db/20))`；低频支路不经过 tanh；
//!   `Intensity=0` 仍走直通分支，位精确。
//!
//! config 语法（EAPO 风格）：
//! `Wide: Intensity 0.354331`

use crate::pipeline::dsp::filter::Filter;
use crate::pipeline::dsp::fir::PartitionedFir;

#[derive(Debug, Clone, Copy)]
pub struct WideParams {
    /// 加宽强度（范围 [0, 1]，0 时严格直通）。
    pub intensity: f32,
}

impl Default for WideParams {
    fn default() -> Self {
        Self {
            // 与原 Wide32.c Quick preset 对齐，保持 config 兼容。
            intensity: 0.354331,
        }
    }
}

/// FIR 分频点（Hz），以下低频不处理。
const CROSSOVER_HZ: f32 = 200.0;
/// 直接 FIR 上限（超过走分块 FFT，与 PEQ 一致；避免高采样率长 IR 超实时预算）。
const DIRECT_FIR_MAX_LEN: usize = 2048;
/// FIR 长度下限（低采样率最小抽头数）。
const FIR_MIN_LEN: usize = 1024;
/// FIR 长度上限（384k 时 8192 抽头 ≈ 21.3ms，分块 FFT 承担）。
const FIR_MAX_LEN: usize = 8192;
/// 高频段侧信号增益斜率（1 + 2.3·Intensity^0.6）。
const SIDE_GAIN_HIGH_SLOPE: f32 = 2.3;
/// 中央信号补偿斜率（1 - 0.10·Intensity^0.6）。
const CENTER_COMP_SLOPE: f32 = 0.10;
/// Intensity 幂指数（把甜点移到 0.5 附近）。
const INTENSITY_EXP: f32 = 0.6;
/// tanh 软限幅 headroom 范围（Intensity=1 时最小，Intensity→0 时最大）。
const HEADROOM_MIN_DB: f32 = 0.2;
const HEADROOM_MAX_DB: f32 = 1.0;

/// FIR 分频长度随采样率缩放（≈21.3ms 时间长度，与 PEQ 同公式）：
/// 44.1/48k→1024，96k→2048，192k→4096，384k→8192。
fn wide_fir_len(sr: u32) -> usize {
    let n = ((sr as f32 * 0.0213).round() as usize).max(1);
    n.next_power_of_two().clamp(FIR_MIN_LEN, FIR_MAX_LEN)
}

/// 线性相位 FIR 分频：低通 FIR + 互补高通（高频 = 延迟对齐原信号 − 低通）。
/// 抽头 ≤2048 走直接环形缓冲；更长走分块 FFT（延迟 = block-1，同样补对齐）。
#[derive(Debug)]
struct FirSplit {
    engine: LpEngine,
    /// 每声道延迟对齐线（Direct：整条 FIR 延迟；Partitioned：分块延迟）。
    delay_lines: Vec<Vec<f32>>,
    write_positions: Vec<usize>,
}

#[derive(Debug)]
enum LpEngine {
    /// 直接卷积：逆序 IR + 环形缓冲。
    Direct {
    /// 逆序低通 IR（与 convolution::dot 配合）。
    ir_rev: Vec<f32>,
    /// FIR 长度。
    ir_len: usize,
    /// 环形缓冲长度（next_power_of_two(ir_len）)。
    delay_len: usize,
    mask: usize,
    /// 线性相位中心（群延迟采样数）。
    center: usize,
    },
    /// 分块 FFT 卷积（延迟 = block_len - 1）。
    Partitioned {
        pf: PartitionedFir,
        /// 分块延迟对齐环（每声道；长度 = block_len）。
        dlen: usize,
        dmask: usize,
        latency: usize,
    },
}

impl FirSplit {
    fn new(ir: Vec<f32>, channels: usize) -> Self {
        #[cfg(target_arch = "x86_64")]
        crate::pipeline::dsp::fir::init_fir_simd();
        let ir_len = ir.len().max(1);
        let engine = if ir_len <= DIRECT_FIR_MAX_LEN {
            let delay_len = ir_len.next_power_of_two();
            LpEngine::Direct {
                ir_rev: ir.iter().rev().copied().collect(),
                ir_len,
                delay_len,
                mask: delay_len - 1,
                center: (ir_len - 1) / 2,
            }
        } else {
            let pf = PartitionedFir::new(&ir, channels);
            let dlen = pf.block_len();
            LpEngine::Partitioned {
                pf,
                dlen,
                dmask: dlen - 1,
                latency: dlen - 1,
            }
        };
        let delay_len = match &engine {
            LpEngine::Direct { delay_len, .. } => *delay_len,
            LpEngine::Partitioned { dlen, .. } => *dlen,
        };
        Self {
            engine,
            delay_lines: vec![vec![0.0; delay_len]; channels],
            write_positions: vec![0; channels],
        }
    }

    /// 单声道分频：返回(低频支路, 高频支路)，两路之和 = 延迟 center 帧的原信号。
    fn split_channel(&mut self, k: usize, x: f32) -> (f32, f32) {
        match &mut self.engine {
            LpEngine::Direct {
                ir_rev,
                ir_len,
                delay_len,
                mask,
                center,
            } => {
                let ir_len = *ir_len;
                let delay_len = *delay_len;
                let mask = *mask;
                let center = *center;
                let delay = &mut self.delay_lines[k];
                let pos = &mut self.write_positions[k];
                delay[*pos] = x;
                *pos = (*pos + 1) & mask;

                let start = (*pos).wrapping_sub(1) & mask;
                let oldest = (*pos + delay_len - ir_len) & mask;
                let lp = if oldest <= start {
                    crate::pipeline::dsp::fir::dot(ir_rev, &delay[oldest..=start])
                } else {
                    let len_old = delay_len - oldest;
                    crate::pipeline::dsp::fir::dot(&ir_rev[..len_old], &delay[oldest..])
                        + crate::pipeline::dsp::fir::dot(
                            &ir_rev[len_old..],
                            &delay[0..=start],
                        )
                };
                let delayed_x = delay[(*pos + delay_len - 1 - center) & mask];
                (lp, delayed_x - lp)
            }
            LpEngine::Partitioned {
                pf,
                dmask,
                ..
            } => {
                let lp = pf.process_channel(k, x);
                let dmask = *dmask;
                // 互补高通需与原信号延迟对齐：分块延迟 = latency = block-1。
                let delay = &mut self.delay_lines[k];
                let pos = &mut self.write_positions[k];
                delay[*pos] = x;
                *pos = (*pos + 1) & dmask;
                // 写后 pos 指向最旧槽：该槽即 x[n-latency]（dlen=block_len，
                // latency=dlen-1，写入间隔 dlen 覆盖 latency+1 步，取 pos）。
                let delayed_x = delay[*pos];
                (lp, delayed_x - lp)
            }
        }
    }

    fn reset(&mut self) {
        for line in self.delay_lines.iter_mut() {
            line.fill(0.0);
        }
        self.write_positions.fill(0);
        match &mut self.engine {
            LpEngine::Direct { .. } => {}
            LpEngine::Partitioned { pf, .. } => pf.reset(),
        }
    }
}

/// Kaiser 窗 β（≈-60dB 旁瓣，过渡带比 Hamming 更窄、停带更深）。
const KAISER_BETA: f32 = 6.2;

/// 零阶修正贝塞尔 I0（级数近似，x ≤ 32 收敛良好）。
fn kaiser_i0(x: f32) -> f32 {
    let mut sum = 1.0f32;
    let mut term = 1.0f32;
    let x2 = x * x;
    for k in 1..=16 {
        term *= x2 / (4.0 * k as f32 * k as f32);
        sum += term;
    }
    sum
}

/// 设计线性相位低通 FIR（理想低通 × Kaiser 窗，DC 增益归一）。
/// Kaiser（β=6.2）比原 Hamming 停带更深、过渡带更窄——高采样率下
/// 同样抽头数的 200Hz 分频质量显著更好。
fn design_lowpass_ir(fc_hz: f32, sample_rate: u32, n: usize) -> Vec<f32> {
    let sr = sample_rate.max(1) as f32;
    let fc = fc_hz.min(sr * 0.45).max(1.0);
    let center = (n - 1) as f32 * 0.5;
    let mut ir = vec![0.0f32; n];
    let mut sum = 0.0f32;
    let i0_beta = kaiser_i0(KAISER_BETA);
    for i in 0..n {
        let m = i as f32 - center;
        let sinc = if m.abs() < 1.0e-6 {
            2.0 * fc / sr
        } else {
            (core::f32::consts::TAU * fc * m / sr).sin() / (core::f32::consts::PI * m)
        };
        let arg = (1.0 - ((i as f32 - center) / center).powi(2)).max(0.0).sqrt() * KAISER_BETA;
        let w = kaiser_i0(arg) / i0_beta;
        ir[i] = sinc * w;
        sum += ir[i];
    }
    for v in ir.iter_mut() {
        *v /= sum;
    }
    ir
}

#[derive(Debug)]
pub struct WideFilter {
    params: WideParams,
    channel_indices: Vec<usize>,
    active: bool,
    gain_side_high: f32,
    gain_comp: f32,
    headroom_factor: f32,
    fir: FirSplit,
}

impl WideFilter {
    pub fn new(params: WideParams) -> Self {
        Self {
            params,
            channel_indices: Vec::new(),
            active: false,
            gain_side_high: 1.0,
            gain_comp: 1.0,
            headroom_factor: 1.0,
            fir: FirSplit::new(vec![1.0], 0),
        }
    }
}

impl Filter for WideFilter {
    fn initialize(&mut self, sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        if self.channel_indices.is_empty() {
            self.channel_indices = (0..channel_names.len()).collect();
        }
        let stereo = self.channel_indices.len() >= 2;
        self.active = stereo && self.params.intensity > 0.0;
        if !self.active {
            return None;
        }

        let i = self.params.intensity;
        let eff = i.powf(INTENSITY_EXP);
        self.gain_side_high = 1.0 + SIDE_GAIN_HIGH_SLOPE * eff;
        self.gain_comp = 1.0 - CENTER_COMP_SLOPE * eff;
        let headroom_db = HEADROOM_MIN_DB + (HEADROOM_MAX_DB - HEADROOM_MIN_DB) * (1.0 - i);
        self.headroom_factor = 10.0f32.powf(-headroom_db / 20.0);

        let ir = design_lowpass_ir(CROSSOVER_HZ, sample_rate, wide_fir_len(sample_rate));
        self.fir = FirSplit::new(ir, 2);
        None
    }

    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        if !self.active || self.channel_indices.len() < 2 {
            return;
        }
        let l = self.channel_indices[0];
        let r = self.channel_indices[1];
        if l >= samples.len() || r >= samples.len() {
            return;
        }
        let frame_count = frame_count.min(samples[l].len()).min(samples[r].len());

        let g_high = self.gain_side_high;
        let g_comp = self.gain_comp;
        let headroom = self.headroom_factor;

        for f in 0..frame_count {
            let xl = samples[l][f];
            let xr = samples[r][f];

            // FIR 分频：低频直通不处理，高频做 M/S 宽度处理。
            let (ll, hl) = self.fir.split_channel(0, xl);
            let (rl, hr) = self.fir.split_channel(1, xr);

            let mid_h = (hl + hr) * 0.5;
            let side_h = (hl - hr) * 0.5;
            let out_mid_h = mid_h * g_comp;
            let out_side_h = side_h * g_high;

            let proc_l = (out_mid_h + out_side_h) * headroom;
            let proc_r = (out_mid_h - out_side_h) * headroom;
            samples[l][f] = ll + proc_l.tanh();
            samples[r][f] = rl + proc_r.tanh();
        }
    }

    fn latency(&self) -> u32 {
        match &self.fir.engine {
            LpEngine::Direct { center, .. } => *center as u32,
            LpEngine::Partitioned { latency, .. } => *latency as u32,
        }
    }

    fn set_channel_indices(&mut self, indices: &[usize]) {
        self.channel_indices = indices.to_vec();
    }

    fn reset(&mut self) {
        self.fir.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 临时诊断：Kaiser 分频在各采样率的停带表现（500Hz 衰减应 ≥-55dB）。
    #[test]
    fn kaiser_crossover_response() {
        fn lp_gain_db(ir: &[f32], freq: f32, sr: u32) -> f32 {
            let w = std::f32::consts::TAU * freq / sr as f32;
            let mut re = 0.0f32;
            let mut im = 0.0f32;
            for (i, &h) in ir.iter().enumerate() {
                re += h * (w * i as f32).cos();
                im -= h * (w * i as f32).sin();
            }
            20.0 * (re * re + im * im).sqrt().max(1e-6).log10()
        }
        for sr in [44_100u32, 48_000, 96_000, 192_000, 384_000] {
            let n = wide_fir_len(sr);
            let ir = design_lowpass_ir(CROSSOVER_HZ, sr, n);
            eprintln!(
                "DIAG kaiser sr={} n={} lp200={:.1}dB lp500={:.1}dB",
                sr,
                n,
                lp_gain_db(&ir, 200.0, sr),
                lp_gain_db(&ir, 500.0, sr),
            );
        }
    }

    #[test]
    fn intensity_zero_is_passthrough() {
        let mut f = WideFilter::new(WideParams { intensity: 0.0 });
        f.initialize(48000, &["L".into(), "R".into()]);
        let mut samples = vec![vec![0.4f32; 256], vec![0.2f32; 256]];
        let before = samples.clone();
        f.process(&mut samples, 256);
        for (a, b) in samples.iter().zip(before.iter()) {
            for (x, y) in a.iter().zip(b.iter()) {
                assert_eq!(*x, *y, "intensity 0 must be bit-exact passthrough");
            }
        }
    }

    #[test]
    fn mono_is_passthrough() {
        let mut f = WideFilter::new(WideParams { intensity: 1.0 });
        f.initialize(48000, &["Mono".into()]);
        let mut samples = vec![vec![0.8f32; 64]];
        f.process(&mut samples, 64);
        for &v in &samples[0] {
            assert_eq!(v, 0.8);
        }
    }

    #[test]
    fn silence_stays_silent() {
        let mut f = WideFilter::new(WideParams { intensity: 1.0 });
        f.initialize(48000, &["L".into(), "R".into()]);
        let mut samples = vec![vec![0.0f32; 4800], vec![0.0f32; 4800]];
        f.process(&mut samples, 4800);
        for ch in &samples {
            for &v in ch {
                assert_eq!(v, 0.0);
            }
        }
    }

    /// 稳态段上 |L-R| 的 RMS（宽度度量）。
    fn width_rms(samples: &[Vec<f32>], start: usize) -> f32 {
        let n = samples[0].len() - start;
        let mut sum = 0.0f32;
        for i in start..samples[0].len() {
            let d = samples[0][i] - samples[1][i];
            sum += d * d;
        }
        (sum / n as f32).sqrt()
    }

    #[test]
    fn side_signal_is_widened() {
        // 1 kHz 带侧成分输入（低幅度，tanh 近似线性）：宽度应随 Intensity 单调递增。
        let run = |intensity: f32| -> f32 {
            let mut f = WideFilter::new(WideParams { intensity });
            f.initialize(48000, &["L".into(), "R".into()]);
            let n = 4800usize;
            let mut samples = vec![vec![0.0f32; n], vec![0.0f32; n]];
            for i in 0..n {
                let v = (core::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin();
                samples[0][i] = 0.2 * v;
                samples[1][i] = 0.04 * v;
            }
            f.process(&mut samples, n);
            width_rms(&samples, 2000)
        };

        let base = run(0.0);
        let default = run(0.354331);
        let half = run(0.5);
        let full = run(1.0);
        assert!(
            default > base && half > default && full > half,
            "width should grow monotonically: {base} < {default} < {half} < {full}"
        );
        assert!(
            half / base > 1.8,
            "midpoint should be near old full width: {half} vs {base}"
        );
        assert!(
            full / half > 1.15,
            "full should clearly exceed midpoint: {full} vs {half}"
        );
    }

    #[test]
    fn intensity_mapping_curve() {
        // 幂指数映射：i' = i^0.6，gHigh = 1+2.3·i'，gComp = 1-0.15·i'。
        let setup = |intensity: f32| -> (f32, f32) {
            let mut f = WideFilter::new(WideParams { intensity });
            f.initialize(48000, &["L".into(), "R".into()]);
            (f.gain_side_high, f.gain_comp)
        };
        let eff = |i: f32| i.powf(0.6);

        let (g_def, c_def) = setup(0.354331);
        let (g_mid, c_mid) = setup(0.5);
        let (g_max, c_max) = setup(1.0);

        assert!((g_def - (1.0 + 2.3 * eff(0.354331))).abs() < 1e-4);
        assert!((c_def - (1.0 - 0.10 * eff(0.354331))).abs() < 1e-4);
        assert!((g_mid - (1.0 + 2.3 * eff(0.5))).abs() < 1e-4);
        assert!((c_mid - (1.0 - 0.10 * eff(0.5))).abs() < 1e-4);
        assert!((g_max - 3.3).abs() < 1e-4);
        assert!((c_max - 0.90).abs() < 1e-4);
        // 甜点（0.5）≈ 满档 2.5×，满档明显超过甜点。
        assert!((g_mid - 2.5).abs() < 0.05, "sweet spot should be ~2.5, got {g_mid}");
        assert!(g_max > g_mid && g_mid > g_def);
    }

    #[test]
    fn center_signal_is_compensated_and_symmetric() {
        // 纯中央信号：无侧成分 → 输出仍对称，幅度按 1-0.15·i'（i=1 → 0.85）补偿；
        // 低幅度下 tanh 近似线性，容差内验证。
        let mut f = WideFilter::new(WideParams { intensity: 1.0 });
        f.initialize(48000, &["L".into(), "R".into()]);
        let n = 4800usize;
        let mut samples = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = 0.1 * (core::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin();
            samples[0][i] = v;
            samples[1][i] = v;
        }
        f.process(&mut samples, n);
        let peak = samples[0][2000..]
            .iter()
            .fold(0.0f32, |m, &v| m.max(v.abs()));
        for i in 2000..n {
            assert!(
                (samples[0][i] - samples[1][i]).abs() < 1e-6,
                "center signal must stay symmetric"
            );
        }
        // 输出相对输入延迟 center 帧，逐样本对照无意义；验证峰值 ≈ 0.1·0.90
        // 且 tanh 只造成轻微压缩（0.08~0.092）。
        assert!(
            peak > 0.080 && peak < 0.092,
            "center compensation wrong, peak {peak}"
        );
    }

    #[test]
    fn bass_is_untouched_highs_widened() {
        // 200 Hz 以下低频支路完全不处理：60 Hz 带侧成分应原样通过；
        // 1 kHz 侧成分被放大。
        let run = |freq: f32, n: usize| -> (f32, f32) {
            let mut f = WideFilter::new(WideParams { intensity: 1.0 });
            f.initialize(48000, &["L".into(), "R".into()]);
            let mut samples = vec![vec![0.0f32; n], vec![0.0f32; n]];
            for i in 0..n {
                let v = (core::f32::consts::TAU * freq * i as f32 / 48000.0).sin();
                samples[0][i] = 0.5 * v;
                samples[1][i] = 0.1 * v;
            }
            f.process(&mut samples, n);
            let mut diff = 0.0f32;
            let mut peak_l = 0.0f32;
            for i in (n / 2)..n {
                diff = diff.max((samples[0][i] - samples[1][i]).abs());
                peak_l = peak_l.max(samples[0][i].abs());
            }
            (diff, peak_l)
        };

        let (diff_low, peak_low) = run(60.0, 9600);
        let (diff_high, _) = run(1000.0, 4800);
        assert!(
            (peak_low - 0.5).abs() < 0.02,
            "bass level must stay untouched: peak {peak_low}"
        );
        assert!(
            (diff_low - 0.4).abs() < 0.03,
            "bass keeps original stereo info: diff {diff_low}"
        );
        assert!(
            diff_high > 0.5,
            "highs should be widened: high {diff_high}"
        );
    }

    #[test]
    fn fir_split_reconstructs_delayed_input() {
        // 线性相位 FIR 完美重建：低频支路 + 高频支路 = 延迟 center 帧的原信号
        // （逐样本，含相位——这是 FIR 相对 IIR 分频的核心优势）。
        let mut f = WideFilter::new(WideParams { intensity: 1.0 });
        f.initialize(48000, &["L".into(), "R".into()]);
        let center = f.latency() as usize;
        let n = 4800usize;
        let input: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / 48000.0;
                0.3 * (core::f32::consts::TAU * 60.0 * t).sin()
                    + 0.4 * (core::f32::consts::TAU * 1000.0 * t).sin()
                    + 0.2 * (core::f32::consts::TAU * 6000.0 * t).sin()
            })
            .collect();
        let mut max_err = 0.0f32;
        for i in 0..n {
            let (lp, hp) = f.fir.split_channel(0, input[i]);
            if i >= center {
                max_err = max_err.max((lp + hp - input[i - center]).abs());
            }
        }
        assert!(
            max_err < 1.0e-4,
            "FIR split must reconstruct delayed input, max err {max_err}"
        );
    }

    #[test]
    fn fir_split_reconstructs_partitioned_high_rate() {
        // 192k：4096 抽头走分块 FFT——低通支路 + 互补高通必须仍等于
        // 延迟 latency 帧的原信号（延迟对齐环正确性）。
        let mut f = WideFilter::new(WideParams { intensity: 1.0 });
        f.initialize(192_000, &["L".into(), "R".into()]);
        let latency = f.latency() as usize;
        assert_eq!(latency, 127, "192k 应走分块 FFT（延迟 127）");
        let n = 4000usize;
        let input: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / 192_000.0;
                0.3 * (core::f32::consts::TAU * 60.0 * t).sin()
                    + 0.4 * (core::f32::consts::TAU * 1000.0 * t).sin()
                    + 0.2 * (core::f32::consts::TAU * 6000.0 * t).sin()
            })
            .collect();
        let mut max_err = 0.0f32;
        for i in 0..n {
            let (lp, hp) = f.fir.split_channel(0, input[i]);
            if i >= latency {
                max_err = max_err.max((lp + hp - input[i - latency]).abs());
            }
        }
        assert!(
            max_err < 1.0e-4,
            "partitioned split must reconstruct delayed input, max err {max_err}"
        );
    }

    #[test]
    fn fir_delays_by_center_samples() {
        // 单脉冲经低通 FIR 的主峰应出现在 center 帧（对称 FIR 群延迟）。
        let mut f = WideFilter::new(WideParams { intensity: 1.0 });
        f.initialize(48000, &["L".into(), "R".into()]);
        let center = f.latency() as usize;
        let n = center + 128;
        let mut best = 0usize;
        let mut best_v = 0.0f32;
        for i in 0..n {
            let x = if i == 0 { 1.0 } else { 0.0 };
            let (lp, _) = f.fir.split_channel(0, x);
            if lp.abs() > best_v {
                best_v = lp.abs();
                best = i;
            }
        }
        assert_eq!(best, center, "LP peak should be at FIR center");
    }

    #[test]
    fn latency_is_fir_center() {
        let mut f = WideFilter::new(WideParams { intensity: 1.0 });
        f.initialize(48000, &["L".into(), "R".into()]);
        assert_eq!(f.latency(), ((wide_fir_len(48000) - 1) / 2) as u32);
    }

    #[test]
    fn low_level_is_transparent() {
        // 低电平下 tanh 近似线性：2 次/3 次谐波应可忽略。
        let mut f = WideFilter::new(WideParams { intensity: 1.0 });
        f.initialize(48000, &["L".into(), "R".into()]);
        let n = 9600usize;
        let mut samples = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = 0.05 * (core::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin();
            samples[0][i] = v;
            samples[1][i] = 0.01 * v;
        }
        f.process(&mut samples, n);

        // 计算左声道 2k/3k 谱幅度（窗口 4800..9600 = 100 整周期）。
        let amp = |freq: f32| -> f32 {
            let mut re = 0.0f32;
            let mut im = 0.0f32;
            for i in 4800..n {
                let t = i as f32 / 48000.0;
                let w = core::f32::consts::TAU * freq * t;
                re += samples[0][i] * w.cos();
                im += samples[0][i] * w.sin();
            }
            2.0 * (re * re + im * im).sqrt() / 4800.0
        };
        assert!(amp(2000.0) < 1.0e-3, "2nd harmonic should be absent");
        assert!(amp(3000.0) < 1.0e-3, "3rd harmonic should be absent");
    }

    #[test]
    fn extreme_antiphase_is_bounded() {
        // 1 kHz 纯反相、满强度：输出应被 tanh 限制在 ±1 内且明显放大。
        let mut f = WideFilter::new(WideParams { intensity: 1.0 });
        f.initialize(48000, &["L".into(), "R".into()]);
        let n = 4800usize;
        let mut samples = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = 0.9 * (core::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin();
            samples[0][i] = v;
            samples[1][i] = -v;
        }
        f.process(&mut samples, n);
        let mut peak = 0.0f32;
        for ch in &samples {
            for &v in ch {
                assert!(v.is_finite());
                // 低频支路原样相加允许 ±1.1 级轻微超限（高频支路已被 tanh 限制）。
                assert!(v.abs() <= 1.1 + 1e-6, "soft limiter must bound output: {v}");
                peak = peak.max(v.abs());
            }
        }
        assert!(peak > 0.9, "full-width antiphase should be strongly widened, peak {peak}");
    }

    #[test]
    fn deterministic_and_reproducible() {
        let run = || -> Vec<f32> {
            let mut f = WideFilter::new(WideParams { intensity: 0.7 });
            f.initialize(48000, &["L".into(), "R".into()]);
            let mut samples = vec![vec![0.0f32; 2048], vec![0.0f32; 2048]];
            for i in 0..2048 {
                let v = 0.4 * (core::f32::consts::TAU * 440.0 * i as f32 / 48000.0).sin();
                samples[0][i] = v;
                samples[1][i] = v * 0.5;
            }
            f.process(&mut samples, 2048);
            samples.into_iter().flatten().collect()
        };
        let a = run();
        let b = run();
        for (x, y) in a.iter().zip(b.iter()) {
            assert!((x - y).abs() < 1e-9, "same params must be reproducible");
        }
    }

    #[test]
    fn finite_across_intensities_and_sample_rates() {
        for sr in [44_100u32, 48_000, 96_000] {
            for intensity in [0.0f32, 0.354331, 0.7, 1.0] {
                let mut f = WideFilter::new(WideParams { intensity });
                f.initialize(sr, &["L".into(), "R".into()]);
                let mut samples = vec![vec![0.0f32; 480], vec![0.0f32; 480]];
                for i in 0..480 {
                    samples[0][i] =
                        (core::f32::consts::TAU * 440.0 * i as f32 / sr as f32).sin() * 0.9;
                    samples[1][i] = -samples[0][i] * 0.6;
                }
                f.process(&mut samples, 480);
                for ch in &samples {
                    for &v in ch {
                        assert!(v.is_finite());
                    }
                }
            }
        }
    }

}
