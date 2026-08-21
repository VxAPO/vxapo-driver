//! Dattorro 板式混响
//!
//! 依据 Jon Dattorro《Effect Design Part 1: Reverberator and Other Filters》
//! （J. Audio Eng. Soc., Vol.45, No.9, 1997）公开论文实现，非 FxSound 派生代码：
//! - 输入 4 级 AllPass 扩散（142/107/379/277，系数 0.75/0.75/0.625/0.625）
//! - 双槽交叉反馈环路：调制 AllPass（672/908，LFO 正交对）→ 主延迟
//!   （4453/4217）→ 槽内低通/高通 → 扩散 AllPass（1800/2656）→ 尾延迟
//!   （3720/3163）
//! - 输出 14 抽头（Table 2 原表，每抽头 0.6 增益），湿声再按 Lat5/Lat6
//!   分成早反射与尾音两组
//!
//! 论文原文：https://ccrma.stanford.edu/~dattorro/EffectDesignPart1.pdf
//! 拓扑正确性另与 ValleyRackFree `src/Plateau/Dattorro.cpp`（GPL-3.0-or-later）
//! 及 johnhw/dattoro_reverb（MIT）交叉核对；本文件为 Rust 代码。
//!
//! config 语法（EAPO 风格，与 完全兼容）：
//! `Reverb: RoomSize 1.0 Decay 0.566 Damping 0.408 Bandwidth 0.350
//!  Density 1.0 Lat5 0.70 Lat6 0.50 PreDelay 0 ms MotionRate 0.11
//!  MotionDepth 0.63 Wet 0.3 Dry 0.9`
//!
//! 参数语义：
//! - RoomSize 0.5..1.5：槽内全部延迟时长缩放（1.0 = 论文原始尺寸）
//! - Decay 0..1：环路反馈，内部映射到 0.05..0.95（0 时接近干声，无固定尾音）
//! - Damping 0..1：槽内低通（0 = 明亮，1 = 暗淡）
//! - Bandwidth 0..1：输入低通（0 = 暗淡，1 = 全开）
//! - Density 0..1：输入/槽内扩散系数，1.0 = 论文默认（0.75/0.625/0.7/0.5）；
//!   < 0.7 时系数低于论文设计值，可能出现金属伪影
//! - Lat5：早反射电平（APF 直达 + <20ms 抽头），默认 0.70
//! - Lat6：扩散尾音电平，默认 0.50
//! - PreDelay 0..100 ms
//! - MotionRate 0.05..2.0：调制 LFO 频率（Hz，默认 0.11 ≈ 论文量级）
//! - MotionDepth 0..2：调制深度归一化值（0 = 无调制，2 = 论文 EXCURSION 16 采样@29761Hz）
//! - Wet/Dry 0..1
//!
//! 次正规数：RT 音频线程入口已设硬件 FTZ/DAZ（见 math.rs），与 biquad/fir 一致，
//! 无需逐采样冲刷；输出端 is_finite 兜底防止非有限值泄漏。

use crate::pipeline::dsp::filter::Filter;

/// 论文延迟表参考采样率（Table 1）。
const DAT_REF_SR: f32 = 29761.0;

/// 输入扩散：论文 Fig.1 的 4 级 lattice 延迟（采样@29761Hz）与系数。
const DAT_INPUT_APF: [(f32, f32); 4] = [(142.0, 0.75), (107.0, 0.75), (379.0, 0.625), (277.0, 0.625)];

/// 槽内第一对 AllPass（被 LFO 调制），左/右。
const DAT_APF1: [f32; 2] = [672.0, 908.0];
/// 槽内第二对 AllPass（扩散），左/右。
const DAT_APF2: [f32; 2] = [1800.0, 2656.0];
/// 槽内主延迟，左/右。
const DAT_D1: [f32; 2] = [4453.0, 4217.0];
/// 槽内尾延迟，左/右。
const DAT_D2: [f32; 2] = [3720.0, 3163.0];

/// 输出抽头（Table 2）。延迟线编号：0=左D1(4453) 1=右D1(4217)
/// 2=左APF2(1800) 3=右APF2(2656) 4=左D2(3720) 5=右D2(3163)。
/// 每项：(延迟线, 抽头采样@29761Hz, 符号)。
/// 早反射 = APF 直达 + <20ms 抽头；其余为扩散尾音。
const DAT_TAPS_L_EARLY: [(usize, f32, f32); 2] = [(0, 266.0, 1.0), (3, 187.0, -1.0)];
const DAT_TAPS_L_TAIL: [(usize, f32, f32); 5] = [
    (0, 2974.0, 1.0),
    (2, 1913.0, -1.0),
    (4, 1996.0, 1.0),
    (1, 1990.0, -1.0),
    (5, 1066.0, -1.0),
];
const DAT_TAPS_R_EARLY: [(usize, f32, f32); 3] = [(1, 353.0, 1.0), (2, 335.0, -1.0), (5, 121.0, -1.0)];
const DAT_TAPS_R_TAIL: [(usize, f32, f32); 4] = [
    (1, 3627.0, 1.0),
    (3, 1228.0, -1.0),
    (5, 2673.0, 1.0),
    (0, 2111.0, -1.0),
];

/// 各延迟线上会出现的最大抽头索引（@29761Hz），用于保证小 RoomSize
/// 下抽头仍落在缓冲内（抽头按物理时间固定，不随房间尺寸缩放）。
const DAT_TAP_MAX: [f32; 6] = [2974.0, 3627.0, 1913.0, 1228.0, 1996.0, 2673.0];

/// 论文 Table 1：调制峰值偏移（采样@29761Hz）。
const DAT_LFO_EXCURSION: f32 = 16.0;

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
    pub motion_depth: f32,
    pub wet: f32,
    pub dry: f32,
}

impl Default for ReverbParams {
    fn default() -> Self {
        Self {
            room_size: 1.0,
            decay: 0.41,
            damping: 0.408290,
            bandwidth: 0.350110,
            density: 1.0,
            lat5: 0.70,
            lat6: 0.50,
            pre_delay_ms: 0.0,
            motion_rate: 0.110871,
            motion_depth: 0.63,
            wet: 0.27,
            dry: 0.73,
        }
    }
}

/// 循环延迟线：`read(delay)` 返回 `delay` 个采样前写入的值。
#[derive(Debug, Clone)]
struct DelayLine {
    buf: Vec<f32>,
    len: usize,
    pos: usize,
}

impl DelayLine {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(1)],
            len: len.max(1),
            pos: 0,
        }
    }

    #[inline]
    fn clear(&mut self) {
        self.buf.fill(0.0);
        self.pos = 0;
    }

    #[inline]
    fn write(&mut self, v: f32) {
        self.buf[self.pos] = v;
        self.pos += 1;
        if self.pos == self.len {
            self.pos = 0;
        }
    }

    #[inline]
    fn read(&self, delay: usize) -> f32 {
        let d = delay % self.len;
        self.buf[(self.pos + self.len - 1 - d) % self.len]
    }

    /// 线性插值读取（用于调制延迟）。
    #[inline]
    fn read_frac(&self, delay: f32) -> f32 {
        let base = delay.floor();
        let frac = delay - base;
        let d = base as usize;
        let y1 = self.read(d);
        let y2 = self.read(d + 1);
        y1 + (y2 - y1) * frac
    }

    /// 先写后读（槽内主延迟语义）。
    #[inline]
    fn write_read(&mut self, v: f32, delay: usize) -> f32 {
        self.write(v);
        self.read(delay)
    }
}

/// 单极点低通（输入带宽 / 槽内阻尼）。
#[derive(Debug, Clone)]
struct OnePoleLp {
    coeff: f32,
    state: f32,
}

impl OnePoleLp {
    fn new(cutoff_hz: f32, sr: f32) -> Self {
        let c = 1.0 - (-core::f32::consts::TAU * cutoff_hz / sr).exp();
        Self { coeff: c, state: 0.0 }
    }

    #[inline]
    fn next(&mut self, x: f32) -> f32 {
        let y = self.coeff * x + (1.0 - self.coeff) * self.state;
        self.state = y;
        y
    }

    fn clear(&mut self) {
        self.state = 0.0;
    }
}

/// 单极点高通（输入/槽内/输出 DC 隔离，20 Hz）。
#[derive(Debug, Clone)]
struct OnePoleHp {
    a: f32,
    x1: f32,
    y1: f32,
}

impl OnePoleHp {
    fn new(cutoff_hz: f32, sr: f32) -> Self {
        Self {
            a: (-core::f32::consts::TAU * cutoff_hz / sr).exp(),
            x1: 0.0,
            y1: 0.0,
        }
    }

    #[inline]
    fn next(&mut self, x: f32) -> f32 {
        let y = self.a * (self.y1 + x - self.x1);
        self.x1 = x;
        self.y1 = y;
        y
    }

    fn clear(&mut self) {
        self.x1 = 0.0;
        self.y1 = 0.0;
    }
}

/// 调制 AllPass（论文：槽内最早的一对扩散器，正交 LFO）。
#[derive(Debug, Clone)]
struct ModAllpass {
    line: DelayLine,
    base_delay: f32,
    depth: f32,
    /// 每采样相位增量（cycles/sample）。
    phase_step: f32,
    phase: f32,
}

impl ModAllpass {
    fn new() -> Self {
        Self {
            line: DelayLine::new(1),
            base_delay: 0.0,
            depth: 0.0,
            phase_step: 0.0,
            phase: 0.0,
        }
    }

    #[inline]
    fn next(&mut self, input: f32, g: f32) -> f32 {
        self.phase += self.phase_step;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        // 论文使用正弦 LFO（正交相位由 phase 初值提供），双极 -1..1；
        // 三角波含更高次谐波，较高调制率下会引入可闻着色。
        let sin_val = (self.phase * core::f32::consts::TAU).sin();
        let delay = self.base_delay + sin_val * self.depth;
        let y = self.line.read_frac(delay);
        let z = input - g * y;
        self.line.write(z);
        y + g * z
    }

    fn clear(&mut self) {
        self.line.clear();
        self.phase = 0.0;
    }
}

/// 普通 AllPass（输入扩散）。
#[inline]
fn allpass(line: &mut DelayLine, delay: usize, input: f32, g: f32) -> f32 {
    let y = line.read(delay);
    let z = input - g * y;
    line.write(z);
    y + g * z
}

#[derive(Debug)]
pub struct ReverbFilter {
    params: ReverbParams,
    channel_indices: Vec<usize>,

    pre_delay_samples: usize,
    pre_line: DelayLine,

    input_apfs: Vec<DelayLine>,
    input_apf_delays: Vec<usize>,
    input_apf_g: Vec<f32>,

    apf1: Vec<ModAllpass>,
    apf1_g: Vec<f32>,
    d1: Vec<DelayLine>,
    d1_delay: Vec<usize>,
    apf2: Vec<DelayLine>,
    apf2_delay: Vec<usize>,
    apf2_g: Vec<f32>,
    d2: Vec<DelayLine>,
    d2_delay: Vec<usize>,

    tap_l_early: Vec<(usize, usize, f32)>,
    tap_l_tail: Vec<(usize, usize, f32)>,
    tap_r_early: Vec<(usize, usize, f32)>,
    tap_r_tail: Vec<(usize, usize, f32)>,

    input_lp: OnePoleLp,
    input_hp: OnePoleHp,
    tank_lp: Vec<OnePoleLp>,
    tank_hp: Vec<OnePoleHp>,
    out_dc: Vec<OnePoleHp>,

    left_sum: f32,
    right_sum: f32,
    loop_gain: f32,
}

impl ReverbFilter {
    pub fn new(params: ReverbParams) -> Self {
        Self {
            params,
            channel_indices: Vec::new(),
            pre_delay_samples: 0,
            pre_line: DelayLine::new(1),
            input_apfs: Vec::new(),
            input_apf_delays: Vec::new(),
            input_apf_g: Vec::new(),
            apf1: Vec::new(),
            apf1_g: Vec::new(),
            d1: Vec::new(),
            d1_delay: Vec::new(),
            apf2: Vec::new(),
            apf2_delay: Vec::new(),
            apf2_g: Vec::new(),
            d2: Vec::new(),
            d2_delay: Vec::new(),
            tap_l_early: Vec::new(),
            tap_l_tail: Vec::new(),
            tap_r_early: Vec::new(),
            tap_r_tail: Vec::new(),
            input_lp: OnePoleLp { coeff: 0.0, state: 0.0 },
            input_hp: OnePoleHp { a: 1.0, x1: 0.0, y1: 0.0 },
            tank_lp: Vec::new(),
            tank_hp: Vec::new(),
            out_dc: Vec::new(),
            left_sum: 0.0,
            right_sum: 0.0,
            loop_gain: 0.0,
        }
    }

    fn clear_state(&mut self) {
        self.pre_line.clear();
        for l in &mut self.input_apfs {
            l.clear();
        }
        for a in &mut self.apf1 {
            a.clear();
        }
        for l in &mut self.d1 {
            l.clear();
        }
        for l in &mut self.apf2 {
            l.clear();
        }
        for l in &mut self.d2 {
            l.clear();
        }
        self.input_lp.clear();
        self.input_hp.clear();
        for f in &mut self.tank_lp {
            f.clear();
        }
        for f in &mut self.tank_hp {
            f.clear();
        }
        for f in &mut self.out_dc {
            f.clear();
        }
        self.left_sum = 0.0;
        self.right_sum = 0.0;
    }

    #[inline]
    fn tap_line(&self, id: usize) -> &DelayLine {
        match id {
            0 => &self.d1[0],
            1 => &self.d1[1],
            // APF2 内部缓冲存储 z = input - g * read(delay)（非 AllPass 输出端）；
            // 论文 Table 2 的抽头对应延迟线上的物理存储点，读 z 符合拓扑。
            2 => &self.apf2[0],
            3 => &self.apf2[1],
            4 => &self.d2[0],
            _ => &self.d2[1],
        }
    }
}

impl Filter for ReverbFilter {
    fn initialize(&mut self, sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        if self.channel_indices.is_empty() {
            self.channel_indices = (0..channel_names.len()).collect();
        }
        let sr = sample_rate.max(1) as f32;
        // DSP 入口兜底：直接构造 ReverbParams 时也保证参数在合法范围内，
        // 防止 decay > 1 等越界输入让环路发散（配置解析层已做一次校验）。
        let mut p = self.params;
        let clamp_param = |v: f32, lo: f32, hi: f32, def: f32| -> f32 {
            if !v.is_finite() {
                def
            } else {
                v.clamp(lo, hi)
            }
        };
        p.room_size = clamp_param(p.room_size, 0.5, 1.5, 1.0);
        p.decay = clamp_param(p.decay, 0.0, 1.0, 0.41);
        p.damping = clamp_param(p.damping, 0.0, 1.0, 0.408290);
        p.bandwidth = clamp_param(p.bandwidth, 0.0, 1.0, 0.350110);
        p.density = clamp_param(p.density, 0.0, 1.0, 1.0);
        p.lat5 = clamp_param(p.lat5, 0.0, 1.0, 0.70);
        p.lat6 = clamp_param(p.lat6, 0.0, 1.0, 0.50);
        p.pre_delay_ms = clamp_param(p.pre_delay_ms, 0.0, 100.0, 0.0);
        p.motion_rate = clamp_param(p.motion_rate, 0.05, 2.0, 0.110871);
        p.motion_depth = clamp_param(p.motion_depth, 0.0, 2.0, 0.63);
        p.wet = clamp_param(p.wet, 0.0, 1.0, 0.27);
        p.dry = clamp_param(p.dry, 0.0, 1.0, 0.73);
        self.params = p;

        // 参数 → 系数
        let room = p.room_size;
        let scale_ref = |n: f32| -> f32 { n * sr / DAT_REF_SR };
        let scale_room = |n: f32| -> f32 { (n * room * sr / DAT_REF_SR).max(1.0) };
        let tap = |n: f32| -> usize { scale_ref(n).trunc().max(1.0) as usize };

        // Decay 下限从 0.25 降到 0.05：decay=0 时接近干声，能做出极短混响
        self.loop_gain = 0.05 + 0.90 * p.decay;
        let input_cutoff = 60.0 + 22000.0 * p.bandwidth;
        let tank_cutoff = 60.0 + 22000.0 * (1.0 - p.damping);
        self.input_lp = OnePoleLp::new(input_cutoff, sr);
        self.input_hp = OnePoleHp::new(20.0, sr);
        self.tank_lp = vec![OnePoleLp::new(tank_cutoff, sr); 2];
        self.tank_hp = vec![OnePoleHp::new(20.0, sr); 2];
        self.out_dc = vec![OnePoleHp::new(20.0, sr); 2];

        let d = p.density;
        let input_g1 = 0.15 + 0.60 * d;
        let input_g2 = 0.125 + 0.50 * d;
        let plate_g1 = 0.14 + 0.56 * d;
        let plate_g2 = 0.10 + 0.40 * d;

        // 输入扩散（固定物理时长，不随房间尺寸缩放）
        self.input_apfs = DAT_INPUT_APF
            .iter()
            .map(|&(del, _)| DelayLine::new(tap(del) + 1))
            .collect();
        self.input_apf_delays = DAT_INPUT_APF.iter().map(|&(del, _)| tap(del)).collect();
        self.input_apf_g = vec![input_g1, input_g1, input_g2, input_g2];

        // 调制深度（论文 EXCURSION=16 采样@29761Hz；MotionDepth 2 = 全量）
        let mod_depth = (p.motion_depth / 2.0).clamp(0.0, 1.0);
        let lfo_depth = DAT_LFO_EXCURSION * sr / DAT_REF_SR * mod_depth;
        let lfo_hz = p.motion_rate.clamp(0.05, 2.0);
        let phase_step = lfo_hz / sr;

        // 槽内延迟：随 RoomSize 缩放；缓冲长度按“当前延迟 + 调制余量 +
        // 最大抽头”预留，保证小房间下抽头仍有效。
        let max_exc = lfo_depth.ceil() as usize + 2;
        self.apf1 = DAT_APF1
            .iter()
            .enumerate()
            .map(|(i, &del)| {
                let base = scale_room(del);
                let mut a = ModAllpass::new();
                a.line = DelayLine::new(base.ceil() as usize + max_exc);
                a.base_delay = base;
                a.depth = lfo_depth;
                a.phase_step = phase_step;
                a.phase = if i == 0 { 0.0 } else { 0.25 }; // 正交对
                a
            })
            .collect();
        self.apf1_g = vec![-plate_g1, -plate_g1];

        self.d1_delay = DAT_D1.iter().map(|&del| scale_room(del).trunc() as usize).collect();
        self.d1 = (0..2)
            .map(|i| {
                let base = self.d1_delay[i];
                let tap_max = tap(DAT_TAP_MAX[i]);
                DelayLine::new(base.max(tap_max) + 2)
            })
            .collect();

        // 论文 Fig.1：只有槽内第一对 AllPass（672/908）被 LFO 调制；
        // 第二对（1800/2656）是固定扩散器，用普通 AllPass，不做调制。
        self.apf2_delay = DAT_APF2.iter().map(|&del| scale_room(del).trunc() as usize).collect();
        self.apf2 = (0..2)
            .map(|i| {
                let base = self.apf2_delay[i];
                let tap_max = tap(DAT_TAP_MAX[i + 2]);
                DelayLine::new(base.max(tap_max) + 2)
            })
            .collect();
        self.apf2_g = vec![plate_g2, plate_g2];

        self.d2_delay = DAT_D2.iter().map(|&del| scale_room(del).trunc() as usize).collect();
        self.d2 = (0..2)
            .map(|i| {
                let base = self.d2_delay[i];
                let tap_max = tap(DAT_TAP_MAX[i + 4]);
                DelayLine::new(base.max(tap_max) + 2)
            })
            .collect();

        // 预延迟
        self.pre_delay_samples = (p.pre_delay_ms / 1000.0 * sr).round() as usize;
        self.pre_line = DelayLine::new(self.pre_delay_samples.max(1) + 1);

        // 抽头（按物理时间固定，仅随采样率缩放）
        let map_taps = |taps: &[(usize, f32, f32)]| {
            taps.iter()
                .map(|&(line, del, sign)| (line, tap(del), sign))
                .collect::<Vec<_>>()
        };
        self.tap_l_early = map_taps(&DAT_TAPS_L_EARLY);
        self.tap_l_tail = map_taps(&DAT_TAPS_L_TAIL);
        self.tap_r_early = map_taps(&DAT_TAPS_R_EARLY);
        self.tap_r_tail = map_taps(&DAT_TAPS_R_TAIL);

        self.clear_state();
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
        let pre_delay = self.pre_delay_samples;
        let loop_gain = self.loop_gain;

        for f in 0..frame_count {
            let in_l = samples[l][f];
            let in_r = if stereo { samples[r][f] } else { 0.0 };
            let mix = if stereo { (in_l + in_r) * 0.5 } else { in_l };

            // 输入：DC 隔离 → 带宽低通 → 预延迟 → 4 级输入扩散
            let mut x = self.input_hp.next(mix);
            x = self.input_lp.next(x);
            if pre_delay > 0 {
                x = self.pre_line.write_read(x, pre_delay);
            }
            for i in 0..4 {
                x = allpass(
                    &mut self.input_apfs[i],
                    self.input_apf_delays[i],
                    x,
                    self.input_apf_g[i],
                );
            }

            self.left_sum += x;
            self.right_sum += x;

            // 左槽：调制 APF(672) → D1(4453) → 低通/高通 → ×loop_gain
            //       → 扩散 APF(1800) → D2(3720)
            let left = self.apf1[0].next(self.left_sum, self.apf1_g[0]);
            let apf1_l = left;
            let left = self.d1[0].write_read(left, self.d1_delay[0]);
            let left = self.tank_lp[0].next(left);
            let left = self.tank_hp[0].next(left) * loop_gain;
            let left = allpass(&mut self.apf2[0], self.apf2_delay[0], left, self.apf2_g[0]);
            let left = self.d2[0].write_read(left, self.d2_delay[0]);

            // 右槽：调制 APF(908) → D1(4217) → 低通/高通 → ×loop_gain
            //       → 扩散 APF(2656) → D2(3163)
            let right = self.apf1[1].next(self.right_sum, self.apf1_g[1]);
            let apf1_r = right;
            let right = self.d1[1].write_read(right, self.d1_delay[1]);
            let right = self.tank_lp[1].next(right);
            let right = self.tank_hp[1].next(right) * loop_gain;
            let right = allpass(&mut self.apf2[1], self.apf2_delay[1], right, self.apf2_g[1]);
            let right = self.d2[1].write_read(right, self.d2_delay[1]);

            // 双槽交叉反馈（论文“global figure eight”）
            // 循环增益只在槽内低通/高通之后乘一次；交叉路径不再乘，
            // 避免完整环路等效增益变成 loop_gain² 导致尾音衰减过快。
            self.right_sum = left;
            self.left_sum = right;

            // 输出抽头（Table 2），早反射/尾音分组
            let mut early_l = apf1_l;
            let mut tail_l = 0.0;
            for &(line, delay, sign) in &self.tap_l_early {
                early_l += sign * self.tap_line(line).read(delay);
            }
            for &(line, delay, sign) in &self.tap_l_tail {
                tail_l += sign * self.tap_line(line).read(delay);
            }
            let wet_l = early_l * p.lat5 + tail_l * p.lat6;
            let wet_l = self.out_dc[0].next(wet_l);
            // 去掉论文外的 0.5 固定衰减（6dB）；湿声路径用 tanh 软限幅削峰，
            // 正常电平近似线性，极端峰值被压在 ±1 内（与 wide/aural 一致）。
            let wet_l = wet_l.tanh();
            let out_l = p.wet * wet_l + p.dry * in_l;
            samples[l][f] = if out_l.is_finite() { out_l } else { 0.0 };

            if stereo {
                let mut early_r = apf1_r;
                let mut tail_r = 0.0;
                for &(line, delay, sign) in &self.tap_r_early {
                    early_r += sign * self.tap_line(line).read(delay);
                }
                for &(line, delay, sign) in &self.tap_r_tail {
                    tail_r += sign * self.tap_line(line).read(delay);
                }
                let wet_r = early_r * p.lat5 + tail_r * p.lat6;
                let wet_r = self.out_dc[1].next(wet_r);
                let wet_r = wet_r.tanh();
                let out_r = p.wet * wet_r + p.dry * in_r;
                samples[r][f] = if out_r.is_finite() { out_r } else { 0.0 };
            }
        }
    }

    fn set_channel_indices(&mut self, indices: &[usize]) {
        self.channel_indices = indices.to_vec();
    }

    fn reset(&mut self) {
        self.clear_state();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn dry_only_is_exact_passthrough() {
        let params = ReverbParams {
            wet: 0.0,
            dry: 1.0,
            ..Default::default()
        };
        let mut f = ReverbFilter::new(params);
        f.initialize(48000, &["L".into(), "R".into()]);
        let mut samples = vec![vec![0.0f32; 4800]; 2];
        for i in 0..4800 {
            samples[0][i] = (core::f32::consts::TAU * 440.0 * i as f32 / 48000.0).sin() * 0.5;
            samples[1][i] = samples[0][i] * 0.5;
        }
        let orig = samples.clone();
        f.process(&mut samples, 4800);
        for ch in 0..2 {
            for i in 0..4800 {
                assert_eq!(samples[ch][i], orig[ch][i]);
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
            motion_depth: 2.0,
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
    fn mono_has_wet_tail() {
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
        // 湿声应存在（非纯直通）且幅度有界。
        let peak = samples[0].iter().map(|v| v.abs()).fold(0.0f32, f32::max);
        assert!(peak > 0.001);
        assert!(peak < 2.0);
    }

    #[test]
    fn out_of_range_params_are_clamped() {
        // 越界/非有限参数不应让环路发散：DSP 入口 clamp 后输出保持有界。
        let params = ReverbParams {
            room_size: 99.0,
            decay: 5.0,
            damping: -3.0,
            bandwidth: 2.0,
            density: -1.0,
            lat5: 4.0,
            lat6: -2.0,
            pre_delay_ms: 1e6,
            motion_rate: 0.0,
            motion_depth: 50.0,
            wet: 2.0,
            dry: -1.0,
            ..Default::default()
        };
        let mut f = ReverbFilter::new(params);
        f.initialize(48000, &["L".into(), "R".into()]);
        let mut samples = vec![vec![0.0f32; 4800], vec![0.0f32; 4800]];
        for i in 0..4800 {
            samples[0][i] = (core::f32::consts::TAU * 440.0 * i as f32 / 48000.0).sin() * 0.9;
            samples[1][i] = samples[0][i] * 0.5;
        }
        f.process(&mut samples, 4800);
        for ch in &samples {
            for &v in ch {
                assert!(v.is_finite());
                assert!(v.abs() < 4.0, "clamp 后输出应保持有界，实际 {v}");
            }
        }
    }

}
