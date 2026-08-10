//! Maximizer（自动增益 + lookahead 峰值限幅器）
//!
//! v9.8 起为独立实现（原创 Rust 代码，不再移植自 FxSound `Maxi16.c`，
//! 已移除 AGPL 版权头）。算法结构：
//! - 自动增益：全声道单极点电平估计（约 250 ms），`Target` 控制增益回退起点
//!   （`GainBoost · rms > Target` 时有效增益降为 `max(Target/rms, 1.0)`）；
//! - lookahead 峰值限幅：环形延迟线 + 线性 attack 包络 + 多峰事件队列
//!   （参考 FFmpeg `alimiter` 的 attack/release 调度思想，独立实现），
//!   输出硬钳位到 `MaxOutput`；
//! - 抖动：独立 xorshift64* PRNG，Uniform / Triangular / Shaped 均为 16-bit 量化；
//! - 最终 Wet/Dry 混合。
//!
//! config 语法（EAPO 风格）：
//! `Maximizer: GainBoost 6 dB MaxOutput -0.3 dB Release 100 ms
//!  Target 0.32 Lookahead 0.75 ms Dither Shaped [Wet 1.0 Dry 0.0]`

use crate::pipeline::dsp::filter::{ConfigLoader, DspContext, Filter, FilterCreateResult, FilterFactory};

/// 抖动类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DitherType {
    None,
    Uniform,
    Triangular,
    Shaped,
}

#[derive(Debug, Clone, Copy)]
pub struct MaximizerParams {
    pub gain_boost_db: f32,
    pub max_output_db: f32,
    pub release_ms: f32,
    pub target: f32,
    pub lookahead_ms: f32,
    pub dither: DitherType,
    pub wet: f32,
    pub dry: f32,
}

impl Default for MaximizerParams {
    fn default() -> Self {
        Self {
            gain_boost_db: 6.0,
            max_output_db: -0.3,
            release_ms: 10.18,
            target: 0.32,
            lookahead_ms: 0.75,
            dither: DitherType::Shaped,
            wet: 1.0,
            dry: 0.0,
        }
    }
}

pub fn parse_maximizer_params(params: &str) -> Option<MaximizerParams> {
    let mut p = MaximizerParams::default();
    let mut matched = false;
    let tokens: Vec<&str> = params.split_whitespace().collect();
    let mut i = 0;
    while i < tokens.len() {
        let key = tokens[i].to_ascii_lowercase();
        let value = *tokens.get(i + 1)?;
        match key.as_str() {
            "gainboost" | "gain_boost" | "gain" => {
                p.gain_boost_db = value.parse().ok()?;
                i += 2;
                if tokens.get(i).is_some_and(|t| t.eq_ignore_ascii_case("db")) {
                    i += 1;
                }
            }
            "maxoutput" | "max_output" | "max" => {
                p.max_output_db = value.parse().ok()?;
                i += 2;
                if tokens.get(i).is_some_and(|t| t.eq_ignore_ascii_case("db")) {
                    i += 1;
                }
            }
            "release" | "release_time" | "releasems" => {
                p.release_ms = value.parse().ok()?;
                i += 2;
                if tokens.get(i).is_some_and(|t| t.eq_ignore_ascii_case("ms")) {
                    i += 1;
                }
            }
            "target" => {
                p.target = value.parse().ok()?;
                i += 2;
            }
            "lookahead" => {
                p.lookahead_ms = value.parse().ok()?;
                i += 2;
                if tokens.get(i).is_some_and(|t| t.eq_ignore_ascii_case("ms")) {
                    i += 1;
                }
            }
            "dither" => {
                p.dither = match value.to_ascii_lowercase().as_str() {
                    "none" | "off" => DitherType::None,
                    "uniform" => DitherType::Uniform,
                    "triangular" | "triang" | "triangle" => DitherType::Triangular,
                    "shaped" => DitherType::Shaped,
                    _ => return None,
                };
                i += 2;
            }
            "wet" => {
                p.wet = value.parse().ok()?;
                i += 2;
            }
            "dry" => {
                p.dry = value.parse().ok()?;
                i += 2;
            }
            _ => return None,
        }
        matched = true;
    }
    if !matched {
        return None;
    }
    p.gain_boost_db = p.gain_boost_db.clamp(0.0, 30.0);
    p.max_output_db = p.max_output_db.clamp(-30.0, 0.0);
    p.release_ms = p.release_ms.clamp(0.1, 100.0);
    p.target = p.target.clamp(0.01, 1.0);
    p.lookahead_ms = p.lookahead_ms.clamp(0.0, 10.0);
    p.wet = p.wet.clamp(0.0, 1.0);
    p.dry = p.dry.clamp(0.0, 1.0);
    Some(p)
}

/// 16-bit 量化峰值。
const PEAK_LEVEL_16: f32 = 32_768.0;
/// 自动增益电平估计时间常数。
const LEVEL_EST_TAU_S: f32 = 0.25;

/// 独立 xorshift64* PRNG（确定性种子，同参数可复现）。
#[derive(Debug, Clone, Copy)]
struct DitherRng {
    state: u64,
}

impl DitherRng {
    fn new() -> Self {
        Self {
            state: 0x9E37_79B9_7F4A_7C15,
        }
    }

    /// 均匀分布在 [-1, 1)。
    fn next_f32(&mut self) -> f32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        let top = (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32;
        top / 8_388_608.0 * 2.0 - 1.0
    }
}

/// 限幅事件：某个超限峰值样本到达输出位置时应达成的包络状态。
#[derive(Debug, Clone, Copy)]
struct LimiterEvent {
    /// 距触发还剩多少帧（0 = 本帧输出该峰值）。
    remaining: usize,
    /// 触发时包络应切换到的目标增益（limit/peak）。
    gain: f32,
    /// 触发后每帧的增益变化（到下一事件，或 release 回弹）。
    slope: f32,
}

#[derive(Debug)]
pub struct MaximizerFilter {
    params: MaximizerParams,
    gain_boost: f32,
    limit: f32,
    release_frames: f32,
    level_alpha: f32,
    level: f64,
    lookahead: usize,
    w: usize,
    att: f32,
    delta: f32,
    events: std::collections::VecDeque<LimiterEvent>,
    rng: DitherRng,
    shaped_prev: Vec<f32>,
    channel_indices: Vec<usize>,
    delay_lines: Vec<Vec<f32>>,
}

impl MaximizerFilter {
    pub fn new(params: MaximizerParams) -> Self {
        Self {
            params,
            gain_boost: 0.0,
            limit: 0.0,
            release_frames: 1.0,
            level_alpha: 0.0,
            level: 0.0,
            lookahead: 1,
            w: 0,
            att: 1.0,
            delta: 0.0,
            events: std::collections::VecDeque::new(),
            rng: DitherRng::new(),
            shaped_prev: Vec::new(),
            channel_indices: Vec::new(),
            delay_lines: Vec::new(),
        }
    }

    /// 调度限幅包络：新峰值在 `lookahead - 1` 帧后到达输出。
    ///
    /// 队列按触发先后排序（新峰值一定最后触发）。若新峰值要求的全程斜率比
    /// 当前包络更陡，整体替换为单一事件（保守：中间峰值只会过限、不会超限）；
    /// 否则在队列中找第一个「按现有斜率会在新峰值触发时超限」的段，收紧该段
    /// 斜率并把新事件追加到队尾。
    fn schedule(&mut self, gain: f32, release_slope: f32, lookahead: usize) {
        let remaining = lookahead - 1;
        let d_now = (gain - self.att) / lookahead as f32;

        if self.events.is_empty() {
            if d_now < self.delta {
                self.delta = d_now;
                self.events.push_back(LimiterEvent {
                    remaining,
                    gain,
                    slope: release_slope,
                });
            }
            return;
        }

        if d_now < self.delta {
            self.delta = d_now;
            self.events.clear();
            self.events.push_back(LimiterEvent {
                remaining,
                gain,
                slope: release_slope,
            });
            return;
        }

        for i in 0..self.events.len() {
            let ev = self.events[i];
            let dist = remaining.saturating_sub(ev.remaining);
            if dist == 0 {
                continue;
            }
            let pdelta = (gain - ev.gain) / dist as f32;
            if pdelta < ev.slope {
                self.events[i].slope = pdelta;
                if self.events.len() >= self.events.capacity() {
                    // 容量兜底：改走保守替换，保证新峰值仍被覆盖。
                    self.delta = self.delta.min(d_now);
                    self.events.clear();
                }
                self.events.push_back(LimiterEvent {
                    remaining,
                    gain,
                    slope: release_slope,
                });
                return;
            }
        }
    }

    /// 16-bit 量化 + 抖动（None 由调用方跳过）。
    fn quantize_dither(&mut self, value: f32, channel: usize, dither: DitherType) -> f32 {
        let q = 1.0 / PEAK_LEVEL_16;
        let noise = match dither {
            DitherType::Uniform => self.rng.next_f32() * 0.5 * q,
            DitherType::Triangular => (self.rng.next_f32() + self.rng.next_f32()) * 0.5 * q,
            DitherType::Shaped => {
                let n = self.rng.next_f32() * 0.5 * q;
                let prev = self.shaped_prev[channel];
                self.shaped_prev[channel] = n;
                n - prev
            }
            DitherType::None => 0.0,
        };
        let scaled = (value / q + noise).clamp(-PEAK_LEVEL_16, PEAK_LEVEL_16 - 1.0);
        scaled.round() * q
    }
}

impl Filter for MaximizerFilter {
    fn initialize(&mut self, sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        if self.channel_indices.is_empty() {
            self.channel_indices = (0..channel_names.len()).collect();
        }
        let sr = sample_rate.max(1) as f32;

        self.gain_boost = 10.0f32.powf(self.params.gain_boost_db / 20.0);
        self.limit = 10.0f32.powf(self.params.max_output_db / 20.0);
        self.release_frames = (self.params.release_ms * 0.001 * sr).max(1.0);
        self.level_alpha = (-1.0 / (LEVEL_EST_TAU_S * sr)).exp();
        self.level = 0.0;
        self.lookahead = ((sr * self.params.lookahead_ms * 0.001).round() as usize).max(1);

        let capacity = self.lookahead.min(128).max(1);
        self.delay_lines = self
            .channel_indices
            .iter()
            .map(|_| vec![0.0; self.lookahead])
            .collect();
        self.shaped_prev = vec![0.0; self.channel_indices.len()];
        self.w = 0;
        self.att = 1.0;
        self.delta = 0.0;
        self.events = std::collections::VecDeque::with_capacity(capacity);
        self.rng = DitherRng::new();
        None
    }

    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        if self.channel_indices.is_empty() || self.delay_lines.is_empty() {
            return;
        }
        let n = self.channel_indices.len().min(self.delay_lines.len());
        let first = self.channel_indices[0];
        if first >= samples.len() {
            return;
        }
        let frame_count = frame_count.min(samples[first].len());
        let lookahead = self.lookahead;
        let limit = self.limit;
        let release_frames = self.release_frames;
        let wet = self.params.wet;
        let dry = self.params.dry;
        let dither = self.params.dither;
        let alpha = self.level_alpha as f64;
        let gain_boost = self.gain_boost;
        let target = self.params.target;

        for f in 0..frame_count {
            // 1) 全声道 RMS 电平估计（单极点，约 250 ms）与自动增益。
            let mut sum_sq = 0.0f64;
            for k in 0..n {
                let slot = self.channel_indices[k];
                if slot < samples.len() {
                    let x = samples[slot][f] as f64;
                    sum_sq += x * x;
                }
            }
            self.level = self.level * alpha + sum_sq / n as f64 * (1.0 - alpha);
            let rms = self.level.sqrt() as f32;
            let boost = if gain_boost * rms > target {
                (target / rms).max(1.0)
            } else {
                gain_boost
            };

            // 2) 写入延迟线并检测输入峰值。
            let mut peak_in = 0.0f32;
            for k in 0..n {
                let slot = self.channel_indices[k];
                if slot >= samples.len() {
                    continue;
                }
                let v = samples[slot][f] * boost;
                self.delay_lines[k][self.w] = v;
                let a = v.abs();
                if a > peak_in {
                    peak_in = a;
                }
            }

            // 3) 超限则调度 attack 包络（该峰值 lookahead 帧后到达输出）。
            if peak_in > limit {
                let g = limit / peak_in;
                let release_slope = (1.0 - g) / release_frames;
                self.schedule(g, release_slope, lookahead);
            }

            // 4) 包络推进。
            self.att += self.delta;
            if self.att >= 1.0 {
                self.att = 1.0;
                self.delta = 0.0;
                self.events.clear();
            } else if self.att <= 1.0e-9 {
                self.att = 1.0e-9;
                self.delta = (1.0 - self.att) / release_frames;
            }

            // 5) 读取延迟线输出：包络 × 延迟样本 + 硬钳位 + 抖动/量化 + Wet/Dry。
            let rpos = (self.w + 1) % lookahead;
            for k in 0..n {
                let slot = self.channel_indices[k];
                if slot >= samples.len() {
                    continue;
                }
                let input = samples[slot][f];
                let dly = self.delay_lines[k][rpos];
                let mut out = (dly * self.att).clamp(-limit, limit);
                if dither != DitherType::None {
                    out = self.quantize_dither(out, k, dither);
                }
                let mixed = out * wet + input * dry;
                samples[slot][f] = if mixed.is_finite() { mixed } else { 0.0 };
            }

            // 6) 触发到期事件、事件倒计时、推进写头。
            loop {
                match self.events.front() {
                    Some(ev) if ev.remaining == 0 => {
                        let ev = self.events.pop_front().expect("front checked");
                        self.att = ev.gain;
                        self.delta = ev.slope;
                    }
                    _ => break,
                }
            }
            for ev in self.events.iter_mut() {
                ev.remaining -= 1;
            }
            self.w = (self.w + 1) % lookahead;
        }
    }

    fn set_channel_indices(&mut self, indices: &[usize]) {
        self.channel_indices = indices.to_vec();
    }

    fn reset(&mut self) {
        self.level = 0.0;
        self.w = 0;
        self.att = 1.0;
        self.delta = 0.0;
        self.events.clear();
        self.rng = DitherRng::new();
        self.shaped_prev.fill(0.0);
        for d in self.delay_lines.iter_mut() {
            d.fill(0.0);
        }
    }
}

#[derive(Debug)]
pub struct MaximizerFactory;

impl FilterFactory for MaximizerFactory {
    fn create_filter(
        &self,
        params: &str,
        _ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        match parse_maximizer_params(params) {
            Some(p) => FilterCreateResult::Filter(Box::new(MaximizerFilter::new(p))),
            None => FilterCreateResult::NoMatch,
        }
    }

    fn command_name(&self) -> &str {
        "Maximizer"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::dsp::fxsound::{test_ctx, test_loader};

    #[test]
    fn parse_defaults_and_overrides() {
        let p = parse_maximizer_params(
            "GainBoost 12 dB MaxOutput -1 dB Release 50 ms Dither Triangular Wet 0.5 Dry 0.5",
        )
        .unwrap();
        assert!((p.gain_boost_db - 12.0).abs() < 1e-6);
        assert!((p.max_output_db + 1.0).abs() < 1e-6);
        assert!((p.release_ms - 50.0).abs() < 1e-6);
        assert_eq!(p.dither, DitherType::Triangular);
        assert_eq!(p.target, 0.32);
        assert!((p.wet - 0.5).abs() < 1e-6);
        assert!((p.dry - 0.5).abs() < 1e-6);
    }

    #[test]
    fn parse_empty_is_none() {
        assert!(parse_maximizer_params("").is_none());
    }

    #[test]
    fn parse_unknown_key_is_none() {
        assert!(parse_maximizer_params("Bogus 1").is_none());
    }

    #[test]
    fn parse_invalid_dither_is_none() {
        assert!(parse_maximizer_params("Dither Pink").is_none());
    }

    #[test]
    fn clamp_extremes() {
        let p = parse_maximizer_params(
            "GainBoost 999 MaxOutput -999 Release 0 Target 99 Lookahead 99 Wet 2 Dry -1",
        )
        .unwrap();
        assert_eq!(p.gain_boost_db, 30.0);
        assert_eq!(p.max_output_db, -30.0);
        assert_eq!(p.release_ms, 0.1);
        assert_eq!(p.target, 1.0);
        assert_eq!(p.lookahead_ms, 10.0);
        assert_eq!(p.wet, 1.0);
        assert_eq!(p.dry, 0.0);
    }

    #[test]
    fn dry_only_is_passthrough() {
        let mut f = MaximizerFilter::new(MaximizerParams {
            wet: 0.0,
            dry: 1.0,
            dither: DitherType::None,
            ..Default::default()
        });
        f.initialize(48000, &["L".into(), "R".into()]);
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
        let mut f = MaximizerFilter::new(MaximizerParams::default());
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
    fn lookahead_delays_impulse_by_n_minus_one_frames() {
        // GainBoost 0 dB / MaxOutput 0 dB：无增益无限制，纯验证延迟线长度。
        let mut f = MaximizerFilter::new(MaximizerParams {
            gain_boost_db: 0.0,
            max_output_db: 0.0,
            release_ms: 10.0,
            target: 1.0,
            lookahead_ms: 1.0,
            dither: DitherType::None,
            wet: 1.0,
            dry: 0.0,
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let mut samples = vec![vec![0.0f32; 96], vec![0.0f32; 96]];
        samples[0][0] = 0.5;
        samples[1][0] = 0.3;
        f.process(&mut samples, 96);

        let n = (48000.0f32 * 0.001).round() as usize; // 48
        let expect = n - 1;
        let expected = [0.5f32, 0.3f32];
        for (ch, &want) in expected.iter().enumerate() {
            for (i, &v) in samples[ch].iter().enumerate() {
                if i == expect {
                    assert!(
                        (v - want).abs() < 1e-4,
                        "channel {ch}: expected impulse at {expect}, got {v}"
                    );
                } else {
                    assert!(v.abs() < 1e-4, "channel {ch}: unexpected value {v} at {i}");
                }
            }
        }
    }

    #[test]
    fn overshoot_impulse_clamped_at_limit() {
        let mut f = MaximizerFilter::new(MaximizerParams {
            gain_boost_db: 0.0,
            max_output_db: -6.0,
            release_ms: 10.0,
            target: 1.0,
            lookahead_ms: 1.0,
            dither: DitherType::None,
            wet: 1.0,
            dry: 0.0,
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let mut samples = vec![vec![0.0f32; 96], vec![0.0f32; 96]];
        samples[0][0] = 1.0;
        f.process(&mut samples, 96);

        let limit = 10.0f32.powf(-6.0 / 20.0);
        let n = 48usize;
        assert!(
            (samples[0][n - 1] - limit).abs() < 1e-4,
            "peak frame {} = {}, expected {}",
            n - 1,
            samples[0][n - 1],
            limit
        );
        for &v in &samples[0] {
            assert!(v.is_finite());
            assert!(v.abs() <= limit * 1.001 + 1e-6);
        }
    }

    #[test]
    fn envelope_recovers_after_release() {
        let mut f = MaximizerFilter::new(MaximizerParams {
            gain_boost_db: 0.0,
            max_output_db: -6.0,
            release_ms: 10.0,
            target: 1.0,
            lookahead_ms: 1.0,
            dither: DitherType::None,
            wet: 1.0,
            dry: 0.0,
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let mut samples = vec![vec![0.0f32; 1600], vec![0.0f32; 1600]];
        samples[0][0] = 1.0; // 超限脉冲触发 attack
        // release 10 ms = 480 帧；600 帧后包络应已回 1。
        samples[0][600] = 0.1;
        f.process(&mut samples, 1600);
        // 帧 600 的样本在延迟 N-1=47 帧后输出。
        let out_at = 600 + 47;
        assert!(
            (samples[0][out_at] - 0.1).abs() < 1e-3,
            "expected 0.1 after release, got {}",
            samples[0][out_at]
        );
    }

    #[test]
    fn quiet_input_gets_full_boost() {
        // GainBoost 12 dB（3.98×），电平远低于 Target → 保持满增益、不触发限幅。
        let mut f = MaximizerFilter::new(MaximizerParams {
            gain_boost_db: 12.0,
            max_output_db: -0.3,
            release_ms: 10.0,
            target: 0.32,
            lookahead_ms: 1.0,
            dither: DitherType::None,
            wet: 1.0,
            dry: 0.0,
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let mut samples = vec![vec![0.0f32; 4800], vec![0.0f32; 4800]];
        for i in 0..4800 {
            samples[0][i] = 0.01 * (core::f32::consts::TAU * 220.0 * i as f32 / 48000.0).sin();
        }
        f.process(&mut samples, 4800);
        let want = 0.01 * 10.0f32.powf(12.0 / 20.0);
        // 跳过延迟线预热（前 N-1 帧输出为 0），比较稳态峰值幅度。
        let peak = samples[0][100..].iter().fold(0.0f32, |m, &v| m.max(v.abs()));
        assert!(
            (peak - want).abs() < 0.005,
            "expected ≈{want}, got peak {peak}"
        );
        for &v in &samples[0][100..] {
            assert!(v.is_finite());
            assert!(v.abs() <= want * 1.05 + 1e-4);
        }
    }

    #[test]
    fn peak_never_exceeds_max_output() {
        let mut f = MaximizerFilter::new(MaximizerParams {
            gain_boost_db: 30.0,
            max_output_db: -6.0,
            release_ms: 100.0,
            dither: DitherType::None,
            ..Default::default()
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let mut samples = vec![vec![0.0f32; 4800], vec![0.0f32; 4800]];
        for i in 0..4800 {
            samples[0][i] = (core::f32::consts::TAU * 440.0 * i as f32 / 48000.0).sin();
            samples[1][i] = -samples[0][i];
        }
        f.process(&mut samples, 4800);
        let max_abs = 10.0f32.powf(-6.0 / 20.0) * 1.001;
        for ch in &samples {
            for &v in ch {
                assert!(v.is_finite());
                assert!(v.abs() <= max_abs, "peak {} exceeds {}", v.abs(), max_abs);
            }
        }
    }

    #[test]
    fn dither_is_finite_and_reproducible() {
        let run = |dither: DitherType| -> Vec<f32> {
            let mut f = MaximizerFilter::new(MaximizerParams {
                gain_boost_db: 6.0,
                max_output_db: -0.3,
                dither,
                ..Default::default()
            });
            f.initialize(48000, &["L".into(), "R".into()]);
            let mut samples = vec![vec![0.0f32; 512], vec![0.0f32; 512]];
            for i in 0..512 {
                samples[0][i] = (core::f32::consts::TAU * 220.0 * i as f32 / 48000.0).sin()
                    * 0.7;
                samples[1][i] = samples[0][i] * 0.4;
            }
            f.process(&mut samples, 512);
            samples.into_iter().flatten().collect()
        };

        let a1 = run(DitherType::Uniform);
        let a2 = run(DitherType::Uniform);
        assert_eq!(a1.len(), a2.len());
        for (x, y) in a1.iter().zip(a2.iter()) {
            assert!(x.is_finite());
            assert!((x - y).abs() < 1e-9, "same params must be reproducible");
        }

        let shaped = run(DitherType::Shaped);
        let none = run(DitherType::None);
        for v in &shaped {
            assert!(v.is_finite());
        }
        assert_ne!(shaped, none, "dither should change the output");
    }

    #[test]
    fn extreme_params_finite_across_sample_rates() {
        let params = MaximizerParams {
            gain_boost_db: 30.0,
            max_output_db: -30.0,
            release_ms: 100.0,
            target: 0.01,
            lookahead_ms: 10.0,
            dither: DitherType::Triangular,
            ..Default::default()
        };
        for sr in [44_100u32, 48_000, 96_000] {
            let mut f = MaximizerFilter::new(params);
            f.initialize(sr, &["L".into(), "R".into()]);
            let mut samples = vec![vec![0.0f32; 4800], vec![0.0f32; 4800]];
            for i in 0..4800 {
                samples[0][i] =
                    (core::f32::consts::TAU * 440.0 * i as f32 / sr as f32).sin() * 0.9;
                samples[1][i] = samples[0][i] * 0.4;
            }
            f.process(&mut samples, 4800);
            for ch in &samples {
                for &v in ch {
                    assert!(v.is_finite());
                    assert!(v.abs() <= 1.0005);
                }
            }
        }
    }

    #[test]
    fn mono_processes_first_channel_only() {
        let mut f = MaximizerFilter::new(MaximizerParams::default());
        f.initialize(48000, &["Mono".into()]);
        let mut samples = vec![vec![0.0f32; 4800]];
        for i in 0..4800 {
            samples[0][i] =
                (core::f32::consts::TAU * 220.0 * i as f32 / 48000.0).sin() * 0.8;
        }
        f.process(&mut samples, 4800);
        for &v in &samples[0] {
            assert!(v.is_finite());
            assert!(v.abs() <= 1.0005);
        }
    }

    #[test]
    fn factory_matches_named_command() {
        let factory = MaximizerFactory;
        let result = factory.create_filter(
            "GainBoost 6 dB Dither Shaped",
            &test_ctx(),
            &test_loader(),
        );
        assert!(matches!(result, FilterCreateResult::Filter(_)));
        assert_eq!(factory.command_name(), "Maximizer");
    }
}
