//! pipeline/process.rs — APOProcess 调度 + 桥接函数 + 错误策略（v6.3 规范 4.6）

use std::sync::atomic::{AtomicU32, Ordering};

use crate::pipeline::buffer::{evaluate_buffer, BufferAction, BufferInfo, is_silent};
use crate::pipeline::chain::Chain;
use crate::pipeline::interleave::{deinterleave_into, interleave_from_guarded};
use crate::sys::com::apo_types::{APO_CONNECTION_PROPERTY, BUFFER_SILENT, BUFFER_VALID};
use crate::utils::vx_error::Result;

/// 错误恢复策略。
#[derive(Copy, Clone)]
pub enum ErrorPolicy {
    /// 链处理失败时直通（仅当输入有效时安全）。
    Bypass,
    /// 链处理失败时静音。
    Silence,
}

/// 处理参数。
pub struct ProcessParams {
    pub input_channels: u32,
    pub output_channels: u32,
    pub sample_rate: u32,
    pub max_frame_count: usize,
    pub valid_frame_count: usize,
    pub error_policy: ErrorPolicy,
    pub allow_silent_buffer: bool,
}

/// 处理统计（RT 安全原子计数）。
pub struct ProcessStatistics {
    pub error_count: AtomicU32,
}

impl ProcessStatistics {
    pub fn new() -> Self {
        Self { error_count: AtomicU32::new(0) }
    }
}

impl Default for ProcessStatistics {
    fn default() -> Self {
        Self::new()
    }
}

/// 统一错误恢复：链处理失败时按策略恢复输出。
pub fn apply_error_policy(
    result: Result<()>,
    input_slice: &[f32],
    output_buffer: &mut [f32],
    policy: ErrorPolicy,
    stats: &ProcessStatistics,
) {
    match result {
        Ok(()) => {}
        Err(_) => {
            stats.error_count.fetch_add(1, Ordering::Relaxed);
            match policy {
                ErrorPolicy::Bypass => {
                    let copy_len = input_slice.len().min(output_buffer.len());
                    output_buffer[..copy_len].copy_from_slice(&input_slice[..copy_len]);
                    if output_buffer.len() > copy_len {
                        output_buffer[copy_len..].fill(0.0);
                    }
                }
                ErrorPolicy::Silence => {
                    output_buffer.fill(0.0);
                }
            }
        }
    }
}

/// 对单个 Chain 执行完整的交织→去交织→处理→交织流程。
pub fn process_chain_interleaved(
    chain: &mut Chain,
    input: &[f32],
    output: &mut [f32],
    channels: usize,
    frame_count: usize,
    temp: &mut [Vec<f32>],
) -> Result<()> {
    deinterleave_into(input, temp, channels, frame_count);
    chain.process(temp, frame_count)?;
    interleave_from_guarded(temp, output, channels, frame_count);
    Ok(())
}

/// APOProcess 正常模式的完整处理流程（v6.3 规范 4.6）。
pub fn process_audio(
    input_props: &[APO_CONNECTION_PROPERTY],
    output_props: &mut [APO_CONNECTION_PROPERTY],
    params: &ProcessParams,
    chain: &mut Chain,
    stats: &ProcessStatistics,
    temp_buffers: &mut [Vec<f32>],
) -> Result<()> {
    let in_ch = params.input_channels as usize;
    let out_ch = params.output_channels as usize;
    let frames = params.valid_frame_count;

    for (input_prop, output_prop) in input_props.iter().zip(output_props.iter_mut()) {
        let input_info = BufferInfo::from_prop(input_prop, in_ch);
        let mut output_info = BufferInfo::from_prop_mut(output_prop, out_ch);

        // Step 1: evaluate_buffer
        let (action, mut output_flags) = evaluate_buffer(input_prop.u32BufferFlags, params.allow_silent_buffer);
        match action {
            BufferAction::Skip | BufferAction::Silent => {
                output_info.zero();
                output_prop.u32BufferFlags = BUFFER_SILENT;
                // EAPO:482 对齐：所有输出路径必须设置帧数（含静音/无效分支）。
                output_prop.u32ValidFrameCount = frames as u32;
                continue;
            }
            BufferAction::Process => {}
        }

        let input_slice = unsafe { input_info.as_slice() };
        let output_slice = unsafe { output_info.as_slice_mut() };

        // Step 2: 交织 → 去交织
        deinterleave_into(input_slice, &mut temp_buffers[..in_ch], in_ch, frames);

        // Step 3: is_silent 优化检测
        if output_flags == BUFFER_SILENT {
            if is_silent(&temp_buffers[..out_ch.min(temp_buffers.len())], frames) {
                output_info.zero();
                output_prop.u32BufferFlags = BUFFER_SILENT;
                output_prop.u32ValidFrameCount = frames as u32;
                continue;
            } else {
                output_flags = BUFFER_VALID;
            }
        }

        // Step 4: 输出通道扩展（后通道清零 + mono 上混）
        if out_ch > in_ch {
            for ch in in_ch..out_ch.min(temp_buffers.len()) {
                temp_buffers[ch][..frames].fill(0.0);
            }
        }
        if in_ch == 1 && out_ch >= 2 {
            for f in 0..frames {
                temp_buffers[1][f] = temp_buffers[0][f];
            }
        }

        // Step 5: DSP 处理（先取长度避免借用冲突）
        let active_ch = out_ch.min(temp_buffers.len());

        // R3（v6.9）：空链快路径——去交织缓冲原样即输出，跳过链遍历。
        let result = if chain.is_empty() {
            // 空链：temp_buffers 已是输入（deinterleave 写入），无操作即通过。
            Ok(())
        } else {
            // 用 RT 编译期见证（O1）标记此路径为实时处理。
            let _rt = crate::pipeline::realtime::contract::RealtimeContext::new();
            chain.process(&mut temp_buffers[..active_ch], frames)
        };

        // Step 6: 错误恢复
        if result.is_err() {
            apply_error_policy(result, input_slice, output_slice, params.error_policy, stats);
            output_prop.u32BufferFlags = match params.error_policy {
                ErrorPolicy::Bypass => BUFFER_VALID,
                ErrorPolicy::Silence => BUFFER_SILENT,
            };
            output_prop.u32ValidFrameCount = frames as u32;
            continue;
        }

        // Step 7: 去交织 → 交织
        interleave_from_guarded(
            &temp_buffers[..out_ch.min(temp_buffers.len())],
            output_slice,
            out_ch.min(temp_buffers.len()),
            frames,
        );
        output_prop.u32BufferFlags = output_flags;
        // EAPO:482 对齐：显式设置输出帧数（APO 契约要求 APO 写回）——
        // 缺失 → 引擎判输出无效 → 完全无声（2026-08-04 实测 audiodg 加载但无声根因）。
        output_prop.u32ValidFrameCount = frames as u32;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_policy_bypass_copies_input() {
        let stats = ProcessStatistics::new();
        let mut out = vec![0.0; 4];
        let result: Result<()> = Err(crate::utils::vx_error::VxApoError::internal("test"));
        apply_error_policy(result, &[1.0, 2.0, 3.0, 4.0], &mut out, ErrorPolicy::Bypass, &stats);
        assert_eq!(out, vec![1.0, 2.0, 3.0, 4.0]);
        assert_eq!(stats.error_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn error_policy_silence_zeroes() {
        let stats = ProcessStatistics::new();
        let mut out = vec![1.0; 4];
        let result: Result<()> = Err(crate::utils::vx_error::VxApoError::internal("test"));
        apply_error_policy(result, &[9.0; 4], &mut out, ErrorPolicy::Silence, &stats);
        assert_eq!(out, vec![0.0; 4]);
    }

    #[test]
    fn process_chain_interleaved_passthrough() {
        let mut chain = Chain::new();
        let input = vec![1.0, 2.0, 3.0, 4.0];
        let mut output = vec![0.0; 4];
        let mut temp = vec![vec![0.0; 4]; 2];
        process_chain_interleaved(&mut chain, &input, &mut output, 2, 2, &mut temp).unwrap();
        assert_eq!(input, output);
    }
}
