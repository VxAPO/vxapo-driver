//! pipeline/stream/deinterleave.rs — 去交织 / 交织转换（Note 20）
//!
//! Windows APO 的 `APOProcess` 接收与输出交织格式缓冲区（`L0 R0 L1 R1 ...`），
//! 引擎内部处理使用平面格式（`[ch][frame]`）。
//!
//! 提供两方向转换：
//! - `deinterleave`：交织 → 平面
//! - `interleave`：平面 → 交织
//!
//! 编译期优化（Note 20）：
//! - 通道数 1 / 2 / 6 / 8 使用特化宏展开
//! - 其他通道数使用通用循环
//! - 非交织版本（单通道）直接 `memcpy`
//!
//! 此模块运行在实时音频线程中，禁止堆分配、互斥锁与 panic（Note 12）。

// ══════════════════════════════════════════════════════════════════════════════
// 特化宏（Note 20）
// ══════════════════════════════════════════════════════════════════════════════

/// 为指定通道数生成去交织特化代码。
///
/// 编译器看到常量循环边界后会展开循环，消除分支和索引计算开销。
macro_rules! deinterleave_n {
    ($input:expr, $output:expr, $frame_count:expr, $n:expr) => {{
        let frames = $frame_count;
        for f in 0..frames {
            let base = f * $n;
            for c in 0..$n {
                $output[c][f] = $input[base + c];
            }
        }
    }};
}

/// 为指定通道数生成交织特化代码。
macro_rules! interleave_n {
    ($input:expr, $output:expr, $frame_count:expr, $n:expr) => {{
        let frames = $frame_count;
        for f in 0..frames {
            let base = f * $n;
            for c in 0..$n {
                $output[base + c] = $input[c][f];
            }
        }
    }};
}

// ══════════════════════════════════════════════════════════════════════════════
// deinterleave — 交织 → 平面
// ══════════════════════════════════════════════════════════════════════════════

/// 将交织格式音频数据转为平面格式。
///
/// - `input`：交织数据 `[L0 R0 C0 LFE0 L1 R1 C1 LFE1 ...]`，长度 = `channel_count × frame_count`
/// - `output`：平面数据 `[channel][frame]`，每个通道长度 = `frame_count`
/// - `channel_count`：通道数
/// - `frame_count`：帧数（每通道采样数）
///
/// # 实时安全
///
/// 无堆分配、无分支（热路径中通道数为常量时编译器展开）。
///
/// # Panics
///
/// `input.len() < channel_count × frame_count` 或 `output.len() < channel_count` 或
/// `output[ch].len() < frame_count` 时 panic（调试模式下捕获）。
pub fn deinterleave(
    input: &[f32],
    output: &mut [Vec<f32>],
    channel_count: usize,
    frame_count: usize,
) {
    debug_assert!(
        input.len() >= channel_count * frame_count,
        "input buffer too short: need {}, got {}",
        channel_count * frame_count,
        input.len()
    );
    debug_assert!(
        output.len() >= channel_count,
        "output channel array too short: need {}, got {}",
        channel_count,
        output.len()
    );

    // 单通道：直接 memcpy（Note 20）
    if channel_count == 1 {
        output[0][..frame_count].copy_from_slice(&input[..frame_count]);
        return;
    }

    match channel_count {
        2 => deinterleave_n!(input, output, frame_count, 2),
        6 => deinterleave_n!(input, output, frame_count, 6),
        8 => deinterleave_n!(input, output, frame_count, 8),
        _ => deinterleave_generic(input, output, channel_count, frame_count),
    }
}

/// 通用去交织（非特化通道数）。
#[inline(never)]
fn deinterleave_generic(
    input: &[f32],
    output: &mut [Vec<f32>],
    channel_count: usize,
    frame_count: usize,
) {
    for f in 0..frame_count {
        let base = f * channel_count;
        for c in 0..channel_count {
            output[c][f] = input[base + c];
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// interleave — 平面 → 交织
// ══════════════════════════════════════════════════════════════════════════════

/// 将平面格式音频数据转为交织格式。
///
/// - `input`：平面数据 `[channel][frame]`
/// - `output`：交织数据，长度 = `channel_count × frame_count`
/// - `channel_count`：通道数
/// - `frame_count`：帧数
///
/// # 实时安全
///
/// 无堆分配。
pub fn interleave(
    input: &[Vec<f32>],
    output: &mut [f32],
    channel_count: usize,
    frame_count: usize,
) {
    debug_assert!(
        output.len() >= channel_count * frame_count,
        "output buffer too short: need {}, got {}",
        channel_count * frame_count,
        output.len()
    );
    debug_assert!(
        input.len() >= channel_count,
        "input channel array too short: need {}, got {}",
        channel_count,
        input.len()
    );

    // 单通道：直接 memcpy（Note 20）
    if channel_count == 1 {
        output[..frame_count].copy_from_slice(&input[0][..frame_count]);
        return;
    }

    match channel_count {
        2 => interleave_n!(input, output, frame_count, 2),
        6 => interleave_n!(input, output, frame_count, 6),
        8 => interleave_n!(input, output, frame_count, 8),
        _ => interleave_generic(input, output, channel_count, frame_count),
    }
}

/// 通用交织（非特化通道数）。
#[inline(never)]
fn interleave_generic(
    input: &[Vec<f32>],
    output: &mut [f32],
    channel_count: usize,
    frame_count: usize,
) {
    for f in 0..frame_count {
        let base = f * channel_count;
        for c in 0..channel_count {
            output[base + c] = input[c][f];
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 便捷辅助
// ══════════════════════════════════════════════════════════════════════════════

/// 创建平面格式缓冲区（全零初始化）。
///
/// 用于 `pipeline.rs` 初始化时预分配 `allSamples` / `allSamples2`。
/// 非实时路径调用，返回 `Vec<Vec<f32>>`。
pub fn allocate_plane_buffers(channel_count: usize, frame_count: usize) -> Vec<Vec<f32>> {
    vec![vec![0.0f32; frame_count]; channel_count]
}

/// 将平面缓冲区清零（实时路径用）。
///
/// 等效于每个通道的 `fill(0.0)`，但对连续内存更友好。
pub fn clear_plane_buffers(buffers: &mut [Vec<f32>], frame_count: usize) {
    for channel in buffers.iter_mut() {
        for i in 0..frame_count {
            channel[i] = 0.0;
        }
    }
}

/// 从平面缓冲区中复制指定通道子集到输出。
///
/// `channel_indices` 指定要提取的通道索引。
/// 输出缓冲区长度必须 >= `channel_indices.len()`。
pub fn extract_channels(
    input: &[Vec<f32>],
    output: &mut [Vec<f32>],
    channel_indices: &[usize],
    frame_count: usize,
) {
    for (out_idx, &src_idx) in channel_indices.iter().enumerate() {
        output[out_idx][..frame_count].copy_from_slice(&input[src_idx][..frame_count]);
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── 去交织 ──────────────────────────────────────────────────────────────

    #[test]
    fn deinterleave_mono() {
        let input = vec![1.0, 2.0, 3.0, 4.0];
        let mut output = allocate_plane_buffers(1, 4);
        deinterleave(&input, &mut output, 1, 4);
        assert_eq!(output[0], vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn deinterleave_stereo() {
        // L0 R0 L1 R1 L2 R2
        let input = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let mut output = allocate_plane_buffers(2, 3);
        deinterleave(&input, &mut output, 2, 3);
        assert_eq!(output[0], vec![1.0, 3.0, 5.0]); // L
        assert_eq!(output[1], vec![2.0, 4.0, 6.0]); // R
    }

    #[test]
    fn deinterleave_51() {
        // 6 通道，3 帧 = 18 采样
        let input: Vec<f32> = (0..18).map(|i| i as f32).collect();
        let mut output = allocate_plane_buffers(6, 3);
        deinterleave(&input, &mut output, 6, 3);

        assert_eq!(output[0], vec![0.0, 6.0, 12.0]);   // L
        assert_eq!(output[1], vec![1.0, 7.0, 13.0]);   // R
        assert_eq!(output[2], vec![2.0, 8.0, 14.0]);   // C
        assert_eq!(output[3], vec![3.0, 9.0, 15.0]);   // LFE
        assert_eq!(output[4], vec![4.0, 10.0, 16.0]);  // RL
        assert_eq!(output[5], vec![5.0, 11.0, 17.0]);  // RR
    }

    #[test]
    fn deinterleave_71() {
        // 8 通道，2 帧 = 16 采样
        let input: Vec<f32> = (0..16).map(|i| i as f32).collect();
        let mut output = allocate_plane_buffers(8, 2);
        deinterleave(&input, &mut output, 8, 2);

        assert_eq!(output[0], vec![0.0, 8.0]);
        assert_eq!(output[1], vec![1.0, 9.0]);
        assert_eq!(output[7], vec![7.0, 15.0]);
    }

    #[test]
    fn deinterleave_generic_3ch() {
        // 3 通道，2 帧 = 6 采样
        let input = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let mut output = allocate_plane_buffers(3, 2);
        deinterleave(&input, &mut output, 3, 2);
        assert_eq!(output[0], vec![1.0, 4.0]);
        assert_eq!(output[1], vec![2.0, 5.0]);
        assert_eq!(output[2], vec![3.0, 6.0]);
    }

    #[test]
    fn deinterleave_generic_5ch() {
        // 5 通道，2 帧 = 10 采样
        let input: Vec<f32> = (0..10).map(|i| i as f32).collect();
        let mut output = allocate_plane_buffers(5, 2);
        deinterleave(&input, &mut output, 5, 2);
        assert_eq!(output[0], vec![0.0, 5.0]);
        assert_eq!(output[4], vec![4.0, 9.0]);
    }

    // ── 交织 ────────────────────────────────────────────────────────────────

    #[test]
    fn interleave_mono() {
        let input = vec![vec![1.0, 2.0, 3.0]];
        let mut output = vec![0.0; 3];
        interleave(&input, &mut output, 1, 3);
        assert_eq!(output, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn interleave_stereo() {
        let input = vec![
            vec![1.0, 3.0, 5.0], // L
            vec![2.0, 4.0, 6.0], // R
        ];
        let mut output = vec![0.0; 6];
        interleave(&input, &mut output, 2, 3);
        assert_eq!(output, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn interleave_51() {
        let input: Vec<Vec<f32>> = (0..6)
            .map(|ch| vec![ch as f32, ch as f32 + 6.0, ch as f32 + 12.0])
            .collect();
        let mut output = vec![0.0; 18];
        interleave(&input, &mut output, 6, 3);

        let expected: Vec<f32> = (0..18).map(|i| i as f32).collect();
        assert_eq!(output, expected);
    }

    #[test]
    fn interleave_71() {
        let input: Vec<Vec<f32>> = (0..8)
            .map(|ch| vec![ch as f32, ch as f32 + 8.0])
            .collect();
        let mut output = vec![0.0; 16];
        interleave(&input, &mut output, 8, 2);

        let expected: Vec<f32> = (0..16).map(|i| i as f32).collect();
        assert_eq!(output, expected);
    }

    // ── 往返测试 ────────────────────────────────────────────────────────────

    #[test]
    fn roundtrip_stereo() {
        let original = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let mut planes = allocate_plane_buffers(2, 3);
        deinterleave(&original, &mut planes, 2, 3);

        let mut result = vec![0.0; 6];
        interleave(&planes, &mut result, 2, 3);
        assert_eq!(original, result);
    }

    #[test]
    fn roundtrip_51() {
        let original: Vec<f32> = (0..600).map(|i| (i as f32) * 0.001).collect();
        let mut planes = allocate_plane_buffers(6, 100);
        deinterleave(&original, &mut planes, 6, 100);

        let mut result = vec![0.0; 600];
        interleave(&planes, &mut result, 6, 100);
        assert_eq!(original, result);
    }

    #[test]
    fn roundtrip_71() {
        let original: Vec<f32> = (0..800).map(|i| (i as f32) * 0.001).collect();
        let mut planes = allocate_plane_buffers(8, 100);
        deinterleave(&original, &mut planes, 8, 100);

        let mut result = vec![0.0; 800];
        interleave(&planes, &mut result, 8, 100);
        assert_eq!(original, result);
    }

    #[test]
    fn roundtrip_3ch_generic() {
        let original = vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6];
        let mut planes = allocate_plane_buffers(3, 2);
        deinterleave(&original, &mut planes, 3, 2);

        let mut result = vec![0.0; 6];
        interleave(&planes, &mut result, 3, 2);
        assert_eq!(original, result);
    }

    // ── allocate_plane_buffers ──────────────────────────────────────────────

    #[test]
    fn allocate_dimensions() {
        let buf = allocate_plane_buffers(8, 480);
        assert_eq!(buf.len(), 8);
        for ch in &buf {
            assert_eq!(ch.len(), 480);
            assert!(ch.iter().all(|&v| v == 0.0));
        }
    }

    // ── clear_plane_buffers ─────────────────────────────────────────────────

    #[test]
    fn clear_resets_to_zero() {
        let mut buf = vec![vec![1.0; 128], vec![2.0; 128], vec![3.0; 128]];
        clear_plane_buffers(&mut buf, 128);
        for ch in &buf {
            assert!(ch.iter().all(|&v| v == 0.0));
        }
    }

    #[test]
    fn clear_partial() {
        let mut buf = vec![vec![99.0; 128]];
        clear_plane_buffers(&mut buf, 64);
        // 前 64 个清零，后 64 个仍为 99.0
        assert!(buf[0][..64].iter().all(|&v| v == 0.0));
        assert!(buf[0][64..].iter().all(|&v| v == 99.0));
    }

    // ── extract_channels ────────────────────────────────────────────────────

    #[test]
    fn extract_two_from_four() {
        let input = vec![
            vec![1.0, 2.0], // ch0
            vec![3.0, 4.0], // ch1
            vec![5.0, 6.0], // ch2
            vec![7.0, 8.0], // ch3
        ];
        let mut output = allocate_plane_buffers(2, 2);
        extract_channels(&input, &mut output, &[0, 2], 2);
        assert_eq!(output[0], vec![1.0, 2.0]);
        assert_eq!(output[1], vec![5.0, 6.0]);
    }

    #[test]
    fn extract_reorder() {
        let input = vec![
            vec![1.0, 2.0], // L
            vec![3.0, 4.0], // R
            vec![5.0, 6.0], // C
        ];
        let mut output = allocate_plane_buffers(3, 2);
        // 重排为 C, L, R
        extract_channels(&input, &mut output, &[2, 0, 1], 2);
        assert_eq!(output[0], vec![5.0, 6.0]); // C
        assert_eq!(output[1], vec![1.0, 2.0]); // L
        assert_eq!(output[2], vec![3.0, 4.0]); // R
    }

    #[test]
    fn extract_same_as_input() {
        let input = vec![vec![1.0, 2.0], vec![3.0, 4.0]];
        let mut output = allocate_plane_buffers(2, 2);
        extract_channels(&input, &mut output, &[0, 1], 2);
        assert_eq!(output, input);
    }
}