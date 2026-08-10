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

//! Wide（立体声加宽器 / Surround）
//!
//! 移植自 FxSound `Wide32.c`（Theremino V2.0.3 简化环绕版，AGPL-3.0-or-later）。
//! 算法：M/S 分解——`mono = (L+R)·0.5`，侧信号 `L-mono` / `R-mono` 按
//! `1 + 3·Intensity` 放大，mono 按 `1 - 0.3·Intensity` 补偿衰减；
//! 总体音量不变、无滤波、无延迟。
//!
//! config 语法（EAPO 风格）：
//! `Wide: Intensity 0.354331`

use crate::pipeline::dsp::filter::{ConfigLoader, DspContext, Filter, FilterCreateResult, FilterFactory};

#[derive(Debug, Clone, Copy)]
pub struct WideParams {
    /// 环绕强度（对应 `DSP_WID_INTENSITY`，范围 [0, 1]）。
    pub intensity: f32,
}

impl Default for WideParams {
    fn default() -> Self {
        Self {
            // Wide32.c Starting Presets：Intensity 35 → 0.354331。
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

#[derive(Debug)]
pub struct WideFilter {
    params: WideParams,
    channel_indices: Vec<usize>,
}

impl WideFilter {
    pub fn new(params: WideParams) -> Self {
        Self {
            params,
            channel_indices: Vec::new(),
        }
    }
}

impl Filter for WideFilter {
    fn initialize(&mut self, _sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        if self.channel_indices.is_empty() {
            self.channel_indices = (0..channel_names.len()).collect();
        }
        None
    }

    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        if self.channel_indices.is_empty() {
            return;
        }
        // 与 C 端一致：Wide 是立体声 M/S 插件，只处理前两个选中通道；
        // 单声道时按 C 语义输出减半（`out *= 0.5`）。
        let stereo = self.channel_indices.len() >= 2;
        let l = self.channel_indices[0];
        let r = if stereo { self.channel_indices[1] } else { l };
        if l >= samples.len() || (stereo && r >= samples.len()) {
            return;
        }
        let frame_count = frame_count.min(samples[l].len());

        let intensity = self.params.intensity;
        let gain_side = 1.0 + 3.0 * intensity;
        let gain_comp = 1.0 - 0.3 * intensity;

        for f in 0..frame_count {
            let in1 = samples[l][f];
            let in2 = if stereo { samples[r][f] } else { 0.0 };

            let mono = (in1 + in2) * 0.5;
            let l_minus_mono = in1 - mono;
            let r_minus_mono = in2 - mono;

            let mono = mono * gain_comp;
            let mut out1 = mono + gain_side * l_minus_mono;
            let mut out2 = mono + gain_side * r_minus_mono;

            if !stereo {
                out1 *= 0.5;
                out2 *= 0.5;
            }

            samples[l][f] = if out1.is_finite() { out1 } else { 0.0 };
            if stereo {
                samples[r][f] = if out2.is_finite() { out2 } else { 0.0 };
            }
        }
    }

    fn set_channel_indices(&mut self, indices: &[usize]) {
        self.channel_indices = indices.to_vec();
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
    use crate::pipeline::dsp::fxsound::{test_ctx, test_loader};

    #[test]
    fn parse_valid_and_defaults() {
        let p = parse_wide_params("Intensity 0.7").unwrap();
        assert!((p.intensity - 0.7).abs() < 1e-6);
        let p = parse_wide_params("Surround 0.2").unwrap();
        assert!((p.intensity - 0.2).abs() < 1e-6);
        // 缺参 → None（与其余 fxsound 解析器一致）。
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
                assert!((x - y).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn side_signal_widens_and_mono_compensates() {
        let mut f = WideFilter::new(WideParams { intensity: 1.0 });
        f.initialize(48000, &["L".into(), "R".into()]);
        // 纯侧信号：L=+s, R=-s → mono=0，两侧放大 1+3=4 倍。
        let mut side = vec![vec![0.0f32; 64], vec![0.0f32; 64]];
        for i in 0..64 {
            side[0][i] = 0.25;
            side[1][i] = -0.25;
        }
        f.process(&mut side, 64);
        for i in 0..64 {
            assert!((side[0][i] - 1.0).abs() < 1e-5);
            assert!((side[1][i] + 1.0).abs() < 1e-5);
        }

        // 纯中央信号：L=R=0.5 → 侧=0，中央按 1-0.3=0.7 衰减。
        let mut center = vec![vec![0.5f32; 64], vec![0.5f32; 64]];
        f.process(&mut center, 64);
        for i in 0..64 {
            assert!((center[0][i] - 0.35).abs() < 1e-5);
            assert!((center[1][i] - 0.35).abs() < 1e-5);
        }
    }

    #[test]
    fn mono_semantics_match_c() {
        let mut f = WideFilter::new(WideParams { intensity: 1.0 });
        f.initialize(48000, &["Mono".into()]);
        let mut samples = vec![vec![0.8f32; 64]];
        f.process(&mut samples, 64);
        // mono：in2=0 → mono=0.4，侧=0.4，out=0.4*(1-0.3)+4*0.4=1.88，再 *0.5=0.94。
        assert!((samples[0][0] - 0.94).abs() < 1e-5);
    }

    #[test]
    fn finite_across_intensities() {
        for intensity in [0.0f32, 0.354331, 0.7, 1.0] {
            let mut f = WideFilter::new(WideParams { intensity });
            f.initialize(48000, &["L".into(), "R".into()]);
            let mut samples = vec![vec![0.0f32; 480], vec![0.0f32; 480]];
            for i in 0..480 {
                samples[0][i] =
                    (core::f32::consts::TAU * 440.0 * i as f32 / 48000.0).sin() * 0.9;
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

    #[test]
    fn factory_matches_named_command() {
        let factory = WideFactory;
        let result = factory.create_filter("Intensity 0.5", &test_ctx(), &test_loader());
        assert!(matches!(result, FilterCreateResult::Filter(_)));
        assert_eq!(factory.command_name(), "Wide");
    }
}
