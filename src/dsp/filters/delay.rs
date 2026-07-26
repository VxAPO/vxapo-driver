//! dsp/filters/delay.rs — 延迟线滤波器
//!
//! 实现 `Delay:` 命令。
//!
//! 使用环形缓冲区实现固定延迟。
//! 支持亚采样精度延迟（线性插值）。
//!
//! `process` 方法遵守 RT-safety 约束（Note 12）。
//! 缓冲区在 `initialize` 时预分配。

use crate::dsp::filter::Filter;

/// 最大延迟（采样数，约 1 秒 @ 48kHz）。
const MAX_DELAY_SAMPLES: usize = 65536;

/// 延迟线滤波器。
#[derive(Debug)]
pub struct DelayFilter {
    /// 延迟时间（毫秒）。
    delay_ms: f32,
    /// 延迟采样数（浮点，用于亚采样插值）。
    delay_samples_f: f32,
    /// 延迟采样数（整数部分）。
    delay_samples: usize,
    /// 亚采样插值因子。
    frac: f32,
    /// 环形缓冲区：`buffer[channel][sample]`。
    buffer: Vec<Vec<f32>>,
    /// 写入位置。
    write_pos: usize,
    /// 缓冲区大小（2 的幂，优化取模）。
    buf_size: usize,
    /// buf_size 掩码（`buf_size - 1`）。
    buf_mask: usize,
    /// 采样率（用于 ms ↔ samples 转换）。
    sample_rate: u32,
}

impl DelayFilter {
    /// 创建延迟线滤波器。
    ///
    /// - `delay_ms`：延迟时间（毫秒，可为负表示提前）
    pub fn new(delay_ms: f32) -> Self {
        Self {
            delay_ms,
            delay_samples_f: 0.0,
            delay_samples: 0,
            frac: 0.0,
            buffer: Vec::new(),
            write_pos: 0,
            buf_size: 0,
            buf_mask: 0,
            sample_rate: 0,
        }
    }

    /// 获取延迟时间（毫秒）。
    pub fn delay_ms(&self) -> f32 {
        self.delay_ms
    }
}

impl Filter for DelayFilter {
    fn initialize(&mut self, sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        self.sample_rate = sample_rate;
        let num_channels = channel_names.len().max(1);

        // 计算延迟采样数
        let delay_abs = self.delay_ms.abs();
        self.delay_samples_f = delay_abs * sample_rate as f32 / 1000.0;
        self.delay_samples = self.delay_samples_f as usize;
        self.frac = self.delay_samples_f - self.delay_samples as f32;

        // 环形缓冲区大小（向上取整到 2 的幂）
        let needed = self.delay_samples + 2; // +2 for interpolation lookahead
        self.buf_size = next_power_of_two(needed.max(256).min(MAX_DELAY_SAMPLES));
        self.buf_mask = self.buf_size - 1;

        // 预分配缓冲区
        self.buffer = vec![vec![0.0f32; self.buf_size]; num_channels];
        self.write_pos = 0;

        None
    }

    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        let num_ch = self.buffer.len().min(samples.len());
        let delay = self.delay_samples;
        let frac = self.frac;
        let mask = self.buf_mask;
        let buf_size = self.buf_size;

        for f in 0..frame_count {
            for ch in 0..num_ch {
                // 写入当前采样到环形缓冲区
                self.buffer[ch][self.write_pos] = samples[ch][f];

                // 读取延迟后的采样
                let read_pos = (self.write_pos + buf_size - delay) & mask;
                let read_pos_next = (read_pos + 1) & mask;

                // 线性插值（亚采样精度）
                let s0 = self.buffer[ch][read_pos];
                let s1 = self.buffer[ch][read_pos_next];
                samples[ch][f] = s0 + frac * (s1 - s0);
            }

            self.write_pos = (self.write_pos + 1) & mask;
        }
    }

    fn latency(&self) -> u32 {
        self.delay_samples as u32
    }
}

/// 向上取整到 2 的幂。
fn next_power_of_two(n: usize) -> usize {
    if n == 0 {
        return 1;
    }
    let mut p = 1usize;
    while p < n {
        p = p.saturating_mul(2);
    }
    p
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

    // ── next_power_of_two ───────────────────────────────────────────────────

    #[test]
    fn pow2_basic() {
        assert_eq!(next_power_of_two(0), 1);
        assert_eq!(next_power_of_two(1), 1);
        assert_eq!(next_power_of_two(2), 2);
        assert_eq!(next_power_of_two(3), 4);
        assert_eq!(next_power_of_two(255), 256);
        assert_eq!(next_power_of_two(256), 256);
    }

    // ── DelayFilter 基础 ────────────────────────────────────────────────────

    #[test]
    fn zero_delay_passthrough() {
        let mut filter = DelayFilter::new(0.0);
        filter.initialize(48000, &stereo_names());

        let mut samples = vec![vec![1.0, 2.0, 3.0, 4.0], vec![0.5, 1.0, 1.5, 2.0]];
        filter.process(&mut samples, 4);

        // 0ms 延迟：输出应接近输入（可能有极小的插值误差）
        for f in 0..4 {
            assert!((samples[0][f] - (f as f32 + 1.0)).abs() < 0.01);
        }
    }

    #[test]
    fn delay_shifts_signal() {
        let mut filter = DelayFilter::new(1.0); // 1ms @ 48kHz = 48 samples
        filter.initialize(48000, &stereo_names());

        // 脉冲在 t=0
        let mut samples = vec![vec![0.0f32; 100]; 2];
        samples[0][0] = 1.0;

        filter.process(&mut samples, 100);

        // 输出：前 48 个采样应为 0（延迟期间），t=48 应有脉冲
        for f in 0..47 {
            assert!(samples[0][f].abs() < 0.01, "frame {} should be silent", f);
        }
        assert!(
            samples[0][48].abs() > 0.5,
            "pulse should appear at frame 48"
        );
    }

    #[test]
    fn delay_preserves_signal() {
        let mut filter = DelayFilter::new(2.0); // 2ms = 96 samples @ 48kHz
        filter.initialize(48000, &stereo_names());

        let len = 200;
        let mut samples = vec![vec![0.0f32; len]; 2];

        // 正弦波输入
        for f in 0..len {
            samples[0][f] = (f as f32 * 0.1).sin();
        }

        let input = samples[0].clone();
        filter.process(&mut samples, len);

        // 延迟 96 采样后，信号应被平移
        for f in 96..len {
            assert!(
                (samples[0][f] - input[f - 96]).abs() < 0.05,
                "signal mismatch at frame {}: got {}, expected {}",
                f,
                samples[0][f],
                input[f - 96]
            );
        }
    }

    #[test]
    fn delay_latency() {
        let mut filter = DelayFilter::new(5.0); // 5ms = 240 samples @ 48kHz
        filter.initialize(48000, &stereo_names());
        assert_eq!(filter.latency(), 240);
    }

    #[test]
    fn delay_ms_accessor() {
        let filter = DelayFilter::new(12.5);
        assert_eq!(filter.delay_ms(), 12.5);
    }

    #[test]
    fn multi_channel_delay() {
        let names = vec!["L".into(), "R".into(), "C".into()];
        let mut filter = DelayFilter::new(1.0);
        filter.initialize(48000, &names);

        let mut samples = vec![vec![0.0f32; 100]; 3];
        samples[0][0] = 1.0;
        samples[1][0] = 0.5;
        samples[2][0] = 0.25;

        filter.process(&mut samples, 100);

        // 三个通道都应被延迟
        for ch in 0..3 {
            for f in 0..47 {
                assert!(samples[ch][f].abs() < 0.01);
            }
            assert!(samples[ch][48].abs() > 0.1);
        }
    }
}