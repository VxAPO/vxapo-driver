//! Aural Enhancer（谐波激励器）
//!
//! 独立实现（原创 Rust 代码，不再移植自 FxSound `Auralp.c`，已移除 AGPL 版权头）。
//! 算法（参考 Jatin Chowdhury / FAUST 类电平独立软饱和设计）：
//! - 二阶 Butterworth 高通（`TuneHz`）提取待激励频段；
//! - 峰值电平跟随器（瞬时 attack / ~120 ms release，各声道共享）→ 归一化驱动，
//!   谐波占比与输入电平无关（电平独立饱和，小音量不丢细节、大音量不发毛）；
//! - `tanh` 软饱和生成奇次谐波；半波整流 + 20 Hz DC 阻塞生成偶次谐波；
//! - `out = in + even·even_harm + odd·odd_harm`，再 Wet/Dry。
//!
//! config 语法（EAPO 风格）：
//! `AuralEnhancer: TuneHz 1760 Drive 1.77 Odd 1.5 Even 0.0 Wet 1.0 Dry 0.0`

use crate::pipeline::dsp::filter::Filter;

/// 默认 Aural Tune（对应原 Quick preset 1 / MIDI 53 映射，约 1.76 kHz）。
pub const DEFAULT_TUNE_HZ: f32 = 1760.0;
/// 电平跟随器 release 时间常数。
const ENV_RELEASE_S: f32 = 0.120;
/// 偶次谐波 DC 阻塞高通频率。
const DC_BLOCK_HZ: f32 = 20.0;
/// 电平跟随器下限（防除零/非规格化）。
const ENV_FLOOR: f32 = 1.0e-6;

#[derive(Debug, Clone, Copy)]
pub struct AuralParams {
    pub tune_hz: f32,
    pub drive: f32,
    pub odd: f32,
    pub even: f32,
    pub wet: f32,
    pub dry: f32,
}

impl Default for AuralParams {
    fn default() -> Self {
        Self {
            tune_hz: DEFAULT_TUNE_HZ,
            drive: 1.76993,
            odd: 1.5,
            even: 0.0,
            wet: 1.0,
            dry: 0.0,
        }
    }
}

/// 二阶 Butterworth 高通状态（Direct Form II transposed，与原设计一致）。
#[derive(Debug, Clone, Copy, Default)]
struct HpState {
    out1: f32,
    out2: f32,
    in1: f32,
    in2: f32,
}

/// 一阶 DC 阻塞状态。
#[derive(Debug, Clone, Copy, Default)]
struct DcBlockState {
    x1: f32,
    y1: f32,
}

#[derive(Debug, Clone, Copy, Default)]
struct AuralChannelState {
    hp: HpState,
    dc: DcBlockState,
}

#[derive(Debug)]
pub struct AuralEnhancerFilter {
    params: AuralParams,
    hp_gain: f32,
    hp_a1: f32,
    hp_a0: f32,
    dc_a: f32,
    env_release: f32,
    env: f32,
    channel_indices: Vec<usize>,
    states: Vec<AuralChannelState>,
    scratch: Vec<f32>,
}

impl AuralEnhancerFilter {
    pub fn new(params: AuralParams) -> Self {
        Self {
            params,
            hp_gain: 0.0,
            hp_a1: 0.0,
            hp_a0: 0.0,
            dc_a: 0.0,
            env_release: 0.0,
            env: 0.0,
            channel_indices: Vec::new(),
            states: Vec::new(),
            scratch: Vec::new(),
        }
    }
}

impl Filter for AuralEnhancerFilter {
    fn initialize(&mut self, sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        if self.channel_indices.is_empty() {
            self.channel_indices = (0..channel_names.len()).collect();
        }
        let sr = sample_rate.max(1) as f32;

        // 标准二阶 Butterworth 高通（线性变换形式）。
        let omega = core::f32::consts::TAU * self.params.tune_hz / sr;
        let omega2 = omega * omega;
        let two_root2_omega = 2.0 * core::f32::consts::SQRT_2 * omega;
        let tmp = 1.0 / (4.0 + omega2 + two_root2_omega);
        self.hp_gain = 4.0 * tmp;
        self.hp_a1 = (8.0 - 2.0 * omega2) * tmp;
        self.hp_a0 = (two_root2_omega - 4.0 - omega2) * tmp;

        self.dc_a = (-core::f32::consts::TAU * DC_BLOCK_HZ / sr).exp();
        self.env_release = (-1.0 / (ENV_RELEASE_S * sr)).exp();
        self.env = 0.0;

        let n = self.channel_indices.len().max(1);
        self.states = vec![AuralChannelState::default(); n];
        self.scratch = vec![0.0; n];
        None
    }

    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        let n = self.channel_indices.len().min(self.states.len()).min(self.scratch.len());
        if n == 0 {
            return;
        }
        let drive = self.params.drive;
        let odd = self.params.odd;
        let even = self.params.even;
        let wet = self.params.wet;
        let dry = self.params.dry;

        for f in 0..frame_count {
            // 1) 高通各声道并统计峰值（共享电平跟随器，保持立体声相干）。
            let mut peak = 0.0f32;
            for k in 0..n {
                let slot = self.channel_indices[k];
                if slot >= samples.len() {
                    continue;
                }
                let x = samples[slot][f];
                let st = &mut self.states[k];
                let filt = st.hp.out1 * self.hp_a1
                    + st.hp.out2 * self.hp_a0
                    + (x - 2.0 * st.hp.in1 + st.hp.in2) * self.hp_gain;
                st.hp.out2 = st.hp.out1;
                st.hp.out1 = filt;
                st.hp.in2 = st.hp.in1;
                st.hp.in1 = x;
                self.scratch[k] = filt;
                let a = filt.abs();
                if a > peak {
                    peak = a;
                }
            }

            // 2) 峰值电平跟随：瞬时 attack，指数 release。
            if peak > self.env {
                self.env = peak;
            } else {
                self.env *= self.env_release;
                if self.env < ENV_FLOOR {
                    self.env = ENV_FLOOR;
                }
            }
            let inv_env = 1.0 / self.env;

            // 3) 归一化驱动 + tanh 软饱和 + 偶次半波整流 + 混合。
            for k in 0..n {
                let slot = self.channel_indices[k];
                if slot >= samples.len() {
                    continue;
                }
                let x = samples[slot][f];
                let s = self.scratch[k] * inv_env;
                let y = (drive * s).tanh();
                let odd_harm = self.env * y;

                let rect = 0.5 * (y + y.abs());
                let st = &mut self.states[k];
                let dc = rect - st.dc.x1 + self.dc_a * st.dc.y1;
                st.dc.x1 = rect;
                st.dc.y1 = dc;
                let even_harm = self.env * dc;

                let processed = x + even * even_harm + odd * odd_harm;
                let out = processed * wet + x * dry;
                samples[slot][f] = if out.is_finite() { out } else { 0.0 };
            }
        }
    }

    fn set_channel_indices(&mut self, indices: &[usize]) {
        self.channel_indices = indices.to_vec();
    }

    fn reset(&mut self) {
        self.env = 0.0;
        for st in self.states.iter_mut() {
            *st = AuralChannelState::default();
        }
        self.scratch.fill(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_only_is_passthrough() {
        let mut f = AuralEnhancerFilter::new(AuralParams {
            wet: 0.0,
            dry: 1.0,
            ..Default::default()
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let mut samples = vec![vec![0.5f32; 64], vec![0.25f32; 64]];
        let before = samples.clone();
        f.process(&mut samples, 64);
        for (a, b) in samples.iter().zip(before.iter()) {
            for (x, y) in a.iter().zip(b.iter()) {
                assert!((x - y).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn wet_process_is_finite_and_changes_signal() {
        let mut f = AuralEnhancerFilter::new(AuralParams::default());
        f.initialize(48000, &["L".into(), "R".into()]);
        let mut samples = vec![vec![0.0f32; 480], vec![0.0f32; 480]];
        for i in 0..480 {
            samples[0][i] = (core::f32::consts::TAU * 440.0 * i as f32 / 48000.0).sin() * 0.5;
            samples[1][i] = samples[0][i] * 0.5;
        }
        let before = samples.clone();
        f.process(&mut samples, 480);
        let mut changed = false;
        for ch in 0..2 {
            for i in 0..480 {
                assert!(samples[ch][i].is_finite());
                changed |= (samples[ch][i] - before[ch][i]).abs() > 1e-4;
            }
        }
        assert!(changed);
    }

    #[test]
    fn silence_stays_silent() {
        let mut f = AuralEnhancerFilter::new(AuralParams::default());
        f.initialize(48000, &["L".into(), "R".into()]);
        let mut samples = vec![vec![0.0f32; 4800], vec![0.0f32; 4800]];
        f.process(&mut samples, 4800);
        for ch in &samples {
            for &v in ch {
                assert_eq!(v, 0.0);
            }
        }
    }

    /// 稳态段上输出相对输入的失真占比（RMS）。
    fn distortion_ratio(amp: f32, sr: u32) -> f32 {
        let mut f = AuralEnhancerFilter::new(AuralParams {
            tune_hz: 1760.0,
            drive: 1.77,
            odd: 1.0,
            even: 0.0,
            wet: 1.0,
            dry: 0.0,
        });
        f.initialize(sr, &["L".into()]);
        let n = 4800usize;
        let mut samples = vec![vec![0.0f32; n]];
        for i in 0..n {
            samples[0][i] =
                amp * (core::f32::consts::TAU * 3000.0 * i as f32 / sr as f32).sin();
        }
        f.process(&mut samples, n);
        let mut d2 = 0.0f32;
        let mut i2 = 0.0f32;
        for i in 2000..n {
            let x = amp * (core::f32::consts::TAU * 3000.0 * i as f32 / sr as f32).sin();
            let d = samples[0][i] - x;
            d2 += d * d;
            i2 += x * x;
        }
        (d2 / i2).sqrt()
    }

    #[test]
    fn harmonics_are_level_independent() {
        // 电平独立：输入幅度 10 倍变化，失真占比应基本一致。
        let r_low = distortion_ratio(0.05, 48_000);
        let r_high = distortion_ratio(0.5, 48_000);
        assert!(
            (r_low / r_high - 1.0).abs() < 0.2,
            "level independence broken: low {r_low}, high {r_high}"
        );
        assert!(r_low > 0.01 && r_low < 5.0, "unexpected ratio {r_low}");
    }

    #[test]
    fn odd_path_has_no_dc() {
        let mut f = AuralEnhancerFilter::new(AuralParams {
            tune_hz: 1760.0,
            drive: 1.77,
            odd: 1.5,
            even: 0.0,
            wet: 1.0,
            dry: 0.0,
        });
        f.initialize(48000, &["L".into()]);
        let n = 4800usize;
        let mut samples = vec![vec![0.0f32; n]];
        for i in 0..n {
            samples[0][i] = 0.5 * (core::f32::consts::TAU * 3000.0 * i as f32 / 48000.0).sin();
        }
        f.process(&mut samples, n);
        // 3000 Hz @48k = 16 帧/周期；取 2000..4400 共 150 个整周期，避免窗口残差。
        let mean = samples[0][2000..4400].iter().sum::<f32>() / 2400.0;
        assert!(mean.abs() < 1.0e-3, "odd path should have no DC, mean {mean}");
    }

    /// 样本段在指定频率处的单侧幅度（窗口须覆盖整数周期）。
    fn spectral_amp(samples: &[f32], start: usize, freq_hz: f32, sr: u32) -> f32 {
        let n = (samples.len() - start) as f32;
        let mut re = 0.0f32;
        let mut im = 0.0f32;
        for i in start..samples.len() {
            let t = i as f32 / sr as f32;
            let w = core::f32::consts::TAU * freq_hz * t;
            re += samples[i] * w.cos();
            im += samples[i] * w.sin();
        }
        2.0 * (re * re + im * im).sqrt() / n
    }

    #[test]
    fn even_adds_second_harmonic_odd_adds_third() {
        let run = |even: f32, odd: f32| -> (f32, f32) {
            let mut f = AuralEnhancerFilter::new(AuralParams {
                tune_hz: 1760.0,
                drive: 1.77,
                odd,
                even,
                wet: 1.0,
                dry: 0.0,
            });
            f.initialize(48000, &["L".into()]);
            let n = 9600usize;
            let mut samples = vec![vec![0.0f32; n]];
            for i in 0..n {
                samples[0][i] = 0.5
                    * (core::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin();
            }
            f.process(&mut samples, n);
            // 4800..9600 = 100 个整周期（1000 Hz @48k）。
            (
                spectral_amp(&samples[0], 4800, 2000.0, 48000),
                spectral_amp(&samples[0], 4800, 3000.0, 48000),
            )
        };

        let (even_2nd, even_3rd) = run(1.0, 0.0);
        assert!(
            even_2nd > 0.02,
            "even path should add 2nd harmonic, amp {even_2nd}"
        );
        assert!(
            even_3rd < even_2nd,
            "even path should be dominated by 2nd harmonic: {even_3rd} vs {even_2nd}"
        );

        let (odd_2nd, odd_3rd) = run(0.0, 1.0);
        assert!(
            odd_2nd < 0.005,
            "odd path should have no 2nd harmonic, amp {odd_2nd}"
        );
        assert!(
            odd_3rd > 0.01,
            "odd path should add 3rd harmonic, amp {odd_3rd}"
        );
    }

    #[test]
    fn init_and_process_across_sample_rates() {
        for sr in [44_100u32, 48_000, 96_000] {
            let mut f = AuralEnhancerFilter::new(AuralParams {
                tune_hz: 9000.0,
                drive: 4.25, // 参数上限（原 DRIVE_MAX，parse 死代码清理后内联）
                even: 0.75,  // 参数上限（原 EVEN_MAX）
                ..Default::default()
            });
            f.initialize(sr, &["L".into(), "R".into()]);
            assert!(f.hp_gain.is_finite() && f.hp_a1.is_finite() && f.hp_a0.is_finite());
            assert_eq!(f.states.len(), 2);
            let mut samples = vec![vec![0.0f32; 512], vec![0.0f32; 512]];
            for i in 0..512 {
                samples[0][i] =
                    (core::f32::consts::TAU * 500.0 * i as f32 / sr as f32).sin();
                samples[1][i] = samples[0][i] * 0.3;
            }
            f.process(&mut samples, 512);
            for ch in &samples {
                for &v in ch {
                    assert!(v.is_finite());
                }
            }
        }
    }

    #[test]
    fn coefficients_match_c_butterworth_design() {
        // 标准二阶 Butterworth 高通在 1760 Hz 下的参考值（与原 C 端设计一致）。
        let cases = [
            (44_100u32, 0.8382004, 1.650048, -0.7027535),
            (48_000u32, 0.8502137, 1.6778642, -0.72299066),
            (96_000u32, 0.9218543, 1.8375925, -0.84982467),
        ];
        for (sr, gain, a1, a0) in cases {
            let mut f = AuralEnhancerFilter::new(AuralParams::default());
            f.initialize(sr, &["L".into(), "R".into()]);
            assert!(
                (f.hp_gain - gain).abs() < 1e-6,
                "gain {}/{}",
                f.hp_gain,
                gain
            );
            assert!((f.hp_a1 - a1).abs() < 1e-6, "a1 {}/{}", f.hp_a1, a1);
            assert!((f.hp_a0 - a0).abs() < 1e-6, "a0 {}/{}", f.hp_a0, a0);
        }
    }

}
