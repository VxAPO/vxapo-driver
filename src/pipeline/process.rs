//! pipeline/process.rs — APOProcess 调度 + 桥接函数 + 错误策略（规范 4.6）

use std::sync::atomic::{AtomicU32, Ordering};

use crate::pipeline::buffer::{evaluate_buffer, BufferAction, BufferInfo};
use crate::pipeline::chain::Chain;
use crate::pipeline::interleave::{deinterleave_into, interleave_from_guarded};
use crate::sys::com::apo_types::{APO_CONNECTION_PROPERTY, BUFFER_SILENT, BUFFER_VALID};
use crate::utils::vx_error::Result;

/// 错误恢复策略。
#[derive(Copy, Clone)]
pub enum ErrorPolicy {
    /// 链处理失败时直通（仅当输入有效时安全）。
    Bypass,
    #[allow(dead_code)] // 死簇：仅被已死的调用链引用，删除需整链评估
    /// 链处理失败时静音。
    Silence,
}

#[allow(dead_code)] // 死簇：仅被已死的调用链引用，删除需整链评估
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
                    // 输入输出可能是同一块 in-place 缓冲，不能用 copy_from_slice
                    // （重叠 UB）；逐元素拷贝等价 memmove。
                    for i in 0..copy_len {
                        output_buffer[i] = input_slice[i];
                    }
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
    // 防御：过渡路径可能收到超过平面缓冲容量的帧数，
    // 直接旁通而不是 panic，避免 audiodg 崩溃/连锁静音。
    let io_ok = frame_count
        .checked_mul(channels)
        .map_or(false, |n| n <= input.len() && n <= output.len());
    let ready = temp.len() >= channels
        && temp[..channels].iter().all(|b| b.len() >= frame_count)
        && io_ok;
    if !ready {
        let copy_len = input.len().min(output.len());
        for i in 0..copy_len {
            output[i] = input[i];
        }
        if output.len() > copy_len {
            output[copy_len..].fill(0.0);
        }
        return Ok(());
    }
    deinterleave_into(input, temp, channels, frame_count);
    chain.process(temp, frame_count)?;
    interleave_from_guarded(temp, output, channels, frame_count);
    Ok(())
}

/// APOProcess 正常模式的完整处理流程（规范 4.6）。
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
    // （审查 #12）：切片长度由 valid_frame_count 生成时，防御检查恒真——
    // 必须先 clamp 到 max_frame_count 再构造切片，引擎违约时以截断代替越界。
    let frames = params.valid_frame_count.min(params.max_frame_count);

    for (input_prop, output_prop) in input_props.iter().zip(output_props.iter_mut()) {
        let input_info = BufferInfo::from_prop(input_prop, in_ch);
        let mut output_info = BufferInfo::from_prop_mut(output_prop, out_ch);

        // Step 1: evaluate_buffer
        let (action, _) = evaluate_buffer(input_prop.u32BufferFlags, params.allow_silent_buffer);
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

        // BUFFER_SILENT 标志是权威的——“内容无效，勿读”。
        // 引擎（如浏览器音效菜单切换）会复用上一帧缓冲，SILENT 标志下里面
        // 残留的可能是我们自己上一帧的输出；若按有效数据处理会形成
        // “输出→下一帧输入”的自我反馈爆音（日志实证 in_peak≈30 @ in_flags=2）。
        let input_silent = input_prop.u32BufferFlags == BUFFER_SILENT;
        let input_slice = unsafe { input_info.as_slice() };
        let output_slice = unsafe { output_info.as_slice_mut() };

        // 防御（多流崩溃根因）：引擎传入的帧数偶尔会超过按
        // max_frame_count 分配的临时缓冲。此时直通而非 panic，避免 audiodg 崩溃。
        let input_ok = frames
            .checked_mul(in_ch)
            .map_or(false, |n| n <= input_slice.len());
        let output_ok = frames
            .checked_mul(out_ch)
            .map_or(false, |n| n <= output_slice.len());
        let deinterleave_ready = temp_buffers.len() >= in_ch
            && temp_buffers[..in_ch].iter().all(|b| b.len() >= frames)
            && input_ok
            && output_ok;
        if !deinterleave_ready {
            let copy_len = input_slice.len().min(output_slice.len());
            apply_error_policy(
                Err(crate::utils::vx_error::VxApoError::internal(
                    "temp buffer smaller than valid frame count",
                )),
                input_slice,
                output_slice,
                ErrorPolicy::Bypass,
                stats,
            );
            output_prop.u32BufferFlags = BUFFER_VALID;
            // 不能谎报帧数：输出缓冲装不下 frames 时，只报实际写入的帧数。
            output_prop.u32ValidFrameCount =
                (copy_len / out_ch.max(1)) as u32;
            continue;
        }

        // Step 2: 交织 → 去交织（SILENT 输入按全零处理，绝不读取残留内容）
        if input_silent {
            for ch in temp_buffers[..in_ch].iter_mut() {
                ch[..frames].fill(0.0);
            }
        } else {
            deinterleave_into(input_slice, &mut temp_buffers[..in_ch], in_ch, frames);
        }

        // Step 3: 输出通道扩展（后通道清零 + mono 上混）
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

        // Step 4: DSP 处理（先取长度避免借用冲突）
        let active_ch = out_ch.min(temp_buffers.len());

        // 空链快路径——去交织缓冲原样即输出，跳过链遍历。
        let result = if chain.is_empty() {
            // 空链：temp_buffers 已是输入（deinterleave 写入），无操作即通过。
            Ok(())
        } else {
            // 用 RT 编译期见证标记此路径为实时处理。
            let _rt = crate::pipeline::realtime::contract::RealtimeContext::new();
            chain.process(&mut temp_buffers[..active_ch], frames)
        };

        // Step 5: 错误恢复
        if result.is_err() {
            apply_error_policy(result, input_slice, output_slice, params.error_policy, stats);
            output_prop.u32BufferFlags = match params.error_policy {
                ErrorPolicy::Bypass => BUFFER_VALID,
                ErrorPolicy::Silence => BUFFER_SILENT,
            };
            output_prop.u32ValidFrameCount = frames as u32;
            continue;
        }

        // Step 6: 去交织 → 交织
        interleave_from_guarded(
            &temp_buffers[..out_ch.min(temp_buffers.len())],
            output_slice,
            out_ch.min(temp_buffers.len()),
            frames,
        );
        if input_silent {
            // 引擎声明静音：输出必须为静音（链状态照常更新，但内容不落到输出）。
            output_slice.fill(0.0);
            output_prop.u32BufferFlags = BUFFER_SILENT;
        } else {
            output_prop.u32BufferFlags = BUFFER_VALID;
        }
        // EAPO:482 对齐：显式设置输出帧数（APO 契约要求 APO 写回）——
        // 缺失 → 引擎判输出无效 → 完全无声（实测 audiodg 加载但无声根因）。
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

    /// 回归：引擎标记 BUFFER_SILENT 但缓冲内残留大数值（脏静音缓冲）时，
    /// 必须输出静音且不得把残留内容当有效音频处理（自我反馈爆音根因）。
    #[test]
    fn silent_buffer_with_garbage_outputs_silence() {
        let mut chain = Chain::new();
        let stats = ProcessStatistics::new();
        let mut input_buf = vec![5.0f32; 4]; // 残留“上一帧输出”式的大数值
        let mut output_buf = vec![1.0f32; 4];
        let input_prop = APO_CONNECTION_PROPERTY {
            pBuffer: input_buf.as_mut_ptr() as usize,
            u32ValidFrameCount: 4,
            u32BufferFlags: BUFFER_SILENT,
            u32Signature: 0,
        };
        let mut output_prop = APO_CONNECTION_PROPERTY {
            pBuffer: output_buf.as_mut_ptr() as usize,
            u32ValidFrameCount: 4,
            u32BufferFlags: BUFFER_VALID,
            u32Signature: 0,
        };
        let params = ProcessParams {
            input_channels: 1,
            output_channels: 1,
            sample_rate: 48_000,
            max_frame_count: 4,
            valid_frame_count: 4,
            error_policy: ErrorPolicy::Bypass,
            allow_silent_buffer: true,
        };
        let mut temp = vec![vec![0.0f32; 4]; 1];
        process_audio(
            std::slice::from_ref(&input_prop),
            std::slice::from_mut(&mut output_prop),
            &params,
            &mut chain,
            &stats,
            &mut temp,
        )
        .unwrap();
        assert!(output_buf.iter().all(|&v| v == 0.0), "silent input must zero output");
        assert_eq!(output_prop.u32BufferFlags, BUFFER_SILENT);
        assert_eq!(output_prop.u32ValidFrameCount, 4);
    }

    /// 回归（审查 #12）：引擎违约给出超过 max_frame_count 的帧数时，
    /// 必须先 clamp 再处理——不得在切片构造处越界，输出帧数按实际写入上报。
    #[test]
    fn process_audio_clamps_frames_to_max_frame_count() {
        let mut chain = Chain::new();
        let stats = ProcessStatistics::new();
        // 缓冲按 max_frame_count=4 分配，但引擎违约报 99 帧。
        let mut input_buf = vec![0.0f32; 4];
        let mut output_buf = vec![9.0f32; 4];
        let input_prop = APO_CONNECTION_PROPERTY {
            pBuffer: input_buf.as_mut_ptr() as usize,
            u32ValidFrameCount: 99,
            u32BufferFlags: BUFFER_VALID,
            u32Signature: 0,
        };
        let mut output_prop = APO_CONNECTION_PROPERTY {
            pBuffer: output_buf.as_mut_ptr() as usize,
            u32ValidFrameCount: 99,
            u32BufferFlags: BUFFER_VALID,
            u32Signature: 0,
        };
        let params = ProcessParams {
            input_channels: 1,
            output_channels: 1,
            sample_rate: 48_000,
            max_frame_count: 4,
            valid_frame_count: 99,
            error_policy: ErrorPolicy::Bypass,
            allow_silent_buffer: true,
        };
        let mut temp = vec![vec![0.0f32; 4]; 1];
        process_audio(
            std::slice::from_ref(&input_prop),
            std::slice::from_mut(&mut output_prop),
            &params,
            &mut chain,
            &stats,
            &mut temp,
        )
        .unwrap();
        assert_eq!(output_prop.u32ValidFrameCount, 4, "必须上报实际写入的 clamp 后帧数");
        assert!(output_buf.iter().all(|&v| v == 0.0));
    }
}
