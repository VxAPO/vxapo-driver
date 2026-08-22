//! Wide（立体声加宽器）
//!
//! 核心思路：低频是地基纹丝不动，处理是油漆从分频点往上平滑涂抹。
//!
//! 信号流：
//! 1. 线性相位 FIR 分频（Kaiser 窗，抽头数随采样率/分频点缩放），
//!    低频支路干净直通，高频支路进 M/S 处理；
//! 2. Mid 走空气吸收（高频架 4k~5.5k + 二阶 Bessel 低通 10k~16k，
//!    中频不动、极高频按 f² 曲线陡削），把中置人声推远；
//!    Side 增强量由 `gain` 控制（0→1×，1→+1.5×），带动态包络
//!    （10ms 攻击 / 120ms 释放，强侧自动收增益）和 ITD 去相关
//!    （线性相位 FIR 分频，仅 1.5kHz 以上泛音区走左 +5 / 右 +7
//!    样本时间差；中低频增强直通保持实体感）；
//! 3. 处理增量（air + 侧增强相对原始高频的差）先过一阶高通
//!    （截止 = 分频点，6dB/oct），再 tanh 限幅，最后乘 `mix`；
//!    原始高频与限幅增量同步延迟 1 样本叠加，保持同一时间基准；
//! 4. 低频 + 高频叠加后输出端软膝限幅兜底。
//!
//! config 语法（EAPO 风格）：
//! `Wide: Gain 0.0 Air 0.354331 Mix 0.6 Crossover 200`

use crate::pipeline::dsp::biquad::{compute_coeffs, BiquadCoeffs, BiquadState, BiquadType};
use crate::pipeline::dsp::filter::Filter;
use crate::pipeline::dsp::fir::PartitionedFir;

#[derive(Debug, Clone, Copy)]
pub struct WideParams {
    /// 高频补偿（0..1）：控制侧通道增强量（0 → 1×，1 → +1.5×）。
    pub gain: f32,
    /// 空气吸收（0..1，中声道按物理曲线渐进吸收高频，把中置人声推远）。
    pub air: f32,
    /// 干湿混合（0..1，默认 0.6）：处理增量最终渗入量。
    pub mix: f32,
    /// M/S 分频点（Hz，100..1000，默认 200；以下低频保持原样）。
    pub crossover_hz: f32,
}

impl Default for WideParams {
    fn default() -> Self {
        Self {
            gain: 0.0,
            // 与原 Wide32.c Quick preset 的默认距离对齐。
            air: 0.354331,
            mix: 0.6,
            crossover_hz: 200.0,
        }
    }
}

/// FIR 分频点范围（Hz）。
const CROSSOVER_MIN_HZ: f32 = 200.0;
const CROSSOVER_MAX_HZ: f32 = 1000.0;
/// 直接 FIR 上限（超过走分块 FFT，与 PEQ 一致；避免高采样率长 IR 超实时预算）。
const DIRECT_FIR_MAX_LEN: usize = 2048;
/// FIR 长度下限（高采样率最小抽头数）。
const FIR_MIN_LEN: usize = 512;
/// FIR 长度上限（384k 时 8192 抽头 ≈ 21.3ms，分块 FFT 承担）。
const FIR_MAX_LEN: usize = 8192;
/// 高频段侧信号增益斜率（1 + 1.5·Gain，由用户手动控制）。
const SIDE_GAIN_HIGH_SLOPE: f32 = 1.5;
/// ITD 去相关：左侧增强延迟（采样点）。
const ITD_DELAY_L: usize = 5;
/// ITD 去相关：右侧增强延迟（采样点），与左路差 2 样本破坏同频相消。
const ITD_DELAY_R: usize = 7;
/// ITD 延迟缓冲长度（≥ 最大延迟）。
const ITD_BUF_LEN: usize = 8;
/// ITD 作用频率下限（Hz）：仅 1.5kHz 以上侧泛音区走时间差去相关，
/// 中低频侧增强直通，保持实体感。分离用线性相位 FIR。
const SIDE_ITD_CROSSOVER_HZ: f32 = 1500.0;
/// 动态 M/S：侧通道包络阈值（超过后开始收增益）。
const SIDE_DYN_THRESHOLD: f32 = 0.2;
/// 动态 M/S：包络压缩比指数（0.55 ≈ 2:1 软膝）。
const SIDE_DYN_RATIO: f32 = 0.55;
/// 动态 M/S：包络攻击时间（s，慢攻避免瞬态触发过度压缩）。
const SIDE_DYN_ATTACK_SECS: f32 = 0.010;
/// 动态 M/S：包络释放时间（s）。
const SIDE_DYN_RELEASE_SECS: f32 = 0.12;
/// tanh 软限幅 headroom 范围（Gain=1 时最小，Gain→0 时最大；
/// 只有用户手动加的侧增益会推电平）。
const HEADROOM_MIN_DB: f32 = 1.0;
const HEADROOM_MAX_DB: f32 = 1.6;
/// 空气吸收：高频架增益范围（air 0→1：-0.5dB → -3.5dB）。
const AIR_SHELF_GAIN_MIN_DB: f32 = -0.5;
const AIR_SHELF_GAIN_MAX_DB: f32 = -3.5;
/// 高频架拐点（Hz，air 0→1：5.5k → 4k），只削减极高频泛音，避免中频凹陷。
const AIR_SHELF_FC_MIN_HZ: f32 = 5500.0;
const AIR_SHELF_FC_MAX_HZ: f32 = 4000.0;
/// Bessel 低通截止（Hz，air 0→1：16k → 10k），滚降起点提前、更接近 f² 曲线。
const AIR_LP_FC_MIN_HZ: f32 = 16000.0;
const AIR_LP_FC_MAX_HZ: f32 = 10000.0;

/// 输出端软膝限幅：|x| <= 0.9 线性直通（低音/正常电平不动），
/// 超出部分用 tanh 圆角压缩，输出上限 1.0，防止高频增益下硬削波。
#[inline]
fn output_soft_clip(x: f32) -> f32 {
    const KNEE: f32 = 0.9;
    let a = x.abs();
    if a <= KNEE {
        x
    } else {
        x.signum() * (KNEE + (a - KNEE).tanh() * (1.0 - KNEE))
    }
}

/// 空气吸收级联（按物理曲线的极高频渐进衰减）：
/// 高频架（RBJ HighShelf，Q=0.707，4k~5.5k）微调极高频泛音，
/// 之后接**二阶 Bessel 低通**（10k~16k）——Bessel 群延迟最平坦、
/// 相位失真最小，12dB/oct 更接近 f² 物理曲线，只削极高频不碰中频。
#[derive(Debug, Clone)]
struct AirAbsorption {
    shelf_coeffs: BiquadCoeffs,
    shelf: BiquadState,
    lp_coeffs: BiquadCoeffs,
    lp: BiquadState,
}

impl AirAbsorption {
    fn new(air: f32, sample_rate: u32) -> Self {
        let a = air.clamp(0.0, 1.0);
        if sample_rate == 0 || a <= 0.0 {
            return Self {
                shelf_coeffs: BiquadCoeffs::BYPASS,
                shelf: BiquadState::new(),
                lp_coeffs: BiquadCoeffs::BYPASS,
                lp: BiquadState::new(),
            };
        }
        let shelf_gain =
            AIR_SHELF_GAIN_MIN_DB + (AIR_SHELF_GAIN_MAX_DB - AIR_SHELF_GAIN_MIN_DB) * a;
        let shelf_fc = AIR_SHELF_FC_MIN_HZ + (AIR_SHELF_FC_MAX_HZ - AIR_SHELF_FC_MIN_HZ) * a;
        let lp_fc = AIR_LP_FC_MIN_HZ + (AIR_LP_FC_MAX_HZ - AIR_LP_FC_MIN_HZ) * a;
        Self {
            shelf_coeffs: compute_coeffs(
                BiquadType::HighShelf,
                shelf_fc,
                shelf_gain,
                0.707,
                sample_rate,
            ),
            shelf: BiquadState::new(),
            lp_coeffs: bessel_lp_coeffs(lp_fc, sample_rate),
            lp: BiquadState::new(),
        }
    }

    #[inline]
    fn next(&mut self, x: f32) -> f32 {
        self.lp.process_sample(&self.lp_coeffs, self.shelf.process_sample(&self.shelf_coeffs, x))
    }

    fn clear(&mut self) {
        self.shelf.clear();
        self.lp.clear();
    }
}

/// 二阶 Bessel 低通（bilinear 变换）：
/// 模拟原型 H(s)=3/(s²+3s+3)，-3dB 点在 ω=1.3617（Q=1/√3，
/// 群延迟最平坦）。缩放使 -3dB 落在 `cutoff_hz`，再经双线性变换到数字域。
fn bessel_lp_coeffs(cutoff_hz: f32, sample_rate: u32) -> BiquadCoeffs {
    let fs = sample_rate.max(1) as f64;
    let fc = cutoff_hz.max(1.0) as f64;
    // ω² = 3(√5−1)/2 是 |H(jω)|²=1/2 的解。
    let w3db = (3.0 * (5.0f64.sqrt() - 1.0) / 2.0).sqrt();
    let w0 = core::f64::consts::TAU * fc / w3db;
    let c = 2.0 * fs;
    let d = c * c + 3.0 * w0 * c + 3.0 * w0 * w0;
    let b0 = 3.0 * w0 * w0 / d;
    BiquadCoeffs {
        b0: b0 as f32,
        b1: (2.0 * b0) as f32,
        b2: b0 as f32,
        a1: ((-2.0 * c * c + 6.0 * w0 * w0) / d) as f32,
        a2: ((c * c - 3.0 * w0 * c + 3.0 * w0 * w0) / d) as f32,
    }
}

/// 一阶 IIR 高通（6dB/oct），用于增量安全锁：
/// 截止频率 = 分频点，分频点以下增量被衰减，处理量平滑淡入。
/// bilinear 标准公式：k = tan(ω/2)，b0=1/(1+k)，b1=-1/(1+k)，
/// a1=(k-1)/(1+k)（差分方程 y = b0·x + b1·x[n-1] − a1·y[n-1]，
/// 即分母 1 + a1·z⁻¹ = 1 − ((1−k)/(1+k))·z⁻¹）。
#[derive(Debug, Clone)]
struct FirstOrderHpf {
    b0: f32,
    b1: f32,
    a1: f32,
    x_prev: f32,
    y_prev: f32,
}

impl FirstOrderHpf {
    fn new(cutoff_hz: f32, sample_rate: u32) -> Self {
        let sr = sample_rate.max(1) as f32;
        let w = core::f32::consts::TAU * cutoff_hz / sr;
        let k = (w * 0.5).tan();
        let inv = 1.0 / (1.0 + k);
        Self {
            b0: inv,
            b1: -inv,
            a1: (k - 1.0) * inv,
            x_prev: 0.0,
            y_prev: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x_prev - self.a1 * self.y_prev;
        self.x_prev = x;
        self.y_prev = y;
        y
    }

    fn clear(&mut self) {
        self.x_prev = 0.0;
        self.y_prev = 0.0;
    }
}

/// 采样点级延迟线（ITD 去相关）：固定延迟的环形缓冲。
#[derive(Debug, Clone)]
struct ItdDelay {
    buf: [f32; ITD_BUF_LEN],
    pos: usize,
    delay: usize,
}

impl ItdDelay {
    fn new(delay: usize) -> Self {
        Self {
            buf: [0.0; ITD_BUF_LEN],
            pos: 0,
            delay: delay.min(ITD_BUF_LEN - 1),
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let idx = (self.pos + ITD_BUF_LEN - self.delay) % ITD_BUF_LEN;
        let y = self.buf[idx];
        self.buf[self.pos] = x;
        self.pos = (self.pos + 1) % ITD_BUF_LEN;
        y
    }

    fn clear(&mut self) {
        self.buf.fill(0.0);
        self.pos = 0;
    }
}

/// FIR 分频长度随采样率与分频点缩放：
/// Kaiser β=6.2 的过渡带宽度 ∝ fs/N，按 Δf ≈ fc/2 取
/// N ≈ (A-8)/(2.285·Δω) ≈ 7.24·fs/fc，再取 2 的幂。
/// 48k/200 → 2048，48k/500 → 1024，96k/200 → 4096，192k/200 → 8192。
fn wide_fir_len(sr: u32, xover_hz: f32) -> usize {
    let fs = sr.max(1) as f32;
    let fc = xover_hz.clamp(CROSSOVER_MIN_HZ, CROSSOVER_MAX_HZ);
    let n = (7.24 * fs / fc).round() as usize;
    n.next_power_of_two().clamp(FIR_MIN_LEN, FIR_MAX_LEN)
}

/// 侧增强的 ITD 分频 FIR 长度：固定 1.5kHz 分频点，
/// 过渡带约 fc/2（48k → 256 抽头，延迟 ≈ 2.6ms）。
fn side_fir_len(sr: u32) -> usize {
    let fs = sr.max(1) as f32;
    let n = (7.24 * fs / SIDE_ITD_CROSSOVER_HZ).round() as usize;
    n.next_power_of_two().clamp(128, 4096)
}

/// 线性相位 FIR 分频：低通 FIR + 互补高通（高频 = 延迟对齐原信号 − 低通）。
/// 抽头 ≤2048 走直接环形缓冲；更长走分块 FFT（延迟 = block-1，同样补对齐）。
#[derive(Debug)]
struct FirSplit {
    engine: LpEngine,
    /// 每声道延迟对齐线（Direct：整条 FIR 延迟；Partitioned：分块延迟）。
    delay_lines: Vec<Vec<f32>>,
    write_positions: Vec<usize>,
}

#[derive(Debug)]
enum LpEngine {
    /// 直接卷积：逆序 IR + 环形缓冲。
    Direct {
    /// 逆序低通 IR（与 convolution::dot 配合）。
    ir_rev: Vec<f32>,
    /// FIR 长度。
    ir_len: usize,
    /// 环形缓冲长度（next_power_of_two(ir_len）)。
    delay_len: usize,
    mask: usize,
    /// 线性相位中心（群延迟采样数）。
    center: usize,
    },
    /// 分块 FFT 卷积（延迟 = block_len - 1）。
    Partitioned {
        pf: PartitionedFir,
        /// 分块延迟对齐环（每声道；长度 = block_len）。
        dlen: usize,
        dmask: usize,
        latency: usize,
    },
}

impl FirSplit {
    fn new(ir: Vec<f32>, channels: usize) -> Self {
        #[cfg(target_arch = "x86_64")]
        crate::pipeline::dsp::fir::init_fir_simd();
        let ir_len = ir.len().max(1);
        let engine = if ir_len <= DIRECT_FIR_MAX_LEN {
            let delay_len = ir_len.next_power_of_two();
            LpEngine::Direct {
                ir_rev: ir.iter().rev().copied().collect(),
                ir_len,
                delay_len,
                mask: delay_len - 1,
                center: (ir_len - 1) / 2,
            }
        } else {
            let pf = PartitionedFir::new(&ir, channels);
            let dlen = pf.block_len();
            LpEngine::Partitioned {
                pf,
                dlen,
                dmask: dlen - 1,
                latency: dlen - 1,
            }
        };
        let delay_len = match &engine {
            LpEngine::Direct { delay_len, .. } => *delay_len,
            LpEngine::Partitioned { dlen, .. } => *dlen,
        };
        Self {
            engine,
            delay_lines: vec![vec![0.0; delay_len]; channels],
            write_positions: vec![0; channels],
        }
    }

    /// 单声道分频：返回(低频支路, 高频支路)，两路之和 = 延迟 center 帧的原信号。
    fn split_channel(&mut self, k: usize, x: f32) -> (f32, f32) {
        match &mut self.engine {
            LpEngine::Direct {
                ir_rev,
                ir_len,
                delay_len,
                mask,
                center,
            } => {
                let ir_len = *ir_len;
                let delay_len = *delay_len;
                let mask = *mask;
                let center = *center;
                let delay = &mut self.delay_lines[k];
                let pos = &mut self.write_positions[k];
                delay[*pos] = x;
                *pos = (*pos + 1) & mask;

                let start = (*pos).wrapping_sub(1) & mask;
                let oldest = (*pos + delay_len - ir_len) & mask;
                let lp = if oldest <= start {
                    crate::pipeline::dsp::fir::dot(ir_rev, &delay[oldest..=start])
                } else {
                    let len_old = delay_len - oldest;
                    crate::pipeline::dsp::fir::dot(&ir_rev[..len_old], &delay[oldest..])
                        + crate::pipeline::dsp::fir::dot(
                            &ir_rev[len_old..],
                            &delay[0..=start],
                        )
                };
                let delayed_x = delay[(*pos + delay_len - 1 - center) & mask];
                (lp, delayed_x - lp)
            }
            LpEngine::Partitioned {
                pf,
                dmask,
                ..
            } => {
                let lp = pf.process_channel(k, x);
                let dmask = *dmask;
                // 互补高通需与原信号延迟对齐：分块延迟 = latency = block-1。
                let delay = &mut self.delay_lines[k];
                let pos = &mut self.write_positions[k];
                delay[*pos] = x;
                *pos = (*pos + 1) & dmask;
                // 写后 pos 指向最旧槽：该槽即 x[n-latency]（dlen=block_len，
                // latency=dlen-1，写入间隔 dlen 覆盖 latency+1 步，取 pos）。
                let delayed_x = delay[*pos];
                (lp, delayed_x - lp)
            }
        }
    }

    fn reset(&mut self) {
        for line in self.delay_lines.iter_mut() {
            line.fill(0.0);
        }
        self.write_positions.fill(0);
        match &mut self.engine {
            LpEngine::Direct { .. } => {}
            LpEngine::Partitioned { pf, .. } => pf.reset(),
        }
    }
}

/// Kaiser 窗 β（≈-60dB 旁瓣，过渡带比 Hamming 更窄、停带更深）。
const KAISER_BETA: f32 = 6.2;

/// 零阶修正贝塞尔 I0（级数近似，x ≤ 32 收敛良好）。
fn kaiser_i0(x: f32) -> f32 {
    let mut sum = 1.0f32;
    let mut term = 1.0f32;
    let x2 = x * x;
    for k in 1..=16 {
        term *= x2 / (4.0 * k as f32 * k as f32);
        sum += term;
    }
    sum
}

/// 设计线性相位低通 FIR（理想低通 × Kaiser 窗，DC 增益归一）。
/// Kaiser（β=6.2）比原 Hamming 停带更深、过渡带更窄——高采样率下
/// 同样抽头数的 200Hz 分频质量显著更好。
fn design_lowpass_ir(fc_hz: f32, sample_rate: u32, n: usize) -> Vec<f32> {
    let sr = sample_rate.max(1) as f32;
    let fc = fc_hz.min(sr * 0.45).max(1.0);
    let center = (n - 1) as f32 * 0.5;
    let mut ir = vec![0.0f32; n];
    let mut sum = 0.0f32;
    let i0_beta = kaiser_i0(KAISER_BETA);
    for i in 0..n {
        let m = i as f32 - center;
        let sinc = if m.abs() < 1.0e-6 {
            2.0 * fc / sr
        } else {
            (core::f32::consts::TAU * fc * m / sr).sin() / (core::f32::consts::PI * m)
        };
        let arg = (1.0 - ((i as f32 - center) / center).powi(2)).max(0.0).sqrt() * KAISER_BETA;
        let w = kaiser_i0(arg) / i0_beta;
        ir[i] = sinc * w;
        sum += ir[i];
    }
    for v in ir.iter_mut() {
        *v /= sum;
    }
    ir
}

#[derive(Debug)]
pub struct WideFilter {
    params: WideParams,
    channel_indices: Vec<usize>,
    active: bool,
    gain_side_high: f32,
    headroom_factor: f32,
    mix: f32,
    air: AirAbsorption,
    hpf_l: FirstOrderHpf,
    hpf_r: FirstOrderHpf,
    itd_l: ItdDelay,
    itd_r: ItdDelay,
    /// ITD 前的线性相位 FIR 分频（1.5kHz）：把侧增强拆成
    /// 中低频直通 + 高频泛音区，分离相位干净。
    side_fir: FirSplit,
    /// 1 样本延迟（HPF 群延迟补偿）：上一帧 FIR 输出。
    orig_ll: f32,
    orig_hl: f32,
    orig_rl: f32,
    orig_hr: f32,
    /// 上一帧的限幅增量（与原始信号同步延迟 1 样本，保证同基准叠加）。
    dl_prev_l: f32,
    dl_prev_r: f32,
    side_attack_c: f32,
    side_release_c: f32,
    side_env: f32,
    fir: FirSplit,
}

impl WideFilter {
    pub fn new(params: WideParams) -> Self {
        Self {
            params,
            channel_indices: Vec::new(),
            active: false,
            gain_side_high: 1.0,
            headroom_factor: 1.0,
            mix: 0.6,
            air: AirAbsorption::new(0.0, 48000),
            hpf_l: FirstOrderHpf::new(200.0, 48000),
            hpf_r: FirstOrderHpf::new(200.0, 48000),
            itd_l: ItdDelay::new(ITD_DELAY_L),
            itd_r: ItdDelay::new(ITD_DELAY_R),
            side_fir: FirSplit::new(vec![1.0], 0),
            orig_ll: 0.0,
            orig_hl: 0.0,
            orig_rl: 0.0,
            orig_hr: 0.0,
            dl_prev_l: 0.0,
            dl_prev_r: 0.0,
            side_attack_c: 0.01,
            side_release_c: 0.0001,
            side_env: 0.0,
            fir: FirSplit::new(vec![1.0], 0),
        }
    }
}

impl Filter for WideFilter {
    fn initialize(&mut self, sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        if self.channel_indices.is_empty() {
            self.channel_indices = (0..channel_names.len()).collect();
        }
        let stereo = self.channel_indices.len() >= 2;
        let p0 = self.params;
        self.active = stereo && (p0.air > 0.0 || p0.gain > 0.0);
        if !self.active {
            return None;
        }

        let p = self.params;
        let gain = p.gain.clamp(0.0, 1.0);
        let air = p.air.clamp(0.0, 1.0);
        let mix = p.mix.clamp(0.0, 1.0);
        let xover = p.crossover_hz.clamp(CROSSOVER_MIN_HZ, CROSSOVER_MAX_HZ);
        let sr = sample_rate.max(1) as f32;
        // 高频侧增益完全由用户 Gain 控制。
        self.gain_side_high = 1.0 + SIDE_GAIN_HIGH_SLOPE * gain;
        self.mix = mix;
        // headroom 只随用户 Gain 变化：Gain 越大余量越小。
        let headroom_db = HEADROOM_MIN_DB + (HEADROOM_MAX_DB - HEADROOM_MIN_DB) * (1.0 - gain);
        self.headroom_factor = 10.0f32.powf(-headroom_db / 20.0);
        // 空气吸收深度只由 air 参数控制（物理距离曲线）。
        self.air = AirAbsorption::new(air, sample_rate);
        // 增量安全锁：一阶高通（截止 = 分频点）+ ITD 去相关延迟线。
        self.hpf_l = FirstOrderHpf::new(xover, sample_rate);
        self.hpf_r = FirstOrderHpf::new(xover, sample_rate);
        self.itd_l = ItdDelay::new(ITD_DELAY_L);
        self.itd_r = ItdDelay::new(ITD_DELAY_R);
        let side_ir = design_lowpass_ir(
            SIDE_ITD_CROSSOVER_HZ,
            sample_rate,
            side_fir_len(sample_rate),
        );
        self.side_fir = FirSplit::new(side_ir, 1);
        self.orig_ll = 0.0;
        self.orig_hl = 0.0;
        self.orig_rl = 0.0;
        self.orig_hr = 0.0;
        self.dl_prev_l = 0.0;
        self.dl_prev_r = 0.0;
        self.side_attack_c = 1.0 - (-1.0 / (SIDE_DYN_ATTACK_SECS * sr)).exp();
        self.side_release_c = 1.0 - (-1.0 / (SIDE_DYN_RELEASE_SECS * sr)).exp();
        self.side_env = 0.0;

        let ir = design_lowpass_ir(xover, sample_rate, wide_fir_len(sample_rate, xover));
        self.fir = FirSplit::new(ir, 2);
        None
    }

    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        if !self.active || self.channel_indices.len() < 2 {
            return;
        }
        let l = self.channel_indices[0];
        let r = self.channel_indices[1];
        if l >= samples.len() || r >= samples.len() {
            return;
        }
        let frame_count = frame_count.min(samples[l].len()).min(samples[r].len());

        let g_high = self.gain_side_high;
        let headroom = self.headroom_factor;
        let mix = self.mix;

        for f in 0..frame_count {
            let xl = samples[l][f];
            let xr = samples[r][f];

            // FIR 分频：低频直通不处理，高频做 M/S 宽度 + 空气吸收（中心距离）。
            let (ll, hl) = self.fir.split_channel(0, xl);
            let (rl, hr) = self.fir.split_channel(1, xr);
            // 1 样本延迟补偿（匹配一阶 HPF 群延迟）：干声与增量严格对齐。
            let p_ll = self.orig_ll;
            let p_hl = self.orig_hl;
            let p_rl = self.orig_rl;
            let p_hr = self.orig_hr;
            self.orig_ll = ll;
            self.orig_hl = hl;
            self.orig_rl = rl;
            self.orig_hr = hr;

            let mid_h = (hl + hr) * 0.5;
            let side_h = (hl - hr) * 0.5;

            // 动态 M/S：侧通道包络跟随，强侧信号自动收增益（防削波、保留动态）。
            let side_abs = side_h.abs();
            if side_abs > self.side_env {
                self.side_env += self.side_attack_c * (side_abs - self.side_env);
            } else {
                self.side_env += self.side_release_c * (side_abs - self.side_env);
            }
            let gr = if self.side_env > SIDE_DYN_THRESHOLD {
                (SIDE_DYN_THRESHOLD / self.side_env).powf(SIDE_DYN_RATIO)
            } else {
                1.0
            };
            // 只增强用户 Gain 指定的部分，原始侧信号保留；
            // 增强部分按频率分流（线性相位 FIR，1.5kHz）：
            // 中低频直通（保持实体感），泛音区过 ITD 延迟线
            // 去相关（左 +5 / 右 +7）。
            let boost = (g_high - 1.0) * gr;
            let boost_full = side_h * boost;
            let (boost_lp, boost_hp) = self.side_fir.split_channel(0, boost_full);
            let side_boost_l = boost_lp + self.itd_l.process(boost_hp);
            let side_boost_r = boost_lp + self.itd_r.process(boost_hp);
            let out_side_l = side_h + side_boost_l;
            let out_side_r = side_h + side_boost_r;
            // mid 走空气吸收（物理距离曲线）；不做静态负增益。
            let out_mid_h = self.air.next(mid_h);

            // 原始增量 → 一阶高通安全锁（截止 = 分频点）→ 先滤波后 tanh。
            let delta_l = out_mid_h + out_side_l - hl;
            let delta_r = out_mid_h - out_side_r - hr;
            let delta_hpf_l = self.hpf_l.process(delta_l);
            let delta_hpf_r = self.hpf_r.process(delta_r);
            let delta_limited_l = (delta_hpf_l * headroom).tanh() * mix;
            let delta_limited_r = (delta_hpf_r * headroom).tanh() * mix;
            // 干声叠加：原始高频与限幅增量同步延迟 1 样本，
            // 保持同一时间基准（避免高频相位错位）。
            let dl_prev = self.dl_prev_l;
            let dr_prev = self.dl_prev_r;
            self.dl_prev_l = delta_limited_l;
            self.dl_prev_r = delta_limited_r;
            let hf_l = p_hl + dl_prev;
            let hf_r = p_hr + dr_prev;
            // 低频干净直通（线性相位 FIR 分频不破坏瞬态）+ 输出端软膝限幅。
            samples[l][f] = output_soft_clip(p_ll + hf_l);
            samples[r][f] = output_soft_clip(p_rl + hf_r);
        }
    }

    fn latency(&self) -> u32 {
        // FIR 中心 + 1 样本（整体延迟补偿）。
        let base = match &self.fir.engine {
            LpEngine::Direct { center, .. } => *center as u32,
            LpEngine::Partitioned { latency, .. } => *latency as u32,
        };
        base + 1
    }

    fn set_channel_indices(&mut self, indices: &[usize]) {
        self.channel_indices = indices.to_vec();
    }

    fn reset(&mut self) {
        self.fir.reset();
        self.air.clear();
        self.hpf_l.clear();
        self.hpf_r.clear();
        self.itd_l.clear();
        self.itd_r.clear();
        self.side_fir.reset();
        self.orig_ll = 0.0;
        self.orig_hl = 0.0;
        self.orig_rl = 0.0;
        self.orig_hr = 0.0;
        self.dl_prev_l = 0.0;
        self.dl_prev_r = 0.0;
        self.side_env = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 临时诊断：Kaiser 分频在各采样率的停带表现（500Hz 衰减应 ≥-55dB）。
    #[test]
    fn kaiser_crossover_response() {
        fn lp_gain_db(ir: &[f32], freq: f32, sr: u32) -> f32 {
            let w = std::f32::consts::TAU * freq / sr as f32;
            let mut re = 0.0f32;
            let mut im = 0.0f32;
            for (i, &h) in ir.iter().enumerate() {
                re += h * (w * i as f32).cos();
                im -= h * (w * i as f32).sin();
            }
            20.0 * (re * re + im * im).sqrt().max(1e-6).log10()
        }
        for sr in [44_100u32, 48_000, 96_000, 192_000, 384_000] {
            let n = wide_fir_len(sr, 200.0);
            let ir = design_lowpass_ir(200.0, sr, n);
            eprintln!(
                "DIAG kaiser sr={} n={} lp200={:.1}dB lp500={:.1}dB",
                sr,
                n,
                lp_gain_db(&ir, 200.0, sr),
                lp_gain_db(&ir, 500.0, sr),
            );
        }
    }

    #[test]
    fn all_zero_is_passthrough() {
        let mut f = WideFilter::new(WideParams {
            gain: 0.0,
            air: 0.0,
            ..Default::default()
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let mut samples = vec![vec![0.4f32; 256], vec![0.2f32; 256]];
        let before = samples.clone();
        f.process(&mut samples, 256);
        for (a, b) in samples.iter().zip(before.iter()) {
            for (x, y) in a.iter().zip(b.iter()) {
                assert_eq!(*x, *y, "air 0 / gain 0 must be bit-exact passthrough");
            }
        }
    }

    #[test]
    fn mono_is_passthrough() {
        let mut f = WideFilter::new(WideParams { air: 1.0, ..Default::default() });
        f.initialize(48000, &["Mono".into()]);
        let mut samples = vec![vec![0.8f32; 64]];
        f.process(&mut samples, 64);
        for &v in &samples[0] {
            assert_eq!(v, 0.8);
        }
    }

    #[test]
    fn silence_stays_silent() {
        let mut f = WideFilter::new(WideParams { air: 1.0, ..Default::default() });
        f.initialize(48000, &["L".into(), "R".into()]);
        let mut samples = vec![vec![0.0f32; 4800], vec![0.0f32; 4800]];
        f.process(&mut samples, 4800);
        for ch in &samples {
            for &v in ch {
                assert_eq!(v, 0.0);
            }
        }
    }

    /// 稳态段上 |L-R| 的 RMS（宽度度量）。
    fn width_rms(samples: &[Vec<f32>], start: usize) -> f32 {
        let n = samples[0].len() - start;
        let mut sum = 0.0f32;
        for i in start..samples[0].len() {
            let d = samples[0][i] - samples[1][i];
            sum += d * d;
        }
        (sum / n as f32).sqrt()
    }

    #[test]
    fn side_signal_is_widened() {
        // 低频（300Hz，直通路径）与高频（5kHz，ITD 路径）带侧成分：
        // 宽度应随用户 Gain 单调递增（避开线性相位 FIR 分离的
        // 梳状谷频率，如 945Hz/1.9kHz）。
        let run = |gain: f32| -> f32 {
            let mut f = WideFilter::new(WideParams {
                gain,
                mix: 1.0,
                ..Default::default()
            });
            f.initialize(48000, &["L".into(), "R".into()]);
            let n = 4800usize;
            let mut samples = vec![vec![0.0f32; n], vec![0.0f32; n]];
            for i in 0..n {
                let t = i as f32 / 48000.0;
                let v = 0.4 * (core::f32::consts::TAU * 300.0 * t).sin()
                    + 0.3 * (core::f32::consts::TAU * 5000.0 * t).sin();
                samples[0][i] = 0.2 * v;
                samples[1][i] = 0.04 * v;
            }
            f.process(&mut samples, n);
            width_rms(&samples, 2000)
        };

        let base = run(0.0);
        let quarter = run(0.25);
        let half = run(0.5);
        let full = run(1.0);
        assert!(
            quarter > base && half > quarter && full > half,
            "width should grow monotonically with Gain: {base} < {quarter} < {half} < {full}"
        );
        assert!(
            full / base > 1.35,
            "full Gain should clearly widen: {full} vs {base}"
        );
        assert!(
            half / base > 1.1,
            "mid Gain should already widen: {half} vs {base}"
        );
    }

    #[test]
    fn gain_mapping_curve() {
        // 侧增益只随用户 Gain（1 + 1.5·Gain）。
        let setup = |gain: f32| -> f32 {
            let mut f = WideFilter::new(WideParams {
                gain,
                ..Default::default()
            });
            f.initialize(48000, &["L".into(), "R".into()]);
            f.gain_side_high
        };
        assert!((setup(0.0) - 1.0).abs() < 1e-5, "Gain=0 side must stay flat");
        assert!((setup(0.5) - 1.75).abs() < 1e-4);
        assert!((setup(1.0) - 2.5).abs() < 1e-4, "Gain=1 side gain should be 2.5x");
    }

    #[test]
    fn air_depth_drives_center_attenuation() {
        // 空气吸收深度只由 air 参数控制：8k 纯中置正弦，
        // air 越大衰减越深，air=0 时保持原样。
        fn center_8k_energy(air: f32) -> f32 {
            let mut f = WideFilter::new(WideParams {
                air,
                mix: 1.0,
                ..Default::default()
            });
            f.initialize(48000, &["L".into(), "R".into()]);
            let n = 9600usize;
            let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
            for i in 0..n {
                let v = 0.3 * (core::f32::consts::TAU * 8000.0 * i as f32 / 48000.0).sin();
                s[0][i] = v;
                s[1][i] = v;
            }
            f.process(&mut s, n);
            let mut e = 0.0f32;
            for i in 4800..n {
                let m = (s[0][i] + s[1][i]) * 0.5;
                e += m * m;
            }
            e
        }
        let e_none = center_8k_energy(0.0);
        let e_half = center_8k_energy(0.5);
        let e_full = center_8k_energy(1.0);
        assert!(e_none > 0.0);
        assert!(
            e_half < e_none * 0.95 && e_full < e_half * 0.95,
            "air should attenuate monotonically: none={e_none} half={e_half} full={e_full}"
        );
    }

    #[test]
    fn center_signal_preserved_without_air_and_symmetric() {
        // 纯中央信号、无空气吸收：mid 不做任何静态负增益，
        // 输出应保持原电平（无中频糊感来源）。
        let mut f = WideFilter::new(WideParams { air: 0.0, ..Default::default() });
        f.initialize(48000, &["L".into(), "R".into()]);
        let n = 4800usize;
        let mut samples = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = 0.1 * (core::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin();
            samples[0][i] = v;
            samples[1][i] = v;
        }
        f.process(&mut samples, n);
        let peak = samples[0][2000..]
            .iter()
            .fold(0.0f32, |m, &v| m.max(v.abs()));
        for i in 2000..n {
            assert!(
                (samples[0][i] - samples[1][i]).abs() < 1e-6,
                "center signal must stay symmetric"
            );
        }
        // 峰值应接近输入 0.1（无负增益、无梳状、无压缩）。
        assert!(
            peak > 0.095 && peak < 0.105,
            "center must be preserved without air, peak {peak}"
        );
    }

    #[test]
    fn bass_keeps_energy_and_width_highs_widened() {
        // 低频干净直通：60 Hz 带侧成分原样通过（线性相位 FIR 不破坏瞬态）；
        // 1 kHz 侧成分被用户 Gain 放大。
        let run = |freq: f32, n: usize| -> (f32, f32) {
            let mut f = WideFilter::new(WideParams {
                gain: 1.0,
                mix: 1.0,
                ..Default::default()
            });
            f.initialize(48000, &["L".into(), "R".into()]);
            let mut samples = vec![vec![0.0f32; n], vec![0.0f32; n]];
            for i in 0..n {
                let v = (core::f32::consts::TAU * freq * i as f32 / 48000.0).sin();
                samples[0][i] = 0.5 * v;
                samples[1][i] = 0.1 * v;
            }
            f.process(&mut samples, n);
            let mut diff = 0.0f32;
            let mut peak_l = 0.0f32;
            for i in (n / 2)..n {
                diff = diff.max((samples[0][i] - samples[1][i]).abs());
                peak_l = peak_l.max(samples[0][i].abs());
            }
            (diff, peak_l)
        };

        let (diff_low, peak_low) = run(60.0, 9600);
        let (diff_high, _) = run(5000.0, 4800);
        assert!(
            (peak_low - 0.5).abs() < 0.02,
            "bass must stay at original level: peak {peak_low}"
        );
        assert!(
            (diff_low - 0.4).abs() < 0.03,
            "bass keeps original stereo info: diff {diff_low}"
        );
        // 5kHz 在 ITD 泛音区（避开线性相位 FIR 分离的梳状谷）。
        assert!(diff_high > 0.45, "highs should be widened: high {diff_high}");
    }

    #[test]
    fn fir_split_reconstructs_delayed_input() {
        // 线性相位 FIR 完美重建：低频支路 + 高频支路 = 延迟 center 帧的原信号
        // （逐样本，含相位——这是 FIR 相对 IIR 分频的核心优势）。
        let mut f = WideFilter::new(WideParams { air: 1.0, ..Default::default() });
        f.initialize(48000, &["L".into(), "R".into()]);
        // 只测 FIR 分频本身：用 FIR 群延迟中心，不含 Haas 延迟。
        let center = ((wide_fir_len(48000, 200.0) - 1) / 2) as usize;
        let n = 4800usize;
        let input: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / 48000.0;
                0.3 * (core::f32::consts::TAU * 60.0 * t).sin()
                    + 0.4 * (core::f32::consts::TAU * 1000.0 * t).sin()
                    + 0.2 * (core::f32::consts::TAU * 6000.0 * t).sin()
            })
            .collect();
        let mut max_err = 0.0f32;
        for i in 0..n {
            let (lp, hp) = f.fir.split_channel(0, input[i]);
            if i >= center {
                max_err = max_err.max((lp + hp - input[i - center]).abs());
            }
        }
        assert!(
            max_err < 1.0e-4,
            "FIR split must reconstruct delayed input, max err {max_err}"
        );
    }

    #[test]
    fn fir_split_reconstructs_partitioned_high_rate() {
        // 192k：4096 抽头走分块 FFT——低通支路 + 互补高通必须仍等于
        // 延迟 latency 帧的原信号（延迟对齐环正确性）。
        let mut f = WideFilter::new(WideParams { air: 1.0, ..Default::default() });
        f.initialize(192_000, &["L".into(), "R".into()]);
        // 192k/200Hz：8192 抽头走分块 FFT；分块延迟 = 报告延迟 − 1
        // （报告值含 1 样本整体补偿）。
        let latency = (f.latency() - 1) as usize;
        let n = 4000usize;
        let input: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / 192_000.0;
                0.3 * (core::f32::consts::TAU * 60.0 * t).sin()
                    + 0.4 * (core::f32::consts::TAU * 1000.0 * t).sin()
                    + 0.2 * (core::f32::consts::TAU * 6000.0 * t).sin()
            })
            .collect();
        let mut max_err = 0.0f32;
        for i in 0..n {
            let (lp, hp) = f.fir.split_channel(0, input[i]);
            if i >= latency {
                max_err = max_err.max((lp + hp - input[i - latency]).abs());
            }
        }
        assert!(
            max_err < 1.0e-4,
            "partitioned split must reconstruct delayed input, max err {max_err}"
        );
    }

    #[test]
    fn fir_delays_by_center_samples() {
        // 单脉冲经低通 FIR 的主峰应出现在 center 帧（对称 FIR 群延迟）。
        let mut f = WideFilter::new(WideParams { air: 1.0, ..Default::default() });
        f.initialize(48000, &["L".into(), "R".into()]);
        let center = ((wide_fir_len(48000, 200.0) - 1) / 2) as usize;
        let n = center + 128;
        let mut best = 0usize;
        let mut best_v = 0.0f32;
        for i in 0..n {
            let x = if i == 0 { 1.0 } else { 0.0 };
            let (lp, _) = f.fir.split_channel(0, x);
            if lp.abs() > best_v {
                best_v = lp.abs();
                best = i;
            }
        }
        assert_eq!(best, center, "LP peak should be at FIR center");
    }

    #[test]
    fn latency_is_fir_center() {
        let mut f = WideFilter::new(WideParams { air: 1.0, ..Default::default() });
        f.initialize(48000, &["L".into(), "R".into()]);
        // FIR 中心 + 1 样本（HPF 群延迟补偿）。
        assert_eq!(f.latency(), ((wide_fir_len(48000, 200.0) - 1) / 2) as u32 + 1);
    }

    #[test]
    fn air_absorption_follows_physical_curve() {
        // 纯中置输入（L==R）：空气吸收应随频率升高衰减加深（物理曲线形状），
        // 且比原单极点低通柔和——1k 处衰减不超过 -3dB，16k 处明显更深。
        fn tone_attenuation_db(air: f32, freq: f32) -> f32 {
            let mut f = WideFilter::new(WideParams {
                air,
                mix: 1.0,
                ..Default::default()
            });
            f.initialize(48000, &["L".into(), "R".into()]);
            let n = 19200usize;
            let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
            for i in 0..n {
                let v = 0.5 * (core::f32::consts::TAU * freq * i as f32 / 48000.0).sin();
                s[0][i] = v;
                s[1][i] = v;
            }
            f.process(&mut s, n);
            // 稳态段（跳过 FIR 中心 + 滤波器暂态）RMS。
            let start = 9600usize;
            let mut sum = 0.0f32;
            for i in start..n {
                let m = (s[0][i] + s[1][i]) * 0.5;
                sum += m * m;
            }
            let out_rms = (sum / (n - start) as f32).sqrt();
            let in_rms = 0.5 / core::f32::consts::SQRT_2;
            20.0 * (out_rms / in_rms).max(1e-6).log10()
        }

        let att_1k = tone_attenuation_db(1.0, 1000.0);
        let att_4k = tone_attenuation_db(1.0, 4000.0);
        let att_8k = tone_attenuation_db(1.0, 8000.0);
        let att_16k = tone_attenuation_db(1.0, 16000.0);
        // 物理曲线形状：衰减随频率单调加深，且 16k 明显深于 1k。
        assert!(
            att_4k < att_1k - 1.0 && att_8k < att_4k - 0.5 && att_16k < att_8k - 1.5,
            "air curve should deepen with frequency: 1k={att_1k:.2} 4k={att_4k:.2} \
             8k={att_8k:.2} 16k={att_16k:.2} dB"
        );
        // 柔和性：满档也不至于完全闷死。
        assert!(
            att_16k < -8.0 && att_16k > -20.0,
            "16k attenuation out of physical range: {att_16k:.2} dB"
        );
        // 单调性：air 越大衰减越深。
        fn air_only_attenuation_db(air: f32) -> f32 {
            let mut f = WideFilter::new(WideParams {
                air,
                mix: 1.0,
                ..Default::default()
            });
            f.initialize(48000, &["L".into(), "R".into()]);
            let n = 9600usize;
            let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
            for i in 0..n {
                let v = 0.5 * (core::f32::consts::TAU * 8000.0 * i as f32 / 48000.0).sin();
                s[0][i] = v;
                s[1][i] = v;
            }
            f.process(&mut s, n);
            let mut sum = 0.0f32;
            for i in 4800..n {
                let m = (s[0][i] + s[1][i]) * 0.5;
                sum += m * m;
            }
            let out_rms = (sum / 4800.0).sqrt();
            let in_rms = 0.5 / core::f32::consts::SQRT_2;
            20.0 * (out_rms / in_rms).max(1e-6).log10()
        }
        let att_8k_half = air_only_attenuation_db(0.5);
        assert!(att_8k_half > att_8k, "more air should attenuate deeper: {att_8k_half} vs {att_8k}");
    }

    #[test]
    fn air_zero_is_center_passthrough() {
        // air=0 时空气吸收级联完全旁路：中声道逐样本位精确直通（FIR 分频除外）。
        let mut f = WideFilter::new(WideParams {
            air: 0.0,
            ..Default::default()
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        // 跳过 FIR 分频 + 梳状延迟的启动瞬态，只验证稳态对称性。
        let n = 4096usize;
        let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = (core::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin() * 0.3;
            s[0][i] = v;
            s[1][i] = v;
        }
        f.process(&mut s, n);
        for i in 2048..n {
            assert!(
                (s[0][i] - s[1][i]).abs() < 1e-6,
                "air=0 must not break mid symmetry at {i}"
            );
        }
    }

    #[test]
    fn air_adds_negligible_harmonics() {
        // 空气吸收失真检查：纯中置 1k 正弦 + 满空气，输出谐波应极小。
        // （RBJ 架 + Butterworth 低通都是线性滤波器；只要 tanh 未饱和就不产生谐波。）
        let mut f = WideFilter::new(WideParams {
            air: 1.0,
            ..Default::default()
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let n = 9600usize;
        let mut samples = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = 0.2 * (core::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin();
            samples[0][i] = v;
            samples[1][i] = v;
        }
        f.process(&mut samples, n);

        // 输出稳态段 2k/3k 谐波幅度（4800..9600 = 100 个 1k 整周期）。
        let amp = |freq: f32| -> f32 {
            let mut re = 0.0f32;
            let mut im = 0.0f32;
            for i in 4800..n {
                let t = i as f32 / 48000.0;
                let w = core::f32::consts::TAU * freq * t;
                re += samples[0][i] * w.cos();
                im += samples[0][i] * w.sin();
            }
            2.0 * (re * re + im * im).sqrt() / 4800.0
        };
        assert!(
            amp(2000.0) < 2.0e-3,
            "air must not add 2nd harmonic: {}",
            amp(2000.0)
        );
        assert!(
            amp(3000.0) < 2.0e-3,
            "air must not add 3rd harmonic: {}",
            amp(3000.0)
        );
    }

    #[test]
    fn crossover_endpoints_are_bounded() {
        for xover in [200.0f32, 1000.0] {
            let mut f = WideFilter::new(WideParams {
                gain: 1.0,
                air: 1.0,
                mix: 1.0,
                crossover_hz: xover,
            });
            f.initialize(48000, &["L".into(), "R".into()]);
            let n = 4800usize;
            let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
            for i in 0..n {
                let v = 0.9 * (core::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin();
                s[0][i] = v;
                s[1][i] = -v;
            }
            f.process(&mut s, n);
            for ch in &s {
                for &v in ch {
                    assert!(v.is_finite());
                    assert!(v.abs() < 4.0, "crossover {xover}: bounded, got {v}");
                }
            }
        }
    }

    #[test]
    fn out_of_range_params_are_clamped() {
        let mut f = WideFilter::new(WideParams {
            gain: 2.0,
            air: -1.0,
            mix: 2.0,
            crossover_hz: 99999.0,
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let n = 4800usize;
        let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = 0.8 * (core::f32::consts::TAU * 500.0 * i as f32 / 48000.0).sin();
            s[0][i] = v;
            s[1][i] = v * 0.5;
        }
        f.process(&mut s, n);
        for ch in &s {
            for &v in ch {
                assert!(v.is_finite(), "clamped params must stay finite");
            }
        }
    }

    #[test]
    fn low_level_is_transparent() {
        // 低电平下 tanh 近似线性：2 次/3 次谐波应可忽略。
        let mut f = WideFilter::new(WideParams { air: 1.0, ..Default::default() });
        f.initialize(48000, &["L".into(), "R".into()]);
        let n = 9600usize;
        let mut samples = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = 0.05 * (core::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin();
            samples[0][i] = v;
            samples[1][i] = 0.01 * v;
        }
        f.process(&mut samples, n);

        // 计算左声道 2k/3k 谱幅度（窗口 4800..9600 = 100 整周期）。
        let amp = |freq: f32| -> f32 {
            let mut re = 0.0f32;
            let mut im = 0.0f32;
            for i in 4800..n {
                let t = i as f32 / 48000.0;
                let w = core::f32::consts::TAU * freq * t;
                re += samples[0][i] * w.cos();
                im += samples[0][i] * w.sin();
            }
            2.0 * (re * re + im * im).sqrt() / 4800.0
        };
        assert!(amp(2000.0) < 1.0e-3, "2nd harmonic should be absent");
        assert!(amp(3000.0) < 1.0e-3, "3rd harmonic should be absent");
    }

    #[test]
    fn first_order_hpf_response() {
        // 一阶高通安全锁：截止频率处 -3dB；半截止处约 -7dB
        // （6dB/oct 是渐近近似，单极点真实值 20log10(0.5/√1.25)≈-7dB）。
        fn hpf_gain_db(fc: f32, freq: f32) -> f32 {
            let mut h = FirstOrderHpf::new(fc, 48000);
            let n = 96_000usize;
            let mut sum = 0.0f32;
            let mut i = 0;
            while i < n {
                let x = (core::f32::consts::TAU * freq * i as f32 / 48000.0).sin();
                let y = h.process(x);
                if i >= 48_000 {
                    sum += y * y;
                }
                i += 1;
            }
            let out_rms = (sum / 48_000.0).sqrt();
            let in_rms = 1.0 / core::f32::consts::SQRT_2;
            20.0 * (out_rms / in_rms).log10()
        }
        for fc in [200.0f32, 500.0, 1000.0] {
            let at_fc = hpf_gain_db(fc, fc);
            let at_half = hpf_gain_db(fc, fc * 0.5);
            assert!(
                (at_fc + 3.0).abs() < 0.5,
                "HPF at Fc={fc} should be -3dB, got {at_fc:.2}"
            );
            assert!(
                (at_half + 7.0).abs() < 0.5,
                "HPF at 0.5Fc={} should be about -7dB, got {at_half:.2}",
                fc * 0.5
            );
        }
    }

    #[test]
    fn itd_delays_differ_by_two_samples() {
        // ITD 去相关：左 +5、右 +7 样本，脉冲峰值位置差 2。
        let mut l = ItdDelay::new(ITD_DELAY_L);
        let mut r = ItdDelay::new(ITD_DELAY_R);
        let mut l_peak = usize::MAX;
        let mut r_peak = usize::MAX;
        for i in 0..32 {
            let x = if i == 0 { 1.0 } else { 0.0 };
            if l.process(x) > 0.5 {
                l_peak = i;
            }
            if r.process(x) > 0.5 {
                r_peak = i;
            }
        }
        assert_eq!(l_peak, ITD_DELAY_L);
        assert_eq!(r_peak, ITD_DELAY_R);
        assert_eq!(r_peak - l_peak, 2, "L/R ITD must differ by 2 samples");
    }

    #[test]
    fn side_fir_splits_at_1k5() {
        // ITD 限频的线性相位 FIR 分离：500Hz 归低频支路（hp≈0），
        // 4kHz 归高频支路（lp≈0），保证 ITD 只作用于泛音区。
        fn hp_energy_ratio(freq: f32) -> f32 {
            let ir = design_lowpass_ir(SIDE_ITD_CROSSOVER_HZ, 48000, side_fir_len(48000));
            let mut fir = FirSplit::new(ir, 1);
            let n = 4800usize;
            let mut lp_sum = 0.0f32;
            let mut hp_sum = 0.0f32;
            for i in 0..n {
                let x = 0.5 * (core::f32::consts::TAU * freq * i as f32 / 48000.0).sin();
                let (lp, hp) = fir.split_channel(0, x);
                if i >= 2400 {
                    lp_sum += lp * lp;
                    hp_sum += hp * hp;
                }
            }
            (hp_sum / (lp_sum + hp_sum + 1e-9)).sqrt()
        }
        assert!(
            hp_energy_ratio(500.0) < 0.1,
            "500Hz must stay in the straight-through path"
        );
        assert!(
            hp_energy_ratio(4000.0) > 0.9,
            "4kHz must go through the ITD path"
        );
    }

    #[test]
    fn mix_zero_is_delayed_passthrough() {
        // Mix=0：增量完全不入，整条链路退化为纯延迟——
        // 输出 = 输入延迟 (FIR 中心 + 1) 帧（忽略 FIR 浮点重建误差）。
        let mut f = WideFilter::new(WideParams {
            air: 0.354331,
            mix: 0.0,
            ..Default::default()
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let center = ((wide_fir_len(48000, 200.0) - 1) / 2) as usize + 1;
        let n = 4800usize;
        let mut input = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let t = i as f32 / 48000.0;
            input[0][i] = 0.2 * (core::f32::consts::TAU * 440.0 * t).sin()
                + 0.1 * (core::f32::consts::TAU * 2200.0 * t).sin();
            input[1][i] = 0.15 * (core::f32::consts::TAU * 330.0 * t).sin()
                + 0.08 * (core::f32::consts::TAU * 5000.0 * t).sin();
        }
        let mut out = input.clone();
        f.process(&mut out, n);
        for ch in 0..2 {
            for i in center..n {
                let err = (out[ch][i] - input[ch][i - center]).abs();
                assert!(
                    err < 1e-5,
                    "mix=0 must be delayed passthrough: ch{ch} i={i} err={err}"
                );
            }
        }
    }

    #[test]
    fn reset_clears_all_state() {
        // 处理非零信号后 reset，再喂静音必须无状态残留。
        let mut f = WideFilter::new(WideParams {
            air: 1.0,
            gain: 1.0,
            ..Default::default()
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let mut s = vec![vec![0.0f32; 2048], vec![0.0f32; 2048]];
        for i in 0..2048 {
            let v = 0.5 * (core::f32::consts::TAU * 300.0 * i as f32 / 48000.0).sin();
            s[0][i] = v;
            s[1][i] = -v;
        }
        f.process(&mut s, 2048);
        f.reset();
        let mut z = vec![vec![0.0f32; 256], vec![0.0f32; 256]];
        f.process(&mut z, 256);
        for ch in &z {
            for &v in ch {
                assert_eq!(v, 0.0, "reset must clear all filter state");
            }
        }
    }

    #[test]
    fn extreme_antiphase_is_bounded() {
        // 1 kHz 纯反相、用户 Gain 满档：输出应被软限幅约束且明显放大。
        let mut f = WideFilter::new(WideParams {
            air: 1.0,
            gain: 1.0,
            mix: 1.0,
            ..Default::default()
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let n = 4800usize;
        let mut samples = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = 0.9 * (core::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin();
            samples[0][i] = v;
            samples[1][i] = -v;
        }
        f.process(&mut samples, n);
        let mut peak = 0.0f32;
        for ch in &samples {
            for &v in ch {
                assert!(v.is_finite());
                // 低频支路原样相加允许 ±1.1 级轻微超限（高频支路已被 tanh 限制）。
                assert!(v.abs() <= 1.1 + 1e-6, "soft limiter must bound output: {v}");
                peak = peak.max(v.abs());
            }
        }
        assert!(peak > 0.9, "full-width antiphase should be strongly widened, peak {peak}");
    }

    #[test]
    fn wide_material_preserves_dynamics() {
        // 动态 M/S：低于包络阈值的侧信号保持线性（动态不被压），
        // 超过阈值后自动收增益（防削波），这是动态 M/S 的核心行为。
        let run = |amp: f32| -> f32 {
            let mut f = WideFilter::new(WideParams {
                air: 1.0,
                gain: 1.0,
                mix: 1.0,
                ..Default::default()
            });
            f.initialize(48000, &["L".into(), "R".into()]);
            let n = 9600usize;
            let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
            for i in 0..n {
                let v = 0.9 * (core::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin();
                s[0][i] = amp * v;
                s[1][i] = -amp * v;
            }
            f.process(&mut s, n);
            let pk = |ch: &[f32]| ch[4800..].iter().fold(0.0f32, |m, &x| m.max(x.abs()));
            (pk(&s[0]) + pk(&s[1])) * 0.5
        };
        let p_lo = run(0.1);
        let p_mid = run(0.2);
        let p_hi = run(0.5);
        // 低电平（阈值下）：2 倍输入 → 输出接近 2 倍。
        let ratio_linear = p_mid / p_lo;
        assert!(
            ratio_linear > 1.7 && ratio_linear < 2.3,
            "below threshold dynamics should scale linearly: ratio {ratio_linear}"
        );
        // 高电平（阈值上）：动态增益介入，输出不再线性放大。
        let ratio_compressed = p_hi / p_mid;
        assert!(
            ratio_compressed < 2.2,
            "loud side should be dynamically tamed (linear would be 2.5): ratio {ratio_compressed}"
        );
    }

    #[test]
    fn deterministic_and_reproducible() {
        let run = || -> Vec<f32> {
            let mut f = WideFilter::new(WideParams { air: 0.7, ..Default::default() });
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
    fn finite_across_params_and_sample_rates() {
        for sr in [44_100u32, 48_000, 96_000] {
            for air in [0.0f32, 0.354331, 0.7, 1.0] {
                let mut f = WideFilter::new(WideParams { air, ..Default::default() });
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

}
