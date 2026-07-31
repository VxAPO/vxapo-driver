//! pipeline/dsp/filter.rs — Filter trait + FilterCreateResult + FilterFactory + DspContext + ConfigLoader（v6.2 规范 4.9）
//!
//! 职责：纯 Rust 定义，不包含任何 Windows API 依赖。
//!
//! 导出给：`pipeline/dsp/*.rs`、`pipeline/dsp/factory.rs`、`config/commands/*.rs`。

use std::collections::HashMap;

// ══════════════════════════════════════════════════════════════════════════════
// Filter trait
// ══════════════════════════════════════════════════════════════════════════════

/// 音频处理过滤器 trait（v6.2 规范）。
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

// ══════════════════════════════════════════════════════════════════════════════
// FilterCreateResult
// ══════════════════════════════════════════════════════════════════════════════

/// 工厂创建结果（四种状态）。
#[derive(Debug)]
pub enum FilterCreateResult {
    /// 匹配成功，产生了过滤器实例。
    Filter(Box<dyn Filter>),
    /// 匹配成功，但不需要产生过滤器（如 `Device:` 匹配、`Include:` 已递归加载）。
    NoFilter,
    /// 匹配失败，继续尝试下一工厂。
    NoMatch,
    /// 当前文件应停止解析（如 `Device:` 不匹配）。
    AbortFile,
}

// ══════════════════════════════════════════════════════════════════════════════
// FilterFactory trait
// ══════════════════════════════════════════════════════════════════════════════

/// 过滤器工厂 trait。
///
/// 每个配置命令对应一个工厂实现。
/// 遍历工厂列表，第一个返回 `Filter` 或 `NoFilter` 的工厂胜出。
pub trait FilterFactory: Send {
    /// 尝试根据配置行参数创建过滤器。
    ///
    /// - `params`：配置行冒号后的值部分（已 trim）
    /// - `ctx`：引擎上下文
    /// - `loader`：配置加载器回调（用于 `Include:` 递归）
    ///
    /// 返回 `FilterCreateResult` 四种状态之一。
    fn create_filter(
        &self,
        params: &str,
        ctx: &DspContext,
        loader: &dyn ConfigLoader,
    ) -> FilterCreateResult;

    /// 此工厂匹配的命令关键字（用于日志和调试）。
    fn command_name(&self) -> &str;
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
// ConfigLoader trait
// ══════════════════════════════════════════════════════════════════════════════

/// 配置加载器回调 trait。
///
/// `Include:` 命令通过此 trait 回调 `parser.rs` 递归加载子配置文件。
/// `cmd_include.rs` 不能直接 import `parser.rs`，必须通过此 trait 解耦。
pub trait ConfigLoader {
    /// 加载指定路径的配置文件。返回该文件产生的过滤器列表。
    /// 文件不存在或解析失败时返回空列表（降级为 passthrough）。
    fn load_config(&self, path: &str, ctx: &DspContext) -> Vec<Box<dyn Filter>>;
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