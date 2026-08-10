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

//! Aural Enhancer（谐波激励器）
//!
//! 移植自 FxSound `Auralp.c`（AGPL-3.0-or-later）。
//! 算法：二阶 Butterworth 高通 → Drive 驱动 → `sin` 奇次谐波 +
//! 正半波偶次谐波 → 叠加干声 → Wet/Dry。
//!
//! config 语法（EAPO 风格）：
//! `AuralEnhancer: TuneHz 1760 Drive 1.77 Odd 1.5 Even 0.0 Wet 1.0 Dry 0.0`

use crate::pipeline::dsp::filter::{ConfigLoader, DspContext, Filter, FilterCreateResult, FilterFactory};

/// 默认 Aural Tune（对应原 Quick preset 1 / MIDI 53 映射，约 1.76 kHz）。
pub const DEFAULT_TUNE_HZ: f32 = 1760.0;
const DRIVE_MAX: f32 = 4.25;
const ODD_MAX: f32 = 1.5;
const EVEN_MAX: f32 = 0.75;

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

pub fn parse_aural_params(params: &str) -> Option<AuralParams> {
    let mut p = AuralParams::default();
    let mut matched = false;
    let tokens: Vec<&str> = params.split_whitespace().collect();
    let mut i = 0;
    while i < tokens.len() {
        let key = tokens[i].to_ascii_lowercase();
        let value = *tokens.get(i + 1)?;
        match key.as_str() {
            "tunehz" | "tune" => {
                p.tune_hz = value.parse().ok()?;
                i += 2;
                if tokens.get(i).is_some_and(|t| t.eq_ignore_ascii_case("hz")) {
                    i += 1;
                }
            }
            "drive" => {
                p.drive = value.parse().ok()?;
                i += 2;
            }
            "odd" => {
                p.odd = value.parse().ok()?;
                i += 2;
            }
            "even" => {
                p.even = value.parse().ok()?;
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
    p.tune_hz = p.tune_hz.clamp(500.0, 10_000.0);
    p.drive = p.drive.clamp(0.0, DRIVE_MAX);
    p.odd = p.odd.clamp(0.0, ODD_MAX);
    p.even = p.even.clamp(0.0, EVEN_MAX);
    p.wet = p.wet.clamp(0.0, 1.0);
    p.dry = p.dry.clamp(0.0, 1.0);
    Some(p)
}

#[derive(Debug, Clone, Copy)]
struct AuralChannelState {
    out1: f32,
    out2: f32,
    in1: f32,
    in2: f32,
}

impl Default for AuralChannelState {
    fn default() -> Self {
        Self {
            out1: 0.0,
            out2: 0.0,
            in1: 0.0,
            in2: 0.0,
        }
    }
}

#[derive(Debug)]
pub struct AuralEnhancerFilter {
    params: AuralParams,
    gain: f32,
    a1: f32,
    a0: f32,
    channel_indices: Vec<usize>,
    states: Vec<AuralChannelState>,
}

impl AuralEnhancerFilter {
    pub fn new(params: AuralParams) -> Self {
        Self {
            params,
            gain: 0.0,
            a1: 0.0,
            a0: 0.0,
            channel_indices: Vec::new(),
            states: Vec::new(),
        }
    }
}

impl Filter for AuralEnhancerFilter {
    fn initialize(&mut self, sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        if self.channel_indices.is_empty() {
            self.channel_indices = (0..channel_names.len()).collect();
        }
        let sr = sample_rate.max(1) as f32;
        let omega = core::f32::consts::TAU * self.params.tune_hz / sr;
        let omega2 = omega * omega;
        let two_root2_omega = 2.0 * core::f32::consts::SQRT_2 * omega;
        let tmp = 1.0 / (4.0 + omega2 + two_root2_omega);
        self.gain = 4.0 * tmp;
        self.a1 = (8.0 - 2.0 * omega2) * tmp;
        self.a0 = (two_root2_omega - 4.0 - omega2) * tmp;
        self.states = vec![AuralChannelState::default(); self.channel_indices.len().max(1)];
        None
    }

    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        let n = self.channel_indices.len().min(self.states.len());
        for k in 0..n {
            let slot = self.channel_indices[k];
            if slot >= samples.len() {
                continue;
            }
            let st = &mut self.states[k];
            for f in 0..frame_count.min(samples[slot].len()) {
                let input = samples[slot][f];
                let filt = st.out1 * self.a1
                    + st.out2 * self.a0
                    + (input + 1.0e-30 - 2.0 * st.in1 + st.in2) * self.gain;
                st.out2 = st.out1;
                st.out1 = filt;
                st.in2 = st.in1;
                st.in1 = input;

                let driven = filt * self.params.drive;
                let odd_harm = driven.sin();
                let even_harm = if driven > 0.0 { driven } else { 0.0 };
                let processed =
                    input + (self.params.even * even_harm + self.params.odd * odd_harm);
                let out = processed * self.params.wet + input * self.params.dry;
                samples[slot][f] = if out.is_finite() { out } else { 0.0 };
            }
        }
    }

    fn set_channel_indices(&mut self, indices: &[usize]) {
        self.channel_indices = indices.to_vec();
    }

    fn reset(&mut self) {
        for st in self.states.iter_mut() {
            *st = AuralChannelState::default();
        }
    }
}

#[derive(Debug)]
pub struct AuralEnhancerFactory;

impl FilterFactory for AuralEnhancerFactory {
    fn create_filter(
        &self,
        params: &str,
        _ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        match parse_aural_params(params) {
            Some(p) => FilterCreateResult::Filter(Box::new(AuralEnhancerFilter::new(p))),
            None => FilterCreateResult::NoMatch,
        }
    }

    fn command_name(&self) -> &str {
        "AuralEnhancer"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::dsp::fxsound::{test_ctx, test_loader};

    #[test]
    fn parse_defaults_and_overrides() {
        let p = parse_aural_params("Drive 2 Odd 0.5 Wet 0.3 Dry 0.7").unwrap();
        assert!((p.drive - 2.0).abs() < 1e-6);
        assert!((p.odd - 0.5).abs() < 1e-6);
        assert_eq!(p.even, 0.0);
        assert!((p.tune_hz - DEFAULT_TUNE_HZ).abs() < 1e-3);
        assert!((p.wet - 0.3).abs() < 1e-6);
    }

    #[test]
    fn parse_empty_is_none() {
        assert!(parse_aural_params("").is_none());
    }

    #[test]
    fn parse_unknown_key_is_none() {
        assert!(parse_aural_params("Bogus 1").is_none());
    }

    #[test]
    fn clamp_extremes() {
        let p = parse_aural_params("Drive 999 Odd 999 Even 999 Wet 2 Dry -1").unwrap();
        assert_eq!(p.drive, DRIVE_MAX);
        assert_eq!(p.odd, ODD_MAX);
        assert_eq!(p.even, EVEN_MAX);
        assert_eq!(p.wet, 1.0);
        assert_eq!(p.dry, 0.0);
    }

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
    fn init_and_process_across_sample_rates() {
        for sr in [44_100u32, 48_000, 96_000] {
            let mut f = AuralEnhancerFilter::new(AuralParams {
                tune_hz: 9000.0,
                drive: DRIVE_MAX,
                ..Default::default()
            });
            f.initialize(sr, &["L".into(), "R".into()]);
            assert!(f.gain.is_finite() && f.a1.is_finite() && f.a0.is_finite());
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
        // `filtDesign2ndButHighPass` 在 1760 Hz 下的参考值（C 端 double→float）。
        let cases = [
            (44_100u32, 0.8382004, 1.650048, -0.7027535),
            (48_000u32, 0.8502137, 1.6778642, -0.72299066),
            (96_000u32, 0.9218543, 1.8375925, -0.84982467),
        ];
        for (sr, gain, a1, a0) in cases {
            let mut f = AuralEnhancerFilter::new(AuralParams::default());
            f.initialize(sr, &["L".into(), "R".into()]);
            assert!(
                (f.gain - gain).abs() < 1e-6,
                "gain {}/{}",
                f.gain,
                gain
            );
            assert!((f.a1 - a1).abs() < 1e-6, "a1 {}/{}", f.a1, a1);
            assert!((f.a0 - a0).abs() < 1e-6, "a0 {}/{}", f.a0, a0);
        }
    }

    #[test]
    fn factory_matches_named_command() {
        let factory = AuralEnhancerFactory;
        let result = factory.create_filter("Drive 1.77", &test_ctx(), &test_loader());
        assert!(matches!(result, FilterCreateResult::Filter(_)));
        assert_eq!(factory.command_name(), "AuralEnhancer");
    }
}
