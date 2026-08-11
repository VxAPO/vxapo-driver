//! dsp/fir.rs — FIR 执行基础设施（v9.11）
//!
//! - SIMD 点积（AVX2+FMA 运行时探测，标量 mul_add 回退）——从 convolution.rs
//!   迁出，供 Wide / 混合 PEQ 共用；
//! - 直接 FIR 延迟线与分段点积（2 的幂环形缓冲，v9.7 语义）；
//! - 分块 FFT 卷积（uniform partitioned overlap-add，输出驱动 + 补块 flush，
//!   流停止时尾部不压块——修复历史切换“嗡声”）。

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::pipeline::dsp::math::CONVOLUTION_PARTITION_SIZE;

/// AVX2+FMA 可用标志（运行时探测一次）。
#[cfg(target_arch = "x86_64")]
static USE_AVX2_FMA: AtomicBool = AtomicBool::new(false);

/// 运行时探测 AVX2+FMA（非 RT，可重复调用；幂等）。
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

/// 分块 FFT 卷积引擎（uniform partitioned overlap-add）。
pub(crate) struct PartitionedFir {
    blocks: usize,
    block_len: usize,
    fft_len: usize,
    inv_fft_len: f32,
    fft: Arc<dyn Fft<f32>>,
    ifft: Arc<dyn Fft<f32>>,
    scratch: Vec<Complex<f32>>,
    h_ffts: Vec<Complex<f32>>,
    channels: Vec<PartitionedFirChannel>,
}

impl std::fmt::Debug for PartitionedFir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PartitionedFir")
            .field("blocks", &self.blocks)
            .field("block_len", &self.block_len)
            .field("channels", &self.channels.len())
            .finish()
    }
}

#[derive(Debug)]
struct PartitionedFirChannel {
    in_buf: Vec<f32>,
    in_len: usize,
    x_ring: Vec<Complex<f32>>,
    ring_slot: usize,
    x_work: Vec<Complex<f32>>,
    acc: Vec<Complex<f32>>,
    overlap: Vec<f32>,
    out_buf: Vec<f32>,
    out_len: usize,
}

impl PartitionedFir {
    pub(crate) fn new(ir: &[f32], channels: usize) -> Self {
        let block_len = CONVOLUTION_PARTITION_SIZE;
        let fft_len = block_len * 2;
        let blocks = ir.len().div_ceil(block_len).max(1);

        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(fft_len);
        let ifft = planner.plan_fft_inverse(fft_len);
        let scratch_len = fft
            .get_inplace_scratch_len()
            .max(ifft.get_inplace_scratch_len());
        let mut scratch = vec![Complex::new(0.0, 0.0); scratch_len];

        // IR 分块频域（最后一块补零）。
        let mut h_ffts = vec![Complex::new(0.0, 0.0); blocks * fft_len];
        for b in 0..blocks {
            let mut work = vec![Complex::new(0.0, 0.0); fft_len];
            let start = b * block_len;
            let end = (start + block_len).min(ir.len());
            for (i, &v) in ir[start..end].iter().enumerate() {
                work[i] = Complex::new(v, 0.0);
            }
            fft.process_with_scratch(&mut work, &mut scratch);
            h_ffts[b * fft_len..(b + 1) * fft_len].copy_from_slice(&work);
        }

        let channels = (0..channels)
            .map(|_| PartitionedFirChannel {
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

        Self {
            blocks,
            block_len,
            fft_len,
            inv_fft_len: 1.0 / fft_len as f32,
            fft,
            ifft,
            scratch,
            h_ffts,
            channels,
        }
    }

    /// 每帧进样 + 输出一个样本。
    ///
    /// 输出驱动：块满立即处理；`is_last = true` 且输入缓冲有残留时补零处理
    /// （调用结束输入缓冲恒为空，流停止不压块丢尾音）。
    pub(crate) fn process_channel(&mut self, k: usize, x: f32, is_last: bool) -> f32 {
        let block_len = self.block_len;
        let ch = &mut self.channels[k];
        ch.in_buf[ch.in_len] = x;
        ch.in_len += 1;

        let out = if ch.out_len > 0 {
            let idx = ch.out_buf.len() - ch.out_len;
            ch.out_len -= 1;
            ch.out_buf[idx]
        } else if ch.in_len == block_len || (is_last && ch.in_len > 0) {
            if ch.in_len < block_len {
                ch.in_buf[ch.in_len..block_len].fill(0.0);
            }
            process_block(
                &self.fft,
                &self.ifft,
                &mut self.scratch,
                &self.h_ffts,
                self.blocks,
                block_len,
                self.fft_len,
                self.inv_fft_len,
                ch,
            );
            ch.out_len -= 1;
            ch.out_buf[0]
        } else {
            0.0
        };

        if ch.in_len == block_len || (is_last && ch.in_len > 0) {
            ch.in_len = 0;
        }
        out
    }

    pub(crate) fn reset(&mut self) {
        for ch in self.channels.iter_mut() {
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

    /// 算法延迟（块大小，采样）。
    pub(crate) fn latency(&self) -> u32 {
        self.block_len as u32
    }
}

/// 处理一个输入块：FFT → 与 IR 各块频域相乘累加 → IFFT → overlap-add。
fn process_block(
    fft: &Arc<dyn Fft<f32>>,
    ifft: &Arc<dyn Fft<f32>>,
    scratch: &mut [Complex<f32>],
    h_ffts: &[Complex<f32>],
    blocks: usize,
    block_len: usize,
    fft_len: usize,
    inv_fft_len: f32,
    ch: &mut PartitionedFirChannel,
) {
    for (i, &v) in ch.in_buf.iter().enumerate() {
        ch.x_work[i] = Complex::new(v, 0.0);
    }
    for v in ch.x_work.iter_mut().skip(block_len) {
        *v = Complex::new(0.0, 0.0);
    }
    fft.process_with_scratch(&mut ch.x_work, scratch);
    let slot = ch.ring_slot;
    ch.x_ring[slot * fft_len..(slot + 1) * fft_len].copy_from_slice(&ch.x_work);

    ch.acc.fill(Complex::new(0.0, 0.0));
    for b in 0..blocks {
        let xs = &ch.x_ring[((slot + blocks - b) % blocks) * fft_len..];
        let hs = &h_ffts[b * fft_len..];
        for i in 0..fft_len {
            ch.acc[i] += xs[i] * hs[i];
        }
    }
    ifft.process_with_scratch(&mut ch.acc, scratch);

    for j in 0..block_len {
        ch.out_buf[j] = ch.acc[j].re * inv_fft_len + ch.overlap[j];
        ch.overlap[j] = ch.acc[j + block_len].re * inv_fft_len;
    }
    ch.ring_slot = (slot + 1) % blocks;
    ch.out_len = block_len;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_matches_naive_sum() {
        init_fir_simd();
        let a: Vec<f32> = (0..64).map(|i| (i as f32) * 0.25).collect();
        let b: Vec<f32> = (0..64).map(|i| (63 - i) as f32).collect();
        let naive: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        assert!((dot(&a, &b) - naive).abs() < 1e-3);

        // 非 8 倍数长度（AVX2 尾路径）。
        let a2: Vec<f32> = (0..17).map(|i| i as f32 * 0.5).collect();
        let b2: Vec<f32> = (0..17).map(|i| (16 - i) as f32).collect();
        let naive2: f32 = a2.iter().zip(b2.iter()).map(|(x, y)| x * y).sum();
        assert!((dot(&a2, &b2) - naive2).abs() < 1e-3);
    }

    #[test]
    fn partitioned_matches_naive_convolution() {
        let ir: Vec<f32> = (0..300).map(|i| ((i as f32) * 0.7).sin() * 0.1).collect();
        let input: Vec<f32> = (0..500).map(|i| ((i as f32) * 0.13).cos() * 0.4).collect();

        // 朴素卷积（截断到输入长度）。
        let mut naive = vec![0.0f32; input.len()];
        for n in 0..input.len() {
            let mut acc = 0.0f32;
            for j in 0..ir.len().min(n + 1) {
                acc += input[n - j] * ir[j];
            }
            naive[n] = acc;
        }

        let mut pf = PartitionedFir::new(&ir, 1);
        let block_len = CONVOLUTION_PARTITION_SIZE;
        let mut out = Vec::with_capacity(input.len());
        for &x in &input {
            out.push(pf.process_channel(0, x, false));
        }
        // 延迟 = block_len - 1 帧（输出下标 i 对应 naive[i - (block_len-1)]）。
        for i in (block_len - 1)..input.len() {
            let want = naive[i - (block_len - 1)];
            assert!(
                (out[i] - want).abs() < 1e-4,
                "i={i}: got {}, want {}",
                out[i],
                want
            );
        }
    }

    #[test]
    fn partitioned_tail_flush_is_finite_and_resumes() {
        let ir: Vec<f32> = (0..256).map(|i| ((i as f32) * 0.3).cos() * 0.2).collect();
        let mut pf = PartitionedFir::new(&ir, 2);

        // 短流（< 块大小）：最后一帧补零 flush，输出全部有限。
        let short: Vec<f32> = (0..50).map(|i| i as f32 * 0.01).collect();
        let mut out = Vec::new();
        for (i, &x) in short.iter().enumerate() {
            out.push(pf.process_channel(0, x, i == short.len() - 1));
        }
        for v in &out {
            assert!(v.is_finite());
        }

        // 后续长流继续处理无错位（不 panic、有限）。
        let long: Vec<f32> = (0..400).map(|i| ((i as f32) * 0.2).sin() * 0.3).collect();
        for (i, &x) in long.iter().enumerate() {
            let v = pf.process_channel(0, x, i == long.len() - 1);
            assert!(v.is_finite());
        }
        // 双通道状态独立。
        let v = pf.process_channel(1, 0.5, true);
        assert!(v.is_finite());
    }
}
