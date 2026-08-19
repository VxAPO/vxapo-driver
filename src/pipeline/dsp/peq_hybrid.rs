//! pipeline/dsp/peq_hybrid.rs — 混合式 PEQ
//!
//! peaking 段：200 Hz 分频，`Fc < CROSSOVER_HZ` 走 IIR biquad 级联；
//! `Fc >= CROSSOVER_HZ` 由采样率自适应最小相位 FIR 承担。
//! shelf/pass 段：RBJ biquad 解析精确，永远走 IIR（零延迟、零逼近误差）。
//! 级联结构：`输入 → IIR（按 fc 升序）→ 最小相位 FIR → 输出`。
//!
//! 频响合成：目标曲线 `T(f) = Σ 所有段频响（dB）`，IIR 路径频响
//! `L(f) = Σ IIR 段频响（dB）`，FIR 目标 `F(f) = T(f) − L(f)`——级联总响应
//! `L + F = T`，幅度精确拟合目标曲线（跨分频点段自动处理）。

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

use crate::pipeline::dsp::biquad::{compute_coeffs, BiquadType};
use crate::pipeline::dsp::filter::Filter;
use crate::pipeline::dsp::fir::PartitionedFir;
use crate::pipeline::dsp::math::warn_rate_limited;
use crate::pipeline::dsp::model::{CROSSOVER_HZ, PeqBand, PeqBandType, PeqParams};

/// FIR 长度下限（@44.1k/48k）。
const FIR_MIN_LEN: usize = 1024;
/// FIR 长度上限（@384k）。
const FIR_MAX_LEN: usize = 8192;
/// 直接 FIR 最大长度（超过走分块 FFT）。
const DIRECT_FIR_MAX_LEN: usize = 2048;
/// 静音恢复淡入时长（秒）：audiodg 端点重协商会把输入先置静音再恢复，
/// IIR 对阶跃的瞬态产生“哔”声，恢复时线性淡入抑制。
const MUTE_RECOVERY_FADE_S: f32 = 0.008;
/// 输入“有声”判定阈值（峰值，约 -80 dBFS）。
const INPUT_ACTIVE_THRESHOLD: f32 = 1.0e-4;
/// 静音确认保持帧数（约 1.3 ms @48k）：避免低频正弦过零附近的单帧
/// 低样本被误判为静音（否则每周期输出被吃掉 → 频响衰减）。
const SILENCE_HOLD_FRAMES: usize = 64;
/// 频响幅值下限，避免 log(0)。
const MIN_MAG: f32 = 1e-5;

/// 二阶 biquad（RBJ，DF2T）。
#[derive(Debug, Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

#[derive(Debug, Clone, Copy, Default)]
struct BiquadState {
    z1: f32,
    z2: f32,
}

impl Biquad {
    /// 按 PEQ 段类型计算 RBJ biquad 系数。
    ///
    /// peaking 段保留既有本地 f64 计算路径（行为不变）；shelf/pass 段统一走
    /// `biquad::compute_coeffs`（f64 中间计算 + 有限性/稳定性护栏 + 深切地板回退），
    /// 与生产 biquad 模块同一数值路径。
    fn from_band(band: &PeqBand, sr: u32) -> Option<Self> {
        match band.kind {
            PeqBandType::Peaking => Self::peaking(band.fc, band.gain_db, band.q, sr),
            _ => {
                let filter_type = match band.kind {
                    PeqBandType::LowShelf => BiquadType::LowShelf,
                    PeqBandType::HighShelf => BiquadType::HighShelf,
                    PeqBandType::LowPass => BiquadType::LowPass,
                    PeqBandType::HighPass => BiquadType::HighPass,
                    PeqBandType::Peaking => unreachable!(),
                };
                let c = compute_coeffs(filter_type, band.fc, band.gain_db, band.q, sr);
                if !c.is_valid() || c == crate::pipeline::dsp::biquad::BiquadCoeffs::BYPASS {
                    return None;
                }
                Some(Self {
                    b0: c.b0,
                    b1: c.b1,
                    b2: c.b2,
                    a1: c.a1,
                    a2: c.a2,
                })
            }
        }
    }

    /// RBJ peaking（f64 计算，归一化 + 二阶朱利稳定性检查；不稳定返回 None）。
    fn peaking(fc: f32, gain_db: f32, q: f32, sr: u32) -> Option<Self> {
        if sr == 0 || !fc.is_finite() || !q.is_finite() || q <= 0.0 {
            return None;
        }
        let sr = sr as f64;
        let w0 = 2.0 * std::f64::consts::PI * fc as f64 / sr;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * q as f64);
        let sqrt_a = 10.0_f64.powf(gain_db as f64 / 80.0);
        let a = sqrt_a * sqrt_a;

        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * cos_w0;
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha / a;

        let c = Self {
            b0: (b0 / a0) as f32,
            b1: (b1 / a0) as f32,
            b2: (b2 / a0) as f32,
            a1: (a1 / a0) as f32,
            a2: (a2 / a0) as f32,
        };
        // 二阶朱利判据：|a2| < 1 且 |a1| < 1 + a2。
        if c.a2.abs() >= 1.0 || c.a1.abs() >= 1.0 + c.a2 {
            return None;
        }
        Some(c)
    }

    fn process(&self, st: &mut BiquadState, x: f32) -> f32 {
        let y = self.b0 * x + st.z1;
        st.z1 = self.b1 * x - self.a1 * y + st.z2;
        st.z2 = self.b2 * x - self.a2 * y;
        y
    }

    /// 数字频响（dB）。
    fn response_db(&self, freq: f32, sr: u32) -> f32 {
        let w = std::f32::consts::TAU * freq / sr.max(1) as f32;
        let (cw, sw) = (w.cos(), -w.sin());
        // z^-1 = cw + j*sw，z^-2 = cos2w + j*sin2w。
        let (c2w, s2w) = ((2.0 * w).cos(), -(2.0 * w).sin());
        let num_re = self.b0 + self.b1 * cw + self.b2 * c2w;
        let num_im = self.b1 * sw + self.b2 * s2w;
        let den_re = 1.0 + self.a1 * cw + self.a2 * c2w;
        let den_im = self.a1 * sw + self.a2 * s2w;
        let mag = ((num_re * num_re + num_im * num_im) / (den_re * den_re + den_im * den_im))
            .sqrt()
            .max(MIN_MAG);
        20.0 * mag.log10()
    }
}

/// 每通道状态：IIR 级联状态。
#[derive(Debug)]
struct PeqChannel {
    iir: Vec<BiquadState>,
}

/// FIR 执行引擎（直接 / 分块）。
#[derive(Debug)]
enum PeqFir {
    Direct {
        ir_rev: Vec<f32>,
        fir_len: usize,
        delay_len: usize,
        mask: usize,
        delay: Vec<Vec<f32>>,
        pos: Vec<usize>,
    },
    Partitioned(PartitionedFir),
}

/// 混合式 PEQ。
#[derive(Debug)]
pub struct HybridPeqFilter {
    params: PeqParams,
    /// 低频段 biquad（fc < CROSSOVER_HZ，按 fc 升序）。
    iir: Vec<Biquad>,
    /// FIR 执行引擎。
    fir: PeqFir,
    channel_indices: Vec<usize>,
    channels: Vec<PeqChannel>,
    /// 静音检测与恢复淡入状态（修订）。
    input_active: bool,
    fade_total: usize,
    fade_remaining: usize,
    silence_count: usize,
}

impl HybridPeqFilter {
    pub fn new(params: PeqParams) -> Self {
        Self {
            params,
            iir: Vec::new(),
            fir: PeqFir::Direct {
                ir_rev: Vec::new(),
                fir_len: 0,
                delay_len: 0,
                mask: 0,
                delay: Vec::new(),
                pos: Vec::new(),
            },
            channel_indices: Vec::new(),
            channels: Vec::new(),
            input_active: false,
            fade_total: 0,
            fade_remaining: 0,
            silence_count: 0,
        }
    }

    /// 计算 FIR 长度：`next_pow2(round(sr × 0.0213))`，夹在 [1024, 8192]。
    ///
    /// 低频（<200 Hz）由 IIR 承担，IIR 段在邻近频点的叠加是**设计预期**
    /// （调音时避让），FIR 不做低频精确补偿；高频段 1024+ 点分辨率足够。
    fn fir_len_for(sr: u32) -> usize {
        let n = ((sr as f32 * 0.0213).round() as usize).max(1);
        n.next_power_of_two().clamp(FIR_MIN_LEN, FIR_MAX_LEN)
    }
}

impl Filter for HybridPeqFilter {
    fn initialize(&mut self, sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        if self.channel_indices.is_empty() {
            self.channel_indices = (0..channel_names.len()).collect();
        }
        if self.params.bands.is_empty() {
            // 空段：直通（模型层已限制 6–31 段，防御性处理）。
            self.iir.clear();
            self.channels.clear();
            return None;
        }

        // 1) 段按 fc 升序；IIR = fc < CROSSOVER_HZ（200.0 整归 FIR）。
        let mut bands = self.params.bands.clone();
        bands.sort_by(|a, b| a.fc.total_cmp(&b.fc));
        self.iir = bands
            .iter()
            .filter(|b| in_iir_band(b, CROSSOVER_HZ, sample_rate))
            .filter_map(|b| {
                let c = Biquad::from_band(b, sample_rate);
                if c.is_none() {
                    warn_rate_limited(
                        "peq_hybrid_unstable",
                        "peq biquad 超出稳定范围，该段已直通",
                    );
                }
                c
            })
            .collect();

        // 2) FIR 目标 = 总目标 − IIR 频响（dB 相减）→ 最小相位 IR。
        let n = Self::fir_len_for(sample_rate);
        let ir = build_min_phase_ir(&bands, &self.iir, sample_rate, n);
        let count = self.channel_indices.len();
        self.fade_total =
            ((sample_rate.max(1) as f32) * MUTE_RECOVERY_FADE_S).round() as usize;
        self.fade_remaining = 0;
        self.input_active = false;
        self.silence_count = 0;
        if n <= DIRECT_FIR_MAX_LEN {
            let delay_len = n.next_power_of_two();
            self.fir = PeqFir::Direct {
                ir_rev: ir.iter().rev().copied().collect(),
                fir_len: n,
                delay_len,
                mask: delay_len - 1,
                delay: vec![vec![0.0; delay_len]; count],
                pos: vec![0; count],
            };
        } else {
            self.fir = PeqFir::Partitioned(PartitionedFir::new(&ir, count));
        }
        self.channels = (0..count)
            .map(|_| PeqChannel {
                iir: vec![BiquadState::default(); self.iir.len()],
            })
            .collect();
        None
    }

    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        if self.channels.is_empty() {
            return;
        }
        let n_ch = self.channel_indices.len().min(self.channels.len());
        let iir = &self.iir;
        let frames = (0..n_ch)
            .filter_map(|k| samples.get(self.channel_indices[k]).map(|s| s.len()))
            .min()
            .map(|l| frame_count.min(l))
            .unwrap_or(0);

        for f in 0..frames {
            // 静音检测（整帧输入峰值）与淡入系数。
            let mut peak = 0.0f32;
            for k in 0..n_ch {
                let slot = self.channel_indices[k];
                if slot < samples.len() {
                    peak = peak.max(samples[slot][f].abs());
                }
            }
            if peak > INPUT_ACTIVE_THRESHOLD {
                if !self.input_active {
                    self.fade_remaining = self.fade_total;
                }
                self.input_active = true;
                self.silence_count = 0;
            } else if self.input_active {
                self.silence_count += 1;
                if self.silence_count >= SILENCE_HOLD_FRAMES {
                    // 输入确认静音：输出强制 0（抑制 IIR 振铃的“哔”），
                    // 状态照常更新（衰减到 0），恢复时从干净状态淡入。
                    self.input_active = false;
                    self.fade_remaining = 0;
                }
            }
            let fade = if !self.input_active {
                0.0
            } else if self.fade_remaining > 0 {
                // 线性 0→1：恢复第 1 帧 ≈ 0，最后一帧 = 1。
                let g = (self.fade_total - self.fade_remaining + 1) as f32
                    / self.fade_total.max(1) as f32;
                self.fade_remaining -= 1;
                g
            } else {
                1.0
            };

            for k in 0..n_ch {
                let slot = self.channel_indices[k];
                if slot >= samples.len() {
                    continue;
                }
                let ch = &mut self.channels[k];
                let mut x = samples[slot][f];
                // IIR 级联（低频段）。
                for (i, bq) in iir.iter().enumerate() {
                    x = bq.process(&mut ch.iir[i], x);
                }
                let out = match &mut self.fir {
                    PeqFir::Direct { ir_rev, fir_len, delay_len, mask, delay, pos } => {
                        let fir_len = *fir_len;
                        let delay_len = *delay_len;
                        let mask = *mask;
                        let d = &mut delay[k];
                        let p = &mut pos[k];
                        d[*p] = x;
                        *p = (*p + 1) & mask;
                        // delay_len == fir_len（均为 2 的幂），oldest == start 不会
                        // 出现：pos==0 时整段连续（if 分支），其余环绕两段（else）。
                        let start = p.wrapping_sub(1) & mask;
                        let oldest = (*p + delay_len - fir_len) & mask;
                        if oldest <= start {
                            crate::pipeline::dsp::fir::dot(ir_rev, &d[oldest..=start])
                        } else {
                            let len_old = delay_len - oldest;
                            crate::pipeline::dsp::fir::dot(&ir_rev[..len_old], &d[oldest..])
                                + crate::pipeline::dsp::fir::dot(
                                    &ir_rev[len_old..],
                                    &d[0..=start],
                                )
                        }
                    }
                    PeqFir::Partitioned(pf) => pf.process_channel(k, x),
                };
                samples[slot][f] = if out.is_finite() { out * fade } else { 0.0 };
            }
        }
    }

    fn latency(&self) -> u32 {
        match &self.fir {
            // 最小相位 FIR 能量集中在前端，实际群延迟远小于线性相位(N-1)/2；
            // 保守取 fir_len/4（1024 → 256 ≈ 5.3 ms @48k），不上报引擎，
            // 仅链记账/诊断/缓冲预留使用。
            PeqFir::Direct { fir_len, .. } => (*fir_len / 4).max(1) as u32,
            PeqFir::Partitioned(pf) => pf.latency(),
        }
    }

    fn set_channel_indices(&mut self, indices: &[usize]) {
        self.channel_indices = indices.to_vec();
    }

    fn reset(&mut self) {
        self.input_active = false;
        self.fade_remaining = 0;
        self.silence_count = 0;
        for ch in self.channels.iter_mut() {
            ch.iir.fill(BiquadState::default());
        }
        match &mut self.fir {
            PeqFir::Direct { delay, pos, .. } => {
                for d in delay.iter_mut() {
                    d.fill(0.0);
                }
                pos.fill(0);
            }
            PeqFir::Partitioned(pf) => pf.reset(),
        }
    }
}

/// 段是否由 IIR 主实现。
///
/// - 非 peaking 段（low/high shelf、low/high pass）**永远走 IIR**：RBJ biquad
///   就是这些滤波器的解析精确解，FIR 只是逼近，且 IIR 延迟为 0、CPU 更低。
/// - peaking 段维持既有混合逻辑：
///   - `Fc < 分频点` → IIR（低频主责任）；
///   - `Fc == 分频点`（200.0）→ FIR（定稿：分频点归属高频路径）；
///   - `Fc > 分频点` 且影响范围**实质跨过**分频点（分频点处 |dB| > 0.25）→ 也进
///     IIR——宽 Q 段的低频泄漏由 biquad 解析精确实现，避免 FIR 低频分辨率不足
///     （1024 点 @48k = 46.9 Hz/bin）造成的衔接误差；其余段由 FIR 主实现，
///     其在低频的泄漏 < 0.25 dB，FIR 误差无感。
///
/// 注意：peaking 判据是**影响**而非 fc——如 fc=500/Q=1.5 的宽段在 200 Hz 处仍有
/// 明显响应时也会进 IIR（属设计意图：它的低频影响由 IIR 精确承担，FIR
/// 目标自动扣除其高频残余）。
fn in_iir_band(b: &PeqBand, crossover: f32, sr: u32) -> bool {
    if b.kind != PeqBandType::Peaking {
        return true;
    }
    if b.fc < crossover {
        return true;
    }
    if b.fc == crossover {
        return false;
    }
    peaking_response_db(b.fc, b.gain_db, b.q, crossover, sr as f32).abs() > 0.25
}

/// 由「全段频响 − IIR 频响」生成最小相位 FIR（cepstrum，逻辑对齐 GraphicEQ）。
fn build_min_phase_ir(
    bands: &[crate::pipeline::dsp::model::PeqBand],
    iir: &[Biquad],
    sample_rate: u32,
    n: usize,
) -> Vec<f32> {
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n);
    let ifft = planner.plan_fft_inverse(n);
    let scratch_len = fft
        .get_inplace_scratch_len()
        .max(ifft.get_inplace_scratch_len());
    let mut scratch = vec![Complex::new(0.0, 0.0); scratch_len];

    // 1) F(f) = T(f) − L(f)（dB），转幅值 log。
    let sr = sample_rate.max(1) as f32;
    let mut spectrum = vec![Complex::new(0.0, 0.0); n];
    for k in 0..=n / 2 {
        let freq = k as f32 * sr / n as f32;
        let mut t_db = 0.0f32;
        for b in bands {
            t_db += band_response_db(b, freq, sample_rate);
        }
        let mut l_db = 0.0f32;
        for bq in iir {
            l_db += bq.response_db(freq, sample_rate);
        }
        let mag = 10.0f32.powf((t_db - l_db) / 20.0).max(MIN_MAG);
        spectrum[k] = Complex::new(mag.ln(), 0.0);
    }
    for k in 1..n / 2 {
        spectrum[n - k] = spectrum[k];
    }

    // 2) IFFT → 倒谱，归一化（rustfft IFFT 未缩放）。
    ifft.process_with_scratch(&mut spectrum, &mut scratch);
    let inv_n = 1.0 / n as f32;
    for v in spectrum.iter_mut() {
        v.re *= inv_n;
        v.im = 0.0;
    }

    // 3) 最小相位倒谱：n>N/2 置 0，0<n<N/2 加倍，n=N/2 保留原值
    //   （奈奎斯特 bin 参与变换，避免 IR 高频幅度误差）。
    let mut cep_min = vec![Complex::new(0.0, 0.0); n];
    cep_min[0] = spectrum[0];
    for k in 1..n / 2 {
        cep_min[k] = Complex::new(spectrum[k].re * 2.0, 0.0);
    }
    cep_min[n / 2] = spectrum[n / 2];

    // 4) FFT → 最小相位复频谱，exp 恢复幅值并产生相位。
    fft.process_with_scratch(&mut cep_min, &mut scratch);
    for v in cep_min.iter_mut() {
        let e = v.re.exp();
        let re = e * v.im.cos();
        let im = e * v.im.sin();
        *v = Complex::new(re, im);
    }

    // 5) IFFT → 时域 IR，raised-cosine 平滑窗。
    ifft.process_with_scratch(&mut cep_min, &mut scratch);
    let mut ir = Vec::with_capacity(n);
    for (i, v) in cep_min.iter().enumerate() {
        let x = v.re * inv_n;
        let factor = 0.5 * (1.0 + (std::f32::consts::PI * i as f32 / n as f32).cos());
        ir.push(x * factor);
    }
    ir
}

/// 单段目标频响（dB），与 IIR 路径**同一计算路径**。
///
/// 对任一 band 先按 `Biquad::from_band` 计算系数；成功则用与 IIR 级联完全相同的
/// `response_db`，使 `T−L` 对 IIR 段精确归零。若系数不可用（配置已校验，正常不命中），
/// peaking 回退到独立 f32 RBJ 公式作为目标值，其余类型回退 0 dB（IIR 路径同样已直通，
/// 目标与实现一致）。
pub(crate) fn band_response_db(band: &PeqBand, freq: f32, sr: u32) -> f32 {
    if let Some(bq) = Biquad::from_band(band, sr) {
        return bq.response_db(freq, sr);
    }
    if band.kind == PeqBandType::Peaking {
        return peaking_response_db_fallback(band.fc, band.gain_db, band.q, freq, sr as f32);
    }
    0.0
}

/// peaking 频响（dB），兼容入口。
pub(crate) fn peaking_response_db(fc: f32, gain_db: f32, q: f32, freq: f32, sr: f32) -> f32 {
    band_response_db(
        &PeqBand {
            fc,
            gain_db,
            q,
            kind: PeqBandType::Peaking,
        },
        freq,
        sr as u32,
    )
}

/// peaking 独立 f32 RBJ 公式（仅用于系数不可用时的目标值回退）。
fn peaking_response_db_fallback(fc: f32, gain_db: f32, q: f32, freq: f32, sr: f32) -> f32 {
    if sr <= 0.0 || !fc.is_finite() || !q.is_finite() || q <= 0.0 {
        return 0.0;
    }
    let w = std::f32::consts::TAU * freq / sr;
    let (cw, sw) = (w.cos(), -w.sin());
    let (c2w, s2w) = ((2.0 * w).cos(), -(2.0 * w).sin());
    let w0 = std::f32::consts::TAU * fc / sr;
    let cos_w0 = w0.cos();
    let sin_w0 = w0.sin();
    let alpha = sin_w0 / (2.0 * q);
    let sqrt_a = 10.0f32.powf(gain_db / 80.0);
    let a = sqrt_a * sqrt_a;
    let b0 = 1.0 + alpha * a;
    let b1 = -2.0 * cos_w0;
    let b2 = 1.0 - alpha * a;
    let a0 = 1.0 + alpha / a;
    let a1 = -2.0 * cos_w0;
    let a2 = 1.0 - alpha / a;
    let num_re = b0 + b1 * cw + b2 * c2w;
    let num_im = b1 * sw + b2 * s2w;
    let den_re = a0 + a1 * cw + a2 * c2w;
    let den_im = a1 * sw + a2 * s2w;
    let mag = ((num_re * num_re + num_im * num_im) / (den_re * den_re + den_im * den_im))
        .sqrt()
        .max(MIN_MAG);
    20.0 * mag.log10()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::dsp::model::{PeqBand, PeqBandType};

    fn sr_amp(filter: &mut HybridPeqFilter, freq: f32, amp: f32, sr: u32, frames: usize) -> f32 {
        let mut samples = vec![vec![0.0f32; frames], vec![0.0f32; frames]];
        for i in 0..frames {
            let v = amp * (std::f32::consts::TAU * freq * i as f32 / sr as f32).sin();
            samples[0][i] = v;
            samples[1][i] = v * 0.5;
        }
        filter.process(&mut samples, frames);
        // 稳态段 RMS（跳过预热）。
        let start = frames / 2;
        // 低频（如 20 Hz @96k）周期很长：取整周期窗口，避免 RMS 测量误差。
        let period = (sr as f32 / freq).round() as usize;
        let span = ((frames - start) / period.max(1)) * period.max(1);
        let sum: f32 = samples[0][start..start + span].iter().map(|x| x * x).sum();
        (sum / span.max(1) as f32).sqrt()
    }

    fn target_db(bands: &[PeqBand], freq: f32, sr: u32) -> f32 {
        bands
            .iter()
            .map(|b| band_response_db(b, freq, sr))
            .sum()
    }

    #[test]
    fn peaking_response_math() {
        // 单段 1 kHz +6 dB：中心 = 6 dB，远离中心 ≈ 0。
        let at_center = peaking_response_db(1000.0, 6.0, 1.0, 1000.0, 48000.0);
        let far = peaking_response_db(1000.0, 6.0, 1.0, 100.0, 48000.0);
        assert!((at_center - 6.0).abs() < 0.05, "center {at_center}");
        assert!(far.abs() < 0.1, "far {far}");
    }

    #[test]
    fn shelf_and_pass_response_math() {
        // 低架 +6 dB：fc 以下接近 +6 dB，远高于 fc 接近 0。
        let ls = PeqBand { fc: 200.0, gain_db: 6.0, q: 0.707, kind: PeqBandType::LowShelf };
        assert!((band_response_db(&ls, 50.0, 48000) - 6.0).abs() < 0.3);
        assert!(band_response_db(&ls, 10000.0, 48000).abs() < 0.2);
        // 高通：fc 以下显著衰减，fc 以上接近 0 dB。
        let hp = PeqBand { fc: 1000.0, gain_db: 0.0, q: 0.707, kind: PeqBandType::HighPass };
        assert!(band_response_db(&hp, 50.0, 48000) < -20.0);
        assert!(band_response_db(&hp, 10000.0, 48000).abs() < 0.2);
        // 低通：fc 以上显著衰减，fc 以下接近 0 dB。
        let lp = PeqBand { fc: 1000.0, gain_db: 0.0, q: 0.707, kind: PeqBandType::LowPass };
        assert!(band_response_db(&lp, 10000.0, 48000) < -20.0);
        assert!(band_response_db(&lp, 50.0, 48000).abs() < 0.2);
        // 高架 -6 dB：fc 以上接近 -6 dB，远低于 fc 接近 0。
        let hs = PeqBand { fc: 6000.0, gain_db: -6.0, q: 0.707, kind: PeqBandType::HighShelf };
        assert!((band_response_db(&hs, 12000.0, 48000) + 6.0).abs() < 0.3);
        assert!(band_response_db(&hs, 50.0, 48000).abs() < 0.2);
    }

    #[test]
    fn hybrid_shelf_and_pass_match_target() {
        let bands = vec![
            PeqBand { fc: 120.0, gain_db: 6.0, q: 0.707, kind: PeqBandType::LowShelf },
            PeqBand { fc: 6000.0, gain_db: -4.0, q: 0.707, kind: PeqBandType::HighShelf },
            PeqBand { fc: 1200.0, gain_db: 0.0, q: 0.707, kind: PeqBandType::LowPass },
            PeqBand { fc: 80.0, gain_db: 0.0, q: 0.707, kind: PeqBandType::HighPass },
        ];
        for sr in [44_100u32, 48_000, 96_000] {
            let mut f = HybridPeqFilter::new(PeqParams {
                crossover_hz: CROSSOVER_HZ,
                bands: bands.clone(),
            });
            f.initialize(sr, &["L".into(), "R".into()]);
            assert_eq!(f.iir.len(), 4, "shelf/pass 段必须全部走 IIR");
            for freq in [30.0f32, 60.0, 100.0, 120.0, 1000.0, 1200.0, 6000.0, 12000.0] {
                let frames = 12000usize;
                let out_rms = sr_amp(&mut f, freq, 0.25, sr, frames);
                let in_rms = 0.25 / std::f32::consts::SQRT_2;
                let measured = 20.0 * (out_rms / in_rms).log10();
                let target = target_db(&bands, freq, sr);
                assert!(
                    (measured - target).abs() < 0.7,
                    "sr {sr} freq {freq}: measured {measured:.2} dB vs target {target:.2} dB"
                );
            }
        }
    }

    #[test]
    fn hybrid_matches_target_across_crossover() {
        // 多段（含跨 200 Hz）：级联输出频响 ≈ 目标 ±0.5 dB。
        let bands = vec![
            PeqBand { fc: 100.0, gain_db: -3.0, q: 1.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 200.0, gain_db: 4.0, q: 1.2, kind: PeqBandType::Peaking },
            PeqBand { fc: 1000.0, gain_db: 6.0, q: 1.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 4000.0, gain_db: -2.0, q: 2.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 8000.0, gain_db: 3.0, q: 1.5, kind: PeqBandType::Peaking },
            PeqBand { fc: 16000.0, gain_db: -1.0, q: 1.0, kind: PeqBandType::Peaking },
        ];
        for sr in [44_100u32, 48_000, 96_000] {
            let mut f = HybridPeqFilter::new(PeqParams {
                crossover_hz: CROSSOVER_HZ,
                bands: bands.clone(),
            });
            f.initialize(sr, &["L".into(), "R".into()]);
            for freq in [
                20.0f32, 31.5, 50.0, 60.0, 100.0, 125.0, 200.0, 500.0, 1000.0, 4000.0, 8000.0,
                16000.0,
            ] {
                let frames = 12000usize;
                let out_rms = sr_amp(&mut f, freq, 0.25, sr, frames);
                let in_rms = 0.25 / std::f32::consts::SQRT_2;
                let measured = 20.0 * (out_rms / in_rms).log10();
                let target = target_db(&bands, freq, sr);
                assert!(
                    (measured - target).abs() < 0.5,
                    "sr {sr} freq {freq}: measured {measured:.2} dB vs target {target:.2} dB"
                );
            }
        }
    }

    #[test]
    fn low_band_uses_iir_high_band_fir() {
        // fc=100 的段：低频 IIR 承担；高频路径（fir_ir）应接近 0 dB 补偿。
        let bands = vec![PeqBand { fc: 100.0, gain_db: -6.0, q: 1.0, kind: PeqBandType::Peaking }];
        let mut f = HybridPeqFilter::new(PeqParams {
            crossover_hz: CROSSOVER_HZ,
            bands: bands.clone(),
        });
        f.initialize(48000, &["L".into()]);
        assert_eq!(f.iir.len(), 1);
        // 100 Hz 处目标 -6 dB；5 kHz 处目标 ≈ 0。
        let out100 = sr_amp(&mut f, 100.0, 0.25, 48000, 8192);
        let in_rms = 0.25 / std::f32::consts::SQRT_2;
        let m100 = 20.0 * (out100 / in_rms).log10();
        assert!((m100 + 6.0).abs() < 0.5, "100Hz {m100}");
        let out5k = sr_amp(&mut f, 5000.0, 0.25, 48000, 8192);
        let m5k = 20.0 * (out5k / in_rms).log10();
        assert!(m5k.abs() < 0.5, "5k {m5k}");
    }

    #[test]
    fn wide_q_crossing_band_goes_to_iir() {
        // 宽 Q 段（fc=250, q=0.6）影响范围跨过分频点 → IIR 主实现；
        // 窄 Q 段（fc=300, q=5）影响完全在 200 Hz 以上 → FIR。
        let bands = vec![
            PeqBand { fc: 250.0, gain_db: -6.0, q: 0.6, kind: PeqBandType::Peaking },
            PeqBand { fc: 300.0, gain_db: -6.0, q: 8.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 400.0, gain_db: -6.0, q: 2.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 1000.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 2000.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 4000.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
        ];
        let mut f = HybridPeqFilter::new(PeqParams {
            crossover_hz: CROSSOVER_HZ,
            bands,
        });
        f.initialize(48000, &["L".into()]);
        assert_eq!(f.iir.len(), 2, "250/q0.6 与 400/q2.0 应进 IIR，300/q8 不进");
    }

    #[test]
    fn crossing_band_fits_across_crossover() {
        // 宽 Q 段跨分频点：低频由 IIR 精确、高频由 FIR 补偿，总响应 = 目标。
        let bands = vec![
            PeqBand { fc: 150.0, gain_db: -3.0, q: 0.8, kind: PeqBandType::Peaking },
            PeqBand { fc: 250.0, gain_db: -6.0, q: 0.6, kind: PeqBandType::Peaking },
            PeqBand { fc: 1000.0, gain_db: 3.0, q: 1.5, kind: PeqBandType::Peaking },
            PeqBand { fc: 4000.0, gain_db: -2.0, q: 2.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 8000.0, gain_db: 2.0, q: 1.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 16000.0, gain_db: -1.0, q: 1.0, kind: PeqBandType::Peaking },
        ];
        for sr in [48_000u32, 96_000] {
            let mut f = HybridPeqFilter::new(PeqParams {
                crossover_hz: CROSSOVER_HZ,
                bands: bands.clone(),
            });
            f.initialize(sr, &["L".into(), "R".into()]);
            for freq in [
                60.0f32, 100.0, 150.0, 180.0, 220.0, 250.0, 300.0, 400.0, 600.0, 1000.0, 4000.0,
            ] {
                let frames = 12000usize;
                let out_rms = sr_amp(&mut f, freq, 0.25, sr, frames);
                let in_rms = 0.25 / std::f32::consts::SQRT_2;
                let measured = 20.0 * (out_rms / in_rms).log10();
                let target = target_db(&bands, freq, sr);
                assert!(
                    (measured - target).abs() < 0.6,
                    "sr {sr} freq {freq}: measured {measured:.2} dB vs target {target:.2} dB"
                );
            }
        }
    }

    #[test]
    fn silence_stays_silent() {
        let bands = vec![
            PeqBand { fc: 100.0, gain_db: 6.0, q: 1.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 1000.0, gain_db: -6.0, q: 1.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 4000.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 8000.0, gain_db: -3.0, q: 1.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 12000.0, gain_db: 1.0, q: 1.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 16000.0, gain_db: -1.0, q: 1.0, kind: PeqBandType::Peaking },
        ];
        let mut f = HybridPeqFilter::new(PeqParams {
            crossover_hz: CROSSOVER_HZ,
            bands,
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let mut samples = vec![vec![0.0f32; 2048], vec![0.0f32; 2048]];
        f.process(&mut samples, 2048);
        for ch in &samples {
            for &v in ch {
                assert_eq!(v, 0.0);
            }
        }
    }

    #[test]
    fn mute_recovery_fades_in_without_glitch() {
        // 静音 500 帧 → 恢复正弦：输出应从 0 线性淡入（前 8 ms），无阶跃。
        let bands = vec![
            PeqBand { fc: 160.0, gain_db: -2.0, q: 2.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 600.0, gain_db: -6.0, q: 1.5, kind: PeqBandType::Peaking },
            PeqBand { fc: 1000.0, gain_db: 6.0, q: 2.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 2000.0, gain_db: 2.0, q: 1.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 4000.0, gain_db: 1.0, q: 2.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 8000.0, gain_db: -2.0, q: 1.5, kind: PeqBandType::Peaking },
        ];
        let mut f = HybridPeqFilter::new(PeqParams {
            crossover_hz: CROSSOVER_HZ,
            bands,
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let n = 2048usize;
        let mut samples = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 500..n {
            let v = 0.25 * (std::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin();
            samples[0][i] = v;
            samples[1][i] = v;
        }
        f.process(&mut samples, n);
        // 恢复第 1 帧：淡入起点 ≈ 0（无阶跃）。
        assert!(
            samples[0][500].abs() < 0.01,
            "fade-in start should be ~0, got {}",
            samples[0][500]
        );
        // 约 8 ms（384 帧）后淡入完成，幅度恢复正常（目标 1000 Hz +6 dB）。
        let later = samples[0][500 + 400].abs();
        assert!(later > 0.05, "fade should complete, got {later}");
    }

    #[test]
    fn extreme_params_finite_and_deterministic() {
        let mut bands = Vec::new();
        for i in 0..31 {
            let fc = 20.0 * (i as f32 + 1.0) * 1.6;
            bands.push(PeqBand {
                fc: fc.min(20000.0),
                gain_db: if i % 2 == 0 { 30.0 } else { -30.0 },
                q: if i % 3 == 0 { 12.0 } else { 0.1 },
                kind: PeqBandType::Peaking,
            });
        }
        for sr in [48_000u32, 96_000, 192_000] {
            let mut f = HybridPeqFilter::new(PeqParams {
                crossover_hz: CROSSOVER_HZ,
                bands: bands.clone(),
            });
            f.initialize(sr, &["L".into(), "R".into()]);
            let mut a = vec![vec![0.0f32; 2048], vec![0.0f32; 2048]];
            let mut b = vec![vec![0.0f32; 2048], vec![0.0f32; 2048]];
            for i in 0..2048 {
                let v = 0.5 * (std::f32::consts::TAU * 440.0 * i as f32 / sr as f32).sin();
                a[0][i] = v;
                a[1][i] = v * 0.3;
                b[0][i] = v;
                b[1][i] = v * 0.3;
            }
            f.process(&mut a, 2048);
            f.reset();
            f.process(&mut b, 2048);
            for ch in 0..2 {
                for i in 0..2048 {
                    assert!(a[ch][i].is_finite(), "finite @sr {sr}");
                    assert!((a[ch][i] - b[ch][i]).abs() < 1e-9, "deterministic @sr {sr}");
                }
            }
        }
    }

    #[test]
    fn latency_reports_fir_len_div_4() {
        let bands = vec![
            PeqBand { fc: 100.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 1000.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 2000.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 4000.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 8000.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 16000.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
        ];
        let mut f = HybridPeqFilter::new(PeqParams {
            crossover_hz: CROSSOVER_HZ,
            bands,
        });
        f.initialize(48_000, &["L".into()]);
        // 1024 抽头最小相位：保守群延迟估计 = 1024/4 = 256。
        assert_eq!(f.latency(), 256);
        let mut f2 = HybridPeqFilter::new(PeqParams {
            crossover_hz: CROSSOVER_HZ,
            bands: vec![
                PeqBand { fc: 100.0, gain_db: 0.0, q: 1.0, kind: PeqBandType::Peaking },
                PeqBand { fc: 200.0, gain_db: 0.0, q: 1.0, kind: PeqBandType::Peaking },
                PeqBand { fc: 300.0, gain_db: 0.0, q: 1.0, kind: PeqBandType::Peaking },
                PeqBand { fc: 400.0, gain_db: 0.0, q: 1.0, kind: PeqBandType::Peaking },
                PeqBand { fc: 500.0, gain_db: 0.0, q: 1.0, kind: PeqBandType::Peaking },
                PeqBand { fc: 600.0, gain_db: 0.0, q: 1.0, kind: PeqBandType::Peaking },
            ],
        });
        f2.initialize(192_000, &["L".into()]);
        // 192k → 4096 抽头 > 2048 阈值 → 分块 FFT，延迟 = 块大小 128 - 1。
        assert_eq!(f2.latency(), 127);
    }

    /// M16+ 实机配置波形回归：分块喂入（模拟 APOProcess 每 480 帧一调），
    /// 稳态输出必须保持正弦（频率不变、无长零段、RMS 符合目标）。
    #[test]
    fn sine_waveform_preserved_chunked_real_config() {
        let bands = vec![
            PeqBand { fc: 1500.0, gain_db: 1.0, q: 1.5, kind: PeqBandType::Peaking },
            PeqBand { fc: 2000.0, gain_db: 1.0, q: 1.5, kind: PeqBandType::Peaking },
            PeqBand { fc: 4500.0, gain_db: 1.0, q: 1.5, kind: PeqBandType::Peaking },
            PeqBand { fc: 5047.1, gain_db: 1.0, q: 1.5, kind: PeqBandType::Peaking },
            PeqBand { fc: 6000.0, gain_db: 3.0, q: 2.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 7812.0, gain_db: -6.0, q: 1.5, kind: PeqBandType::Peaking },
            PeqBand { fc: 10000.0, gain_db: 3.0, q: 2.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 13000.0, gain_db: -2.0, q: 2.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 16268.0, gain_db: -1.0, q: 2.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 20000.0, gain_db: -3.0, q: 3.0, kind: PeqBandType::Peaking },
        ];
        for sr in [44_100u32, 48_000, 96_000] {
            let mut f = HybridPeqFilter::new(PeqParams {
                crossover_hz: CROSSOVER_HZ,
                bands: bands.clone(),
            });
            f.initialize(sr, &["L".into(), "R".into()]);

            let freq = 1000.0f32;
            let chunk = 480usize;
            let total = 48000usize; // 1 秒
            let mut out = vec![vec![0.0f32; total], vec![0.0f32; total]];
            for start in (0..total).step_by(chunk) {
                let n = chunk.min(total - start);
                let mut block = vec![vec![0.0f32; n], vec![0.0f32; n]];
                for i in 0..n {
                    let v = 0.25
                        * (std::f32::consts::TAU * freq * (start + i) as f32 / sr as f32).sin();
                    block[0][i] = v;
                    block[1][i] = v * 0.5;
                }
                f.process(&mut block, n);
                out[0][start..start + n].copy_from_slice(&block[0]);
                out[1][start..start + n].copy_from_slice(&block[1]);
            }

            let st = 8192usize; // 跳过预热（FIR 1024 + 淡入 384）
            let tail = &out[0][st..];
            // 1) 全有限
            assert!(tail.iter().all(|v| v.is_finite()), "finite @sr {sr}");
            // 2) 稳态无长零段（>128 连续零 = 异常静音/掉帧）
            let mut zeros = 0usize;
            for &v in tail {
                if v.abs() < 1e-6 {
                    zeros += 1;
                    assert!(zeros <= 128, "long zero run @sr {sr} len={zeros}");
                } else {
                    zeros = 0;
                }
            }
            // 3) 过零率保持 2×freq（±10%，FIR 窗边缘不计）
            let period = (sr as f32 / freq).round() as usize;
            let crossings = tail
                .windows(2)
                .filter(|w| (w[0] < 0.0 && w[1] >= 0.0) || (w[0] >= 0.0 && w[1] < 0.0))
                .count();
            let expect = (tail.len() as f32 * 2.0 * freq / sr as f32).round() as usize;
            assert!(
                (crossings as i64 - expect as i64).unsigned_abs() <= (expect as i64 / 10).unsigned_abs() as u64,
                "crossings @sr {sr}: got {crossings}, expect ~{expect}"
            );
            // 4) 稳态 RMS ≈ 目标（1k 处 ≈ -1.17 dB）
            let span = (tail.len() / period) * period;
            let rms: f32 = (tail[..span].iter().map(|x| x * x).sum::<f32>() / span as f32).sqrt();
            let db = 20.0 * (rms / (0.25 / std::f32::consts::SQRT_2)).log10();
            let target = target_db(&bands, freq, sr);
            assert!(
                (db - target).abs() < 0.5,
                "rms @sr {sr}: {db:.2} dB vs target {target:.2} dB"
            );
        }
    }

    /// APP 实际序列化形态：每段一个 `[[effects]] type="peq"` 块 → 驱动级联
    /// 10 个单段 HybridPeqFilter。分块喂入必须保持正弦（无慢放/电流音）。
    #[test]
    fn sine_waveform_preserved_per_band_blocks_cascaded() {
        use crate::pipeline::chain::Chain;
        use crate::pipeline::dsp::filter::Filter;

        let bands = vec![
            PeqBand { fc: 1500.0, gain_db: 1.0, q: 1.5, kind: PeqBandType::Peaking },
            PeqBand { fc: 2000.0, gain_db: 1.0, q: 1.5, kind: PeqBandType::Peaking },
            PeqBand { fc: 4500.0, gain_db: 1.0, q: 1.5, kind: PeqBandType::Peaking },
            PeqBand { fc: 5047.1, gain_db: 1.0, q: 1.5, kind: PeqBandType::Peaking },
            PeqBand { fc: 6000.0, gain_db: 3.0, q: 2.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 7812.0, gain_db: -6.0, q: 1.5, kind: PeqBandType::Peaking },
            PeqBand { fc: 10000.0, gain_db: 3.0, q: 2.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 13000.0, gain_db: -2.0, q: 2.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 16268.0, gain_db: -1.0, q: 2.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 20000.0, gain_db: -3.0, q: 3.0, kind: PeqBandType::Peaking },
        ];
        let sr = 48_000u32;
        let mut chain = Chain::new();
        for band in &bands {
            chain
                .add_filter(Box::new(HybridPeqFilter::new(PeqParams {
                    crossover_hz: CROSSOVER_HZ,
                    bands: vec![*band],
                })))
                .unwrap();
        }
        chain.initialize(sr, &["L".into(), "R".into()]);

        let freq = 1000.0f32;
        let chunk = 480usize;
        let total = 48000usize;
        let mut out = vec![vec![0.0f32; total], vec![0.0f32; total]];
        for start in (0..total).step_by(chunk) {
            let n = chunk.min(total - start);
            let mut block = vec![vec![0.0f32; n], vec![0.0f32; n]];
            for i in 0..n {
                let v =
                    0.25 * (std::f32::consts::TAU * freq * (start + i) as f32 / sr as f32).sin();
                block[0][i] = v;
                block[1][i] = v * 0.5;
            }
            chain.process(&mut block, n).unwrap();
            out[0][start..start + n].copy_from_slice(&block[0]);
            out[1][start..start + n].copy_from_slice(&block[1]);
        }

        let st = 16384usize; // 10 条 FIR 预热更久，跳过前 1/3
        let tail = &out[0][st..];
        assert!(tail.iter().all(|v| v.is_finite()), "finite");
        let mut zeros = 0usize;
        for &v in tail {
            if v.abs() < 1e-6 {
                zeros += 1;
                assert!(zeros <= 128, "long zero run len={zeros}");
            } else {
                zeros = 0;
            }
        }
        let period = (sr as f32 / freq).round() as usize;
        let crossings = tail
            .windows(2)
            .filter(|w| (w[0] < 0.0 && w[1] >= 0.0) || (w[0] >= 0.0 && w[1] < 0.0))
            .count();
        let expect = (tail.len() as f32 * 2.0 * freq / sr as f32).round() as usize;
        assert!(
            (crossings as i64 - expect as i64).unsigned_abs()
                <= (expect as i64 / 10).unsigned_abs() as u64,
            "crossings: got {crossings}, expect ~{expect}"
        );
        let span = (tail.len() / period) * period;
        let rms: f32 = (tail[..span].iter().map(|x| x * x).sum::<f32>() / span as f32).sqrt();
        let db = 20.0 * (rms / (0.25 / std::f32::consts::SQRT_2)).log10();
        let target = target_db(&bands, freq, sr);
        assert!(
            (db - target).abs() < 0.5,
            "rms: {db:.2} dB vs target {target:.2} dB"
        );
    }
}
