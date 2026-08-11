//! pipeline/dsp/filter.rs — Filter trait + FilterCreateResult + FilterFactory + DspContext + ConfigLoader（v6.3 规范 4.9）
//!
//! 职责：纯 Rust 定义，不包含任何 Windows API 依赖。
//!
//! 导出给：`pipeline/dsp/*.rs`、`pipeline/dsp/factory.rs`、`config/commands/*.rs`。

use std::collections::HashMap;
use std::marker::PhantomData;

use crate::pipeline::realtime::contract::RealtimeContext;

// ══════════════════════════════════════════════════════════════════════════════
// Filter trait
// ══════════════════════════════════════════════════════════════════════════════

/// 音频处理过滤器 trait（v6.3 规范）。
///
/// 所有 DSP 滤波器实现此 trait。
/// `process` 方法运行在实时音频线程中，禁止堆分配、互斥锁、I/O、panic。
pub trait Filter: Send + Sync + std::fmt::Debug {
    /// 处理一帧音频数据（实时路径，去交织空间）。
    ///
    /// `samples[channel][frame]`，就地修改。
    /// 不得进行堆分配、I/O、panic。
    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize);

    /// 初始化过滤器。
    ///
    /// 返回输出通道名列表（`Some` = 通道选择，`None` = 通道不变）。
    /// 不执行 I/O，不应失败。
    fn initialize(&mut self, sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>>;

    /// 是否为 `Channel:` 类型命令（通道选择标记）。默认 false。
    fn is_channel_select(&self) -> bool {
        false
    }

    /// 设置本滤波器作用的平面通道槽位（`Channel:` 选择后由 Chain 在 `initialize` 前调用）。
    ///
    /// `indices` 是选中通道在**原始平面缓冲**中的槽位号（如 `Channel: R` → `[1]`）。
    /// 按“前 N 个槽位”处理的滤波器（biquad/gain/delay/convolution 等）必须存储该列表，
    /// 并在 `process` 中只处理对应槽位；无通道语义的滤波器（Copy 等）保持默认忽略。
    fn set_channel_indices(&mut self, _indices: &[usize]) {}

    /// 配置模型 per-effect `channels` 指定的固定通道槽位（v9.11）。
    ///
    /// `Some` 时 Chain 跳过自动计算、直接使用固定槽位（仅 `ChannelScopedFilter`
    /// 返回）；`None` = 由 Chain 按当前通道名自动设置。
    fn fixed_channel_indices(&self) -> Option<Vec<usize>> {
        None
    }

    /// 是否就地处理（in-place，E1/v6.7）。
    ///
    /// 默认 true：滤波器直接修改传入的 `samples` 缓冲，无需额外中间副本。
    /// 若某滤波器 `false`，Chain 需在调用其 `process` 前保存输入副本（E2 未来落点；
    /// 当前内置滤波器均返回 true）。
    fn is_in_place(&self) -> bool {
        true
    }

    /// 过滤器的延迟（采样数）。默认 0。
    fn latency(&self) -> u32 {
        0
    }

    /// 最大帧数约束。默认 None（无约束）。
    /// Some(n) 表示 process 每次最多处理 n 帧。
    fn max_frame_count(&self) -> Option<usize> {
        None
    }

    /// 重置过滤器内部状态。默认空实现。
    fn reset(&mut self) {}
}

// ══════════════════════════════════════════════════════════════════════════════
// PassthroughFilter
// ══════════════════════════════════════════════════════════════════════════════

/// 透传过滤器——不修改任何采样数据。
#[derive(Debug, Clone, Copy)]
pub struct PassthroughFilter;

impl Filter for PassthroughFilter {
    fn process(&mut self, _samples: &mut [Vec<f32>], _frame_count: usize) {}

    fn initialize(&mut self, _sample_rate: u32, _channel_names: &[String]) -> Option<Vec<String>> {
        None
    }
}

/// 按配置模型 `channels` 固定作用通道的包装（v9.11）。
///
/// config 层解析出平面槽位后包一层；`Chain::initialize` 通过
/// `fixed_channel_indices` 识别并跳过自动通道计算。
#[derive(Debug)]
pub struct ChannelScopedFilter {
    inner: Box<dyn Filter>,
    indices: Vec<usize>,
}

impl ChannelScopedFilter {
    pub fn new(inner: Box<dyn Filter>, indices: Vec<usize>) -> Self {
        Self { inner, indices }
    }
}

impl Filter for ChannelScopedFilter {
    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        self.inner.process(samples, frame_count);
    }

    fn initialize(&mut self, sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        self.inner.initialize(sample_rate, channel_names)
    }

    fn latency(&self) -> u32 {
        self.inner.latency()
    }

    fn set_channel_indices(&mut self, indices: &[usize]) {
        self.indices = indices.to_vec();
        self.inner.set_channel_indices(indices);
    }

    fn fixed_channel_indices(&self) -> Option<Vec<usize>> {
        Some(self.indices.clone())
    }

    fn is_in_place(&self) -> bool {
        self.inner.is_in_place()
    }

    fn max_frame_count(&self) -> Option<usize> {
        self.inner.max_frame_count()
    }

    fn reset(&mut self) {
        self.inner.reset();
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// DspContext
// ══════════════════════════════════════════════════════════════════════════════

/// 引擎统一上下文。纯数据结构，不包含任何 Windows 类型。
///
/// 由调用方从 `PipelineContext` + 额外参数构造（调用方依赖 `pipeline/context.rs`）。
/// `filter.rs` 不依赖 `pipeline/context.rs`，保持模块独立性。
#[derive(Debug, Clone)]
pub struct DspContext {
    /// 采样率（Hz）。
    pub sample_rate: u32,
    /// 通道数。
    pub channel_count: u32,
    /// 通道掩码（`dwChannelMask` 格式）。
    pub channel_mask: u32,
    /// 通道名列表（`Copy:` 命令依赖）。
    pub channel_names: Vec<String>,
    /// 最大帧数（`Convolution` 预分配依赖）。
    pub max_frame_count: u32,
    /// 每样本位数。
    pub bits_per_sample: u32,
    /// 设备类型。
    pub device_type: DeviceType,
    /// 处理阶段。
    pub stage: ProcessingStage,
    /// 变量存储（`Eval:` 命令变量）。
    pub variables: HashMap<String, f64>,
    /// 响度补偿开关（默认开；`Loudness: off` 由 config 层关闭，APP 未来接口）。
    pub loudness_enabled: std::cell::Cell<bool>,
    /// RT 编译期见证（O1/v6.6）：标记此配置服务于实时路径。
    pub rt_marker: PhantomData<RealtimeContext>,
}

/// 设备类型枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceType {
    Render,
    Capture,
}

/// 处理阶段枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessingStage {
    None,
    PreMix,
    PostMix,
    Capture,
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> DspContext {
        DspContext {
            sample_rate: 48000,
            channel_count: 2,
            channel_mask: 0x3,
            channel_names: vec!["L".into(), "R".into()],
            max_frame_count: 480,
            bits_per_sample: 32,
            device_type: DeviceType::Render,
            stage: ProcessingStage::None,
            variables: HashMap::new(),
            loudness_enabled: std::cell::Cell::new(true),
            rt_marker: std::marker::PhantomData,
        }
    }

    #[test]
    fn passthrough_no_change() {
        let mut filter = PassthroughFilter;
        let result = filter.initialize(48000, &["L".into(), "R".into()]);
        assert!(result.is_none());
    }

    #[test]
    fn passthrough_process_noop() {
        let mut filter = PassthroughFilter;
        let mut samples = vec![vec![1.0, 2.0, 3.0], vec![4.0, 5.0, 6.0]];
        let original = samples.clone();
        filter.process(&mut samples, 3);
        assert_eq!(samples, original);
    }

    #[test]
    fn passthrough_defaults() {
        let filter = PassthroughFilter;
        assert!(!filter.is_channel_select());
        assert_eq!(filter.latency(), 0);
        assert_eq!(filter.max_frame_count(), None);
    }

    #[test]
    fn dsp_context_has_variables() {
        let mut ctx = test_ctx();
        ctx.variables.insert("gain".into(), -3.0);
        assert_eq!(ctx.variables.get("gain"), Some(&-3.0));
        let ctx2 = ctx.clone();
        assert_eq!(ctx2.variables.get("gain"), Some(&-3.0));
    }

    #[test]
    fn device_type_equality() {
        assert_eq!(DeviceType::Render, DeviceType::Render);
        assert_ne!(DeviceType::Render, DeviceType::Capture);
    }

    #[test]
    fn filter_is_object_safe_v62() {
        let _boxed: Box<dyn Filter> = Box::new(PassthroughFilter);
    }
}
