//! Compressor（动态范围压缩器）
//!
//! 算法结构（每帧，全声道联动检测）：
//! 1. 全声道瞬时 RMS → 检测电平（dBFS）；
//! 2. 静态压缩曲线（含软膝）：超阈值部分按 `1 - 1/ratio` 削减，
//!    软膝带内二次平滑过渡；
//! 3. 增益削减在 dB 域平滑：attack（压缩增加）快、release 慢；
//! 4. 输出 = 输入 × makeup 增益 × 10^(-gr_db/20)，再 Wet/Dry 混合。
//!
//! config 语法（EAPO 风格）：
//! `Compressor: Threshold -18 dB Ratio 4 Knee 3 dB Attack 10 ms
//!  Release 100 ms Makeup 6 dB [Wet 1.0 Dry 0.0]`

use crate::pipeline::dsp::filter::Filter;

#[derive(Debug, Clone, Copy)]
pub struct CompressorParams {
    /// 压缩阈值（dBFS，-60..0）。
    pub threshold_db: f32,
    /// 压缩比（1..20，1 = 不压缩）。
    pub ratio: f32,
    /// 软膝宽度（dB，0..12，0 = 硬膝）。
    pub knee_db: f32,
    /// 攻击时间（ms，0.1..100）。
    pub attack_ms: f32,
    /// 释放时间（ms，10..1000）。
    pub release_ms: f32,
    /// 补偿增益（dB，0..24）。
    pub makeup_gain_db: f32,
    pub wet: f32,
    pub dry: f32,
}

impl Default for CompressorParams {
    fn default() -> Self {
        Self {
            threshold_db: -18.0,
            ratio: 4.0,
            knee_db: 3.0,
            attack_ms: 10.0,
            release_ms: 100.0,
            makeup_gain_db: 6.0,
            wet: 1.0,
            dry: 0.0,
        }
    }
}

#[derive(Debug)]
pub struct CompressorFilter {
    params: CompressorParams,
    channel_indices: Vec<usize>,
    /// 平滑后的增益削减（dB，>=0）。
    gr_db: f32,
    /// RMS 检测器平滑电平（E[x²]），约 10ms，避免单帧平方波动。
    det_level: f64,
    det_alpha: f64,
    attack_c: f32,
    release_c: f32,
    /// makeup 线性增益。
    makeup: f32,
}

impl CompressorFilter {
    pub fn new(params: CompressorParams) -> Self {
        Self {
            params,
            channel_indices: Vec::new(),
            gr_db: 0.0,
            det_level: 0.0,
            det_alpha: 0.0,
            attack_c: 0.0,
            release_c: 0.0,
            makeup: 1.0,
        }
    }

    /// 静态压缩曲线：给定检测电平返回增益削减（dB，>=0）。
    #[inline]
    fn curve(level_db: f32, threshold_db: f32, ratio: f32, knee_db: f32) -> f32 {
        let over = level_db - threshold_db;
        let half = knee_db * 0.5;
        let slope = 1.0 - 1.0 / ratio.max(1.0);
        if over <= -half {
            0.0
        } else if over >= half {
            over * slope
        } else {
            // 软膝：二次插值从 0 平滑到 knee/2 处的压缩量。
            let x = (over + half) / knee_db.max(1e-3);
            x * x * half * slope
        }
    }
}

impl Filter for CompressorFilter {
    fn initialize(&mut self, sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        if self.channel_indices.is_empty() {
            self.channel_indices = (0..channel_names.len()).collect();
        }
        let sr = sample_rate.max(1) as f32;
        let p = self.params;
        let attack_s = p.attack_ms.clamp(0.1, 100.0) * 0.001;
        let release_s = p.release_ms.clamp(10.0, 1000.0) * 0.001;
        self.attack_c = 1.0 - (-1.0 / (attack_s * sr)).exp();
        self.release_c = 1.0 - (-1.0 / (release_s * sr)).exp();
        self.det_alpha = (1.0 - (-1.0 / (0.010 * sr)).exp()) as f64;
        self.makeup = 10.0f32.powf(p.makeup_gain_db.clamp(0.0, 24.0) / 20.0);
        self.gr_db = 0.0;
        self.det_level = 0.0;
        None
    }

    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        if self.channel_indices.is_empty() {
            return;
        }
        let n = self.channel_indices.len();
        let first = self.channel_indices[0];
        if first >= samples.len() {
            return;
        }
        let frame_count = frame_count.min(samples[first].len());
        let threshold = self.params.threshold_db.clamp(-60.0, 0.0);
        let ratio = self.params.ratio.clamp(1.0, 20.0);
        let knee = self.params.knee_db.clamp(0.0, 12.0);
        let makeup = self.makeup;
        let wet = self.params.wet;
        let dry = self.params.dry;

        for f in 0..frame_count {
            // 1) 全声道 RMS 检测器（约 10ms 平滑）→ 检测电平。
            let mut sum_sq = 0.0f64;
            for k in 0..n {
                let slot = self.channel_indices[k];
                if slot < samples.len() {
                    let x = samples[slot][f] as f64;
                    sum_sq += x * x;
                }
            }
            let mean = sum_sq / n as f64;
            self.det_level += self.det_alpha * (mean - self.det_level);
            let level_db = 10.0 * (self.det_level.max(1e-14)).log10() as f32;

            // 2) 静态曲线 → 目标削减。
            let target_gr = Self::curve(level_db, threshold, ratio, knee);

            // 3) dB 域平滑：attack 快、release 慢。
            let coeff = if target_gr > self.gr_db {
                self.attack_c
            } else {
                self.release_c
            };
            self.gr_db += (target_gr - self.gr_db) * coeff;

            // 4) 应用增益（makeup - gr）并干湿混合。
            let gain = makeup * 10.0f32.powf(-self.gr_db / 20.0);
            for k in 0..n {
                let slot = self.channel_indices[k];
                if slot >= samples.len() {
                    continue;
                }
                let input = samples[slot][f];
                let out = input * gain;
                samples[slot][f] = if out.is_finite() {
                    out * wet + input * dry
                } else {
                    0.0
                };
            }
        }
    }

    fn set_channel_indices(&mut self, indices: &[usize]) {
        self.channel_indices = indices.to_vec();
    }

    fn reset(&mut self) {
        self.gr_db = 0.0;
        self.det_level = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        vec!["L".into(), "R".into()]
    }

    fn rms_db(samples: &[Vec<f32>], start: usize) -> f32 {
        let mut sum = 0.0f64;
        let mut count = 0.0f64;
        for ch in samples {
            for (i, &v) in ch.iter().enumerate() {
                if i >= start {
                    sum += (v as f64) * (v as f64);
                    count += 1.0;
                }
            }
        }
        if count <= 0.0 {
            return -120.0;
        }
        (10.0 * (sum / count).max(1e-14).log10()) as f32
    }

    #[test]
    fn dry_only_is_passthrough() {
        let mut f = CompressorFilter::new(CompressorParams {
            wet: 0.0,
            dry: 1.0,
            ..Default::default()
        });
        f.initialize(48000, &names());
        let mut samples = vec![vec![0.3f32; 480], vec![0.2f32; 480]];
        let before = samples.clone();
        f.process(&mut samples, 480);
        for (a, b) in samples.iter().zip(before.iter()) {
            for (x, y) in a.iter().zip(b.iter()) {
                assert!((x - y).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn silence_stays_silent() {
        let mut f = CompressorFilter::new(CompressorParams::default());
        f.initialize(48000, &names());
        let mut samples = vec![vec![0.0f32; 4800], vec![0.0f32; 4800]];
        f.process(&mut samples, 4800);
        for ch in &samples {
            for &v in ch {
                assert_eq!(v, 0.0);
            }
        }
    }

    #[test]
    fn below_threshold_gets_makeup_only() {
        // -40dBFS 输入（远低于阈值 -18）：只加 makeup，不压缩。
        let mut f = CompressorFilter::new(CompressorParams {
            makeup_gain_db: 6.0,
            ..Default::default()
        });
        f.initialize(48000, &names());
        let n = 48_000usize;
        let amp = 10.0f32.powf(-40.0 / 20.0);
        let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = amp * (core::f32::consts::TAU * 220.0 * i as f32 / 48000.0).sin();
            s[0][i] = v;
            s[1][i] = v;
        }
        f.process(&mut s, n);
        let out_rms = rms_db(&s, 24_000);
        let expect = -43.0 + 6.0; // 输入 RMS ≈ -43dB + makeup 6dB
        assert!(
            (out_rms - expect).abs() < 0.5,
            "below threshold: got {out_rms:.1}, expected ≈{expect:.1}"
        );
    }

    #[test]
    fn above_threshold_is_compressed_by_ratio() {
        // 稳态超阈值信号：输出超出部分按 1/ratio 保留。
        // 输入 RMS -6dB，阈值 -18 → over 12dB，ratio 4 → gr 9dB，输出 -15dB（+makeup 6 → -9dB）。
        let run = |ratio: f32| -> f32 {
            let mut f = CompressorFilter::new(CompressorParams {
                threshold_db: -18.0,
                ratio,
                knee_db: 0.0,
                attack_ms: 1.0,
                release_ms: 10.0,
                makeup_gain_db: 0.0,
                ..Default::default()
            });
            f.initialize(48000, &names());
            let n = 96_000usize;
            let amp = 10.0f32.powf(-6.0 / 20.0);
            let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
            for i in 0..n {
                let v = amp * (core::f32::consts::TAU * 220.0 * i as f32 / 48000.0).sin();
                s[0][i] = v;
                s[1][i] = v;
            }
            f.process(&mut s, n);
            rms_db(&s, 48_000)
        };
        let out = run(4.0);
        // 输入 RMS ≈ -9dB；over = 9dB；gr = 9×0.75 = 6.75 → 输出 -15.75dB。
        assert!(
            (out + 15.75).abs() < 0.7,
            "4:1 compression wrong: got {out:.1}, expected ≈-15.8"
        );
        // ratio 更高 → 压缩更多、输出更静。
        let out20 = run(20.0);
        assert!(out20 < out, "higher ratio must compress more: {out20:.1} vs {out:.1}");
    }

    #[test]
    fn attack_is_smoothed_no_jump() {
        // 阶跃超阈值输入：增益削减逐帧逼近，无瞬时跳变。
        let mut f = CompressorFilter::new(CompressorParams {
            threshold_db: -12.0,
            ratio: 10.0,
            knee_db: 0.0,
            attack_ms: 50.0,
            release_ms: 200.0,
            makeup_gain_db: 0.0,
            ..Default::default()
        });
        f.initialize(48000, &names());
        let n = 4800usize;
        let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = 0.7 * (core::f32::consts::TAU * 220.0 * i as f32 / 48000.0).sin();
            s[0][i] = v;
            s[1][i] = v;
        }
        f.process(&mut s, n);
        assert!(
            f.gr_db > 0.0 && f.gr_db < 5.5,
            "gr must rise smoothly, got {}",
            f.gr_db
        );
    }

    #[test]
    fn soft_knee_smooths_transition() {
        // 在阈值附近：软膝输入削减连续（硬膝在阈值处从 0 跳变）。
        let th = -18.0f32;
        let ratio = 4.0f32;
        let knee = 6.0f32;
        let below = CompressorFilter::curve(-18.0 - 3.1, th, ratio, knee);
        let at = CompressorFilter::curve(-18.0, th, ratio, knee);
        let above = CompressorFilter::curve(-18.0 + 3.1, th, ratio, knee);
        assert_eq!(below, 0.0);
        assert!(at > 0.0 && at < above);
        assert!(above > 0.0);
    }

    #[test]
    fn deterministic_and_finite() {
        let run = || -> Vec<f32> {
            let mut f = CompressorFilter::new(CompressorParams::default());
            f.initialize(48000, &names());
            let mut s = vec![vec![0.0f32; 2048], vec![0.0f32; 2048]];
            for i in 0..2048 {
                let v = 0.5 * (core::f32::consts::TAU * 440.0 * i as f32 / 48000.0).sin();
                s[0][i] = v;
                s[1][i] = v * 0.6;
            }
            f.process(&mut s, 2048);
            s.into_iter().flatten().collect()
        };
        let a = run();
        let b = run();
        for (x, y) in a.iter().zip(b.iter()) {
            assert!(x.is_finite());
            assert!((x - y).abs() < 1e-9);
        }
    }

    #[test]
    fn reset_clears_gain_reduction() {
        let mut f = CompressorFilter::new(CompressorParams {
            threshold_db: -30.0,
            ratio: 10.0,
            ..Default::default()
        });
        f.initialize(48000, &names());
        let mut s = vec![vec![0.0f32; 4800], vec![0.0f32; 4800]];
        for i in 0..4800 {
            s[0][i] = 0.8 * (core::f32::consts::TAU * 220.0 * i as f32 / 48000.0).sin();
        }
        f.process(&mut s, 4800);
        assert!(f.gr_db > 0.0);
        f.reset();
        assert_eq!(f.gr_db, 0.0);
    }
}
