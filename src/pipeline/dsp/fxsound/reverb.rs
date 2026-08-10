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

//! Lexicon 风格 Reverb
//!
//! 移植自 FxSound `Lex16.c`（AGPL-3.0-or-later）。
//! 结构：预延迟 + 四级 Lattice AllPass 输入扩散 + 调制延迟网络 +
//! 多抽头早反射/扩散输出。
//!
//! config 语法（EAPO 风格）：
//! `Reverb: RoomSize 1.0 Decay 0.566 Damping 0.408 Bandwidth 0.350
//!  Density 1.0 Lat5 0.70 Lat6 0.50 PreDelay 0 ms MotionRate 0.11
//!  MotionDepth 0.63 ms Wet 0.3 Dry 0.9`

use crate::pipeline::dsp::filter::{ConfigLoader, DspContext, Filter, FilterCreateResult, FilterFactory};

pub const LEX_NUM_OSC_PTS: usize = 8193;

#[derive(Debug, Clone, Copy)]
pub struct ReverbParams {
    pub room_size: f32,
    pub decay: f32,
    pub damping: f32,
    pub bandwidth: f32,
    pub density: f32,
    pub lat5: f32,
    pub lat6: f32,
    pub pre_delay_ms: f32,
    pub motion_rate: f32,
    pub motion_depth_ms: f32,
    pub wet: f32,
    pub dry: f32,
}

impl Default for ReverbParams {
    fn default() -> Self {
        Self {
            room_size: 1.0,
            decay: 0.565664,
            damping: 0.408290,
            bandwidth: 0.350110,
            density: 1.0,
            lat5: 0.70,
            lat6: 0.50,
            pre_delay_ms: 0.0,
            motion_rate: 0.110871,
            motion_depth_ms: 0.63,
            wet: 0.3,
            dry: 0.9,
        }
    }
}

pub fn parse_reverb_params(params: &str) -> Option<ReverbParams> {
    let mut p = ReverbParams::default();
    let mut matched = false;
    let tokens: Vec<&str> = params.split_whitespace().collect();
    let mut i = 0;
    while i < tokens.len() {
        let key = tokens[i].to_ascii_lowercase();
        let value = *tokens.get(i + 1)?;
        match key.as_str() {
            "roomsize" | "room" | "size" => {
                p.room_size = value.parse().ok()?;
                i += 2;
            }
            "decay" => {
                p.decay = value.parse().ok()?;
                i += 2;
            }
            "damping" => {
                p.damping = value.parse().ok()?;
                i += 2;
            }
            "bandwidth" | "rolloff" => {
                p.bandwidth = value.parse().ok()?;
                i += 2;
            }
            "density" => {
                p.density = value.parse().ok()?;
                i += 2;
            }
            "lat5" => {
                p.lat5 = value.parse().ok()?;
                i += 2;
            }
            "lat6" => {
                p.lat6 = value.parse().ok()?;
                i += 2;
            }
            "predelay" | "pre_delay" => {
                p.pre_delay_ms = value.parse().ok()?;
                i += 2;
                if tokens.get(i).is_some_and(|t| t.eq_ignore_ascii_case("ms")) {
                    i += 1;
                }
            }
            "motionrate" | "motion_rate" => {
                p.motion_rate = value.parse().ok()?;
                i += 2;
            }
            "motiondepth" | "motion_depth" => {
                p.motion_depth_ms = value.parse().ok()?;
                i += 2;
                if tokens.get(i).is_some_and(|t| t.eq_ignore_ascii_case("ms")) {
                    i += 1;
                }
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
    p.room_size = p.room_size.clamp(0.5, 1.5);
    p.decay = p.decay.clamp(0.0, 1.0);
    p.damping = p.damping.clamp(0.0, 1.0);
    p.bandwidth = p.bandwidth.clamp(0.0, 1.0);
    p.density = p.density.clamp(0.0, 1.0);
    p.lat5 = p.lat5.clamp(0.0, 1.0);
    p.lat6 = p.lat6.clamp(0.0, 1.0);
    p.pre_delay_ms = p.pre_delay_ms.clamp(0.0, 100.0);
    p.motion_rate = p.motion_rate.clamp(0.05, 2.0);
    p.motion_depth_ms = p.motion_depth_ms.clamp(0.0, 2.0);
    p.wet = p.wet.clamp(0.0, 1.0);
    p.dry = p.dry.clamp(0.0, 1.0);
    Some(p)
}

#[derive(Debug)]
struct DelayLine {
    buf: Vec<f32>,
    pos: usize,
}

impl DelayLine {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(1)],
            pos: 0,
        }
    }

    fn advance(&mut self, n: usize) {
        if !self.buf.is_empty() {
            self.pos = (self.pos + n) % self.buf.len();
        }
    }

    fn read(&self, delay: usize) -> f32 {
        let len = self.buf.len();
        if len == 0 {
            return 0.0;
        }
        let d = delay % len;
        let idx = (self.pos + len - d) % len;
        self.buf[idx]
    }

    fn read_and_write(&mut self, value: f32) -> f32 {
        let old = self.buf[self.pos];
        self.buf[self.pos] = value;
        old
    }

    fn write(&mut self, value: f32) {
        self.buf[self.pos] = value;
    }
}

fn ms_to_samples(ms: f32, sr: f32) -> usize {
    // C 端 `(unsigned long)(r_samp_freq * secs)` 为截断转换，这里保持同语义。
    (ms / 1000.0 * sr).trunc().max(0.0) as usize
}

#[derive(Debug)]
pub struct ReverbFilter {
    params: ReverbParams,
    delay: DelayLine,
    osc_mult: Vec<f32>,
    osc_plus: Vec<f32>,
    osc_table_p: f32,

    pre_delay_samples: usize,
    pre_dly_len: usize,
    lat1_len: usize,
    lat2_len: usize,
    lat3_len: usize,
    lat4_len: usize,
    lat5_nominal: f32,
    lat5_maxlen: usize,
    d1_tap1: usize,
    d1_tap2: usize,
    d1_tap3: usize,
    d1_tap4: usize,
    lat6_tap1: usize,
    lat6_tap2: usize,
    lat6_len: usize,
    d2_tap1: usize,
    d2_tap2: usize,
    d2_tap3: usize,
    lat7_nominal: f32,
    lat7_maxlen: usize,
    d3_tap1: usize,
    d3_tap2: usize,
    d3_tap3: usize,
    d3_tap4: usize,
    lat8_tap1: usize,
    lat8_tap2: usize,
    lat8_len: usize,
    d4_tap1: usize,
    d4_tap2: usize,
    d4_tap3: usize,

    lat1_coeff: f32,
    lat3_coeff: f32,
    lat5_coeff: f32,
    lat6_coeff: f32,
    one_minus_bandwidth: f32,
    one_minus_damping: f32,
    modulation_freq: f32,
    modulation_depth: f32,

    old_damp1: f32,
    old_damp2: f32,
    old_bandwidth: f32,
    d4_out: f32,
    channel_indices: Vec<usize>,
}

impl ReverbFilter {
    pub fn new(params: ReverbParams) -> Self {
        Self {
            params,
            delay: DelayLine::new(1),
            osc_mult: Vec::new(),
            osc_plus: Vec::new(),
            osc_table_p: 0.0,
            pre_delay_samples: 0,
            pre_dly_len: 0,
            lat1_len: 0,
            lat2_len: 0,
            lat3_len: 0,
            lat4_len: 0,
            lat5_nominal: 0.0,
            lat5_maxlen: 0,
            d1_tap1: 0,
            d1_tap2: 0,
            d1_tap3: 0,
            d1_tap4: 0,
            lat6_tap1: 0,
            lat6_tap2: 0,
            lat6_len: 0,
            d2_tap1: 0,
            d2_tap2: 0,
            d2_tap3: 0,
            lat7_nominal: 0.0,
            lat7_maxlen: 0,
            d3_tap1: 0,
            d3_tap2: 0,
            d3_tap3: 0,
            d3_tap4: 0,
            lat8_tap1: 0,
            lat8_tap2: 0,
            lat8_len: 0,
            d4_tap1: 0,
            d4_tap2: 0,
            d4_tap3: 0,
            lat1_coeff: 0.0,
            lat3_coeff: 0.0,
            lat5_coeff: 0.0,
            lat6_coeff: 0.0,
            one_minus_bandwidth: 0.0,
            one_minus_damping: 0.0,
            modulation_freq: 0.0,
            modulation_depth: 0.0,
            old_damp1: 0.0,
            old_damp2: 0.0,
            old_bandwidth: 0.0,
            d4_out: 0.0,
            channel_indices: Vec::new(),
        }
    }
}

impl Filter for ReverbFilter {
    fn initialize(&mut self, sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        if self.channel_indices.is_empty() {
            self.channel_indices = (0..channel_names.len()).collect();
        }
        let sr = sample_rate.max(1) as usize;
        let srf = sr as f32;
        let p = self.params;

        self.lat1_coeff = 0.3 + 0.45 * p.density;
        self.lat3_coeff = 0.25 + 0.375 * p.density;
        self.lat5_coeff = p.lat5;
        self.lat6_coeff = p.lat6;
        self.one_minus_bandwidth = 1.0 - p.bandwidth;
        self.one_minus_damping = 1.0 - p.damping;

        self.pre_dly_len = ms_to_samples(100.0, srf);
        self.pre_delay_samples = ms_to_samples(p.pre_delay_ms, srf);
        self.lat1_len = ms_to_samples(4.77, srf);
        self.lat2_len = ms_to_samples(3.595, srf);
        self.lat3_len = ms_to_samples(12.73, srf);
        self.lat4_len = ms_to_samples(9.31, srf);

        let motion_max = ms_to_samples(2.0, srf);
        self.modulation_depth = ms_to_samples(p.motion_depth_ms, srf) as f32;
        self.modulation_freq = p.motion_rate;

        // 当前房间尺寸下的标称/最大段长。
        // C 端先把标称延迟截断成整数再参与调制（`(float)((long)(...))`）。
        self.lat5_nominal = (srf * 0.0226 * p.room_size).trunc();
        self.lat5_maxlen = self.lat5_nominal as usize + motion_max + 1;
        self.lat7_nominal = (srf * 0.0305 * p.room_size).trunc();
        self.lat7_maxlen = self.lat7_nominal as usize + motion_max + 1;

        let tap = |secs: f32, room: f32| -> usize { (srf * secs * room).trunc() as usize };
        self.d1_tap1 = tap(0.0101, p.room_size);
        self.d1_tap2 = tap(0.0669, p.room_size);
        self.d1_tap3 = tap(0.1219, p.room_size);
        self.d1_tap4 = tap(0.1496, p.room_size);
        self.lat6_tap1 = tap(0.00628, p.room_size);
        self.lat6_tap2 = tap(0.04126, p.room_size);
        // C 端 lat6/lat8 段长随房间尺寸缩放（`LAT6_LEFT_DELAY_LEN * r_roomsize`）。
        self.lat6_len = tap(0.0605, p.room_size);
        self.d2_tap1 = tap(0.0358, p.room_size);
        self.d2_tap2 = tap(0.0898, p.room_size);
        self.d2_tap3 = tap(0.1250, p.room_size);
        self.d3_tap1 = tap(0.0101, p.room_size);
        self.d3_tap2 = tap(0.0709, p.room_size);
        self.d3_tap3 = tap(0.0999, p.room_size);
        self.d3_tap4 = tap(0.1417, p.room_size);
        self.lat8_tap1 = tap(0.01125, p.room_size);
        self.lat8_tap2 = tap(0.0643, p.room_size);
        self.lat8_len = tap(0.0892, p.room_size);
        self.d4_tap1 = tap(0.004065, p.room_size);
        self.d4_tap2 = tap(0.0671, p.room_size);
        self.d4_tap3 = tap(0.1063, p.room_size);

        // 分配缓冲：长度必须与 C 端 MasterLen 完全一致（按实际房间尺寸），
        // 保证“每次迭代整条主延迟净推进 1 个采样”的对齐语义；最大参数时即为上限。
        let master_len = self.pre_dly_len
            + self.lat1_len
            + self.lat2_len
            + self.lat3_len
            + self.lat4_len
            + self.lat5_maxlen
            + self.d1_tap4
            + self.lat6_len
            + self.d2_tap3
            + self.lat7_maxlen
            + self.d3_tap4
            + self.lat8_len
            + self.d4_tap3;
        self.delay = DelayLine::new(master_len);

        // 振荡器插值表（重复端点处理取整误差）。
        self.osc_mult = vec![0.0; LEX_NUM_OSC_PTS];
        self.osc_plus = vec![0.0; LEX_NUM_OSC_PTS];
        for i in 0..LEX_NUM_OSC_PTS - 1 {
            let x1 = i as f32;
            let x2 = (i + 1) as f32;
            let y1 = (core::f32::consts::TAU * x1 / (LEX_NUM_OSC_PTS as f32 - 1.0)).cos();
            let y2 = (core::f32::consts::TAU * x2 / (LEX_NUM_OSC_PTS as f32 - 1.0)).cos();
            self.osc_mult[i] = y1 - y2;
            self.osc_plus[i] = x1 * y2 - x2 * y1;
        }
        self.osc_mult[LEX_NUM_OSC_PTS - 1] = self.osc_mult[0];
        self.osc_plus[LEX_NUM_OSC_PTS - 1] = self.osc_plus[0];
        self.osc_table_p = 0.0;

        self.old_damp1 = 0.0;
        self.old_damp2 = 0.0;
        self.old_bandwidth = 0.0;
        self.d4_out = 0.0;
        None
    }

    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        if self.channel_indices.is_empty() {
            return;
        }
        let stereo = self.channel_indices.len() >= 2;
        let l = self.channel_indices[0];
        let r = if stereo { self.channel_indices[1] } else { l };
        if l >= samples.len() || (stereo && r >= samples.len()) {
            return;
        }
        let p = self.params;
        let frame_count = frame_count.min(samples[l].len());

        for f in 0..frame_count {
            let in1 = samples[l][f] + 1.0e-30;
            let in2 = if stereo { samples[r][f] + 1.0e-30 } else { 0.0 };
            let mix = in1 + in2;

            // 预延迟。
            self.delay.advance(self.pre_dly_len + 1);
            let mut tmp_b = self.delay.read(self.pre_delay_samples);
            let mut next_out = self.delay.read_and_write(mix);

            // 带宽单极点低通。
            tmp_b = tmp_b * self.one_minus_bandwidth + p.bandwidth * self.old_bandwidth;
            self.old_bandwidth = tmp_b;

            // 四级输入扩散。
            let (out, next) =
                self.lattice(tmp_b, self.lat1_coeff, self.lat1_len, next_out);
            tmp_b = out;
            next_out = next;
            let (out, next) =
                self.lattice(tmp_b, self.lat1_coeff, self.lat2_len, next_out);
            tmp_b = out;
            next_out = next;
            let (out, next) =
                self.lattice(tmp_b, self.lat3_coeff, self.lat3_len, next_out);
            tmp_b = out;
            next_out = next;
            let (input_diffuser_out, _next) =
                self.lattice(tmp_b, self.lat3_coeff, self.lat4_len, next_out);

            // 振荡器。
            self.osc_table_p += self.modulation_freq;
            if self.osc_table_p >= 8192.0 {
                self.osc_table_p -= 8192.0;
            }
            let idx = self.osc_table_p as usize;
            let osc = self.osc_table_p * self.osc_mult[idx] + self.osc_plus[idx];

            // Lattice 5（调制延迟）。
            tmp_b = input_diffuser_out + p.decay * self.d4_out;
            let delay_real = self.lat5_nominal + osc * self.modulation_depth;
            self.delay.advance(self.lat5_maxlen);
            let dl_out =
                self.decay_diffuser_read(tmp_b, self.lat5_coeff, delay_real);
            let dl_in = tmp_b + self.lat5_coeff * dl_out;
            next_out = self.delay.read_and_write(dl_in);
            let mut tmp_a = dl_out - self.lat5_coeff * dl_in;

            // D1 四抽头。
            self.delay.advance(self.d1_tap4);
            let tap4_out = next_out;
            next_out = self.delay.read_and_write(tmp_a);
            let tap1_out = self.delay.read(self.d1_tap1);
            let tap2_out = self.delay.read(self.d1_tap2);
            let tap3_out = self.delay.read(self.d1_tap3);
            let mut out1 = -tap2_out;
            let mut out2 = tap1_out + tap3_out;

            // 阻尼单极点。
            tmp_a = tap4_out * self.one_minus_damping + p.damping * self.old_damp1;
            self.old_damp1 = tmp_a;
            tmp_a *= p.decay;

            // Lattice 6（带抽头）。
            self.delay.advance(self.lat6_len);
            let dl_out = next_out;
            let dl_in = tmp_a - self.lat6_coeff * dl_out;
            next_out = self.delay.read_and_write(dl_in);
            tmp_b = dl_out + self.lat6_coeff * dl_in;
            let tap1_out = self.delay.read(self.lat6_tap1);
            let tap2_out = self.delay.read(self.lat6_tap2);
            out1 -= tap1_out;
            out2 -= tap2_out;

            // D2 三抽头。
            self.delay.advance(self.d2_tap3);
            let tap3_out = next_out;
            self.delay.write(tmp_b);
            let tap1_out = self.delay.read(self.d2_tap1);
            let tap2_out = self.delay.read(self.d2_tap2);
            out1 -= tap1_out;
            out2 += tap2_out;

            // Lattice 7（调制延迟，深度取反）。
            tmp_b = input_diffuser_out + p.decay * tap3_out;
            let delay_real = self.lat7_nominal - osc * self.modulation_depth;
            self.delay.advance(self.lat7_maxlen);
            let dl_out =
                self.decay_diffuser_read(tmp_b, self.lat5_coeff, delay_real);
            let dl_in = tmp_b + self.lat5_coeff * dl_out;
            next_out = self.delay.read_and_write(dl_in);
            tmp_a = dl_out - self.lat5_coeff * dl_in;

            // D3 四抽头。
            self.delay.advance(self.d3_tap4);
            let tap4_out = next_out;
            next_out = self.delay.read_and_write(tmp_a);
            let tap1_out = self.delay.read(self.d3_tap1);
            let tap2_out = self.delay.read(self.d3_tap2);
            let tap3_out = self.delay.read(self.d3_tap3);
            out1 += tap1_out + tap3_out;
            out2 -= tap2_out;

            // 阻尼单极点 2。
            tmp_a = tap4_out * self.one_minus_damping + p.damping * self.old_damp2;
            self.old_damp2 = tmp_a;
            tmp_a *= p.decay;

            // Lattice 8（带抽头）。
            self.delay.advance(self.lat8_len);
            let dl_out = next_out;
            let dl_in = tmp_a - self.lat6_coeff * dl_out;
            next_out = self.delay.read_and_write(dl_in);
            tmp_b = dl_out + self.lat6_coeff * dl_in;
            let tap1_out = self.delay.read(self.lat8_tap1);
            let tap2_out = self.delay.read(self.lat8_tap2);
            out1 -= tap2_out;
            out2 -= tap1_out;

            // D4 三抽头。
            self.delay.advance(self.d4_tap3);
            let tap3_out = next_out;
            self.delay.write(tmp_b);
            let tap1_out = self.delay.read(self.d4_tap1);
            let tap2_out = self.delay.read(self.d4_tap2);
            out1 += tap2_out;
            out2 -= tap1_out;
            self.d4_out = tap3_out;

            out1 *= 0.3;
            out2 *= 0.3;
            if !stereo {
                out1 *= 0.5;
                out2 *= 0.5;
            }

            let out_l = p.wet * out1 + p.dry * in1;
            samples[l][f] = if out_l.is_finite() { out_l } else { 0.0 };
            if stereo {
                let out_r = p.wet * out2 + p.dry * in2;
                samples[r][f] = if out_r.is_finite() { out_r } else { 0.0 };
            }
        }
    }

    fn set_channel_indices(&mut self, indices: &[usize]) {
        self.channel_indices = indices.to_vec();
    }

    fn reset(&mut self) {
        self.delay = DelayLine::new(self.delay.buf.len());
        self.osc_table_p = 0.0;
        self.old_damp1 = 0.0;
        self.old_damp2 = 0.0;
        self.old_bandwidth = 0.0;
        self.d4_out = 0.0;
    }
}

impl ReverbFilter {
    fn lattice(&mut self, input: f32, coeff: f32, len: usize, next_out: f32) -> (f32, f32) {
        self.delay.advance(len);
        let d_out = next_out;
        let d_in = input - coeff * d_out;
        let next = self.delay.read_and_write(d_in);
        (d_out + coeff * d_in, next)
    }

    fn decay_diffuser_read(&mut self, _input: f32, _coeff: f32, delay_real: f32) -> f32 {
        let idly = delay_real as usize;
        let del = delay_real - idly as f32;
        let y1 = self.delay.read(idly);
        let y2 = self.delay.read(idly + 1);
        y1 + (y2 - y1) * del
    }
}

#[derive(Debug)]
pub struct ReverbFactory;

impl FilterFactory for ReverbFactory {
    fn create_filter(
        &self,
        params: &str,
        _ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        match parse_reverb_params(params) {
            Some(p) => FilterCreateResult::Filter(Box::new(ReverbFilter::new(p))),
            None => FilterCreateResult::NoMatch,
        }
    }

    fn command_name(&self) -> &str {
        "Reverb"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::dsp::fxsound::{test_ctx, test_loader};

    #[test]
    fn parse_valid() {
        let p = parse_reverb_params(
            "RoomSize 1.2 Decay 0.5 Damping 0.4 Bandwidth 0.3 PreDelay 20 ms MotionRate 0.2 MotionDepth 1 ms Wet 0.4 Dry 0.8",
        )
        .unwrap();
        assert!((p.room_size - 1.2).abs() < 1e-6);
        assert!((p.pre_delay_ms - 20.0).abs() < 1e-6);
        assert!((p.wet - 0.4).abs() < 1e-6);
    }

    #[test]
    fn parse_empty_is_none() {
        assert!(parse_reverb_params("").is_none());
    }

    #[test]
    fn parse_unknown_is_none() {
        assert!(parse_reverb_params("Bogus 1").is_none());
    }

    #[test]
    fn clamps_extremes() {
        let p = parse_reverb_params("RoomSize 99 Decay 99 MotionDepth 99").unwrap();
        assert_eq!(p.room_size, 1.5);
        assert_eq!(p.decay, 1.0);
        assert_eq!(p.motion_depth_ms, 2.0);
    }

    #[test]
    fn silence_stays_silent() {
        let mut f = ReverbFilter::new(ReverbParams::default());
        f.initialize(48000, &["L".into(), "R".into()]);
        let mut samples = vec![vec![0.0f32; 480]; 2];
        f.process(&mut samples, 480);
        for ch in &samples {
            for &v in ch {
                assert!(v.abs() < 1e-9);
            }
        }
    }

    #[test]
    fn non_silent_is_finite() {
        let mut f = ReverbFilter::new(ReverbParams::default());
        f.initialize(48000, &["L".into(), "R".into()]);
        let mut samples = vec![vec![0.0f32; 4800]; 2];
        for i in 0..4800 {
            samples[0][i] = (core::f32::consts::TAU * 440.0 * i as f32 / 48000.0).sin() * 0.5;
            samples[1][i] = samples[0][i] * 0.5;
        }
        f.process(&mut samples, 4800);
        for ch in &samples {
            for &v in ch {
                assert!(v.is_finite());
            }
        }
    }

    #[test]
    fn max_params_bounded_across_sample_rates() {
        let params = ReverbParams {
            room_size: 1.5,
            decay: 1.0,
            density: 1.0,
            pre_delay_ms: 100.0,
            motion_depth_ms: 2.0,
            motion_rate: 2.0,
            wet: 1.0,
            dry: 0.0,
            ..Default::default()
        };
        for sr in [44_100u32, 48_000, 96_000] {
            let mut f = ReverbFilter::new(params);
            f.initialize(sr, &["L".into(), "R".into()]);
            let mut samples = vec![vec![0.0f32; 4800], vec![0.0f32; 4800]];
            for i in 0..4800 {
                samples[0][i] =
                    (core::f32::consts::TAU * 440.0 * i as f32 / sr as f32).sin() * 0.9;
                samples[1][i] = samples[0][i] * 0.5;
            }
            f.process(&mut samples, 4800);
            for ch in &samples {
                for &v in ch {
                    assert!(v.is_finite());
                }
            }
        }
    }

    #[test]
    fn mono_matches_c_semantics() {
        let mut f = ReverbFilter::new(ReverbParams::default());
        f.initialize(48000, &["Mono".into()]);
        let mut samples = vec![vec![0.0f32; 4800]];
        for i in 0..4800 {
            samples[0][i] =
                (core::f32::consts::TAU * 220.0 * i as f32 / 48000.0).sin() * 0.5;
        }
        f.process(&mut samples, 4800);
        for &v in &samples[0] {
            assert!(v.is_finite());
        }
        // 单声道湿声应存在（非纯直通）且幅度被 0.15（0.3 * 0.5）缩放约束。
        let peak = samples[0].iter().map(|v| v.abs()).fold(0.0f32, f32::max);
        assert!(peak > 0.001);
        assert!(peak < 1.0);
    }

    #[test]
    fn factory_matches() {
        let factory = ReverbFactory;
        let result = factory.create_filter("RoomSize 1.0", &test_ctx(), &test_loader());
        assert!(matches!(result, FilterCreateResult::Filter(_)));
        assert_eq!(factory.command_name(), "Reverb");
    }
}
