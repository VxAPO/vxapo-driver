//! dsp/convolution.rs — 卷积滤波器（短 IR 时域 + 长 IR 分块 FFT，RT 安全）
//!
//! 实现 `Convolution:` 命令。
//!
//! # 实现方式（M3）
//!
//! - IR 长度 ≤ `MAX_DIRECT_IR_LEN`（128）：直接时域 FIR（MAC 环形缓冲，零分配）。
//! - IR 长度 > 128 且 ≤ `MAX_PARTITIONED_IR_LEN`（65536）：uniform partitioned
//!   overlap-add 卷积（块大小 = `CONVOLUTION_PARTITION_SIZE`，FFT 长度 = 2×块，
//!   依赖 rustfft 6.4.1）。算法延迟 = 块大小（128 采样）。
//! - 超长 IR：跳过该滤波器（返回 `None`）并 log warn。
//!
//! FFT 计划与全部缓冲在 `initialize`（非 RT）预分配，`process` 零分配、零锁。
//! 增益（dB）在 `initialize` 时线性合并进 IR 系数（P0 clamp 后必有限）。

use std::sync::Arc;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use once_cell::sync::Lazy;

use rustfft::{Fft, FftPlanner, num_complex::Complex};

use crate::pipeline::dsp::filter::Filter;
use crate::pipeline::dsp::math::{
    CONVOLUTION_PARTITION_SIZE, db_to_linear, warn_rate_limited,
};

/// 短 IR 直接卷积的最大长度（采样）。
const MAX_DIRECT_IR_LEN: usize = CONVOLUTION_PARTITION_SIZE;

/// 分块 FFT 卷积支持的最大 IR 长度（采样，约 1.37 s @ 48 kHz）。
const MAX_PARTITIONED_IR_LEN: usize = 65536;

/// WAV 支持的最大 IR 文件大小（防止恶意文件耗尽内存）。
const MAX_WAV_FILE_SIZE: usize = 1 << 20; // 1 MiB

/// 卷积滤波器。
pub struct ConvolutionFilter {
    /// IR 文件路径（用于日志/错误报告）。
    ir_path: String,
    /// 由调用方直接注入的 IR（GraphicEQ 运行时生成）；优先于文件加载。
    ir_override: Option<Vec<f32>>,
    /// 强制走直接时域 FIR（GraphicEQ 用：无分区块延迟，延迟不上报引擎）。
    force_direct: bool,
    /// 增益（dB）。
    gain_db: f32,
    /// 运行模式（直通 / 直接 FIR / 分块 FFT）。
    mode: ConvMode,
    /// 本滤波器作用的平面通道槽位（`Channel:` 选择，空 = 顺序 0..N）。
    channel_indices: Vec<usize>,
}

impl std::fmt::Debug for ConvolutionFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConvolutionFilter")
            .field("ir_path", &self.ir_path)
            .field("gain_db", &self.gain_db)
            .field("mode", &self.mode)
            .field("channel_indices", &self.channel_indices)
            .finish()
    }
}

/// 卷积运行模式。
enum ConvMode {
    /// 未加载 / 全零 IR（直通）。
    Passthrough,
    /// 短 IR：直接时域 FIR。
    Direct(DirectConv),
    /// 长 IR：分块 FFT overlap-add。
    Partitioned(PartitionedConv),
}

impl std::fmt::Debug for ConvMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Passthrough => write!(f, "Passthrough"),
            Self::Direct(d) => write!(f, "Direct(ir_len={})", d.ir_len),
            Self::Partitioned(p) => write!(f, "Partitioned(blocks={})", p.blocks),
        }
    }
}

/// 直接时域 FIR 状态。
#[derive(Debug)]
struct DirectConv {
    /// 逆序 IR（`ir_rev[j] = ir[ir_len-1-j]`），与“旧→新”连续样本段点积（v9.7）。
    ir_rev: Vec<f32>,
    /// 每通道延迟线（历史输入，2 的幂环形 buffer）。
    delay_lines: Vec<Vec<f32>>,
    /// 延迟线写头。
    write_positions: Vec<usize>,
    /// IR 长度（延迟 = ir_len - 1）。
    ir_len: usize,
}

// ══════════════════════════════════════════════════════════════════════════════
// 向量化 FIR（v9.7）
// ══════════════════════════════════════════════════════════════════════════════

/// AVX2+FMA 可用标志（运行时探测一次，build_direct 初始化）。
#[cfg(target_arch = "x86_64")]
static USE_AVX2_FMA: AtomicBool = AtomicBool::new(false);

/// 运行时探测 AVX2+FMA（非 RT，build_direct 调用；幂等）。
#[cfg(target_arch = "x86_64")]
pub(crate) fn init_fir_simd() {
    if std::arch::is_x86_feature_detected!("avx2")
        && std::arch::is_x86_feature_detected!("fma")
    {
        USE_AVX2_FMA.store(true, Ordering::Relaxed);
    }
}

/// 连续段点积 `Σ a[i]·b[i]`（等长）。AVX2+FMA 8 路 FMA；回退标量 mul_add
/// （release 下编译器按 SSE2 自动向量化 4 路）。
#[inline]
pub(crate) fn dot(a: &[f32], b: &[f32]) -> f32 {
    #[cfg(target_arch = "x86_64")]
    {
        if USE_AVX2_FMA.load(Ordering::Relaxed) {
            // SAFETY: 标志仅在 CPU 探测通过后置位；dot_avx2_fma 带 target_feature。
            return unsafe { dot_avx2_fma(a, b) };
        }
    }
    let mut acc = 0.0f32;
    for (&x, &y) in a.iter().zip(b) {
        acc = x.mul_add(y, acc);
    }
    acc
}

/// AVX2 + FMA 点积（8 路 FMA + SSE2 水平求和 + 标量尾）。
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn dot_avx2_fma(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let n = a.len().min(b.len());
    let mut acc = _mm256_setzero_ps();
    let mut i = 0;
    while i + 8 <= n {
        let va = _mm256_loadu_ps(a.as_ptr().add(i));
        let vb = _mm256_loadu_ps(b.as_ptr().add(i));
        acc = _mm256_fmadd_ps(va, vb, acc);
        i += 8;
    }
    // SSE2 水平求和（不依赖 SSE3 hadd）。
    let lo = _mm256_castps256_ps128(acc);
    let hi = _mm256_extractf128_ps(acc, 1);
    let s0 = _mm_add_ps(lo, hi);
    let s1 = _mm_add_ps(s0, _mm_movehl_ps(s0, s0));
    let s2 = _mm_add_ss(s1, _mm_shuffle_ps(s1, s1, 0b0000_0001));
    let mut s = _mm_cvtss_f32(s2);
    while i < n {
        s = a.get_unchecked(i).mul_add(*b.get_unchecked(i), s);
        i += 1;
    }
    s
}

/// 分块 FFT overlap-add 状态（uniform partitioned convolution）。
struct PartitionedConv {
    /// FFT 长度 = 2 × 块大小。
    fft_len: usize,
    /// 块大小（= CONVOLUTION_PARTITION_SIZE）。
    block_len: usize,
    /// IR 分块数。
    blocks: usize,
    /// IR 各块频域系数（blocks × fft_len，共享，已应用增益）。
    h_ffts: Vec<Complex<f32>>,
    /// 1 / fft_len（rustfft 逆 FFT 未归一化，IFFT 后需乘此系数）。
    inv_fft_len: f32,
    /// 前向 FFT 计划（共享）。
    fft: Arc<dyn Fft<f32>>,
    /// 逆 FFT 计划（共享）。
    ifft: Arc<dyn Fft<f32>>,
    /// 共享 scratch（RT 零分配）。
    scratch: Vec<Complex<f32>>,
    /// 每选中通道状态。
    channels: Vec<PartitionedChannel>,
}

impl std::fmt::Debug for PartitionedConv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PartitionedConv")
            .field("fft_len", &self.fft_len)
            .field("block_len", &self.block_len)
            .field("blocks", &self.blocks)
            .field("channels", &self.channels.len())
            .finish()
    }
}

/// 单通道分块卷积状态。
#[derive(Debug)]
struct PartitionedChannel {
    /// 输入块缓冲（block_len）。
    in_buf: Vec<f32>,
    /// 输入块内已缓冲样本数。
    in_len: usize,
    /// 输入 FFT 环形缓冲（blocks × fft_len）。
    x_ring: Vec<Complex<f32>>,
    /// 当前写入槽位（m mod blocks）。
    ring_slot: usize,
    /// 输入块 FFT 工作缓冲（fft_len）。
    x_work: Vec<Complex<f32>>,
    /// 频域累加 / IFFT 工作缓冲（fft_len）。
    acc: Vec<Complex<f32>>,
    /// overlap-add 进位（block_len）。
    overlap: Vec<f32>,
    /// 输出块缓冲（block_len）。
    out_buf: Vec<f32>,
    /// 输出块内待发射样本数。
    out_len: usize,
}

impl ConvolutionFilter {
    /// 创建卷积滤波器。
    ///
    /// - `ir_path`：IR 文件路径
    /// - `gain_db`：增益（dB）
    pub fn new(ir_path: &str, gain_db: f32) -> Self {
        Self {
            ir_path: ir_path.to_owned(),
            ir_override: None,
            force_direct: false,
            gain_db,
            mode: ConvMode::Passthrough,
            channel_indices: Vec::new(),
        }
    }

    /// 使用内存中已生成的 IR 创建卷积滤波器（GraphicEQ 等运行时生成场景）。
    pub fn with_ir(ir: Vec<f32>, gain_db: f32) -> Self {
        Self {
            ir_path: "<generated>".to_owned(),
            ir_override: Some(ir),
            force_direct: false,
            gain_db,
            mode: ConvMode::Passthrough,
            channel_indices: Vec::new(),
        }
    }

    /// 使用内存 IR 并强制直接时域 FIR（无分区块延迟）。
    pub fn with_ir_direct(ir: Vec<f32>, gain_db: f32) -> Self {
        Self {
            ir_path: "<generated-direct>".to_owned(),
            ir_override: Some(ir),
            force_direct: true,
            gain_db,
            mode: ConvMode::Passthrough,
            channel_indices: Vec::new(),
        }
    }
}

impl Filter for ConvolutionFilter {
    fn initialize(&mut self, _sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        // 尝试加载 WAV IR。失败 → 不生效（跳过该滤波器，不报错）。
        if self.channel_indices.is_empty() {
            self.channel_indices = (0..channel_names.len()).collect();
        }
        let channels = self.channel_indices.len().max(1);

        self.mode = ConvMode::Passthrough;
        let loaded = match &self.ir_override {
            Some(ir) => Ok(ir.clone()),
            None => load_wav_ir(&self.ir_path),
        };
        match loaded {
            Ok(raw_ir) => {
                if raw_ir.is_empty() {
                    return None;
                }
                if raw_ir.iter().any(|v| !v.is_finite()) {
                    warn_rate_limited(
                        "convolution_ir_nonfinite",
                        "Convolution IR 含 NaN/Inf 采样，已置 0 净化",
                    );
                }

                // 增益合并进 IR（P0：clamp 后必有限）。
                let gain = db_to_linear(self.gain_db);
                let ir: Vec<f32> = raw_ir
                    .iter()
                    .map(|&v| {
                        let s = v * gain;
                        if s.is_finite() {
                            s
                        } else {
                            0.0
                        }
                    })
                    .collect();

                // 全零 IR（含增益后全零）→ 直通，省整段卷积。
                if ir.iter().all(|&v| v == 0.0) {
                    return None;
                }

                if ir.len() <= MAX_DIRECT_IR_LEN || self.force_direct {
                    self.mode = ConvMode::Direct(build_direct(&ir, channels));
                } else if ir.len() <= MAX_PARTITIONED_IR_LEN {
                    match build_partitioned(&ir, channels) {
                        Some(pc) => self.mode = ConvMode::Partitioned(pc),
                        None => {
                            log::warn!(
                                "Convolution: IR '{}' 分块 FFT 初始化失败，跳过该滤波器",
                                self.ir_path
                            );
                            return None;
                        }
                    }
                } else {
                    log::warn!(
                        "Convolution: IR '{}' 长度 {} 超过 {}，跳过该滤波器",
                        self.ir_path,
                        ir.len(),
                        MAX_PARTITIONED_IR_LEN
                    );
                    return None;
                }
                None
            }
            Err(e) => {
                log::warn!(
                    "Convolution: 加载 IR '{}' 失败（{}），跳过该滤波器",
                    self.ir_path,
                    e
                );
                None
            }
        }
    }

    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        let num_ch = samples.len().min(self.channel_indices.len());
        match &mut self.mode {
            ConvMode::Passthrough => {}
            ConvMode::Direct(direct) => {
                for k in 0..num_ch {
                    let slot = self.channel_indices[k];
                    if slot >= samples.len() || k >= direct.delay_lines.len() {
                        continue;
                    }
                    let ir_rev = &direct.ir_rev;
                    let ir_len = direct.ir_len;
                    let delay_len = direct.delay_lines[k].len();
                    let mask = delay_len - 1;
                    let pos = &mut direct.write_positions[k];
                    let delay = &mut direct.delay_lines[k];

                    for frame in 0..frame_count {
                        let input = samples[slot][frame];
                        delay[*pos] = input;
                        *pos = (*pos + 1) & mask;

                        // 直接 FIR（v9.7 分段点积 + SIMD）：样本按“旧→新”
                        // 切成最多两段连续切片，与逆序系数点积——编译器可向量化，
                        // 避免逐系数环形回读的标量路径。
                        let start = (*pos).wrapping_sub(1) & mask; // 最新样本索引
                        let oldest = (*pos + delay_len - ir_len) & mask; // 最旧样本索引
                        let acc = if oldest <= start {
                            // 无回绕：一段连续切片。
                            dot(ir_rev, &delay[oldest..=start])
                        } else {
                            // 回绕：旧段 [oldest..len] + 新段 [0..=start]。
                            let len_old = delay_len - oldest;
                            let len_new = start + 1;
                            dot(&ir_rev[..len_old], &delay[oldest..])
                                + dot(&ir_rev[len_old..], &delay[0..=start])
                        };
                        samples[slot][frame] = acc;
                    }
                }
            }
            ConvMode::Partitioned(pc) => {
                let PartitionedConv {
                    fft_len,
                    block_len,
                    blocks,
                    inv_fft_len,
                    h_ffts,
                    fft,
                    ifft,
                    scratch,
                    channels,
                } = pc;
                for k in 0..num_ch {
                    if k >= channels.len() {
                        continue;
                    }
                    let slot = self.channel_indices[k];
                    if slot >= samples.len() {
                        continue;
                    }
                    let ch = &mut channels[k];
                    for frame in 0..frame_count {
                        let input = samples[slot][frame];
                        // 先吐出一个已算好的输出（块延迟 = block_len）。
                        let out = if ch.out_len > 0 {
                            let idx = *block_len - ch.out_len;
                            ch.out_len -= 1;
                            ch.out_buf[idx]
                        } else {
                            0.0
                        };
                        samples[slot][frame] = out;

                        // 进样；块满 → 处理一块。
                        ch.in_buf[ch.in_len] = input;
                        ch.in_len += 1;
                        if ch.in_len == *block_len {
                            process_partitioned_block(
                                ch,
                                h_ffts.as_slice(),
                                &**fft,
                                &**ifft,
                                scratch,
                                *fft_len,
                                *block_len,
                                *blocks,
                                *inv_fft_len,
                            );
                        }
                    }
                }
            }
        }
    }

    fn latency(&self) -> u32 {
        match &self.mode {
            ConvMode::Passthrough => 0,
            ConvMode::Direct(d) => (d.ir_len - 1) as u32,
            // 分块 FFT：算法延迟 = 块大小（输出块在下一块输入期间发射）。
            ConvMode::Partitioned(p) => p.block_len as u32,
        }
    }

    fn reset(&mut self) {
        match &mut self.mode {
            ConvMode::Passthrough => {}
            ConvMode::Direct(d) => {
                for line in d.delay_lines.iter_mut() {
                    line.fill(0.0);
                }
                d.write_positions.fill(0);
            }
            ConvMode::Partitioned(p) => {
                for ch in p.channels.iter_mut() {
                    ch.in_len = 0;
                    ch.ring_slot = 0;
                    ch.x_ring.fill(Complex::new(0.0, 0.0));
                    ch.x_work.fill(Complex::new(0.0, 0.0));
                    ch.acc.fill(Complex::new(0.0, 0.0));
                    ch.overlap.fill(0.0);
                    ch.out_buf.fill(0.0);
                    ch.out_len = 0;
                }
            }
        }
    }

    fn set_channel_indices(&mut self, indices: &[usize]) {
        self.channel_indices = indices.to_vec();
    }
}

/// 构建直接时域 FIR 状态。
fn build_direct(ir: &[f32], channels: usize) -> DirectConv {
    #[cfg(target_arch = "x86_64")]
    init_fir_simd();
    let delay_len = ir.len().next_power_of_two();
    DirectConv {
        ir_rev: ir.iter().rev().copied().collect(),
        delay_lines: vec![vec![0.0; delay_len]; channels],
        write_positions: vec![0; channels],
        ir_len: ir.len(),
    }
}

/// 构建分块 FFT 状态（非 RT：FFT 计划 + 全部缓冲预分配）。
fn build_partitioned(ir: &[f32], channels: usize) -> Option<PartitionedConv> {
    let block_len = CONVOLUTION_PARTITION_SIZE;
    let fft_len = block_len * 2;
    let blocks = ir.len().div_ceil(block_len);

    // v9.6 FFT 计划缓存：rustfft 的计划创建较慢（设置页/多流会瞬间创建大量实例），
    // FFT 长度只有少数几种，进程内缓存后 clone Arc 计划即可（计划本身不可变）。
    static FFT_PLAN_CACHE: Lazy<
        Mutex<HashMap<usize, (Arc<dyn Fft<f32>>, Arc<dyn Fft<f32>>, usize)>>,
    > = Lazy::new(|| Mutex::new(HashMap::new()));
    let (fft, ifft, scratch_len) = {
        let mut cache = FFT_PLAN_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((f, i, s)) = cache.get(&fft_len) {
            (f.clone(), i.clone(), *s)
        } else {
            let mut planner = FftPlanner::<f32>::new();
            let fft = planner.plan_fft_forward(fft_len);
            let ifft = planner.plan_fft_inverse(fft_len);
            let scratch_len = fft
                .get_inplace_scratch_len()
                .max(ifft.get_inplace_scratch_len());
            cache.insert(fft_len, (fft.clone(), ifft.clone(), scratch_len));
            (fft, ifft, scratch_len)
        }
    };
    let mut scratch = vec![Complex::new(0.0, 0.0); scratch_len];

    // 预计算 IR 各块频域系数（共享，已应用增益）。
    let mut h_block = vec![Complex::new(0.0, 0.0); fft_len];
    let mut h_ffts = Vec::with_capacity(blocks * fft_len);
    for p in 0..blocks {
        h_block.fill(Complex::new(0.0, 0.0));
        let start = p * block_len;
        let end = (start + block_len).min(ir.len());
        for (i, &v) in ir[start..end].iter().enumerate() {
            h_block[i] = Complex::new(v, 0.0);
        }
        fft.process_with_scratch(&mut h_block, &mut scratch);
        h_ffts.extend_from_slice(&h_block);
    }

    let channels = (0..channels)
        .map(|_| PartitionedChannel {
            in_buf: vec![0.0; block_len],
            in_len: 0,
            x_ring: vec![Complex::new(0.0, 0.0); blocks * fft_len],
            ring_slot: 0,
            x_work: vec![Complex::new(0.0, 0.0); fft_len],
            acc: vec![Complex::new(0.0, 0.0); fft_len],
            overlap: vec![0.0; block_len],
            out_buf: vec![0.0; block_len],
            out_len: 0,
        })
        .collect();

    Some(PartitionedConv {
        fft_len,
        block_len,
        blocks,
        h_ffts,
        inv_fft_len: 1.0 / fft_len as f32,
        fft,
        ifft,
        scratch,
        channels,
    })
}

/// 处理一个输入块（uniform partitioned overlap-add，RT 零分配）。
fn process_partitioned_block(
    ch: &mut PartitionedChannel,
    h_ffts: &[Complex<f32>],
    fft: &dyn Fft<f32>,
    ifft: &dyn Fft<f32>,
    scratch: &mut [Complex<f32>],
    fft_len: usize,
    block_len: usize,
    blocks: usize,
    inv_fft_len: f32,
) {
    // 1. 输入块（零填充到 fft_len）→ FFT，写入环形槽位。
    ch.x_work.fill(Complex::new(0.0, 0.0));
    for (i, &v) in ch.in_buf.iter().enumerate() {
        ch.x_work[i] = Complex::new(v, 0.0);
    }
    fft.process_with_scratch(&mut ch.x_work, scratch);
    let slot = ch.ring_slot;
    let x_slot = &mut ch.x_ring[slot * fft_len..(slot + 1) * fft_len];
    x_slot.copy_from_slice(&ch.x_work);

    // 2. 频域累加：Σ_p X_{m-p} · H_p。
    ch.acc.fill(Complex::new(0.0, 0.0));
    for p in 0..blocks {
        let x_idx = ((slot + blocks - p) % blocks) * fft_len;
        let h_idx = p * fft_len;
        for i in 0..fft_len {
            ch.acc[i] = ch.acc[i] + ch.x_ring[x_idx + i] * h_ffts[h_idx + i];
        }
    }

    // 3. IFFT → 时域。
    ifft.process_with_scratch(&mut ch.acc, scratch);

    // 4. overlap-add：前块输出 + 进位；第二半进位到下一块。
    for n in 0..block_len {
        ch.out_buf[n] = ch.acc[n].re * inv_fft_len + ch.overlap[n];
        ch.overlap[n] = ch.acc[block_len + n].re * inv_fft_len;
    }
    ch.out_len = block_len;
    ch.in_len = 0;
    ch.ring_slot = (slot + 1) % blocks;
}

/// Convolution 参数解析错误（v7.11，pipeline 4.19）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// 空参数（`Convolution:` 无值）。
    Empty,
    /// ≥3 tokens（`ir.wav -6 abc` 的 `abc` 不再静默忽略）。
    TooManyTokens { count: usize },
    /// 第 2 个 token 非数值（`ir.wav abc`）。
    InvalidGain { token: String },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "缺少参数（需要 IR 路径）"),
            Self::TooManyTokens { count } => {
                write!(f, "参数过多（{} 个 token，最多 2：路径 + 增益）", count)
            }
            Self::InvalidGain { token } => write!(f, "增益 '{}' 不是合法数值", token),
        }
    }
}

/// 解析 `Convolution:` 参数。
///
/// 格式：`path [gain_dB]`
///
/// 严格化（v7.11，pipeline 4.19）：≥3 tokens / 第 2 个非数值 → `Err(ParseError)`，
/// 不再静默忽略多余 token（「配置写错必有反馈」）。
/// `ParseError` 由 config 层包装为 `ConfigError::SyntaxError`（文件 + 行号）。
pub fn parse_convolution_params(params: &str) -> Result<(String, f32), ParseError> {
    let parts: Vec<&str> = params.split_whitespace().collect();
    if parts.is_empty() {
        return Err(ParseError::Empty);
    }
    if parts.len() >= 3 {
        return Err(ParseError::TooManyTokens { count: parts.len() });
    }

    let path = parts[0].to_owned();
    let gain = if parts.len() == 2 {
        parts[1]
            .parse::<f32>()
            .map_err(|_| ParseError::InvalidGain { token: parts[1].to_owned() })?
    } else {
        0.0
    };

    Ok((path, gain))
}

/// 从 WAV 文件加载 IR（PCM 16-bit / 32-bit float，单声道或立体声）。
///
/// 解析 RIFF/FMT/data 头。非 WAV、压缩格式、过大文件返回 Err。
fn load_wav_ir(path: &str) -> Result<Vec<f32>, String> {
    let data = std::fs::read(path).map_err(|e| format!("文件读取失败: {}", e))?;
    if data.len() > MAX_WAV_FILE_SIZE {
        return Err("文件超过 1 MiB 限制".to_owned());
    }
    if data.len() < 44 {
        return Err("文件过短，不是有效的 WAV".to_owned());
    }

    // RIFF 头。
    if &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return Err("不是 RIFF/WAVE 文件".to_owned());
    }

    // 定位 fmt / data chunk（跳过 LIST 等）。
    let mut pos = 12usize;
    let mut audio_format: Option<(u16, u16, u32, u16)> = None; // (format, channels, sample_rate, bits)
    let mut data_chunk: Option<&[u8]> = None;

    while pos + 8 <= data.len() {
        let chunk_id = &data[pos..pos + 4];
        let chunk_size = u32::from_le_bytes([
            data[pos + 4],
            data[pos + 5],
            data[pos + 6],
            data[pos + 7],
        ]) as usize;
        let chunk_start = pos + 8;

        match chunk_id {
            b"fmt " => {
                if chunk_start + 16 <= data.len() {
                    let format = u16::from_le_bytes([data[chunk_start], data[chunk_start + 1]]);
                    let channels = u16::from_le_bytes([
                        data[chunk_start + 2],
                        data[chunk_start + 3],
                    ]);
                    let sample_rate = u32::from_le_bytes([
                        data[chunk_start + 4],
                        data[chunk_start + 5],
                        data[chunk_start + 6],
                        data[chunk_start + 7],
                    ]);
                    let bits = u16::from_le_bytes([
                        data[chunk_start + 14],
                        data[chunk_start + 15],
                    ]);
                    audio_format = Some((format, channels, sample_rate, bits));
                }
            }
            b"data" => {
                if chunk_start <= data.len() {
                    let end = (chunk_start + chunk_size).min(data.len());
                    data_chunk = Some(&data[chunk_start..end]);
                }
                break; // data 通常最后一个 chunk。
            }
            _ => {}
        }
        // 进到下一 chunk（chunk_size 对齐到偶数字节）。
        pos = chunk_start + chunk_size + (chunk_size & 1);
    }

    let (format, channels, _sample_rate, bits) =
        audio_format.ok_or("缺少 fmt chunk".to_owned())?;
    let raw = data_chunk.ok_or("缺少 data chunk".to_owned())?;

    // 取第 0 通道（IR 通常单声道；若为立体声取左通道）。
    let samples = match (format, bits) {
        (1, 16) => parse_pcm16(raw, channels)?,
        (3, 32) => parse_f32(raw, channels)?,
        _ => return Err(format!("不支持的 WAV 格式: format={} bits={}", format, bits)),
    };

    Ok(samples)
}

/// 解析 16-bit PCM 数据（取第 0 通道）。
fn parse_pcm16(raw: &[u8], channels: u16) -> Result<Vec<f32>, String> {
    let ch = channels.max(1) as usize;
    let frame_bytes = ch * 2;
    if raw.len() < frame_bytes {
        return Err("PCM16 数据过短".to_owned());
    }
    let frame_count = raw.len() / frame_bytes;
    let mut out = Vec::with_capacity(frame_count);
    for i in 0..frame_count {
        let base = i * frame_bytes;
        let v = i16::from_le_bytes([raw[base], raw[base + 1]]) as f32 / 32768.0;
        out.push(v);
    }
    Ok(out)
}

/// 解析 32-bit float 数据（取第 0 通道）。
fn parse_f32(raw: &[u8], channels: u16) -> Result<Vec<f32>, String> {
    let ch = channels.max(1) as usize;
    let frame_bytes = ch * 4;
    if raw.len() < frame_bytes {
        return Err("F32 数据过短".to_owned());
    }
    let frame_count = raw.len() / frame_bytes;
    let mut out = Vec::with_capacity(frame_count);
    for i in 0..frame_count {
        let base = i * frame_bytes;
        let v = f32::from_le_bytes([
            raw[base],
            raw[base + 1],
            raw[base + 2],
            raw[base + 3],
        ]);
        out.push(v);
    }
    Ok(out)
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo_names() -> Vec<String> {
        vec!["L".into(), "R".into()]
    }

    /// 构建最小 PCM16 单声道 WAV 字节。
    fn pcm16_wav(samples: &[i16]) -> Vec<u8> {
        let data_len = samples.len() * 2;
        let mut wav = Vec::with_capacity(44 + data_len);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&48000u32.to_le_bytes());
        wav.extend_from_slice(&((48000 * 2) as u32).to_le_bytes()); // byte rate
        wav.extend_from_slice(&2u16.to_le_bytes()); // block align
        wav.extend_from_slice(&16u16.to_le_bytes()); // bits
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data_len as u32).to_le_bytes());
        for s in samples {
            wav.extend_from_slice(&s.to_le_bytes());
        }
        wav
    }

    /// 构建 32-bit float 单声道 WAV 字节。
    fn f32_wav(samples: &[f32]) -> Vec<u8> {
        let data_len = samples.len() * 4;
        let mut wav = Vec::with_capacity(44 + data_len);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&48000u32.to_le_bytes());
        wav.extend_from_slice(&((48000 * 4) as u32).to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&32u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data_len as u32).to_le_bytes());
        for s in samples {
            wav.extend_from_slice(&s.to_le_bytes());
        }
        wav
    }

    // ── parse_convolution_params ─────────────────────────────────────────────

    #[test]
    fn parse_path_only() {
        let (path, gain) = parse_convolution_params("ir.wav").unwrap();
        assert_eq!(path, "ir.wav");
        assert_eq!(gain, 0.0);
    }

    #[test]
    fn parse_path_and_gain() {
        let (path, gain) = parse_convolution_params("ir.wav -6").unwrap();
        assert_eq!(path, "ir.wav");
        assert_eq!(gain, -6.0);
    }

    #[test]
    fn parse_empty() {
        // v7.11：空参数 → Err(ParseError::Empty)。
        assert!(parse_convolution_params("").is_err());
    }

    // ── WAV 解析 ─────────────────────────────────────────────────────────────

    #[test]
    fn wav_pcm16_loads_mono() {
        let wav = pcm16_wav(&[1000, -1000, 500]);
        let dir = std::env::temp_dir();
        let path = dir.join("vxapo_test_ir_pcm16.wav");
        std::fs::write(&path, &wav).unwrap();

        let ir = load_wav_ir(path.to_str().unwrap()).unwrap();
        assert_eq!(ir.len(), 3);
        assert!((ir[0] - (1000.0 / 32768.0)).abs() < 1e-4);
        assert!((ir[1] - (-1000.0 / 32768.0)).abs() < 1e-4);
        assert!((ir[2] - (500.0 / 32768.0)).abs() < 1e-4);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn wav_f32_loads_mono() {
        let wav = f32_wav(&[0.5, -0.25, 0.125]);
        let dir = std::env::temp_dir();
        let path = dir.join("vxapo_test_ir_f32.wav");
        std::fs::write(&path, &wav).unwrap();

        let ir = load_wav_ir(path.to_str().unwrap()).unwrap();
        assert_eq!(ir, vec![0.5, -0.25, 0.125]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn wav_not_riff_errors() {
        let wav = b"NOTWAVE........";
        let dir = std::env::temp_dir();
        let path = dir.join("vxapo_test_ir_bad.wav");
        std::fs::write(&path, wav).unwrap();

        assert!(load_wav_ir(path.to_str().unwrap()).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn wav_unsupported_format_errors() {
        // format=0（非法），bits=8。
        let mut wav = pcm16_wav(&[1, 2, 3]);
        wav[20] = 0; // fmt format tag lo
        wav[21] = 0; // fmt format tag hi → 0x0000
        let dir = std::env::temp_dir();
        let path = dir.join("vxapo_test_ir_badfmt.wav");
        std::fs::write(&path, &wav).unwrap();

        assert!(load_wav_ir(path.to_str().unwrap()).is_err());
        let _ = std::fs::remove_file(&path);
    }

    // ── ConvolutionFilter 行为 ───────────────────────────────────────────────

    #[test]
    fn short_ir_convolves_single() {
        // IR = [1.0]，延迟 = 0，输出 = 输入。仍走卷积路径（delay 长度 1）。
        let wav = f32_wav(&[1.0, 0.0]);
        let dir = std::env::temp_dir();
        let path = dir.join("vxapo_test_ir_identity.wav");
        std::fs::write(&path, &wav).unwrap();

        let mut filter = ConvolutionFilter::new(path.to_str().unwrap(), 0.0);
        let ch = filter.initialize(48000, &stereo_names());
        assert!(ch.is_none());
        assert_eq!(filter.latency(), 1); // ir_len=2 → latency=1

        let mut samples = vec![vec![1.0, 2.0, 3.0], vec![0.5, 1.0, 1.5]];
        filter.process(&mut samples, 3);
        // IR=[1.0, 0.0]：输出 = 1.0*input[n] + 0.0*input[n-1]。
        assert!((samples[0][2] - 3.0).abs() < 1e-4);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn short_ir_two_tap_sums_frames() {
        // IR = [1.0, 1.0]：y[n] = x[n] + x[n-1]（含延迟 1）。
        let wav = f32_wav(&[1.0, 1.0]);
        let dir = std::env::temp_dir();
        let path = dir.join("vxapo_test_ir_two.wav");
        std::fs::write(&path, &wav).unwrap();

        let mut filter = ConvolutionFilter::new(path.to_str().unwrap(), 0.0);
        filter.initialize(48000, &stereo_names());
        assert_eq!(filter.latency(), 1);

        let mut samples = vec![vec![1.0, 1.0, 1.0], vec![0.0, 0.0, 0.0]];
        filter.process(&mut samples, 3);
        // 帧0: x[-1]=0 → y=1; 帧1: y=1+1=2; 帧2: y=1+1=2。
        assert!((samples[0][0] - 1.0).abs() < 1e-4);
        assert!((samples[0][1] - 2.0).abs() < 1e-4);
        assert!((samples[0][2] - 2.0).abs() < 1e-4);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn three_tap_ir_uses_pow2_delay_line() {
        // IR=[1,1,1]（3 抽头 → 延迟线长度补到 4，mask 路径）。
        let wav = f32_wav(&[1.0, 1.0, 1.0]);
        let dir = std::env::temp_dir();
        let path = dir.join("vxapo_test_ir_three.wav");
        std::fs::write(&path, &wav).unwrap();

        let mut filter = ConvolutionFilter::new(path.to_str().unwrap(), 0.0);
        filter.initialize(48000, &stereo_names());
        assert_eq!(filter.latency(), 2);

        let mut samples = vec![vec![1.0, 0.0, 0.0, 0.0], vec![0.0, 0.0, 0.0, 0.0]];
        filter.process(&mut samples, 4);
        // y[n] = x[n] + x[n-1] + x[n-2]：1, 1, 1, 0。
        assert!((samples[0][0] - 1.0).abs() < 1e-4);
        assert!((samples[0][1] - 1.0).abs() < 1e-4);
        assert!((samples[0][2] - 1.0).abs() < 1e-4);
        assert!(samples[0][3].abs() < 1e-4);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn partitioned_fft_matches_direct_reference() {
        // IR 200 采样（>128 → 分块 FFT，2 块；延迟 = 128）。
        let ir: Vec<f32> = (0..200)
            .map(|i| ((i as f32 * 0.05).sin() * 0.3) as f32)
            .collect();
        let wav = f32_wav(&ir);
        let dir = std::env::temp_dir();
        let path = dir.join("vxapo_test_ir_partitioned.wav");
        std::fs::write(&path, &wav).unwrap();

        let mut filter = ConvolutionFilter::new(path.to_str().unwrap(), 0.0);
        filter.initialize(48000, &stereo_names());
        assert_eq!(filter.latency(), CONVOLUTION_PARTITION_SIZE as u32);

        let len = 1024;
        let block = CONVOLUTION_PARTITION_SIZE;
        let input: Vec<f32> = (0..len)
            .map(|i| ((i as f32 * 0.01).sin() * 0.5) as f32)
            .collect();
        let mut samples = vec![input.clone(), vec![0.0f32; len]];
        filter.process(&mut samples, len);

        // 参考：直接时域卷积，分块延迟 = block（output[i] = y[i-block]）。
        for i in 0..len {
            let expected = if i >= block {
                let n = i - block;
                let mut acc = 0.0f32;
                for (k, &h) in ir.iter().enumerate() {
                    if n >= k {
                        acc += h * input[n - k];
                    }
                }
                acc
            } else {
                0.0
            };
            assert!(
                (samples[0][i] - expected).abs() < 2e-3,
                "partitioned mismatch at {i}: {} vs {}",
                samples[0][i],
                expected
            );
        }
        assert!(samples[1].iter().all(|&v| v == 0.0));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reset_clears_delay_line() {
        let wav = f32_wav(&[1.0, 1.0]);
        let dir = std::env::temp_dir();
        let path = dir.join("vxapo_test_ir_reset.wav");
        std::fs::write(&path, &wav).unwrap();

        let mut filter = ConvolutionFilter::new(path.to_str().unwrap(), 0.0);
        filter.initialize(48000, &stereo_names());

        let mut samples = vec![vec![1.0, 1.0], vec![0.0, 0.0]];
        filter.process(&mut samples, 2);
        filter.reset();

        let mut samples2 = vec![vec![1.0, 1.0], vec![0.0, 0.0]];
        filter.process(&mut samples2, 2);
        // 重置后历史清空：帧0 y=x[0]=1；帧1 y=1+1=2。
        assert!((samples2[0][0] - 1.0).abs() < 1e-4);
        assert!((samples2[0][1] - 2.0).abs() < 1e-4);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unloaded_filter_passthrough() {
        // IR 不存在 → 未加载 → process 直通。
        let mut filter = ConvolutionFilter::new("nonexistent_ir_xyz.wav", 0.0);
        filter.initialize(48000, &stereo_names());
        assert_eq!(filter.latency(), 0);

        let mut samples = vec![vec![1.0, 2.0, 3.0], vec![0.5, 1.0, 1.5]];
        let input = samples.clone();
        filter.process(&mut samples, 3);
        assert_eq!(samples, input);
    }

    #[test]
    fn latency_zero_when_unloaded() {
        let filter = ConvolutionFilter::new("ir.wav", 0.0);
        assert_eq!(filter.latency(), 0);
    }

    #[test]
    fn dot_matches_naive_sum() {
        // v9.7：分段点积（AVX2/标量两路径共用同一断言）。
        let a: Vec<f32> = (0..1000)
            .map(|i| ((i as f32 * 0.7).sin() * 0.5) as f32)
            .collect();
        let b: Vec<f32> = (0..1000)
            .map(|i| ((i as f32 * 0.3).cos() * 0.25) as f32)
            .collect();
        let naive: f32 = a.iter().zip(&b).map(|(&x, &y)| x * y).sum();
        let got = dot(&a, &b);
        assert!((got - naive).abs() < 1e-3, "dot mismatch {got} vs {naive}");
    }

    #[test]
    fn direct_fir_segmented_matches_naive_convolution() {
        // v9.7：IR 长度 4（延迟线补到 4）→ 强制回绕分支；与朴素卷积逐点比对。
        let ir: Vec<f32> = vec![0.5, -0.25, 1.0, 0.125];
        let x: Vec<f32> = vec![0.3, -0.7, 1.2, 0.9, -0.4, 0.6, 0.1, -1.0, 0.55];
        let naive: Vec<f32> = (0..x.len())
            .map(|n| {
                (0..ir.len())
                    .map(|i| if n >= i { ir[i] * x[n - i] } else { 0.0 })
                    .sum::<f32>()
            })
            .collect();
        let mut filter = ConvolutionFilter::with_ir_direct(ir, 0.0);
        filter.initialize(48000, &["M".to_owned()]);
        let mut samples = vec![x.clone()];
        filter.process(&mut samples, x.len());
        for n in 0..x.len() {
            assert!(
                (samples[0][n] - naive[n]).abs() < 1e-4,
                "n={n} got={} want={}",
                samples[0][n],
                naive[n]
            );
        }
    }
}
