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
//!    样本时间差；中低频增强直通保持实体感）；侧输出再走
//!    `air_side` 空气吸收（先高频补偿后吸收，与 mid 同曲线）；
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
    /// 侧通道空气吸收（0..1，作用于高频补偿后的侧输出，默认 0 = 关闭）。
    pub air_side: f32,
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
            air_side: 0.0,
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
    /// 侧通道空气吸收（L/R 各一实例；先高频补偿后吸收）。
    side_air_l: AirAbsorption,
    side_air_r: AirAbsorption,
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
            side_air_l: AirAbsorption::new(0.0, 48000),
            side_air_r: AirAbsorption::new(0.0, 48000),
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
        self.active = stereo && (p0.air > 0.0 || p0.air_side > 0.0 || p0.gain > 0.0);
        if !self.active {
            return None;
        }

        let p = self.params;
        let gain = p.gain.clamp(0.0, 1.0);
        let air = p.air.clamp(0.0, 1.0);
        let air_side = p.air_side.clamp(0.0, 1.0);
        let mix = p.mix.clamp(0.0, 1.0);
        let xover = p.crossover_hz.clamp(CROSSOVER_MIN_HZ, CROSSOVER_MAX_HZ);
        let sr = sample_rate.max(1) as f32;
        // 高频侧增益完全由用户 Gain 控制。
        self.gain_side_high = 1.0 + SIDE_GAIN_HIGH_SLOPE * gain;
        self.mix = mix;
        // headroom 只随用户 Gain 变化：Gain 越大余量越小。
        let headroom_db = HEADROOM_MIN_DB + (HEADROOM_MAX_DB - HEADROOM_MIN_DB) * (1.0 - gain);
        self.headroom_factor = 10.0f32.powf(-headroom_db / 20.0);
        // 空气吸收深度由 air（mid）/ air_side（侧）参数控制（物理距离曲线）。
        self.air = AirAbsorption::new(air, sample_rate);
        self.side_air_l = AirAbsorption::new(air_side, sample_rate);
        self.side_air_r = AirAbsorption::new(air_side, sample_rate);
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
            // 先高频补偿（gain 增强）再侧空气吸收（与 mid 同曲线）。
            let out_side_l = self.side_air_l.next(side_h + side_boost_l);
            let out_side_r = self.side_air_r.next(side_h + side_boost_r);
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
        self.side_air_l.clear();
        self.side_air_r.clear();
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
mod tests;
