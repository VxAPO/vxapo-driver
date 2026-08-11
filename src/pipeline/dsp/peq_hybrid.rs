//! pipeline/dsp/peq_hybrid.rs — 混合式 PEQ（v9.11）
//!
//! 200 Hz 分频：`Fc < CROSSOVER_HZ` 的段走 IIR biquad 级联；`Fc >= CROSSOVER_HZ`
//! 的段由采样率自适应最小相位 FIR 承担。级联结构：
//! `输入 → IIR（低频段，按 fc 升序）→ 最小相位 FIR → 输出`。
//!
//! 频响合成：目标曲线 `T(f) = Σ 所有段频响（dB）`，IIR 路径频响
//! `L(f) = Σ 低频段频响（dB）`，FIR 目标 `F(f) = T(f) − L(f)`——级联总响应
//! `L + F = T`，幅度精确拟合目标曲线（跨分频点段自动处理）。

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

use crate::pipeline::dsp::filter::Filter;
use crate::pipeline::dsp::fir::PartitionedFir;
use crate::pipeline::dsp::math::warn_rate_limited;
use crate::pipeline::dsp::model::{CROSSOVER_HZ, PeqParams};

/// FIR 长度下限（@44.1k/48k）。
const FIR_MIN_LEN: usize = 1024;
/// FIR 长度上限（@384k）。
const FIR_MAX_LEN: usize = 8192;
/// 直接 FIR 最大长度（超过走分块 FFT）。
const DIRECT_FIR_MAX_LEN: usize = 2048;
/// 频响幅值下限，避免 log(0)。
const MIN_MAG: f32 = 1e-5;

/// 二阶 peaking biquad（RBJ，DF2T）。
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
            .filter(|b| b.fc < CROSSOVER_HZ)
            .filter_map(|b| {
                let c = Biquad::peaking(b.fc, b.gain_db, b.q, sample_rate);
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

        for k in 0..n_ch {
            let slot = self.channel_indices[k];
            if slot >= samples.len() {
                continue;
            }
            let ch = &mut self.channels[k];
            let frames = frame_count.min(samples[slot].len());

            for f in 0..frames {
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
                samples[slot][f] = if out.is_finite() { out } else { 0.0 };
            }
        }
    }

    fn latency(&self) -> u32 {
        match &self.fir {
            PeqFir::Direct { fir_len, .. } => fir_len.saturating_sub(1) as u32,
            PeqFir::Partitioned(pf) => pf.latency(),
        }
    }

    fn set_channel_indices(&mut self, indices: &[usize]) {
        self.channel_indices = indices.to_vec();
    }

    fn reset(&mut self) {
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
            t_db += peaking_response_db(b.fc, b.gain_db, b.q, freq, sr);
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

    // 3) 最小相位倒谱：n>N/2 置 0，0<n<N/2 加倍。
    let mut cep_min = vec![Complex::new(0.0, 0.0); n];
    cep_min[0] = spectrum[0];
    for k in 1..n / 2 {
        cep_min[k] = Complex::new(spectrum[k].re * 2.0, 0.0);
    }

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

/// peaking 频响（dB）——与 Biquad::peaking 同一 RBJ 数字传输函数。
pub(crate) fn peaking_response_db(fc: f32, gain_db: f32, q: f32, freq: f32, sr: f32) -> f32 {
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
    use crate::pipeline::dsp::model::PeqBand;

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
            .map(|b| peaking_response_db(b.fc, b.gain_db, b.q, freq, sr as f32))
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
    fn hybrid_matches_target_across_crossover() {
        // 多段（含跨 200 Hz）：级联输出频响 ≈ 目标 ±0.5 dB。
        let bands = vec![
            PeqBand { fc: 100.0, gain_db: -3.0, q: 1.0 },
            PeqBand { fc: 200.0, gain_db: 4.0, q: 1.2 },
            PeqBand { fc: 1000.0, gain_db: 6.0, q: 1.0 },
            PeqBand { fc: 4000.0, gain_db: -2.0, q: 2.0 },
            PeqBand { fc: 8000.0, gain_db: 3.0, q: 1.5 },
            PeqBand { fc: 16000.0, gain_db: -1.0, q: 1.0 },
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
        let bands = vec![PeqBand { fc: 100.0, gain_db: -6.0, q: 1.0 }];
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
    fn silence_stays_silent() {
        let bands = vec![
            PeqBand { fc: 100.0, gain_db: 6.0, q: 1.0 },
            PeqBand { fc: 1000.0, gain_db: -6.0, q: 1.0 },
            PeqBand { fc: 4000.0, gain_db: 3.0, q: 1.0 },
            PeqBand { fc: 8000.0, gain_db: -3.0, q: 1.0 },
            PeqBand { fc: 12000.0, gain_db: 1.0, q: 1.0 },
            PeqBand { fc: 16000.0, gain_db: -1.0, q: 1.0 },
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
    fn extreme_params_finite_and_deterministic() {
        let mut bands = Vec::new();
        for i in 0..31 {
            let fc = 20.0 * (i as f32 + 1.0) * 1.6;
            bands.push(PeqBand {
                fc: fc.min(20000.0),
                gain_db: if i % 2 == 0 { 30.0 } else { -30.0 },
                q: if i % 3 == 0 { 12.0 } else { 0.1 },
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
    fn latency_reports_fir_len_minus_one() {
        let bands = vec![
            PeqBand { fc: 100.0, gain_db: 3.0, q: 1.0 },
            PeqBand { fc: 1000.0, gain_db: 3.0, q: 1.0 },
            PeqBand { fc: 2000.0, gain_db: 3.0, q: 1.0 },
            PeqBand { fc: 4000.0, gain_db: 3.0, q: 1.0 },
            PeqBand { fc: 8000.0, gain_db: 3.0, q: 1.0 },
            PeqBand { fc: 16000.0, gain_db: 3.0, q: 1.0 },
        ];
        let mut f = HybridPeqFilter::new(PeqParams {
            crossover_hz: CROSSOVER_HZ,
            bands,
        });
        f.initialize(48_000, &["L".into()]);
        assert_eq!(f.latency(), (1024 - 1) as u32);
        let mut f2 = HybridPeqFilter::new(PeqParams {
            crossover_hz: CROSSOVER_HZ,
            bands: vec![
                PeqBand { fc: 100.0, gain_db: 0.0, q: 1.0 },
                PeqBand { fc: 200.0, gain_db: 0.0, q: 1.0 },
                PeqBand { fc: 300.0, gain_db: 0.0, q: 1.0 },
                PeqBand { fc: 400.0, gain_db: 0.0, q: 1.0 },
                PeqBand { fc: 500.0, gain_db: 0.0, q: 1.0 },
                PeqBand { fc: 600.0, gain_db: 0.0, q: 1.0 },
            ],
        });
        f2.initialize(192_000, &["L".into()]);
        // 192k → 4096 抽头 > 2048 阈值 → 分块 FFT，延迟 = 块大小 128。
        assert_eq!(f2.latency(), 128);
    }
}
