//! dsp/filters/biquad.rs — 双二阶滤波器基础实现（Note 13/53/58）
//!
//! 提供三种经典双二阶结构：
//! - Direct Form I（直接形式 I）
//! - Direct Form II（直接形式 II）
//! - Direct Form II Transposed（转置直接形式 II）
//!
//! 系数计算：均基于传递函数 H(z) 的标准公式，
//! 由 hp_lp.rs / peq.rs / graph_eq.rs 等模块计算系数后传入。
//!
//! 此模块仅依赖 `f32` 和 `Filter` trait，不依赖 Windows API（Note 53）。
//! `process` 方法遵守 RT-safety 约束（Note 12）。

use crate::pipeline::dsp::filter::Filter;

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
/// - `fc`：中心/截止频率（Hz）
/// - `gain_db`：增益（dB，仅 Peaking / LowShelf / HighShelf 有意义）
/// - `q`：品质因数
/// - `sample_rate`：采样率（Hz）
///
/// 所有公式基于 Robert Bristow-Johnson 的 Audio EQ Cookbook。
pub fn compute_coeffs(
    filter_type: BiquadType,
    fc: f32,
    gain_db: f32,
    q: f32,
    sample_rate: u32,
) -> BiquadCoeffs {
    let sr = sample_rate as f32;

    // 频率 / 采样率比（归一化角频率）
    let w0 = 2.0 * std::f32::consts::PI * fc / sr;
    let cos_w0 = w0.cos();
    let sin_w0 = w0.sin();

    // 增益因子（仅 shelf / peaking 需要）
    let a = 10.0_f32.powf(gain_db / 40.0); // 10^(dB/40)
    let alpha = sin_w0 / (2.0 * q);

    match filter_type {
        BiquadType::Peaking => {
            let b0 = 1.0 + alpha * a;
            let b1 = -2.0 * cos_w0;
            let b2 = 1.0 - alpha * a;
            let a0 = 1.0 + alpha / a;
            let a1 = -2.0 * cos_w0;
            let a2 = 1.0 - alpha / a;
            normalize(b0, b1, b2, a0, a1, a2)
        }
        BiquadType::LowShelf => {
            let sq = 2.0 * a.sqrt() * alpha;
            let b0 = a * ((a + 1.0) - (a - 1.0) * cos_w0 + sq);
            let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0);
            let b2 = a * ((a + 1.0) - (a - 1.0) * cos_w0 - sq);
            let a0 = (a + 1.0) + (a - 1.0) * cos_w0 + sq;
            let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0);
            let a2 = (a + 1.0) + (a - 1.0) * cos_w0 - sq;
            normalize(b0, b1, b2, a0, a1, a2)
        }
        BiquadType::HighShelf => {
            let sq = 2.0 * a.sqrt() * alpha;
            let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w0 + sq);
            let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0);
            let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w0 - sq);
            let a0 = (a + 1.0) - (a - 1.0) * cos_w0 + sq;
            let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0);
            let a2 = (a + 1.0) - (a - 1.0) * cos_w0 - sq;
            normalize(b0, b1, b2, a0, a1, a2)
        }
        BiquadType::LowPass => {
            let b0 = (1.0 - cos_w0) / 2.0;
            let b1 = 1.0 - cos_w0;
            let b2 = (1.0 - cos_w0) / 2.0;
            let a0 = 1.0 + alpha;
            let a1 = -2.0 * cos_w0;
            let a2 = 1.0 - alpha;
            normalize(b0, b1, b2, a0, a1, a2)
        }
        BiquadType::HighPass => {
            let b0 = (1.0 + cos_w0) / 2.0;
            let b1 = -(1.0 + cos_w0);
            let b2 = (1.0 + cos_w0) / 2.0;
            let a0 = 1.0 + alpha;
            let a1 = -2.0 * cos_w0;
            let a2 = 1.0 - alpha;
            normalize(b0, b1, b2, a0, a1, a2)
        }
        BiquadType::BandPass => {
            let b0 = alpha;
            let b1 = 0.0;
            let b2 = -alpha;
            let a0 = 1.0 + alpha;
            let a1 = -2.0 * cos_w0;
            let a2 = 1.0 - alpha;
            normalize(b0, b1, b2, a0, a1, a2)
        }
        BiquadType::Notch => {
            let b0 = 1.0;
            let b1 = -2.0 * cos_w0;
            let b2 = 1.0;
            let a0 = 1.0 + alpha;
            let a1 = -2.0 * cos_w0;
            let a2 = 1.0 - alpha;
            normalize(b0, b1, b2, a0, a1, a2)
        }
        BiquadType::AllPass => {
            let b0 = 1.0 - alpha;
            let b1 = -2.0 * cos_w0;
            let b2 = 1.0 + alpha;
            let a0 = 1.0 + alpha;
            let a1 = -2.0 * cos_w0;
            let a2 = 1.0 - alpha;
            normalize(b0, b1, b2, a0, a1, a2)
        }
    }
}

/// 归一化系数（除以 a0）。
fn normalize(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> BiquadCoeffs {
    let inv_a0 = 1.0 / a0;
    BiquadCoeffs {
        b0: b0 * inv_a0,
        b1: b1 * inv_a0,
        b2: b2 * inv_a0,
        a1: a1 * inv_a0,
        a2: a2 * inv_a0,
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
    /// 转置直接形式 II（数值最稳定，EqualizerAPO 默认）。
    DirectFormIITransposed,
}

/// 双二阶滤波器。
///
/// 对每个通道维护独立的状态（延迟线）。
/// 支持三种经典结构切换。
#[derive(Debug)]
pub struct BiquadFilter {
    /// 系数。
    coeffs: BiquadCoeffs,
    /// 结构类型。
    structure: BiquadStructure,
    /// 通道数（initialize 时确定）。
    num_channels: usize,
    /// Direct Form I 状态：`x[n-1]`, `x[n-2]` per channel。
    df1_x: Vec<[f32; 2]>,
    /// Direct Form I 状态：`y[n-1]`, `y[n-2]` per channel。
    df1_y: Vec<[f32; 2]>,
    /// Direct Form II / Transposed 状态：`w[n-1]`, `w[n-2]` per channel。
    df2_w: Vec<[f32; 2]>,
}

impl BiquadFilter {
    /// 创建新的双二阶滤波器。
    pub fn new(coeffs: BiquadCoeffs, structure: BiquadStructure) -> Self {
        Self {
            coeffs,
            structure,
            num_channels: 0,
            df1_x: Vec::new(),
            df1_y: Vec::new(),
            df2_w: Vec::new(),
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

    /// Direct Form II Transposed 处理单采样。
    #[inline]
    fn process_df2t(&mut self, ch: usize, input: f32) -> f32 {
        let c = &self.coeffs;
        let w = &mut self.df2_w[ch];

        let out = c.b0 * input + w[0];
        w[0] = c.b1 * input - c.a1 * out + w[1];
        w[1] = c.b2 * input - c.a2 * out;

        out
    }
}

impl Filter for BiquadFilter {
    fn initialize(&mut self, _sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        self.num_channels = channel_names.len().max(1);
        self.df1_x = vec![[0.0; 2]; self.num_channels];
        self.df1_y = vec![[0.0; 2]; self.num_channels];
        self.df2_w = vec![[0.0; 2]; self.num_channels];
        None // 输出通道不变
    }

    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        let num_ch = self.num_channels.min(samples.len());

        for ch in 0..num_ch {
            for f in 0..frame_count {
                let input = samples[ch][f];
                let output = match self.structure {
                    BiquadStructure::DirectFormI => self.process_df1(ch, input),
                    BiquadStructure::DirectFormII => self.process_df2(ch, input),
                    BiquadStructure::DirectFormIITransposed => self.process_df2t(ch, input),
                };
                samples[ch][f] = output;
            }
        }
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

    // ── compute_coeffs ──────────────────────────────────────────────────────

    #[test]
    fn peaking_zero_gain_is_passthrough() {
        let c = compute_coeffs(BiquadType::Peaking, 1000.0, 0.0, 1.0, 48000);
        assert!(c.is_valid());
        // 0dB peaking: 分子 = 分母，即 b0/a0=1, b1=a1, b2=a2
        // 不是 BYPASS 系数，但 H(z) = 1
        // 验证方式：处理一个脉冲，输出应等于输入
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

    // ── BiquadFilter — Direct Form I ────────────────────────────────────────

    #[test]
    fn df1_bypass_passthrough() {
        let mut filter = BiquadFilter::new(BiquadCoeffs::BYPASS, BiquadStructure::DirectFormI);
        filter.initialize(48000, &stereo_channels());

        let mut samples = make_stereo_buf(4);
        samples[0] = vec![1.0, 2.0, 3.0, 4.0];
        samples[1] = vec![0.5, 1.0, 1.5, 2.0];

        filter.process(&mut samples, 4);

        // bypass 系数：输出应接近输入
        for f in 0..4 {
            assert!((samples[0][f] - (f as f32 + 1.0)).abs() < 0.001);
        }
    }

    #[test]
    fn df1_silence_stays_silent() {
        let coeffs = compute_coeffs(BiquadType::Peaking, 1000.0, 6.0, 1.0, 48000);
        let mut filter = BiquadFilter::new(coeffs, BiquadStructure::DirectFormI);
        filter.initialize(48000, &stereo_channels());

        let mut samples = make_stereo_buf(100); // 全零
        filter.process(&mut samples, 100);

        // 全零输入 → 全零输出（任何稳定的 IIR 滤波器）
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

        // 三种结构应产生近似相同的结果
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

        // 处理一些数据使状态非零
        let mut samples = make_stereo_buf(10);
        samples[0][0] = 1.0;
        filter.process(&mut samples, 10);

        filter.reset_state();

        // 重置后处理全零输入应输出全零
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