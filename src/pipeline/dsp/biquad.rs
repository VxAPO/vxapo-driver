//! dsp/filters/biquad.rs — 双二阶滤波器基础实现（Note 13/53/58）
//!
//! 提供三种经典双二阶结构：
//! - Direct Form I（直接形式 I）
//! - Direct Form II（直接形式 II）
//! - Direct Form II Transposed（转置直接形式 II，生产默认）
//!
//! 系数计算：均基于传递函数 H(z) 的标准公式（RBJ Audio EQ Cookbook），
//! **全程 f64 中间计算**，消除 `fc << sr` 时 `cos(w0) ≈ 1` 的精度丢失；
//! 完成后做有限性 + 极点稳定性校验，非法回退直通（P0 护栏）。
//!
//! 生产路径（DF2T）使用 `repr(C)` f64 `BiquadState`：16 字节固定布局、
//! FMA 单指令、硬件 FTZ/DAZ 下无需逐采样冲刷次正规数。
//!
//! 此模块仅依赖 `f32/f64`、`Filter` trait 与 `dsp/math.rs`，不依赖 Windows API。

use crate::pipeline::dsp::filter::Filter;
use crate::pipeline::dsp::math::{
    FILTER_FREQ_MAX_RATIO, FILTER_FREQ_MIN_HZ, Q_MAX, Q_MIN, clamp_gain_db, is_stable_biquad,
    warn_rate_limited,
};

// ══════════════════════════════════════════════════════════════════════════════
// Biquad 系数
// ══════════════════════════════════════════════════════════════════════════════

/// 双二阶滤波器系数。
///
/// 标准传递函数：
/// ```text
///        b0 + b1*z^-1 + b2*z^-2
/// H(z) = ------------------------
///        a0 + a1*z^-1 + a2*z^-2
/// ```
///
/// `a0` 归一化为 1.0（所有系数已除以 a0）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BiquadCoeffs {
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
    pub a1: f32,
    pub a2: f32,
}

impl BiquadCoeffs {
    /// 直通系数（不修改信号）。
    pub const BYPASS: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    /// 检查系数是否有效（非 NaN / Inf）。
    pub fn is_valid(&self) -> bool {
        self.b0.is_finite()
            && self.b1.is_finite()
            && self.b2.is_finite()
            && self.a1.is_finite()
            && self.a2.is_finite()
    }
}

impl Default for BiquadCoeffs {
    fn default() -> Self {
        Self::BYPASS
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 系数计算函数（供 hp_lp / peq / graph_eq 使用）
// ══════════════════════════════════════════════════════════════════════════════

/// 滤波器类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BiquadType {
    Peaking,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
    BandPass,
    Notch,
    AllPass,
}

/// 计算双二阶系数。
///
/// # 参数
///
/// - `filter_type`：滤波器类型
/// - `fc`：中心/截止频率（Hz），自动 clamp 到 `[FILTER_FREQ_MIN_HZ, 0.45*sr]`
/// - `gain_db`：增益（dB），经 `clamp_gain_db`（仅 Peaking / LowShelf / HighShelf 有意义）
/// - `q`：品质因数，自动 clamp 到 `[Q_MIN, Q_MAX]`
/// - `sample_rate`：采样率（Hz），0 → 直通
///
/// 所有公式基于 Robert Bristow-Johnson 的 Audio EQ Cookbook，
/// **全程 f64 中间计算**，最后转 f32。
/// 归一化后做有限性 + 极点稳定性校验，非法回退 `BYPASS`（P0 护栏）。
pub fn compute_coeffs(
    filter_type: BiquadType,
    fc: f32,
    gain_db: f32,
    q: f32,
    sample_rate: u32,
) -> BiquadCoeffs {
    if sample_rate == 0 {
        return BiquadCoeffs::BYPASS;
    }

    // 入口校验 + clamp（非 RT 路径）。
    let fc = fc.clamp(
        FILTER_FREQ_MIN_HZ,
        (sample_rate as f32 * FILTER_FREQ_MAX_RATIO).max(FILTER_FREQ_MIN_HZ),
    );
    let q = q.clamp(Q_MIN, Q_MAX);
    let gain_db = clamp_gain_db(gain_db);
    if !fc.is_finite() {
        return BiquadCoeffs::BYPASS;
    }

    let sr = sample_rate as f64;
    let f = fc as f64;
    let qq = q as f64;
    let g = gain_db as f64;

    let w0 = 2.0 * std::f64::consts::PI * f / sr;
    let cos_w0 = w0.cos();
    let sin_w0 = w0.sin();
    let alpha = sin_w0 / (2.0 * qq);

    // 增益因子：`sqrt_a` 两次相乘恒等于 `10^(dB/40)`，统一路径避免大增益下 `a²` 溢出。
    let sqrt_a = 10.0_f64.powf(g / 80.0);
    let a = sqrt_a * sqrt_a;

    let (b0, b1, b2, a0, a1, a2) = match filter_type {
        BiquadType::Peaking => {
            let b0 = 1.0 + alpha * a;
            let b1 = -2.0 * cos_w0;
            let b2 = 1.0 - alpha * a;
            let a0 = 1.0 + alpha / a;
            let a1 = -2.0 * cos_w0;
            let a2 = 1.0 - alpha / a;
            (b0, b1, b2, a0, a1, a2)
        }
        BiquadType::LowShelf => {
            let sq = 2.0 * a.sqrt() * alpha;
            let b0 = a * ((a + 1.0) - (a - 1.0) * cos_w0 + sq);
            let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0);
            let b2 = a * ((a + 1.0) - (a - 1.0) * cos_w0 - sq);
            let a0 = (a + 1.0) + (a - 1.0) * cos_w0 + sq;
            let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0);
            let a2 = (a + 1.0) + (a - 1.0) * cos_w0 - sq;
            (b0, b1, b2, a0, a1, a2)
        }
        BiquadType::HighShelf => {
            let sq = 2.0 * a.sqrt() * alpha;
            let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w0 + sq);
            let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0);
            let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w0 - sq);
            let a0 = (a + 1.0) - (a - 1.0) * cos_w0 + sq;
            let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0);
            let a2 = (a + 1.0) - (a - 1.0) * cos_w0 - sq;
            (b0, b1, b2, a0, a1, a2)
        }
        BiquadType::LowPass => {
            let b0 = (1.0 - cos_w0) / 2.0;
            let b1 = 1.0 - cos_w0;
            let b2 = (1.0 - cos_w0) / 2.0;
            let a0 = 1.0 + alpha;
            let a1 = -2.0 * cos_w0;
            let a2 = 1.0 - alpha;
            (b0, b1, b2, a0, a1, a2)
        }
        BiquadType::HighPass => {
            let b0 = (1.0 + cos_w0) / 2.0;
            let b1 = -(1.0 + cos_w0);
            let b2 = (1.0 + cos_w0) / 2.0;
            let a0 = 1.0 + alpha;
            let a1 = -2.0 * cos_w0;
            let a2 = 1.0 - alpha;
            (b0, b1, b2, a0, a1, a2)
        }
        BiquadType::BandPass => {
            let b0 = alpha;
            let b1 = 0.0;
            let b2 = -alpha;
            let a0 = 1.0 + alpha;
            let a1 = -2.0 * cos_w0;
            let a2 = 1.0 - alpha;
            (b0, b1, b2, a0, a1, a2)
        }
        BiquadType::Notch => {
            let b0 = 1.0;
            let b1 = -2.0 * cos_w0;
            let b2 = 1.0;
            let a0 = 1.0 + alpha;
            let a1 = -2.0 * cos_w0;
            let a2 = 1.0 - alpha;
            (b0, b1, b2, a0, a1, a2)
        }
        BiquadType::AllPass => {
            let b0 = 1.0 - alpha;
            let b1 = -2.0 * cos_w0;
            let b2 = 1.0 + alpha;
            let a0 = 1.0 + alpha;
            let a1 = -2.0 * cos_w0;
            let a2 = 1.0 - alpha;
            (b0, b1, b2, a0, a1, a2)
        }
    };

    normalize_checked(b0, b1, b2, a0, a1, a2)
}

/// 归一化系数（除以 a0），带有限性 + 稳定性护栏。
fn normalize_checked(
    b0: f64,
    b1: f64,
    b2: f64,
    a0: f64,
    a1: f64,
    a2: f64,
) -> BiquadCoeffs {
    if !a0.is_finite() || a0.abs() < 1e-30 {
        warn_rate_limited("biquad_invalid_a0", "biquad 系数 a0 非法，已回退直通");
        return BiquadCoeffs::BYPASS;
    }

    let inv_a0 = 1.0 / a0;
    let coeffs = BiquadCoeffs {
        b0: (b0 * inv_a0) as f32,
        b1: (b1 * inv_a0) as f32,
        b2: (b2 * inv_a0) as f32,
        a1: (a1 * inv_a0) as f32,
        a2: (a2 * inv_a0) as f32,
    };

    if !coeffs.is_valid() || !is_stable_biquad(coeffs.a1, coeffs.a2) {
        warn_rate_limited(
            "biquad_unstable",
            "biquad 系数超出稳定范围，已回退直通（参数被 clamp 后仍贴单位圆）",
        );
        return BiquadCoeffs::BYPASS;
    }

    coeffs
}

// ══════════════════════════════════════════════════════════════════════════════
// BiquadState（生产 DF2T 状态，f64）
// ══════════════════════════════════════════════════════════════════════════════

/// DF2T 转置直接形式 II 的通道状态（f64）。
///
/// `repr(C)`：`s1/s2` 两个 f64，固定 16 字节、8 字节对齐，无 tag/padding；
/// 31 段级联数组 496 B 连续内存，cache 友好。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BiquadState {
    s1: f64,
    s2: f64,
}

impl BiquadState {
    /// 创建零状态。
    #[inline]
    pub fn new() -> Self {
        Self { s1: 0.0, s2: 0.0 }
    }

    /// 清零状态。
    #[inline]
    pub fn clear(&mut self) {
        self.s1 = 0.0;
        self.s2 = 0.0;
    }

    /// 处理单个采样（DF2T，f64 FMA）。
    ///
    /// 输出非有限时清零本通道状态并返回 0.0（哨声兜底，正常路径不命中）。
    #[inline]
    pub fn process_sample(&mut self, coeffs: &BiquadCoeffs, input: f32) -> f32 {
        let x = input as f64;
        let b0 = coeffs.b0 as f64;
        let b1 = coeffs.b1 as f64;
        let b2 = coeffs.b2 as f64;
        let a1 = coeffs.a1 as f64;
        let a2 = coeffs.a2 as f64;

        let out = b0.mul_add(x, self.s1);
        self.s1 = b1.mul_add(x, (-a1).mul_add(out, self.s2));
        self.s2 = b2.mul_add(x, -(a2 * out));

        let out32 = out as f32;
        if !out32.is_finite() {
            self.clear();
            0.0
        } else {
            out32
        }
    }
}

impl Default for BiquadState {
    fn default() -> Self {
        Self::new()
    }
}

/// 立体声水平 SIMD：当滤波器自身作用域恰好为 2 个通道时（默认立体声，
/// 或显式 `Channel: L R` 选中两个通道），两个通道共用同一份系数，
/// 用 `__m128d` 两路 f64 并行处理。
///
/// **注意**：`Channel:` 语义是选择通道应用滤波——若作用域为 1 个通道
/// （如 `Channel: L` 或 `Channel: R`），`num_channels == 1`，本路径不触发，
/// 由标量路径处理，不会误碰另一通道。
///
/// 平面缓冲布局下每采样需要 2 次装载 + 2 次提取，但滤波器算术（5 次乘加/通道）
/// 合并为 1 条指令/操作，整体指令数约为标量的 55%–65%。
/// FMA 可用时与标量 `mul_add` 路径结果逐位一致；SSE2 回退误差 < 1e-9 量级。
#[cfg(target_arch = "x86_64")]
mod simd_df2t {
    use super::{BiquadCoeffs, BiquadState};
    use std::arch::x86_64::*;

    /// SSE2 回退：mul + add（x86_64 基线指令）。
    #[inline(always)]
    unsafe fn step_sse2(
        coeffs: &BiquadCoeffs,
        s1: __m128d,
        s2: __m128d,
        x: __m128d,
    ) -> (__m128d, __m128d, __m128d) {
        let b0 = _mm_set1_pd(coeffs.b0 as f64);
        let b1 = _mm_set1_pd(coeffs.b1 as f64);
        let b2 = _mm_set1_pd(coeffs.b2 as f64);
        let na1 = _mm_set1_pd(-(coeffs.a1 as f64));
        let a2 = _mm_set1_pd(coeffs.a2 as f64);

        let out = _mm_add_pd(_mm_mul_pd(b0, x), s1);
        let new_s1 = _mm_add_pd(_mm_add_pd(_mm_mul_pd(b1, x), _mm_mul_pd(na1, out)), s2);
        // 标量语义：s2 = b2*x - a2*out（逐项乘后相减）。
        let new_s2 = _mm_sub_pd(_mm_mul_pd(b2, x), _mm_mul_pd(a2, out));
        (out, new_s1, new_s2)
    }

    /// FMA 路径：与标量 `mul_add` 表达式逐位一致。
    #[target_feature(enable = "fma")]
    #[inline]
    unsafe fn step_fma(
        coeffs: &BiquadCoeffs,
        s1: __m128d,
        s2: __m128d,
        x: __m128d,
    ) -> (__m128d, __m128d, __m128d) {
        let b0 = _mm_set1_pd(coeffs.b0 as f64);
        let b1 = _mm_set1_pd(coeffs.b1 as f64);
        let b2 = _mm_set1_pd(coeffs.b2 as f64);
        let na1 = _mm_set1_pd(-(coeffs.a1 as f64));
        let a2 = _mm_set1_pd(coeffs.a2 as f64);

        let out = _mm_fmadd_pd(b0, x, s1);
        let new_s1 = _mm_fmadd_pd(b1, x, _mm_fmadd_pd(na1, out, s2));
        // 与标量 `b2.mul_add(x, -(a2 * out))` 一致：先算 a2*out，再融合。
        let new_s2 = _mm_fmsub_pd(b2, x, _mm_mul_pd(a2, out));
        (out, new_s1, new_s2)
    }

    /// 处理一个立体声采样对。返回 (L, R)。
    ///
    /// # Safety
    ///
    /// `st0/st1` 必须指向已初始化的左右通道状态；`use_fma` 必须来自
    /// `std::is_x86_feature_detected!("fma")`（false 时保证不调用 FMA 指令）。
    #[inline(always)]
    pub(super) unsafe fn process_sample(
        coeffs: &BiquadCoeffs,
        st0: &mut BiquadState,
        st1: &mut BiquadState,
        x_l: f32,
        x_r: f32,
        use_fma: bool,
    ) -> (f32, f32) {
        let x = _mm_set_pd(x_r as f64, x_l as f64);
        let s1 = _mm_set_pd(st1.s1, st0.s1);
        let s2 = _mm_set_pd(st1.s2, st0.s2);

        let (out, new_s1, new_s2) = if use_fma {
            unsafe { step_fma(coeffs, s1, s2, x) }
        } else {
            unsafe { step_sse2(coeffs, s1, s2, x) }
        };

        st0.s1 = _mm_cvtsd_f64(new_s1);
        st1.s1 = _mm_cvtsd_f64(_mm_unpackhi_pd(new_s1, new_s1));
        st0.s2 = _mm_cvtsd_f64(new_s2);
        st1.s2 = _mm_cvtsd_f64(_mm_unpackhi_pd(new_s2, new_s2));

        let mut out_l = _mm_cvtsd_f64(out) as f32;
        let mut out_r = _mm_cvtsd_f64(_mm_unpackhi_pd(out, out)) as f32;
        if !out_l.is_finite() {
            st0.clear();
            out_l = 0.0;
        }
        if !out_r.is_finite() {
            st1.clear();
            out_r = 0.0;
        }
        (out_l, out_r)
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 滤波器结构体（三种结构）
// ══════════════════════════════════════════════════════════════════════════════

/// 双二阶滤波器结构类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BiquadStructure {
    /// 直接形式 I（最直观，两组延迟线）。
    DirectFormI,
    /// 直接形式 II（一组延迟线，节省内存）。
    DirectFormII,
    /// 转置直接形式 II（数值最稳定，EqualizerAPO 默认，生产推荐）。
    DirectFormIITransposed,
}

/// 双二阶滤波器。
///
/// 对每个通道维护独立的状态。
/// - DF1 / DF2：兼容/测试路径，f32 状态数组；
/// - DF2T：生产路径，`Vec<BiquadState>`（f64，FMA，NaN 兜底）。
#[derive(Debug)]
pub struct BiquadFilter {
    /// 系数。
    coeffs: BiquadCoeffs,
    /// 结构类型。
    structure: BiquadStructure,
    /// 通道数（initialize 时确定）。
    num_channels: usize,
    /// 本滤波器作用的平面通道槽位（`Channel:` 选择，空 = 顺序 0..N）。
    channel_indices: Vec<usize>,
    /// Direct Form I 状态：`x[n-1]`, `x[n-2]` per channel。
    df1_x: Vec<[f32; 2]>,
    /// Direct Form I 状态：`y[n-1]`, `y[n-2]` per channel。
    df1_y: Vec<[f32; 2]>,
    /// Direct Form II 状态：`w[n-1]`, `w[n-2]` per channel（f32，兼容路径）。
    df2_w: Vec<[f32; 2]>,
    /// Direct Form II Transposed 状态（生产路径，f64）。
    df2t_states: Vec<BiquadState>,
}

impl BiquadFilter {
    /// 创建新的双二阶滤波器。
    pub fn new(coeffs: BiquadCoeffs, structure: BiquadStructure) -> Self {
        Self {
            coeffs,
            structure,
            num_channels: 0,
            channel_indices: Vec::new(),
            df1_x: Vec::new(),
            df1_y: Vec::new(),
            df2_w: Vec::new(),
            df2t_states: Vec::new(),
        }
    }

    /// 更新系数（用于参数变化时的平滑过渡，Phase 8+）。
    pub fn set_coeffs(&mut self, coeffs: BiquadCoeffs) {
        self.coeffs = coeffs;
    }

    /// 获取当前系数。
    pub fn coeffs(&self) -> BiquadCoeffs {
        self.coeffs
    }

    /// 重置状态（延迟线清零）。
    pub fn reset_state(&mut self) {
        for s in self.df1_x.iter_mut() {
            *s = [0.0; 2];
        }
        for s in self.df1_y.iter_mut() {
            *s = [0.0; 2];
        }
        for s in self.df2_w.iter_mut() {
            *s = [0.0; 2];
        }
        for s in self.df2t_states.iter_mut() {
            s.clear();
        }
    }

    /// Direct Form I 处理单采样。
    #[inline]
    fn process_df1(&mut self, ch: usize, input: f32) -> f32 {
        let c = &self.coeffs;
        let x = &mut self.df1_x[ch];
        let y = &mut self.df1_y[ch];

        let out = c.b0 * input + c.b1 * x[0] + c.b2 * x[1] - c.a1 * y[0] - c.a2 * y[1];

        x[1] = x[0];
        x[0] = input;
        y[1] = y[0];
        y[0] = out;

        out
    }

    /// Direct Form II 处理单采样。
    #[inline]
    fn process_df2(&mut self, ch: usize, input: f32) -> f32 {
        let c = &self.coeffs;
        let w = &mut self.df2_w[ch];

        let w0 = input - c.a1 * w[0] - c.a2 * w[1];
        let out = c.b0 * w0 + c.b1 * w[0] + c.b2 * w[1];

        w[1] = w[0];
        w[0] = w0;

        out
    }
}

impl Filter for BiquadFilter {
    fn initialize(&mut self, _sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        if self.channel_indices.is_empty() {
            // 直接调用（无 Chain 选择）→ 顺序处理前 N 个槽位。
            self.channel_indices = (0..channel_names.len()).collect();
        }
        self.num_channels = channel_names.len().max(1);
        self.num_channels = self.num_channels.min(self.channel_indices.len());
        self.df1_x = Vec::new();
        self.df1_y = Vec::new();
        self.df2_w = Vec::new();
        self.df2t_states = Vec::new();

        match self.structure {
            BiquadStructure::DirectFormI => {
                self.df1_x = vec![[0.0; 2]; self.num_channels];
                self.df1_y = vec![[0.0; 2]; self.num_channels];
            }
            BiquadStructure::DirectFormII => {
                self.df2_w = vec![[0.0; 2]; self.num_channels];
            }
            BiquadStructure::DirectFormIITransposed => {
                self.df2t_states = vec![BiquadState::new(); self.num_channels];
            }
        }
        None // 输出通道不变
    }

    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        let num_ch = self.num_channels.min(self.channel_indices.len());

        match self.structure {
            BiquadStructure::DirectFormI => {
                for k in 0..num_ch {
                    let slot = self.channel_indices[k];
                    if slot >= samples.len() {
                        continue;
                    }
                    for f in 0..frame_count {
                        samples[slot][f] = self.process_df1(k, samples[slot][f]);
                    }
                }
            }
            BiquadStructure::DirectFormII => {
                for k in 0..num_ch {
                    let slot = self.channel_indices[k];
                    if slot >= samples.len() {
                        continue;
                    }
                    for f in 0..frame_count {
                        samples[slot][f] = self.process_df2(k, samples[slot][f]);
                    }
                }
            }
            BiquadStructure::DirectFormIITransposed => {
                // 立体声水平 SIMD：仅当本滤波器作用域 == 2 通道时触发
                // （`Channel: L R` 或默认立体声）；作用域为 1 通道时走标量。
                // 选中通道 → 平面缓冲前 num_channels 槽位的映射与标量路径一致。
                #[cfg(target_arch = "x86_64")]
                if num_ch == 2 && samples.len() >= 2 && self.df2t_states.len() >= 2 {
                    let i0 = self.channel_indices[0];
                    let i1 = self.channel_indices[1];
                    if i0 != i1 && i0 < samples.len() && i1 < samples.len() {
                        let use_fma = std::is_x86_feature_detected!("fma");
                        let coeffs = &self.coeffs;
                        let (head, tail) = self.df2t_states.split_at_mut(1);
                        let st0 = &mut head[0];
                        let st1 = &mut tail[0];
                        // 两个不同槽位 → split_at_mut 取得两个可变通道缓冲。
                        let (a, b) = if i0 < i1 {
                            let (front, back) = samples.split_at_mut(i1);
                            (&mut front[i0], &mut back[0])
                        } else {
                            let (front, back) = samples.split_at_mut(i0);
                            (&mut back[0], &mut front[i1])
                        };
                        for f in 0..frame_count {
                            // SAFETY: 仅调用 SSE2/FMA 数学 intrinsics；st0/st1 与 samples 均为有效内存。
                            let (l, r) = unsafe {
                                simd_df2t::process_sample(coeffs, st0, st1, a[f], b[f], use_fma)
                            };
                            a[f] = l;
                            b[f] = r;
                        }
                        return;
                    }
                }

                for k in 0..num_ch {
                    let slot = self.channel_indices[k];
                    if slot >= samples.len() {
                        continue;
                    }
                    let state = &mut self.df2t_states[k];
                    let coeffs = &self.coeffs;
                    for f in 0..frame_count {
                        samples[slot][f] = state.process_sample(coeffs, samples[slot][f]);
                    }
                }
            }
        }
    }

    fn set_channel_indices(&mut self, indices: &[usize]) {
        self.channel_indices = indices.to_vec();
    }

    fn latency(&self) -> u32 {
        0 // biquad 无固有延迟
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo_channels() -> Vec<String> {
        vec!["L".into(), "R".into()]
    }

    fn make_stereo_buf(frame_count: usize) -> Vec<Vec<f32>> {
        vec![vec![0.0f32; frame_count]; 2]
    }

    // ── BiquadCoeffs ────────────────────────────────────────────────────────

    #[test]
    fn coeffs_bypass() {
        let c = BiquadCoeffs::BYPASS;
        assert_eq!(c.b0, 1.0);
        assert_eq!(c.b1, 0.0);
        assert!(c.is_valid());
    }

    #[test]
    fn coeffs_default_is_bypass() {
        let c = BiquadCoeffs::default();
        assert_eq!(c, BiquadCoeffs::BYPASS);
    }

    // ── BiquadState 布局 ────────────────────────────────────────────────────

    #[test]
    fn biquad_state_layout_is_16_bytes() {
        assert_eq!(std::mem::size_of::<BiquadState>(), 16);
        assert_eq!(std::mem::align_of::<BiquadState>(), 8);
    }

    #[test]
    fn biquad_state_clear_after_non_finite() {
        let mut state = BiquadState::new();
        let coeffs = BiquadCoeffs {
            b0: f32::INFINITY,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        };
        let out = state.process_sample(&coeffs, 1.0);
        assert_eq!(out, 0.0);
        assert_eq!(state.s1, 0.0);
        assert_eq!(state.s2, 0.0);
    }

    // ── compute_coeffs ──────────────────────────────────────────────────────

    #[test]
    fn peaking_zero_gain_is_passthrough() {
        let c = compute_coeffs(BiquadType::Peaking, 1000.0, 0.0, 1.0, 48000);
        assert!(c.is_valid());
        let mut filter = BiquadFilter::new(c, BiquadStructure::DirectFormIITransposed);
        filter.initialize(48000, &vec!["L".to_owned()]);
        let mut samples = vec![vec![0.5, -0.3, 0.8, -0.1, 0.0]];
        let input = samples[0].clone();
        filter.process(&mut samples, 5);
        for f in 0..5 {
            assert!(
                (samples[0][f] - input[f]).abs() < 0.01,
                "0dB peaking should passthrough: frame {} got {} expected {}",
                f, samples[0][f], input[f]
            );
        }
    }

    #[test]
    fn lowpass_coeffs_valid() {
        let c = compute_coeffs(BiquadType::LowPass, 1000.0, 0.0, 0.707, 48000);
        assert!(c.is_valid());
    }

    #[test]
    fn highpass_coeffs_valid() {
        let c = compute_coeffs(BiquadType::HighPass, 1000.0, 0.0, 0.707, 48000);
        assert!(c.is_valid());
    }

    #[test]
    fn lowshelf_coeffs_valid() {
        let c = compute_coeffs(BiquadType::LowShelf, 200.0, 6.0, 0.707, 48000);
        assert!(c.is_valid());
    }

    #[test]
    fn highshelf_coeffs_valid() {
        let c = compute_coeffs(BiquadType::HighShelf, 5000.0, -3.0, 0.707, 48000);
        assert!(c.is_valid());
    }

    #[test]
    fn bandpass_coeffs_valid() {
        let c = compute_coeffs(BiquadType::BandPass, 1000.0, 0.0, 2.0, 48000);
        assert!(c.is_valid());
    }

    #[test]
    fn notch_coeffs_valid() {
        let c = compute_coeffs(BiquadType::Notch, 1000.0, 0.0, 10.0, 48000);
        assert!(c.is_valid());
    }

    #[test]
    fn allpass_coeffs_valid() {
        let c = compute_coeffs(BiquadType::AllPass, 1000.0, 0.0, 1.0, 48000);
        assert!(c.is_valid());
    }

    // ── P0 边界护栏 ─────────────────────────────────────────────────────────

    #[test]
    fn extreme_gain_clamps_and_stays_finite() {
        // +1000 dB → clamp 到 +48 dB：系数必须有限且稳定（不再溢出/贴单位圆）。
        let c = compute_coeffs(BiquadType::Peaking, 1000.0, 1000.0, 1.0, 48000);
        assert!(c.is_valid());
        assert!(is_stable_biquad(c.a1, c.a2));

        let c2 = compute_coeffs(BiquadType::LowShelf, 1000.0, 1000.0, 1.0, 48000);
        assert!(c2.is_valid());
        assert!(is_stable_biquad(c2.a1, c2.a2));
    }

    #[test]
    fn extreme_cut_falls_back_to_bypass() {
        // -1000 dB → clamp 到 -120 dB：极点贴单位圆 → 回退直通。
        let c = compute_coeffs(BiquadType::Peaking, 1000.0, -1000.0, 1.0, 48000);
        assert_eq!(c, BiquadCoeffs::BYPASS);
    }

    #[test]
    fn invalid_inputs_fall_back_to_bypass() {
        assert_eq!(compute_coeffs(BiquadType::Peaking, 1000.0, 0.0, 1.0, 0), BiquadCoeffs::BYPASS);
        assert_eq!(
            compute_coeffs(BiquadType::Peaking, f32::NAN, 0.0, 1.0, 48000),
            BiquadCoeffs::BYPASS
        );
    }

    #[test]
    fn fc_beyond_nyquist_clamps() {
        // 100 kHz @ 48 kHz → clamp 到 0.45*48k = 21.6 kHz，系数仍有效。
        let c = compute_coeffs(BiquadType::LowPass, 100_000.0, 0.0, 0.707, 48000);
        assert!(c.is_valid());
    }

    #[test]
    fn impulse_response_decays_no_whistle() {
        // +48 dB peaking（clamp 后最大合法增益）：脉冲响应必须在有限时间内衰减。
        let c = compute_coeffs(BiquadType::Peaking, 1000.0, 1000.0, 1.0, 48000);
        let mut filter = BiquadFilter::new(c, BiquadStructure::DirectFormIITransposed);
        filter.initialize(48000, &vec!["L".to_owned()]);

        let mut samples = vec![vec![0.0f32; 8192]];
        samples[0][0] = 1.0;
        filter.process(&mut samples, 8192);

        let tail = &samples[0][4096..];
        let peak = tail.iter().fold(0.0f32, |m, &v| m.max(v.abs()));
        assert!(
            peak < 1e-3,
            "impulse response must decay, peak tail = {peak}"
        );
    }

    // ── BiquadFilter — Direct Form I ────────────────────────────────────────

    #[test]
    fn df1_bypass_passthrough() {
        let mut filter = BiquadFilter::new(BiquadCoeffs::BYPASS, BiquadStructure::DirectFormI);
        filter.initialize(48000, &stereo_channels());

        let mut samples = make_stereo_buf(4);
        samples[0] = vec![1.0, 2.0, 3.0, 4.0];
        samples[1] = vec![0.5, 1.0, 1.5, 2.0];

        filter.process(&mut samples, 4);

        for f in 0..4 {
            assert!((samples[0][f] - (f as f32 + 1.0)).abs() < 0.001);
        }
    }

    #[test]
    fn df1_silence_stays_silent() {
        let coeffs = compute_coeffs(BiquadType::Peaking, 1000.0, 6.0, 1.0, 48000);
        let mut filter = BiquadFilter::new(coeffs, BiquadStructure::DirectFormI);
        filter.initialize(48000, &stereo_channels());

        let mut samples = make_stereo_buf(100);
        filter.process(&mut samples, 100);

        for f in 0..100 {
            assert!(samples[0][f].abs() < 1e-10);
        }
    }

    // ── BiquadFilter — Direct Form II ───────────────────────────────────────

    #[test]
    fn df2_bypass_passthrough() {
        let mut filter = BiquadFilter::new(BiquadCoeffs::BYPASS, BiquadStructure::DirectFormII);
        filter.initialize(48000, &stereo_channels());

        let mut samples = make_stereo_buf(4);
        samples[0] = vec![1.0, 2.0, 3.0, 4.0];

        filter.process(&mut samples, 4);

        for f in 0..4 {
            assert!((samples[0][f] - (f as f32 + 1.0)).abs() < 0.001);
        }
    }

    // ── BiquadFilter — Direct Form II Transposed ────────────────────────────

    #[test]
    fn df2t_bypass_passthrough() {
        let mut filter = BiquadFilter::new(
            BiquadCoeffs::BYPASS,
            BiquadStructure::DirectFormIITransposed,
        );
        filter.initialize(48000, &stereo_channels());

        let mut samples = make_stereo_buf(4);
        samples[0] = vec![1.0, 2.0, 3.0, 4.0];

        filter.process(&mut samples, 4);

        for f in 0..4 {
            assert!((samples[0][f] - (f as f32 + 1.0)).abs() < 0.001);
        }
    }

    // ── 三种结构结果一致性 ──────────────────────────────────────────────────

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn stereo_simd_matches_scalar_channels() {
        // 立体声 SIMD 路径（左右同系数）必须与两个独立单声道标量路径一致。
        let coeffs = compute_coeffs(BiquadType::Peaking, 1000.0, 6.0, 1.0, 48000);
        let len = 512;
        let input_l: Vec<f32> = (0..len)
            .map(|i| ((i as f32 * 0.07).sin() * 0.5) as f32)
            .collect();
        let input_r: Vec<f32> = (0..len)
            .map(|i| ((i as f32 * 0.13).cos() * 0.4) as f32)
            .collect();

        // SIMD 路径：2 通道 BiquadFilter。
        let mut stereo = BiquadFilter::new(coeffs, BiquadStructure::DirectFormIITransposed);
        stereo.initialize(48000, &vec!["L".to_owned(), "R".to_owned()]);
        let mut samples = vec![input_l.clone(), input_r.clone()];
        stereo.process(&mut samples, len);

        // 标量路径：每通道一个 1 通道 BiquadFilter。
        let mut mono_l = BiquadFilter::new(coeffs, BiquadStructure::DirectFormIITransposed);
        mono_l.initialize(48000, &vec!["L".to_owned()]);
        let mut sl = vec![input_l.clone()];
        mono_l.process(&mut sl, len);

        let mut mono_r = BiquadFilter::new(coeffs, BiquadStructure::DirectFormIITransposed);
        mono_r.initialize(48000, &vec!["R".to_owned()]);
        let mut sr = vec![input_r.clone()];
        mono_r.process(&mut sr, len);

        for f in 0..len {
            assert!(
                (samples[0][f] - sl[0][f]).abs() < 1e-9,
                "L mismatch at {f}: simd={} scalar={}",
                samples[0][f],
                sl[0][f]
            );
            assert!(
                (samples[1][f] - sr[0][f]).abs() < 1e-9,
                "R mismatch at {f}: simd={} scalar={}",
                samples[1][f],
                sr[0][f]
            );
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn non_stereo_falls_back_to_scalar() {
        // 3 通道不回退到 SIMD，标量路径仍正确。
        let coeffs = compute_coeffs(BiquadType::Peaking, 500.0, 3.0, 1.0, 48000);
        let mut filter = BiquadFilter::new(coeffs, BiquadStructure::DirectFormIITransposed);
        let names = vec!["L".to_owned(), "R".to_owned(), "C".to_owned()];
        filter.initialize(48000, &names);

        let mut samples = vec![vec![0.0f32; 100]; 3];
        samples[0][0] = 1.0;
        samples[1][0] = 0.5;
        samples[2][0] = 0.25;
        filter.process(&mut samples, 100);

        // 三通道都处理且输出有限。
        for ch in samples.iter() {
            assert!(ch.iter().all(|v| v.is_finite()));
            assert!(ch.iter().any(|&v| v != 0.0));
        }
    }

    #[test]
    fn single_channel_scope_leaves_other_channel_untouched() {
        // 模拟 `Channel: L` 语义：initialize 只收到 1 个通道名 → num_channels == 1，
        // 即使 samples 是 2 缓冲也不触发 SIMD，且作用域外通道必须保持原样。
        let coeffs = compute_coeffs(BiquadType::Peaking, 1000.0, 6.0, 1.0, 48000);
        let mut filter = BiquadFilter::new(coeffs, BiquadStructure::DirectFormIITransposed);
        filter.initialize(48000, &vec!["L".to_owned()]);

        let mut samples = vec![vec![0.5f32; 64], vec![0.25f32; 64]];
        let r_orig = samples[1].clone();
        filter.process(&mut samples, 64);

        assert_eq!(samples[1], r_orig, "作用域外的通道必须保持原样");
        assert!(
            samples[0].iter().any(|&v| v != 0.5),
            "作用域内通道应被滤波（瞬态段应有变化）"
        );
    }

    #[test]
    fn channel_indices_route_to_non_leading_slot() {
        // 模拟 `Channel: R`：槽位索引 [1]，即使缓冲是 2 通道也只处理 R。
        let coeffs = compute_coeffs(BiquadType::Peaking, 1000.0, 6.0, 1.0, 48000);
        let mut filter = BiquadFilter::new(coeffs, BiquadStructure::DirectFormIITransposed);
        filter.set_channel_indices(&[1]);
        filter.initialize(48000, &vec!["R".to_owned()]);

        let mut samples = vec![vec![0.5f32; 64], vec![0.5f32; 64]];
        let l_orig = samples[0].clone();
        filter.process(&mut samples, 64);

        assert_eq!(samples[0], l_orig, "槽位 0（L）不能被处理");
        assert!(
            samples[1].iter().any(|&v| v != 0.5),
            "槽位 1（R）应被滤波（瞬态段应有变化）"
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn stereo_simd_with_non_leading_slots_matches_scalar() {
        // 选中槽位 [2, 1]（如 `Channel: C R`）：SIMD 两路必须作用于正确槽位。
        let coeffs = compute_coeffs(BiquadType::Peaking, 1000.0, 6.0, 1.0, 48000);
        let len = 256;
        let input_c: Vec<f32> = (0..len).map(|i| ((i as f32 * 0.05).sin() * 0.3) as f32).collect();
        let input_r: Vec<f32> = (0..len).map(|i| ((i as f32 * 0.11).cos() * 0.4) as f32).collect();

        // SIMD 路径：2 通道作用域，槽位 [2, 1]。
        let mut stereo = BiquadFilter::new(coeffs, BiquadStructure::DirectFormIITransposed);
        stereo.set_channel_indices(&[2, 1]);
        stereo.initialize(48000, &vec!["C".to_owned(), "R".to_owned()]);
        let mut samples = vec![vec![0.5f32; len], input_r.clone(), input_c.clone()];
        let l_orig = samples[0].clone();
        stereo.process(&mut samples, len);

        // 标量参考：单通道滤波器分别作用在槽位 2 和槽位 1。
        let mut mono_c = BiquadFilter::new(coeffs, BiquadStructure::DirectFormIITransposed);
        mono_c.set_channel_indices(&[2]);
        mono_c.initialize(48000, &vec!["C".to_owned()]);
        let mut sc = vec![
            vec![0.0f32; len],
            vec![0.0f32; len],
            input_c.clone(),
        ];
        mono_c.process(&mut sc, len);

        let mut mono_r = BiquadFilter::new(coeffs, BiquadStructure::DirectFormIITransposed);
        mono_r.set_channel_indices(&[1]);
        mono_r.initialize(48000, &vec!["R".to_owned()]);
        let mut sr = vec![vec![0.0f32; len], input_r.clone()];
        mono_r.process(&mut sr, len);

        assert_eq!(samples[0], l_orig, "槽位 0 不在作用域，必须保持不变");
        for f in 0..len {
            assert!(
                (samples[1][f] - sr[1][f]).abs() < 1e-9,
                "R(slot1) mismatch at {f}: simd={} scalar={}",
                samples[1][f],
                sr[1][f]
            );
            assert!(
                (samples[2][f] - sc[2][f]).abs() < 1e-9,
                "C(slot2) mismatch at {f}: simd={} scalar={}",
                samples[2][f],
                sc[2][f]
            );
        }
    }

    #[test]
    fn three_structures_same_output() {
        let coeffs = compute_coeffs(BiquadType::Peaking, 1000.0, 6.0, 1.0, 48000);
        let input = vec![0.5, -0.3, 0.8, -0.1, 0.0, 0.2, -0.7, 0.4];

        let mut results = Vec::new();
        for structure in &[
            BiquadStructure::DirectFormI,
            BiquadStructure::DirectFormII,
            BiquadStructure::DirectFormIITransposed,
        ] {
            let mut filter = BiquadFilter::new(coeffs, *structure);
            filter.initialize(48000, &vec!["L".to_owned()]);
            let mut samples = vec![input.clone()];
            filter.process(&mut samples, input.len());
            results.push(samples[0].clone());
        }

        for f in 0..input.len() {
            assert!(
                (results[0][f] - results[1][f]).abs() < 1e-6,
                "DF1 vs DF2 diff at frame {}: {} vs {}",
                f, results[0][f], results[1][f]
            );
            assert!(
                (results[0][f] - results[2][f]).abs() < 1e-6,
                "DF1 vs DF2T diff at frame {}: {} vs {}",
                f, results[0][f], results[2][f]
            );
        }
    }

    // ── set_coeffs / reset_state ─────────────────────────────────────────────

    #[test]
    fn update_coeffs() {
        let mut filter = BiquadFilter::new(BiquadCoeffs::BYPASS, BiquadStructure::DirectFormI);
        filter.initialize(48000, &stereo_channels());

        let new_coeffs = compute_coeffs(BiquadType::LowPass, 500.0, 0.0, 0.707, 48000);
        filter.set_coeffs(new_coeffs);
        assert_eq!(filter.coeffs(), new_coeffs);
    }

    #[test]
    fn reset_clears_state() {
        let coeffs = compute_coeffs(BiquadType::Peaking, 1000.0, 6.0, 1.0, 48000);
        let mut filter = BiquadFilter::new(coeffs, BiquadStructure::DirectFormIITransposed);
        filter.initialize(48000, &stereo_channels());

        let mut samples = make_stereo_buf(10);
        samples[0][0] = 1.0;
        filter.process(&mut samples, 10);

        filter.reset_state();

        let mut zeros = make_stereo_buf(10);
        filter.process(&mut zeros, 10);
        for f in 0..10 {
            assert!(zeros[0][f].abs() < 1e-10);
        }
    }

    // ── latency ─────────────────────────────────────────────────────────────

    #[test]
    fn biquad_zero_latency() {
        let filter = BiquadFilter::new(BiquadCoeffs::BYPASS, BiquadStructure::DirectFormI);
        assert_eq!(filter.latency(), 0);
    }
}
