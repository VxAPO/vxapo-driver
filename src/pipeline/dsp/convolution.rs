//! dsp/filters/convolution.rs — 卷积滤波器（算法选型待定，当前 trait 骨架）
//!
//! 实现 `Convolution:` 命令。
//!
//! 当前为占位实现：
//! - 解析参数（IR 文件路径、增益、通道映射）
//! - `process` 直通（passthrough）
//!
//! Phase 8+ 实现选项：
//! 1. 时域卷积（短 IR，< 128 采样）
//! 2. 重叠保留法（Overlap-Save，中等 IR）
//! 3. 分段卷积（Partitioned Convolution，长 IR，Note 13c）
//!
//! 选择取决于 IR 长度和实时性约束（Note 12/53）。

use crate::pipeline::dsp::filter::Filter;

/// 卷积滤波器（Phase 8 占位）。
#[derive(Debug)]
pub struct ConvolutionFilter {
    /// IR 文件路径（用于日志/错误报告）。
    ir_path: String,
    /// 增益（dB）。
    gain_db: f32,
    /// 通道数。
    num_channels: usize,
}

impl ConvolutionFilter {
    /// 创建卷积滤波器。
    ///
    /// - `ir_path`：IR 文件路径
    /// - `gain_db`：增益（dB）
    pub fn new(ir_path: &str, gain_db: f32) -> Self {
        Self {
            ir_path: ir_path.to_owned(),
            gain_db,
            num_channels: 0,
        }
    }
}

impl Filter for ConvolutionFilter {
    fn initialize(&mut self, _sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        self.num_channels = channel_names.len().max(1);

        // TODO: 加载 IR 文件，预分配 FFT 缓冲区
        log::warn!(
            "Convolution: not yet implemented (ir_path='{}', gain={}dB). Passing through.",
            self.ir_path, self.gain_db
        );

        None
    }

    fn process(&mut self, _samples: &mut [Vec<f32>], _frame_count: usize) {
        // Phase 8+：实现卷积算法
        // 当前 passthrough
    }
}

/// 解析 `Convolution:` 参数。
///
/// 格式：`path [gain_dB]`
///
/// 返回 `(ir_path, gain_db)`。
pub fn parse_convolution_params(params: &str) -> Option<(String, f32)> {
    let parts: Vec<&str> = params.split_whitespace().collect();
    if parts.is_empty() {
        return None;
    }

    let path = parts[0].to_owned();
    let gain = if parts.len() >= 2 {
        parts[1].parse::<f32>().unwrap_or(0.0)
    } else {
        0.0
    };

    Some((path, gain))
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
        assert!(parse_convolution_params("").is_none());
    }

    // ── ConvolutionFilter（占位 passthrough） ────────────────────────────────

    #[test]
    fn passthrough_preserves_signal() {
        let mut filter = ConvolutionFilter::new("ir.wav", 0.0);
        filter.initialize(48000, &stereo_names());

        let mut samples = vec![vec![1.0, 2.0, 3.0], vec![0.5, 1.0, 1.5]];
        let input = samples.clone();
        filter.process(&mut samples, 3);

        // 占位 passthrough
        assert_eq!(samples, input);
    }

    #[test]
    fn latency_zero() {
        let filter = ConvolutionFilter::new("ir.wav", 0.0);
        assert_eq!(filter.latency(), 0);
    }
}