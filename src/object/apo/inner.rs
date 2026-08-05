//! object/apo/inner.rs — ApoObjectInner（双链过渡状态）
//!
//! 持有当前链、过渡链、PipelineContext、临时缓冲区与配置指纹。
//! 纯数据结构 + 构造逻辑，不包含 COM 接口实现。

use crate::pipeline::chain::Chain;
use crate::pipeline::context::PipelineContext;
use crate::pipeline::dsp::filter::{DspContext, DeviceType, ProcessingStage};
use crate::pipeline::dsp::transition::SmoothingProvider;
use crate::sys::audio_defs::get_channel_names;

/// 内部可变状态（由 `ApoObject.mutex` 保护）。
pub struct ApoObjectInner {
    pub current_chain: Box<Chain>,
    pub outgoing_chain: Option<Box<Chain>>,
    /// 退役链（R1/v6.9）：过渡完成后由 RT 线程移入，控制线程锁内统一析构。
    pub retired_chain: Option<Box<Chain>>,
    pub pipeline_context: PipelineContext,
    pub transition: Option<SmoothingProvider>,
    pub temp_buffers: Vec<Vec<f32>>,
    pub temp_buffer_old: Vec<f32>,
    pub temp_buffer_new: Vec<f32>,
    pub pending_reload: bool,
    /// 阻塞式重载标志（R2/v6.9）：同一过渡周期内至多触发一次重载。
    pub reloading: bool,
    /// 生效配置指纹（v7.9，P0-4 配置变更检测）——当前生效链的 filter_spec 有序序列。
    pub active_spec: Vec<String>,
}

impl ApoObjectInner {
    pub fn new() -> Self {
        Self {
            current_chain: Box::new(Chain::new()),
            outgoing_chain: None,
            retired_chain: None,
            pipeline_context: PipelineContext::new(),
            transition: None,
            temp_buffers: Vec::new(),
            temp_buffer_old: Vec::new(),
            temp_buffer_new: Vec::new(),
            pending_reload: false,
            reloading: false,
            active_spec: Vec::new(),
        }
    }
}

/// 从 PipelineContext 构建 DspContext（共享逻辑，LockForProcess / hot_reload 用）。
pub(crate) fn build_dsp_context(ctx: &PipelineContext, bits_per_sample: u32) -> DspContext {
    let channel_names = get_channel_names(ctx.channel_mask);
    DspContext {
        sample_rate: ctx.sample_rate,
        channel_count: ctx.input_channels,
        channel_mask: ctx.channel_mask,
        channel_names,
        max_frame_count: ctx.max_frame_count as u32,
        bits_per_sample,
        device_type: DeviceType::Render,
        stage: ProcessingStage::None,
        variables: std::collections::HashMap::new(),
        rt_marker: std::marker::PhantomData,
    }
}
