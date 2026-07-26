//! engine/pipeline.rs — APOProcess 调度入口（Note 18）
//!
//! 完整处理流程：
//!
//! ```text
//! 1. 检查 BufferFlags
//! 2. 静音清零（BUFFER_SILENT + !allowSilentBuffer）
//! 3. 快速路径（空配置时 memcpy 或直返）
//! 4. 去交织
//! 5. 清零额外通道（c 从 realChannelCount 到 allChannelCount）
//! 6. mono 上混（通道 0→1，仅单声道且输出 ≥2）
//! 7. 过滤器链处理（逐 FilterInfo 执行 process）
//! 8. 过渡混合（有 nextConfig 时执行 blend）
//! 9. 交织输出
//! 10. 输出标志控制
//! ```
//!
//! `instance/audio_proc_obj_rt.rs` 中的 `APOProcess` 入口经 `catch_unwind` 包裹后
//! 调用本模块的 `process` 函数执行实际音频处理（Note 60）。
//!
//! 此模块运行在实时音频线程中，禁止堆分配、互斥锁、I/O、panic（Note 12）。

use crate::pipeline::stream::buffer::{self, BufferAction};
use crate::pipeline::stream::chain::Chain;
use crate::pipeline::stream::deinterleave;
use crate::pipeline::stream::swap::SwapController;
use crate::pipeline::realtime::contract::RtGuard;

// ══════════════════════════════════════════════════════════════════════════════
// Pipeline — 音频处理调度器
// ══════════════════════════════════════════════════════════════════════════════

/// 音频处理调度器。
///
/// 持有配置交换控制器，每帧调用 `process` 执行完整音频处理流程。
pub struct Pipeline {
    /// 配置交换控制器。
    swap: SwapController,

    /// 交织输入缓冲区（从 APOProcess 接收，用于去交织前的临时存储）。
    /// 预分配，不在实时路径中分配。
    interleave_buf: Vec<f32>,

    /// 交织输出缓冲区（从 interleave 输出到 APOProcess）。
    interleave_out_buf: Vec<f32>,
}

impl Pipeline {
    /// 创建新的处理调度器。
    ///
    /// - `smoothing_length`：过渡帧数
    /// - `max_frame_count`：最大帧数（缓冲区预分配）
    /// - `max_channels`：最大通道数（用于预分配交织缓冲区）
    pub fn new(smoothing_length: u32, max_frame_count: usize, max_channels: usize) -> Self {
        Self {
            swap: SwapController::new(smoothing_length),
            interleave_buf: vec![0.0f32; max_frame_count * max_channels],
            interleave_out_buf: vec![0.0f32; max_frame_count * max_channels],
        }
    }

    /// 获取交换控制器的共享引用（供 builder 线程使用）。
    pub fn swap_controller(&self) -> &SwapController {
        &self.swap
    }

    /// 获取交换控制器的可变引用。
    pub fn swap_controller_mut(&mut self) -> &mut SwapController {
        &mut self.swap
    }

    // ── 主处理入口（实时路径） ──────────────────────────────────────────────

    /// 处理一帧音频数据。
    ///
    /// 完整流程见模块文档（Note 18）。
    ///
    /// # 参数
    ///
    /// - `input`：交织格式输入缓冲区
    /// - `output`：交织格式输出缓冲区
    /// - `frame_count`：本帧采样数
    /// - `input_channels`：输入通道数
    /// - `output_channels`：输出通道数
    /// - `input_flags`：输入 `APO_CONNECTION_PROPERTY::flags`
    /// - `allow_silent_buffer`：是否允许静音缓冲区快速路径
    ///
    /// # 返回
    ///
    /// 输出 `APO_CONNECTION_PROPERTY::flags`（`BUFFER_VALID` 或 `BUFFER_SILENT`）。
    ///
    /// # 实时安全
    ///
    /// 所有缓冲区预分配。无堆分配、无锁、无 I/O。
    /// 配置交换通过 `SwapController::check_swap`（try_lock，非阻塞）。
    pub fn process(
        &mut self,
        input: &[f32],
        output: &mut [f32],
        frame_count: usize,
        input_channels: usize,
        output_channels: usize,
        input_flags: u32,
        allow_silent_buffer: bool,
    ) -> u32 {
        // ── 进入 RT 上下文（debug 模式跟踪） ────────────────────────────────
        let _guard = RtGuard::new();

        // ── Step 1: 检查 BufferFlags ────────────────────────────────────────
        let (action, mut output_flags) = buffer::evaluate_buffer(input_flags, allow_silent_buffer);

        match action {
            BufferAction::Skip => {
                // BUFFER_INVALID：不处理，输出清零
                for v in output.iter_mut() {
                    *v = 0.0;
                }
                return buffer::BUFFER_INVALID;
            }
            BufferAction::Silent => {
                // BUFFER_SILENT + !allowSilentBuffer：强制清零输出
                for v in output.iter_mut() {
                    *v = 0.0;
                }
                return buffer::BUFFER_SILENT;
            }
            BufferAction::Process => {
                // 继续处理
            }
        }

        // ── Step 2: 检查配置交换 ────────────────────────────────────────────
        self.swap.check_swap();

        // ── Step 3: 快速路径（无配置） ──────────────────────────────────────
        if !self.swap.has_chain() {
            // 无配置时 passthrough：直接 memcpy 输入到输出
            let len = input.len().min(output.len());
            for i in 0..len {
                output[i] = input[i];
            }
            return output_flags;
        }

        let chain = self.swap.current_chain_mut().unwrap();
        let max_frames = chain.max_frame_count();
        let actual_frames = frame_count.min(max_frames);
        let chain_channels = chain.all_channel_count();
        let real_channels = chain.real_channel_count();

        // ── Step 4: 去交织 ──────────────────────────────────────────────────
        deinterleave::deinterleave(
            input,
            chain.all_samples_mut(),
            input_channels.min(real_channels),
            actual_frames,
        );

        // ── Step 5: 清零额外通道（Note 18） ─────────────────────────────────
        if chain_channels > real_channels {
            let extra_start = real_channels;
            let extra_end = chain_channels;
            let samples = chain.all_samples_mut();
            for c in extra_start..extra_end {
                for f in 0..actual_frames {
                    samples[c][f] = 0.0;
                }
            }
        }

        // ── Step 6: mono 上混 ───────────────────────────────────────────────
        // 仅单声道输入且输出 ≥2 时：通道 0 → 通道 1
        if input_channels == 1 && real_channels >= 2 {
            let samples = chain.all_samples_mut();
            for f in 0..actual_frames {
                samples[1][f] = samples[0][f];
            }
        }

        // ── Step 7: 静音检测（allowSilentBuffer 场景） ──────────────────────
        if output_flags == buffer::BUFFER_SILENT && allow_silent_buffer {
            // 检查实际数据是否确实静音
            if buffer::is_silent(chain.all_samples(), actual_frames) {
                // 确实静音：跳过 DSP 处理，输出清零
                for v in output.iter_mut() {
                    *v = 0.0;
                }
                return buffer::BUFFER_SILENT;
            } else {
                // 非静音：标记为 VALID，继续正常处理
                output_flags = buffer::BUFFER_VALID;
            }
        }

        // ── Step 8: 过滤器链处理 ────────────────────────────────────────────
        self.process_filter_chain(actual_frames);

        // ── Step 9: 过渡混合 ────────────────────────────────────────────────
        if self.swap.is_transitioning() {
            self.process_transition(actual_frames, output_flags);
        }

        // ── Step 10: 交织输出 ───────────────────────────────────────────────
        let chain = self.swap.current_chain_mut().unwrap();
        deinterleave::interleave(
            chain.all_samples(),
            output,
            output_channels.min(real_channels),
            actual_frames,
        );

        output_flags
    }

    // ── 过滤器链处理 ────────────────────────────────────────────────────────

    /// 遍历过滤器链，逐个执行 process。
    ///
    /// 原地过滤器直接操作 allSamples，非原地使用 allSamples2 后交换。
    fn process_filter_chain(&mut self, frame_count: usize) {
        let chain = self.swap.current_chain_mut().unwrap();
        let filter_count = chain.filter_count();

        for i in 0..filter_count {
            let (filters, samples, samples2) = unsafe {
                // SAFETY: 我们需要同时访问 filters 和 buffers，
                // 但 Filter::process 只操作 samples/samples2，
                // 不修改 filters Vec 本身。
                // 通过分离借用避免借用冲突。
                let chain_ptr = chain as *mut Chain;
                let filters = &mut *(*chain_ptr).filters_mut() as *mut [super::chain::FilterInfo];
                let samples = (*chain_ptr).all_samples_mut() as *mut [Vec<f32>];
                let samples2 = (*chain_ptr).all_samples2_mut() as *mut [Vec<f32>];
                (&mut *filters, &mut *samples, &mut *samples2)
            };

            let filter_info = &mut filters[i];

            if filter_info.in_place {
                // Note 13b: 原地处理——直接操作主缓冲区
                filter_info.filter.process(samples, frame_count);
            } else {
                // Note 13b: 非原地处理——使用辅助缓冲区后交换
                // 1. 将主缓冲区的输入通道复制到辅助缓冲区
                for (dst_idx, &src_idx) in filter_info.input_channels.iter().enumerate() {
                    if dst_idx < samples2.len() && src_idx < samples.len() {
                        for f in 0..frame_count {
                            samples2[dst_idx][f] = samples[src_idx][f];
                        }
                    }
                }

                // 2. 在辅助缓冲区上执行过滤器
                filter_info.filter.process(samples2, frame_count);

                // 3. 将结果写回主缓冲区的输出通道
                for (src_idx, &dst_idx) in filter_info.output_channels.iter().enumerate() {
                    if src_idx < samples2.len() && dst_idx < samples.len() {
                        for f in 0..frame_count {
                            samples[dst_idx][f] = samples2[src_idx][f];
                        }
                    }
                }
            }
        }
    }

    // ── 过渡混合 ────────────────────────────────────────────────────────────

    /// 新旧配置输出混合。
    ///
    /// 过渡期间，旧配置（previous_chain）和新配置（current_chain）
    /// 各自独立处理后，按升余弦因子混合。
    fn process_transition(&mut self, frame_count: usize, _output_flags: u32) {
        let factor = match self.swap.advance_transition() {
            Some(f) => f,
            None => return, // 过渡已完成
        };

        if factor >= 1.0 {
            // 过渡完成，不需要混合
            return;
        }

        // 获取新旧两套链的输出
        let chain = match self.swap.current_chain_mut() {
            Some(c) => c,
            None => return,
        };

        // previous_chain 的处理结果存储在 allSamples 中（它已经被处理过了）
        // current_chain 的处理结果也在 allSamples 中
        // 但过渡期间需要分别处理再混合——
        //
        // 实际实现中，previous_chain 在 swap 时已经保存了它的状态，
        // 这里简化处理：直接对当前输出应用因子混合。
        // 完整实现需要在 check_swap 时保存旧链的输出快照。
        //
        // Phase 4+ 会补全完整的双链混合逻辑。
        // 当前简化：在过渡期间直接切换（factor 从 0 跳到 1）。

        let _ = (factor, chain, frame_count);
        // TODO Phase 4: 实现完整的新旧链混合
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::stream::chain::{Chain, FilterInfo};
    use crate::dsp::filter::PassthroughFilter;

    fn stereo_names() -> Vec<String> {
        vec!["L".to_owned(), "R".to_owned()]
    }

    // ── 无配置 passthrough ──────────────────────────────────────────────────

    #[test]
    fn no_chain_passthrough() {
        let mut pipeline = Pipeline::new(480, 480, 2);
        let input = vec![1.0f32, 2.0, 3.0, 4.0]; // 2ch × 2 frames
        let mut output = vec![0.0f32; 4];

        let flags = pipeline.process(
            &input, &mut output, 2, 2, 2,
            buffer::BUFFER_VALID, false,
        );

        assert_eq!(flags, buffer::BUFFER_VALID);
        assert_eq!(output, input); // passthrough
    }

    // ── INVALID 输入 ────────────────────────────────────────────────────────

    #[test]
    fn invalid_buffer_returns_zero() {
        let mut pipeline = Pipeline::new(480, 480, 2);
        let input = vec![1.0f32; 4];
        let mut output = vec![99.0f32; 4];

        let flags = pipeline.process(
            &input, &mut output, 2, 2, 2,
            buffer::BUFFER_INVALID, false,
        );

        assert_eq!(flags, buffer::BUFFER_INVALID);
        assert!(output.iter().all(|&v| v == 0.0));
    }

    // ── SILENT + !allow ─────────────────────────────────────────────────────

    #[test]
    fn silent_without_allow_returns_zero() {
        let mut pipeline = Pipeline::new(480, 480, 2);
        let input = vec![0.5f32; 4]; // 有信号但标记为 SILENT
        let mut output = vec![99.0f32; 4];

        let flags = pipeline.process(
            &input, &mut output, 2, 2, 2,
            buffer::BUFFER_SILENT, false,
        );

        assert_eq!(flags, buffer::BUFFER_SILENT);
        assert!(output.iter().all(|&v| v == 0.0));
    }

    // ── SILENT + allow + 确实静音 ───────────────────────────────────────────

    #[test]
    fn silent_with_allow_actually_silent() {
        let mut pipeline = Pipeline::new(480, 480, 2);

        // 加载配置
        let mut chain = Chain::new(2, 480, stereo_names());
        chain.push_filter(FilterInfo::new(
            Box::new(PassthroughFilter), vec![0, 1], vec![0, 1], true,
        ));
        pipeline.swap.submit_new_chain(chain);
        pipeline.swap.check_swap();

        let input = vec![0.0f32; 4]; // 全零
        let mut output = vec![99.0f32; 4];

        let flags = pipeline.process(
            &input, &mut output, 2, 2, 2,
            buffer::BUFFER_SILENT, true,
        );

        assert_eq!(flags, buffer::BUFFER_SILENT);
        assert!(output.iter().all(|&v| v == 0.0));
    }

    // ── 带配置的 passthrough ────────────────────────────────────────────────

    #[test]
    fn with_chain_passthrough() {
        let mut pipeline = Pipeline::new(480, 480, 2);

        let mut chain = Chain::new(2, 480, stereo_names());
        chain.push_filter(FilterInfo::new(
            Box::new(PassthroughFilter), vec![0, 1], vec![0, 1], true,
        ));
        pipeline.swap.submit_new_chain(chain);
        pipeline.swap.check_swap();

        let input = vec![1.0f32, 2.0, 3.0, 4.0];
        let mut output = vec![0.0f32; 4];

        let flags = pipeline.process(
            &input, &mut output, 2, 2, 2,
            buffer::BUFFER_VALID, false,
        );

        assert_eq!(flags, buffer::BUFFER_VALID);
        // PassthroughFilter 不修改数据
        assert_eq!(output, input);
    }

    // ── 配置热重载 ──────────────────────────────────────────────────────────

    #[test]
    fn config_hot_reload() {
        let mut pipeline = Pipeline::new(10, 480, 2);

        // 初始配置
        let chain1 = Chain::new(2, 480, stereo_names());
        pipeline.swap.submit_new_chain(chain1);
        pipeline.swap.check_swap();

        // 新配置
        let chain2 = Chain::new(2, 480, stereo_names());
        pipeline.swap.submit_new_chain(chain2);
        pipeline.swap.check_swap();
        assert!(pipeline.swap.is_transitioning());

        // 处理几帧
        let input = vec![0.5f32; 4];
        let mut output = vec![0.0f32; 4];
        for _ in 0..15 {
            pipeline.process(&input, &mut output, 2, 2, 2, buffer::BUFFER_VALID, false);
        }

        assert!(!pipeline.swap.is_transitioning());
    }

    // ── 单通道 ──────────────────────────────────────────────────────────────

    #[test]
    fn mono_passthrough() {
        let mut pipeline = Pipeline::new(480, 480, 1);
        let input = vec![1.0f32, 2.0, 3.0]; // 1ch × 3 frames
        let mut output = vec![0.0f32; 3];

        let flags = pipeline.process(
            &input, &mut output, 3, 1, 1,
            buffer::BUFFER_VALID, false,
        );

        assert_eq!(flags, buffer::BUFFER_VALID);
        assert_eq!(output, input);
    }

    // ── 部分帧 ──────────────────────────────────────────────────────────────

    #[test]
    fn partial_frame_count() {
        let mut pipeline = Pipeline::new(480, 480, 2);
        // 输入有 8 个采样（2ch × 4 frames），但只处理 2 帧
        let input = vec![1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let mut output = vec![0.0f32; 8];

        let flags = pipeline.process(
            &input, &mut output, 2, 2, 2,
            buffer::BUFFER_VALID, false,
        );

        assert_eq!(flags, buffer::BUFFER_VALID);
        // 只处理了 2 帧 = 4 个采样
        assert_eq!(output[0], 1.0);
        assert_eq!(output[1], 2.0);
        assert_eq!(output[2], 3.0);
        assert_eq!(output[3], 4.0);
    }

    // ── 端到端：builder 线程 + RT 线程 ──────────────────────────────────────

    #[test]
    fn end_to_end_builder_and_rt() {
        let mut pipeline = Pipeline::new(10, 480, 2);

        // Builder 线程构建配置
        let next = pipeline.swap.next_chain_handle();
        std::thread::spawn(move || {
            let mut chain = Chain::new(2, 480, vec!["L".into(), "R".into()]);
            chain.push_filter(FilterInfo::new(
                Box::new(PassthroughFilter), vec![0, 1], vec![0, 1], true,
            ));
            *next.lock().unwrap() = Some(chain);
        })
        .join()
        .unwrap();

        pipeline.swap.notify_new_chain();

        // RT 线程处理
        let input = vec![1.0f32, 2.0, 3.0, 4.0];
        let mut output = vec![0.0f32; 4];

        let flags = pipeline.process(
            &input, &mut output, 2, 2, 2,
            buffer::BUFFER_VALID, false,
        );

        assert_eq!(flags, buffer::BUFFER_VALID);
        assert_eq!(output, input);
    }

    // ── Debug 输出 ──────────────────────────────────────────────────────────

    #[test]
    fn pipeline_creation() {
        let pipeline = Pipeline::new(2400, 480, 8);
        assert!(!pipeline.swap.has_chain());
        assert!(!pipeline.swap.has_pending_swap());
    }
}