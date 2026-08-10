// FxSound
// Copyright (C) 2025  FxSound LLC
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! Maximizer（自动增益/限幅器）
//!
//! 移植自 FxSound `Maxi16.c`（AGPL-3.0-or-later）。算法：
//! - 0.1 Hz 单极点电平估计（仅以左声道/首通道输入平方驱动，与 C 一致）；
//! - lookahead 环形缓冲 + 包络 ramp/release 峰值限幅；
//! - `kernoise.h` 的 LCG 抖动（Uniform/Triangular/Shaped）与 16-bit 量化；
//! - 最终 Wet/Dry 混合。
//!
//! config 语法（EAPO 风格）：
//! `Maximizer: GainBoost 6 dB MaxOutput -0.3 dB Release 100 ms
//!  Target 0.32 Lookahead 0.75 ms Dither Shaped [Wet 1.0 Dry 0.0]`

use crate::pipeline::dsp::filter::{ConfigLoader, DspContext, Filter, FilterCreateResult, FilterFactory};

/// 抖动类型（对应 `KERNOISE_DITHER_*`）。
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
            // 原 Quick preset 1（44.1 kHz）：gain_boost=1.99526 → 6 dB；
            // max_output=0.966051 → -0.3 dB；release_time_beta=0.997776 → ≈10.18 ms。
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

/// `kernoise.h` 的 LCG：`seed = (3141592621 * seed + 2718282829) % 4294967291`。
/// 输出按 C 宏把无符号 seed 位模式重解释为有符号 long，再乘峰值系数。
#[derive(Debug, Clone, Copy)]
struct NoiseGen {
    seed: u64,
}

impl NoiseGen {
    fn new() -> Self {
        Self {
            seed: 10_322_234, // MAXIMIZE_NOISE_SEED
        }
    }

    fn next(&mut self) -> f32 {
        self.seed = (3_141_592_621u64 * self.seed + 2_718_282_829u64) % 4_294_967_291u64;
        (self.seed as u32 as i32) as f32 / 2_147_483_648.0
    }
}

/// `MAXI_ENVELOPE_BIAS`，避免包络下溢。
const ENVELOPE_BIAS: f32 = 1.0e-24;
/// 16-bit 量化峰值（`KERNOISE_PEAK_LEVEL_16`）。
const PEAK_LEVEL_16: f32 = 32_768.0;

#[derive(Debug)]
struct MaximizerChannelState {
    delay: Vec<f32>,
    pos: usize,
    max_abs: f32,
    delta: f32,
    env: f32,
    ramp_count: usize,
}

impl MaximizerChannelState {
    fn new(capacity: usize) -> Self {
        Self {
            delay: vec![0.0; capacity],
            pos: 0,
            max_abs: 0.0,
            delta: 0.0,
            env: 0.0,
            ramp_count: 0,
        }
    }
}

#[derive(Debug)]
pub struct MaximizerFilter {
    params: MaximizerParams,
    gain_boost: f32,
    max_output: f32,
    release_beta: f32,
    a0: f64,
    filt_gain: f64,
    level: f64,
    max_delay: usize,
    noise: NoiseGen,
    noise1_old: f32,
    noise2_old: f32,
    channel_indices: Vec<usize>,
    channels: Vec<MaximizerChannelState>,
}

impl MaximizerFilter {
    pub fn new(params: MaximizerParams) -> Self {
        Self {
            params,
            gain_boost: 0.0,
            max_output: 0.0,
            release_beta: 0.0,
            a0: 0.0,
            filt_gain: 0.0,
            level: 0.0,
            max_delay: 1,
            noise: NoiseGen::new(),
            noise1_old: 0.0,
            noise2_old: 0.0,
            channel_indices: Vec::new(),
            channels: Vec::new(),
        }
    }
}

impl Filter for MaximizerFilter {
    fn initialize(&mut self, sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        if self.channel_indices.is_empty() {
            self.channel_indices = (0..channel_names.len()).collect();
        }
        let sr = sample_rate.max(1) as f32;
        let sr_f64 = sample_rate.max(1) as f64;

        self.gain_boost = 10.0f32.powf(self.params.gain_boost_db / 20.0);
        self.max_output = 10.0f32.powf(self.params.max_output_db / 20.0);
        self.release_beta =
            (-1.0f32 / (self.params.release_ms * 0.001 * sr)).exp();

        // 0.1 Hz 单极点电平估计低通（`MAXIMIZE_LEVEL_FILT_CUTOFF`），double 设计。
        let r_omega = core::f64::consts::TAU * 0.1 / sr_f64;
        let cos_om = r_omega.cos();
        let root_calc = (cos_om * cos_om - 4.0 * cos_om + 3.0).sqrt();
        let d_tmp = 2.0 - cos_om - root_calc;
        self.a0 = d_tmp;
        self.filt_gain = 1.0 - d_tmp;
        self.level = 0.0;

        self.max_delay = (sr * 0.00075).trunc().max(1.0) as usize;
        // 预留上限与 C 端 MAXI_MAX_DELAY_LEN(96) 一致，环形长度仍用实际 max_delay。
        let capacity = self.max_delay.max(96);
        self.channels = self
            .channel_indices
            .iter()
            .map(|_| MaximizerChannelState::new(capacity))
            .collect();
        self.noise = NoiseGen::new();
        self.noise1_old = 0.0;
        self.noise2_old = 0.0;
        None
    }

    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        if self.channel_indices.is_empty() || self.channels.is_empty() {
            return;
        }
        let n = self.channel_indices.len().min(self.channels.len());
        let first = self.channel_indices[0];
        if first >= samples.len() {
            return;
        }
        let frame_count = frame_count.min(samples[first].len());
        let max_delay = self.max_delay;
        let d = self.params.dither;

        for f in 0..frame_count {
            // 电平估计：仅首通道输入平方驱动（与 C 的 in1 一致）。
            let in0 = samples[first][f];
            self.level = self.level * self.a0 + (in0 as f64) * (in0 as f64) * self.filt_gain;
            let sqrt_level = self.level.sqrt() as f32;
            let gain_boost = if self.gain_boost * sqrt_level > self.params.target {
                // 自动回退增益，最小 1.06（防音量抽吸）。
                (self.params.target / sqrt_level).max(1.06)
            } else {
                self.gain_boost
            };

            for k in 0..n {
                let slot = self.channel_indices[k];
                if slot >= samples.len() {
                    continue;
                }
                let input = samples[slot][f];
                let st = &mut self.channels[k];

                let dly_out = st.delay[st.pos];
                let new_val = gain_boost * self.max_output * input;
                st.delay[st.pos] = new_val;
                st.pos += 1;
                if st.pos >= max_delay {
                    st.pos = 0;
                }
                let new_abs = new_val.abs();

                // 包络更新：ramp 模式追赶新峰值，否则按 release 指数衰减。
                if st.ramp_count > 0 {
                    let abs_out = dly_out.abs();
                    if abs_out > st.env {
                        st.env = abs_out;
                    }
                    if new_abs > st.max_abs {
                        st.max_abs = new_abs;
                        st.ramp_count = max_delay;
                        let tmp_delta = (new_abs - st.env) / (max_delay + 1) as f32;
                        if tmp_delta > st.delta {
                            st.delta = tmp_delta;
                        }
                    } else {
                        st.ramp_count -= 1;
                    }
                    st.env += st.delta;
                } else {
                    st.env = st.env * self.release_beta + ENVELOPE_BIAS;
                    let abs_out = dly_out.abs();
                    if abs_out > st.env {
                        st.env = abs_out;
                    }
                    if new_abs > st.env {
                        st.max_abs = new_abs;
                        st.delta = (new_abs - st.env) / (max_delay + 1) as f32;
                        st.env += st.delta;
                        st.ramp_count = max_delay;
                    }
                }

                // 峰值归一化输出（lookahead 后的旧值）。
                let out = if st.env > self.max_output {
                    dly_out * self.max_output / st.env
                } else {
                    dly_out
                };

                let out = if d == DitherType::None {
                    out
                } else {
                    let dither = match d {
                        DitherType::Uniform => self.noise.next() * 0.5,
                        DitherType::Triangular => {
                            (self.noise.next() + self.noise.next()) * 0.5
                        }
                        DitherType::Shaped => {
                            let noise_tmp = self.noise.next() * 0.325;
                            let shaped = if k == 0 {
                                let v = noise_tmp - self.noise1_old;
                                self.noise1_old = noise_tmp;
                                v
                            } else if k == 1 {
                                let v = noise_tmp - self.noise2_old;
                                self.noise2_old = noise_tmp;
                                v
                            } else {
                                // 扩展通道：无历史状态，直接使用当前噪声。
                                noise_tmp
                            };
                            shaped
                        }
                        DitherType::None => unreachable!(),
                    };
                    let v = out * PEAK_LEVEL_16 + dither;
                    let q = if v >= 0.0 {
                        (v + 0.5) as f32
                    } else {
                        (v - 0.5) as f32
                    };
                    q / PEAK_LEVEL_16
                };

                let mixed = out * self.params.wet + input * self.params.dry;
                samples[slot][f] = if mixed.is_finite() { mixed } else { 0.0 };
            }
        }
    }

    fn set_channel_indices(&mut self, indices: &[usize]) {
        self.channel_indices = indices.to_vec();
    }

    fn reset(&mut self) {
        self.level = 0.0;
        self.noise = NoiseGen::new();
        self.noise1_old = 0.0;
        self.noise2_old = 0.0;
        for st in self.channels.iter_mut() {
            st.delay.fill(0.0);
            st.pos = 0;
            st.max_abs = 0.0;
            st.delta = 0.0;
            st.env = 0.0;
            st.ramp_count = 0;
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
