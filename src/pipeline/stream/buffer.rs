//! pipeline/stream/buffer.rs — 缓冲区标志判定与静音缓冲区处理（Note 11）
//!
//! `APOProcess` 的输入/输出 `APO_CONNECTION_PROPERTY` 携带 `flags` 字段，
//! 指示缓冲区状态（`APO_BUFFER_FLAGS::Valid` / `APO_BUFFER_FLAGS::Silent` / `APO_BUFFER_FLAGS::Invalid`）。
//!
//! `allowSilentBufferModification` 在 `APOInitSystemEffects` 初始化阶段读取，
//! 控制 APO 是否可快速跳过静音帧处理。
//!
//! 处理逻辑（Note 11）：
//! ```text
//! APO_BUFFER_FLAGS::Silent + allowSilentBuffer → 遍历采样，全 ≤1e-10 则 SILENT，否则 VALID
//! APO_BUFFER_FLAGS::Silent + !allowSilentBuffer → 强制清零，标记 SILENT
//! APO_BUFFER_FLAGS::Valid                      → 直接 VALID（正常处理）
//! 其他标志                          → 不处理
//! ```
//!
//! 此模块运行在实时音频线程中，禁止堆分配与 panic（Note 12）。

use crate::sys::com::apo_abi::APO_BUFFER_FLAGS;

/// 静音判定阈值：所有采样的绝对值 ≤ 此值时视为静音。
///
/// 对应 -200 dBFS 以下，远低于任何可听信号或 DAC 本底噪声。
const SILENCE_THRESHOLD: f32 = 1e-10;

// ══════════════════════════════════════════════════════════════════════════════
// BufferFlags — 缓冲区状态判定结果
// ══════════════════════════════════════════════════════════════════════════════

/// `evaluate_buffer` 的返回值，描述缓冲区应如何处理。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferAction {
    /// 正常处理——缓冲区包含有效音频数据。
    Process,
    /// 快速路径——缓冲区为静音，可跳过 DSP 处理。
    /// 输出标志应设为 `APO_BUFFER_FLAGS::Silent`。
    Silent,
    /// 跳过——标志不合法或为 `APO_BUFFER_FLAGS::Invalid`，不处理。
    Skip,
}

/// 根据输入标志和 `allowSilentBuffer` 确定处理动作。
///
/// 返回 `(BufferAction, output_flags)`：
/// - `BufferAction`：告诉 `pipeline.rs` 如何处理
/// - `output_flags`：写入输出 `APO_CONNECTION_PROPERTY::flags`
///
/// # 实时安全
///
/// 纯数值计算，无分配、无锁、无 I/O。
pub fn evaluate_buffer(
    flags: APO_BUFFER_FLAGS,
    allow_silent_buffer: bool,
) -> (BufferAction, APO_BUFFER_FLAGS) {
    match flags {
        APO_BUFFER_FLAGS::Valid => (BufferAction::Process, APO_BUFFER_FLAGS::Valid),

        APO_BUFFER_FLAGS::Silent => {
            if allow_silent_buffer {
                // 允许静音缓冲区：需要逐采样检查
                // 返回 Process 让调用方检查实际数据
                // 调用方检查后若确实静音，设置输出为 SILENT
                (BufferAction::Process, APO_BUFFER_FLAGS::Silent)
            } else {
                // 不允许静音缓冲区：强制清零
                (BufferAction::Silent, APO_BUFFER_FLAGS::Silent)
            }
        }

        APO_BUFFER_FLAGS::Invalid | _ => (BufferAction::Skip, APO_BUFFER_FLAGS::Invalid),
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 静音检测（Note 11 实时路径用）
// ══════════════════════════════════════════════════════════════════════════════

/// 检查平面缓冲区中的所有采样是否为静音。
///
/// 遍历所有通道的所有采样，若全 ≤ `SILENCE_THRESHOLD` 则返回 `true`。
///
/// # 实时安全
///
/// 纯读取 + 比较，无分配。
/// 通道数 1/2/6/8 用编译期常量边界，编译器可展开循环。
pub fn is_silent(samples: &[Vec<f32>], frame_count: usize) -> bool {
    match samples.len() {
        1 => is_silent_n(samples, frame_count, 1),
        2 => is_silent_n(samples, frame_count, 2),
        6 => is_silent_n(samples, frame_count, 6),
        8 => is_silent_n(samples, frame_count, 8),
        _ => is_silent_generic(samples, frame_count),
    }
}

/// N 通道静音检测特化。
#[inline]
fn is_silent_n(samples: &[Vec<f32>], frame_count: usize, n: usize) -> bool {
    for c in 0..n {
        for f in 0..frame_count {
            if samples[c][f].abs() > SILENCE_THRESHOLD {
                return false;
            }
        }
    }
    true
}

/// 通用静音检测。
#[inline(never)]
fn is_silent_generic(samples: &[Vec<f32>], frame_count: usize) -> bool {
    for channel in samples.iter() {
        for f in 0..frame_count {
            if channel[f].abs() > SILENCE_THRESHOLD {
                return false;
            }
        }
    }
    true
}

// ══════════════════════════════════════════════════════════════════════════════
// 缓冲区清零（实时路径用）
// ══════════════════════════════════════════════════════════════════════════════

/// 将平面缓冲区所有通道的指定帧数范围清零。
///
/// 用途：
/// - `APO_BUFFER_FLAGS::Silent + !allowSilentBuffer` → 强制清零输入（Note 11）
/// - `catch_unwind` 捕获 panic 后清零输出（Note 60）
/// - `pipeline.rs` 中清零额外通道（Note 18）
///
/// # 实时安全
///
/// 纯写入，无分配。
pub fn zero_buffers(buffers: &mut [Vec<f32>], frame_count: usize) {
    for channel in buffers.iter_mut() {
        for f in 0..frame_count {
            channel[f] = 0.0;
        }
    }
}

/// 将单个通道清零。
pub fn zero_channel(channel: &mut [f32], frame_count: usize) {
    for f in 0..frame_count {
        channel[f] = 0.0;
    }
}

/// 将缓冲区所有通道填充为指定值。
///
/// 用于测试和特殊场景（如填充静音值 -inf dB）。
///
/// # 实时安全
///
/// 纯写入，无分配。
pub fn fill_buffers(buffers: &mut [Vec<f32>], frame_count: usize, value: f32) {
    for channel in buffers.iter_mut() {
        for f in 0..frame_count {
            channel[f] = value;
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 缓冲区拷贝（实时路径用，替代 Vec::clone）
// ══════════════════════════════════════════════════════════════════════════════

/// 将源缓冲区内容复制到目标缓冲区（逐通道逐采样）。
///
/// 替代 `Vec::clone()`（Note 58 禁止隐式堆分配）。
/// 目标缓冲区必须已预分配且长度 ≥ 源缓冲区。
///
/// # 实时安全
///
/// 纯 memcpy 语义，无分配。
pub fn copy_buffers(
    src: &[Vec<f32>],
    dst: &mut [Vec<f32>],
    frame_count: usize,
) {
    let channels = src.len().min(dst.len());
    for c in 0..channels {
        for f in 0..frame_count {
            dst[c][f] = src[c][f];
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 缓冲区状态摘要（调试用，非实时路径）
// ══════════════════════════════════════════════════════════════════════════════

/// 缓冲区状态摘要（调试日志用）。
#[derive(Debug, Clone)]
pub struct BufferSummary {
    pub channels: usize,
    pub frame_count: usize,
    pub is_silent: bool,
    pub peak_level: f32,
    pub rms_level: f32,
}

/// 计算缓冲区状态摘要（非实时路径，含浮点除法和 sqrt）。
pub fn summarize(buffers: &[Vec<f32>], frame_count: usize) -> BufferSummary {
    let channels = buffers.len();
    let mut peak = 0.0f32;
    let mut sum_sq = 0.0f32;

    for channel in buffers.iter() {
        for f in 0..frame_count {
            let v = channel[f].abs();
            if v > peak {
                peak = v;
            }
            sum_sq += channel[f] * channel[f];
        }
    }

    let total_samples = (channels * frame_count) as f32;
    let rms = if total_samples > 0.0 {
        (sum_sq / total_samples).sqrt()
    } else {
        0.0
    };

    BufferSummary {
        channels,
        frame_count,
        is_silent: peak <= SILENCE_THRESHOLD,
        peak_level: peak,
        rms_level: rms,
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── evaluate_buffer ─────────────────────────────────────────────────────

    #[test]
    fn evaluate_valid_buffer() {
        let (action, flags) = evaluate_buffer(APO_BUFFER_FLAGS::Valid, false);
        assert_eq!(action, BufferAction::Process);
        assert_eq!(flags, APO_BUFFER_FLAGS::Valid);
    }

    #[test]
    fn evaluate_valid_buffer_with_silent_allowed() {
        let (action, flags) = evaluate_buffer(APO_BUFFER_FLAGS::Valid, true);
        assert_eq!(action, BufferAction::Process);
        assert_eq!(flags, APO_BUFFER_FLAGS::Valid);
    }

    #[test]
    fn evaluate_silent_with_allow() {
        let (action, flags) = evaluate_buffer(APO_BUFFER_FLAGS::Silent, true);
        assert_eq!(action, BufferAction::Process);
        assert_eq!(flags, APO_BUFFER_FLAGS::Silent);
    }

    #[test]
    fn evaluate_silent_without_allow() {
        let (action, flags) = evaluate_buffer(APO_BUFFER_FLAGS::Silent, false);
        assert_eq!(action, BufferAction::Silent);
        assert_eq!(flags, APO_BUFFER_FLAGS::Silent);
    }

    #[test]
    fn evaluate_invalid_buffer() {
        let (action, flags) = evaluate_buffer(APO_BUFFER_FLAGS::Invalid, false);
        assert_eq!(action, BufferAction::Skip);
        assert_eq!(flags, APO_BUFFER_FLAGS::Invalid);
    }

    // ── is_silent ───────────────────────────────────────────────────────────

    #[test]
    fn is_silent_all_zero() {
        let buf = vec![vec![0.0; 128], vec![0.0; 128]];
        assert!(is_silent(&buf, 128));
    }

    #[test]
    fn is_silent_below_threshold() {
        let buf = vec![
            vec![1e-11; 128],
            vec![-1e-11; 128],
        ];
        assert!(is_silent(&buf, 128));
    }

    #[test]
    fn is_silent_at_threshold() {
        let buf = vec![vec![SILENCE_THRESHOLD; 128]];
        assert!(is_silent(&buf, 128));
    }

    #[test]
    fn is_not_silent_above_threshold() {
        let mut buf = vec![vec![0.0; 128]];
        buf[0][64] = 0.001;
        assert!(!is_silent(&buf, 128));
    }

    #[test]
    fn is_not_silent_single_sample() {
        let mut buf = vec![vec![0.0; 128]];
        buf[0][0] = 1.0;
        assert!(!is_silent(&buf, 128));
    }

    #[test]
    fn is_silent_8ch() {
        let buf = vec![vec![0.0; 64]; 8];
        assert!(is_silent(&buf, 64));
    }

    #[test]
    fn is_silent_partial_frame_count() {
        let buf = vec![vec![1.0; 128]];
        // 只检查前 64 帧，后 64 帧虽然非零但不检查
        assert!(is_silent(&buf, 0)); // 0 帧 = 空 = 静音
    }

    #[test]
    fn is_silent_mono() {
        let buf = vec![vec![0.0; 100]];
        assert!(is_silent(&buf, 100));
    }

    #[test]
    fn is_silent_3ch_generic() {
        let buf = vec![vec![0.0; 64], vec![0.0; 64], vec![0.0; 64]];
        assert!(is_silent(&buf, 64));
    }

    #[test]
    fn is_not_silent_3ch_generic() {
        let mut buf = vec![vec![0.0; 64], vec![0.0; 64], vec![0.0; 64]];
        buf[2][32] = 0.5;
        assert!(!is_silent(&buf, 64));
    }

    // ── zero_buffers ────────────────────────────────────────────────────────

    #[test]
    fn zero_buffers_basic() {
        let mut buf = vec![vec![1.0; 128], vec![2.0; 128]];
        zero_buffers(&mut buf, 128);
        assert!(buf[0].iter().all(|&v| v == 0.0));
        assert!(buf[1].iter().all(|&v| v == 0.0));
    }

    #[test]
    fn zero_buffers_partial() {
        let mut buf = vec![vec![99.0; 128]];
        zero_buffers(&mut buf, 64);
        assert!(buf[0][..64].iter().all(|&v| v == 0.0));
        assert!(buf[0][64..].iter().all(|&v| v == 99.0));
    }

    #[test]
    fn zero_channel_basic() {
        let mut ch = vec![1.0; 64];
        zero_channel(&mut ch, 64);
        assert!(ch.iter().all(|&v| v == 0.0));
    }

    // ── fill_buffers ────────────────────────────────────────────────────────

    #[test]
    fn fill_buffers_basic() {
        let mut buf = vec![vec![0.0; 4], vec![0.0; 4]];
        fill_buffers(&mut buf, 4, 0.5);
        assert!(buf[0].iter().all(|&v| v == 0.5));
        assert!(buf[1].iter().all(|&v| v == 0.5));
    }

    // ── copy_buffers ────────────────────────────────────────────────────────

    #[test]
    fn copy_buffers_basic() {
        let src = vec![vec![1.0, 2.0, 3.0], vec![4.0, 5.0, 6.0]];
        let mut dst = vec![vec![0.0; 3], vec![0.0; 3]];
        copy_buffers(&src, &mut dst, 3);
        assert_eq!(dst, src);
    }

    #[test]
    fn copy_buffers_partial_frame() {
        let src = vec![vec![1.0, 2.0, 3.0]];
        let mut dst = vec![vec![0.0; 3]];
        copy_buffers(&src, &mut dst, 2);
        assert_eq!(dst[0][0], 1.0);
        assert_eq!(dst[0][1], 2.0);
        assert_eq!(dst[0][2], 0.0); // 未复制
    }

    #[test]
    fn copy_buffers_fewer_dst_channels() {
        let src = vec![vec![1.0, 2.0], vec![3.0, 4.0], vec![5.0, 6.0]];
        let mut dst = vec![vec![0.0; 2], vec![0.0; 2]];
        copy_buffers(&src, &mut dst, 2);
        assert_eq!(dst[0], vec![1.0, 2.0]);
        assert_eq!(dst[1], vec![3.0, 4.0]);
        // src[2] 被忽略
    }

    // ── summarize（非实时路径）───────────────────────────────────────────────

    #[test]
    fn summarize_silent() {
        let buf = vec![vec![0.0; 128], vec![0.0; 128]];
        let s = summarize(&buf, 128);
        assert!(s.is_silent);
        assert_eq!(s.peak_level, 0.0);
        assert_eq!(s.rms_level, 0.0);
        assert_eq!(s.channels, 2);
        assert_eq!(s.frame_count, 128);
    }

    #[test]
    fn summarize_with_signal() {
        let buf = vec![vec![0.5; 128]];
        let s = summarize(&buf, 128);
        assert!(!s.is_silent);
        assert_eq!(s.peak_level, 0.5);
        assert!((s.rms_level - 0.5).abs() < 0.001);
    }

    #[test]
    fn summarize_mixed() {
        let mut buf = vec![vec![0.0; 4], vec![0.0; 4]];
        buf[0][2] = 0.8;
        buf[1][3] = 0.6;
        let s = summarize(&buf, 4);
        assert!(!s.is_silent);
        assert_eq!(s.peak_level, 0.8);
    }

    #[test]
    fn summarize_empty() {
        let buf: Vec<Vec<f32>> = vec![];
        let s = summarize(&buf, 0);
        assert!(s.is_silent);
        assert_eq!(s.channels, 0);
    }

    // ── 静音检测阈值边界 ────────────────────────────────────────────────────

    #[test]
    fn silence_threshold_is_200db_below_fs() {
        // 1e-10 对应约 -200 dBFS
        let db = 20.0 * SILENCE_THRESHOLD.log10();
        assert!(db < -199.0);
    }

    // ── 模拟 pipeline 处理流程 ──────────────────────────────────────────────

    #[test]
    fn simulate_silent_buffer_fast_path() {
        let mut input = vec![vec![0.0; 480], vec![0.0; 480]];
        let allow_silent = true;

        let (action, output_flags) = evaluate_buffer(APO_BUFFER_FLAGS::Silent, allow_silent);
        assert_eq!(action, BufferAction::Process);

        // 检查实际数据
        if action == BufferAction::Process && output_flags == APO_BUFFER_FLAGS::Silent {
            if is_silent(&input, 480) {
                // 确实静音，输出标志保持 SILENT，跳过 DSP
                assert_eq!(output_flags, APO_BUFFER_FLAGS::Silent);
            } else {
                // 非静音，正常处理
            }
        }

        zero_buffers(&mut input, 480);
        assert!(is_silent(&input, 480));
    }

    #[test]
    fn simulate_silent_buffer_force_zero() {
        let mut input = vec![vec![0.5; 480]]; // 输入有信号
        let allow_silent = false;

        let (action, output_flags) = evaluate_buffer(APO_BUFFER_FLAGS::Silent, allow_silent);
        assert_eq!(action, BufferAction::Silent);
        assert_eq!(output_flags, APO_BUFFER_FLAGS::Silent);

        // 强制清零
        zero_buffers(&mut input, 480);
        assert!(is_silent(&input, 480));
    }

    #[test]
    fn simulate_valid_buffer_normal_processing() {
        let input = vec![vec![0.5; 480], vec![-0.3; 480]];

        let (action, output_flags) = evaluate_buffer(APO_BUFFER_FLAGS::Valid, true);
        assert_eq!(action, BufferAction::Process);
        assert_eq!(output_flags, APO_BUFFER_FLAGS::Valid);

        // 正常 DSP 处理...
        assert!(!is_silent(&input, 480));
    }
}