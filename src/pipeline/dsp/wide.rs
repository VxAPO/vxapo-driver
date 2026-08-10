//! Wide（立体声加宽器）
//!
//! 独立实现（原创 Rust 代码，不再移植自 FxSound `Wide32.c`，已移除 AGPL 版权头）。
//! 算法参考 DAFx24「An Open Source Stereo Widening Plugin」（O. Das）：
//! - 双频段：4 阶 Linkwitz-Riley 分频（500 Hz），低频段宽度只取高频段 25%，
//!   低频保持紧实、不破坏单声道兼容性；
//! - 去相关：每声道一条确定性 velvet-noise 稀疏脉冲卷积（25 ms 尾音、
//!   对数间隔分布、能量归一），两声道序列独立 → 降低声道间相干度；
//! - 混合：`out = cos(β)·dry + sin(β)·decorr`，β 随 Intensity 从 0 平滑到
//!   π/2（高频段）；总体电平基本不变，无侧信号放大带来的发硬/发刺。
//!
//! config 语法（EAPO 风格）：
//! `Wide: Intensity 0.354331`

use crate::pipeline::dsp::filter::{ConfigLoader, DspContext, Filter, FilterCreateResult, FilterFactory};

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

pub fn parse_wide_params(params: &str) -> Option<WideParams> {
    let mut p = WideParams::default();
    let mut matched = false;
    let tokens: Vec<&str> = params.split_whitespace().collect();
    let mut i = 0;
    while i < tokens.len() {
        let key = tokens[i].to_ascii_lowercase();
        let value = *tokens.get(i + 1)?;
        match key.as_str() {
            "intensity" | "surround" | "int" => {
                p.intensity = value.parse().ok()?;
                i += 2;
            }
            _ => return None,
        }
        matched = true;
    }
    if !matched {
        return None;
    }
    p.intensity = p.intensity.clamp(0.0, 1.0);
    Some(p)
}

/// 分频点（Hz）。
const CROSSOVER_HZ: f32 = 500.0;
/// 低频段宽度占高频段宽度的比例。
const LOW_BAND_RATIO: f32 = 0.25;

/// 确定性 xorshift64* PRNG（固定种子，同参数可复现）。
#[derive(Debug, Clone, Copy)]
struct WideRng {
    state: u64,
}

impl WideRng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// 均匀分布在 [0, 1)。
    fn next_f32(&mut self) -> f32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        let top = (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32;
        top / 8_388_608.0
    }
}

/// 二阶 Butterworth biquad（RBJ 系数，Q = 1/√2）。
#[derive(Debug, Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Biquad {
    fn butter_lowpass(fc_hz: f32, sr: f32) -> Self {
        let w0 = core::f32::consts::TAU * fc_hz / sr;
        let cosw = w0.cos();
        let alpha = w0.sin() / core::f32::consts::SQRT_2;
        let a0 = 1.0 + alpha;
        Self {
            b0: ((1.0 - cosw) * 0.5) / a0,
            b1: (1.0 - cosw) / a0,
            b2: ((1.0 - cosw) * 0.5) / a0,
            a1: (-2.0 * cosw) / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    fn butter_highpass(fc_hz: f32, sr: f32) -> Self {
        let w0 = core::f32::consts::TAU * fc_hz / sr;
        let cosw = w0.cos();
        let alpha = w0.sin() / core::f32::consts::SQRT_2;
        let a0 = 1.0 + alpha;
        Self {
            b0: ((1.0 + cosw) * 0.5) / a0,
            b1: -(1.0 + cosw) / a0,
            b2: ((1.0 + cosw) * 0.5) / a0,
            a1: (-2.0 * cosw) / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    fn process(&self, st: &mut BiquadState, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * st.x1 + self.b2 * st.x2 - self.a1 * st.y1 - self.a2 * st.y2;
        st.x2 = st.x1;
        st.x1 = x;
        st.y2 = st.y1;
        st.y1 = y;
        y
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct BiquadState {
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

/// 4 阶 Linkwitz-Riley 分频（两个二阶 Butterworth 级联，LP+HP 幅度互补）。
#[derive(Debug, Clone, Copy)]
struct Crossover {
    lp: [Biquad; 2],
    hp: [Biquad; 2],
}

impl Crossover {
    fn new(fc_hz: f32, sr: u32) -> Self {
        let sr = (sr as f32).max(1.0);
        let fc = fc_hz.min(sr * 0.4).max(1.0);
        Self {
            lp: [Biquad::butter_lowpass(fc, sr), Biquad::butter_lowpass(fc, sr)],
            hp: [Biquad::butter_highpass(fc, sr), Biquad::butter_highpass(fc, sr)],
        }
    }

    fn process(&self, st: &mut CrossoverState, x: f32) -> (f32, f32) {
        let lp1 = self.lp[0].process(&mut st.lp[0], x);
        let lp2 = self.lp[1].process(&mut st.lp[1], lp1);
        let hp1 = self.hp[0].process(&mut st.hp[0], x);
        let hp2 = self.hp[1].process(&mut st.hp[1], hp1);
        (lp2, hp2)
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct CrossoverState {
    lp: [BiquadState; 2],
    hp: [BiquadState; 2],
}

/// velvet-noise 去相关器：确定性稀疏脉冲卷积。
#[derive(Debug)]
struct VelvetNoise {
    taps: Vec<(usize, f32)>,
    delay: Vec<f32>,
    w: usize,
}

impl VelvetNoise {
    fn new(sr: u32, seed: u64) -> Self {
        let sr_f = sr.max(1) as f32;
        let len_ms = 25.0f32;
        let density_hz = 1500.0f32;
        let decay_db = 12.0f32;

        let len = ((sr_f * len_ms / 1000.0).round() as usize).max(2);
        let count = ((sr_f / density_hz) as usize).max(1);
        let count = count.min(len / 2);

        let mut rng = WideRng::new(seed);
        let max_pos = len - 1;
        let k = core::f32::consts::LN_10 * decay_db / 20.0;
        let mut taps = Vec::with_capacity(count);
        let mut energy = 0.0f32;

        for i in 0..count {
            let t = i as f32 / count as f32;
            let t_next = (i as f32 + 1.0) / count as f32;
            let lo = (max_pos as f32 * t.powf(1.6)) as usize;
            let hi = ((max_pos as f32 * t_next.powf(1.6)) as usize).min(max_pos);
            let span = hi.saturating_sub(lo).max(1);
            let pos = (lo + (rng.next_f32() * span as f32) as usize).clamp(1, max_pos);
            let sign = if rng.next_f32() < 0.5 { -1.0f32 } else { 1.0f32 };
            let amp = sign * (-k * i as f32 / count as f32).exp();
            energy += amp * amp;
            taps.push((pos, amp));
        }

        let norm = energy.sqrt().max(1.0e-9);
        for (_, g) in taps.iter_mut() {
            *g /= norm;
        }
        taps.sort_unstable_by_key(|(d, _)| *d);

        Self {
            taps,
            delay: vec![0.0; len],
            w: 0,
        }
    }

    fn process(&mut self, x: f32) -> f32 {
        self.delay[self.w] = x;
        let len = self.delay.len();
        let mut out = 0.0f32;
        for (d, g) in &self.taps {
            out += self.delay[(self.w + len - d) % len] * g;
        }
        self.w = (self.w + 1) % len;
        out
    }

    fn reset(&mut self) {
        self.delay.fill(0.0);
        self.w = 0;
    }
}

#[derive(Debug)]
pub struct WideFilter {
    params: WideParams,
    channel_indices: Vec<usize>,
    active: bool,
    cos_low: f32,
    sin_low: f32,
    cos_high: f32,
    sin_high: f32,
    crossover: Crossover,
    velvet: Vec<VelvetNoise>,
    dry_states: Vec<CrossoverState>,
    decorr_states: Vec<CrossoverState>,
}

impl WideFilter {
    pub fn new(params: WideParams) -> Self {
        Self {
            params,
            channel_indices: Vec::new(),
            active: false,
            cos_low: 1.0,
            sin_low: 0.0,
            cos_high: 1.0,
            sin_high: 0.0,
            crossover: Crossover::new(CROSSOVER_HZ, 48_000),
            velvet: Vec::new(),
            dry_states: Vec::new(),
            decorr_states: Vec::new(),
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
            self.velvet.clear();
            self.dry_states.clear();
            self.decorr_states.clear();
            return None;
        }

        let beta_high = self.params.intensity * core::f32::consts::FRAC_PI_2;
        let beta_low = beta_high * LOW_BAND_RATIO;
        self.cos_high = beta_high.cos();
        self.sin_high = beta_high.sin();
        self.cos_low = beta_low.cos();
        self.sin_low = beta_low.sin();

        self.crossover = Crossover::new(CROSSOVER_HZ, sample_rate);
        let base_seed = 0x8D3A_9C1F_6E2B_4075u64;
        self.velvet = (0..2)
            .map(|k| VelvetNoise::new(sample_rate, base_seed ^ (k as u64 * 0x9E37_79B9)))
            .collect();
        self.dry_states = vec![CrossoverState::default(); 2];
        self.decorr_states = vec![CrossoverState::default(); 2];
        None
    }

    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        if !self.active || self.channel_indices.len() < 2 || self.velvet.len() < 2 {
            return;
        }
        let l = self.channel_indices[0];
        let r = self.channel_indices[1];
        if l >= samples.len() || r >= samples.len() {
            return;
        }
        let frame_count = frame_count.min(samples[l].len()).min(samples[r].len());

        for f in 0..frame_count {
            let xl = samples[l][f];
            let xr = samples[r][f];

            let dl = self.velvet[0].process(xl);
            let dr = self.velvet[1].process(xr);

            let (ll, lh) = self.crossover.process(&mut self.dry_states[0], xl);
            let (dl_low, dl_high) = self.crossover.process(&mut self.decorr_states[0], dl);
            let (rl, rh) = self.crossover.process(&mut self.dry_states[1], xr);
            let (dr_low, dr_high) = self.crossover.process(&mut self.decorr_states[1], dr);

            let out_l = self.cos_low * ll + self.sin_low * dl_low
                + self.cos_high * lh + self.sin_high * dl_high;
            let out_r = self.cos_low * rl + self.sin_low * dr_low
                + self.cos_high * rh + self.sin_high * dr_high;

            samples[l][f] = if out_l.is_finite() { out_l } else { 0.0 };
            samples[r][f] = if out_r.is_finite() { out_r } else { 0.0 };
        }
    }

    fn set_channel_indices(&mut self, indices: &[usize]) {
        self.channel_indices = indices.to_vec();
    }

    fn reset(&mut self) {
        for v in self.velvet.iter_mut() {
            v.reset();
        }
        for st in self.dry_states.iter_mut() {
            *st = CrossoverState::default();
        }
        for st in self.decorr_states.iter_mut() {
            *st = CrossoverState::default();
        }
    }
}

#[derive(Debug)]
pub struct WideFactory;

impl FilterFactory for WideFactory {
    fn create_filter(
        &self,
        params: &str,
        _ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        match parse_wide_params(params) {
            Some(p) => FilterCreateResult::Filter(Box::new(WideFilter::new(p))),
            None => FilterCreateResult::NoMatch,
        }
    }

    fn command_name(&self) -> &str {
        "Wide"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::dsp::{test_ctx, test_loader};

    #[test]
    fn parse_valid_and_defaults() {
        let p = parse_wide_params("Intensity 0.7").unwrap();
        assert!((p.intensity - 0.7).abs() < 1e-6);
        let p = parse_wide_params("Surround 0.2").unwrap();
        assert!((p.intensity - 0.2).abs() < 1e-6);
        // 缺参 → None（与其余效果器解析器一致）。
        assert!(parse_wide_params("").is_none());
    }

    #[test]
    fn parse_unknown_key_is_none() {
        assert!(parse_wide_params("Bogus 1").is_none());
    }

    #[test]
    fn clamp_extremes() {
        let p = parse_wide_params("Intensity 999").unwrap();
        assert_eq!(p.intensity, 1.0);
        let p = parse_wide_params("Intensity -1").unwrap();
        assert_eq!(p.intensity, 0.0);
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

    #[test]
    fn stereo_decorrelation_breaks_symmetry() {
        // 1 kHz 中央信号：高频段全宽 → 两声道独立 velvet 序列使 L/R 不再相同，
        // 且总能量基本保持（cos²+sin²=1）。
        let mut f = WideFilter::new(WideParams { intensity: 1.0 });
        f.initialize(48000, &["L".into(), "R".into()]);
        let n = 4800usize;
        let mut samples = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = 0.5 * (core::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin();
            samples[0][i] = v;
            samples[1][i] = v;
        }
        f.process(&mut samples, n);

        let mut diff = 0.0f32;
        let mut in_rms = 0.0f32;
        let mut out_rms = 0.0f32;
        for i in 2000..n {
            diff = diff.max((samples[0][i] - samples[1][i]).abs());
            in_rms += samples[0][i] * samples[0][i];
            out_rms += samples[0][i] * samples[0][i] + samples[1][i] * samples[1][i];
        }
        let in_rms = (in_rms / (n - 2000) as f32).sqrt();
        let out_rms = (out_rms / (2 * (n - 2000)) as f32).sqrt();
        assert!(
            diff > 0.05,
            "decorrelation should break L/R symmetry, max diff {diff}"
        );
        assert!(
            out_rms / in_rms > 0.25 && out_rms / in_rms < 1.75,
            "level should stay sane (no implosion/explosion), ratio {}",
            out_rms / in_rms
        );
    }

    #[test]
    fn bass_band_is_widened_much_less() {
        // 60 Hz 中央信号与 1 kHz 中央信号：低频段宽度只有高频段 25%，
        // 低频 L/R 差异应显著小于高频。
        let run = |freq: f32, n: usize| -> f32 {
            let mut f = WideFilter::new(WideParams { intensity: 1.0 });
            f.initialize(48000, &["L".into(), "R".into()]);
            let mut samples = vec![vec![0.0f32; n], vec![0.0f32; n]];
            for i in 0..n {
                let v = 0.5 * (core::f32::consts::TAU * freq * i as f32 / 48000.0).sin();
                samples[0][i] = v;
                samples[1][i] = v;
            }
            f.process(&mut samples, n);
            let mut diff = 0.0f32;
            for i in (n / 2)..n {
                diff = diff.max((samples[0][i] - samples[1][i]).abs());
            }
            diff
        };

        let diff_low = run(60.0, 9600);
        let diff_high = run(1000.0, 4800);
        assert!(
            diff_high > 0.05 && diff_low < diff_high,
            "bass should stay tighter than treble: low {diff_low}, high {diff_high}"
        );
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

    #[test]
    fn factory_matches_named_command() {
        let factory = WideFactory;
        let result = factory.create_filter("Intensity 0.5", &test_ctx(), &test_loader());
        assert!(matches!(result, FilterCreateResult::Filter(_)));
        assert_eq!(factory.command_name(), "Wide");
    }
}
