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
    /// 上次成功 Lock 的 (spec, 采样率, 通道) 键（v9.12 修订：同键 Relock 直接
    /// 复用现有链、保留滤波器状态，避免端点重协商时的重建瞬态/哔声）。
    pub last_lock_key: Option<(Vec<String>, u32, Vec<String>)>,
    /// 启动淡入总长度（采样，v9.6）：流建立初期引擎可能仍在加载目标 APO 链，
    /// 直接播会产生“首秒断续慢速”。先静音保持再线性淡入，听感为“加载完再播”。
    pub startup_fade_total: usize,
    /// 启动淡入剩余采样数。
    pub startup_fade_remaining: usize,
    /// 最近几次 APOProcess 调用记录（v9.6 诊断，RT 固定数组零分配）：
    /// `(秒, 输入帧数, 输入 flags, 输入峰值, 输出 flags, 输出峰值)`——Unlock 时随日志输出，
    /// 用于区分“浏览器/引擎注入超大输入”与“DSP 自身数值爆炸”（v9.15 诊断增强）。
    pub last_calls: [(u64, u32, u32, f32, u32, f32); 8],
    /// `last_calls` 环形写索引（自增，取模即可）。
    pub last_call_idx: u64,
    /// 锁定周期内输出峰值高水位（RT 写、Unlock 读，v9.15 诊断）。
    pub hot_out_peak: f32,
    /// 高水位对应帧的输入峰值。
    pub hot_in_peak: f32,
    /// 高水位对应帧的时间戳（秒）。
    pub hot_secs: u64,
    /// 输入标志为 BUFFER_SILENT 但内容非零（引擎脏静音缓冲）的调用次数（v9.15 诊断）。
    pub silent_dirty_calls: u32,
    /// 脏静音缓冲中输入峰值的最大值。
    pub silent_dirty_max_in: f32,
}

/// 启动静音保持时长（ms，v9.6 用户决策：直接静音 100ms，不做淡入）。
pub(crate) const STARTUP_FADE_HOLD_MS: u32 = 100;
/// 启动淡入时长（ms）：0 = 无淡入，静音结束后直接恢复正常音量。
pub(crate) const STARTUP_FADE_RAMP_MS: u32 = 0;

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
            last_lock_key: None,
            startup_fade_total: 0,
            startup_fade_remaining: 0,
            last_calls: [(0, 0, 0, 0.0, 0, 0.0); 8],
            last_call_idx: 0,
            hot_out_peak: 0.0,
            hot_in_peak: 0.0,
            hot_secs: 0,
            silent_dirty_calls: 0,
            silent_dirty_max_in: 0.0,
        }
    }

    /// 对输出交织缓冲应用启动静音保持（默认 500ms，无淡入，RT 零分配，v9.6）。
    pub(crate) fn apply_startup_fade(
        &mut self,
        out: &mut [f32],
        frames: usize,
        out_ch: usize,
    ) {
        let total = self.startup_fade_total;
        if total == 0 || out_ch == 0 {
            self.startup_fade_remaining = 0;
            return;
        }
        let hold = total * STARTUP_FADE_HOLD_MS as usize
            / (STARTUP_FADE_HOLD_MS + STARTUP_FADE_RAMP_MS) as usize;
        let ramp = (total - hold).max(1);
        let mut rem = self.startup_fade_remaining;
        let usable = frames.min(out.len() / out_ch);
        for f in 0..usable {
            if rem == 0 {
                break;
            }
            let elapsed = total - rem;
            let factor = if elapsed < hold {
                0.0
            } else {
                ((elapsed - hold) as f32 / ramp as f32).min(1.0)
            };
            for c in 0..out_ch {
                out[f * out_ch + c] *= factor;
            }
            rem -= 1;
        }
        self.startup_fade_remaining = rem;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_silence_holds_then_recovers() {
        let mut inner = ApoObjectInner::new();
        // 500ms 保持 + 0 淡入。
        let total = (STARTUP_FADE_HOLD_MS + STARTUP_FADE_RAMP_MS) as usize;
        inner.startup_fade_total = total;
        inner.startup_fade_remaining = total;
        let mut out = vec![1.0f32; total * 2];
        inner.apply_startup_fade(&mut out, total, 2);

        // 保持段全静音；结束后计数器归零。
        assert_eq!(out[0], 0.0);
        assert_eq!(out[(total - 1) * 2], 0.0);
        assert_eq!(inner.startup_fade_remaining, 0);
        // 静音结束后（计数器归零）后续处理不再改动输出。
        let mut out2 = vec![1.0f32; 4];
        inner.apply_startup_fade(&mut out2, 2, 2);
        assert_eq!(out2, vec![1.0f32; 4]);
    }
}

/// 从 PipelineContext 构建 DspContext（共享逻辑，LockForProcess / hot_reload 用）。
pub(crate) fn build_dsp_context(ctx: &PipelineContext) -> DspContext {
    let channel_names = get_channel_names(ctx.channel_mask);
    DspContext {
        sample_rate: ctx.sample_rate,
        channel_count: ctx.input_channels,
        channel_mask: ctx.channel_mask,
        channel_names,
        max_frame_count: ctx.max_frame_count as u32,
        bits_per_sample: ctx.bits_per_sample,
        device_type: DeviceType::Render,
        stage: ProcessingStage::None,
        variables: std::collections::HashMap::new(),
        loudness_enabled: std::cell::Cell::new(true),
        rt_marker: std::marker::PhantomData,
    }
}
