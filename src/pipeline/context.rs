//! pipeline/context.rs — PipelineContext（规范 4.1）
//!
//! 职责：运行时上下文。存放 LockForProcess 时确定的静态格式信息，APOProcess 期间只读。

/// 管道运行时上下文（静态格式信息）。
#[derive(Debug, Clone)]
pub struct PipelineContext {
    pub sample_rate: u32,
    pub input_channels: u32,
    pub output_channels: u32,
    pub channel_mask: u32,
    pub max_frame_count: usize,
    /// 锁定时的实际位深（16/24/32），热重载必须与 LockForProcess 同口径。
    pub bits_per_sample: u32,
}

impl PipelineContext {
    /// 创建空上下文（默认值）。
    pub fn new() -> Self {
        Self {
            sample_rate: 0,
            input_channels: 0,
            output_channels: 0,
            channel_mask: 0,
            max_frame_count: 0,
            bits_per_sample: 0,
        }
    }

    /// 每帧字节数（32-bit float × 通道数）。
#[cfg(test)]
    pub fn bytes_per_frame(&self) -> usize {
        self.input_channels.max(self.output_channels) as usize * 4
    }
}

impl Default for PipelineContext {
    fn default() -> Self {
        Self::new()
    }
}
