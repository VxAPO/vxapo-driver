//! pipeline/interleave.rs — 通道数据搬运（v6.3 规范 4.4）

/// 交织格式 → 去交织平面缓冲区（分配新 Vec<Vec<f32>>）。非实时路径用。
pub fn deinterleave(input: &[f32], channels: usize, frames: usize) -> Vec<Vec<f32>> {
    let mut output = vec![vec![0.0f32; frames]; channels];
    deinterleave_into(input, &mut output, channels, frames);
    output
}

/// 去交织平面缓冲区 → 交织格式（分配新 Vec<f32>）。非实时路径用。
pub fn interleave(channels: &[Vec<f32>], frames: usize) -> Vec<f32> {
    let ch_count = channels.len();
    let mut output = vec![0.0f32; ch_count * frames];
    interleave_from(channels, &mut output, ch_count, frames);
    output
}

/// 交织格式 → 已分配的去交织缓冲区（零分配，写入 output）。
pub fn deinterleave_into(input: &[f32], output: &mut [Vec<f32>], channels: usize, frames: usize) {
    for f in 0..frames {
        for ch in 0..channels {
            output[ch][f] = input[f * channels + ch];
        }
    }
}

/// 已分配的去交织缓冲区 → 交织格式（零分配，写入 output）。
pub fn interleave_from(input: &[Vec<f32>], output: &mut [f32], channels: usize, frames: usize) {
    for f in 0..frames {
        for ch in 0..channels {
            output[f * channels + ch] = input[ch][f];
        }
    }
}

/// 去交织平面缓冲区 → 交织格式（零分配），非有限值写 0（链级防线）。
///
/// 在 `is_finite()` 编译为一次整数比较（x86_64），正常路径几乎不命中；
/// 命中即表示上游 bug 或极端配置——静音优于啸叫。
pub fn interleave_from_guarded(
    input: &[Vec<f32>],
    output: &mut [f32],
    channels: usize,
    frames: usize,
) {
    for f in 0..frames {
        for ch in 0..channels {
            let v = input[ch][f];
            output[f * channels + ch] = if v.is_finite() { v } else { 0.0 };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deinterleave_stereo() {
        let input = vec![1.0, 2.0, 3.0, 4.0];
        let out = deinterleave(&input, 2, 2);
        assert_eq!(out[0], vec![1.0, 3.0]);
        assert_eq!(out[1], vec![2.0, 4.0]);
    }

    #[test]
    fn interleave_stereo_roundtrip() {
        let ch = vec![vec![1.0, 3.0], vec![2.0, 4.0]];
        let out = interleave(&ch, 2);
        assert_eq!(out, vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn zero_alloc_into_roundtrip() {
        let input = (0..16).map(|i| i as f32).collect::<Vec<_>>();
        let mut planar = vec![vec![0.0; 8]; 2];
        deinterleave_into(&input, &mut planar, 2, 8);
        let mut back = vec![0.0; 16];
        interleave_from(&planar, &mut back, 2, 8);
        assert_eq!(input, back);
    }

    #[test]
    fn mono_roundtrip() {
        let input = vec![1.0, 2.0, 3.0];
        let mut planar = vec![vec![0.0; 3]; 1];
        deinterleave_into(&input, &mut planar, 1, 3);
        let mut back = vec![0.0; 3];
        interleave_from(&planar, &mut back, 1, 3);
        assert_eq!(input, back);
    }

    #[test]
    fn guarded_interleave_replaces_non_finite() {
        let planar = vec![vec![1.0f32, f32::NAN, f32::INFINITY], vec![2.0, -2.0, 0.5]];
        let mut out = vec![0.0f32; 6];
        interleave_from_guarded(&planar, &mut out, 2, 3);
        assert_eq!(out, vec![1.0, 2.0, 0.0, -2.0, 0.0, 0.5]);
    }
}
