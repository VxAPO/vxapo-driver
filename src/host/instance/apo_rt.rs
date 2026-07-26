//! instance/audio_proc_obj_rt.rs — APOProcess + GetLatency + catch_unwind 防御（Note 60）
//!
//! 实现 `IAudioProcessingObjectRT` 接口：
//! - `APOProcess`：实时音频处理入口，整个 VxAPO 的热路径
//! - `GetLatency`：返回当前处理延迟（零基准加子 APO 延迟，Note 10）
//!
//! `APOProcess` 运行在多媒体实时线程上，所有 RT-safety 约束在此处强制执行（Note 12/57）。
//!
//! panic 防御（Note 60）：
//! - 入口处用 `std::panic::catch_unwind` 包裹所有实时处理逻辑
//! - 捕获到 panic 时：记录到 `logger.rs` 的 ring_logger → 输出缓冲区清零
//!   → 设置输出标志为 `BUFFER_SILENT` → 返回（不重新 panic）
//! - `catch_unwind` 在 release（`panic = "abort"`）下为空操作，
//!   真正防线为 `telemetry/panic.rs` 的 hook + `panic = "abort"` 进程终止
//! - debug / 测试环境下 `catch_unwind` 提供回溯信息与测试失败报告
//!
//! 此模块调用 `engine/pipeline.rs` 的 `process` 函数执行实际音频处理（Note 18）。

use crate::sys::com::apo_abi::BUFFER_SILENT;
use crate::pipeline::stream::process::Pipeline;
use crate::pipeline::realtime::ring::Ring1K;

// ══════════════════════════════════════════════════════════════════════════════
// APOProcess 入口（Note 60）
// ══════════════════════════════════════════════════════════════════════════════

/// APO 处理入口函数。
///
/// 由 COM 接口的 `APOProcess` 方法调用。
///
/// # 参数
///
/// - `pipeline`：处理调度器（持有配置、缓冲区、交换控制器）
/// - `input_buffer`：交织格式输入缓冲区
/// - `output_buffer`：交织格式输出缓冲区
/// - `input_channels`：输入通道数
/// - `output_channels`：输出通道数
/// - `frame_count`：本帧采样数
/// - `input_flags`：输入 `APO_CONNECTION_PROPERTY::flags`
/// - `allow_silent_buffer`：是否允许静音缓冲区快速路径
///
/// # 返回
///
/// 输出 `flags`（`BUFFER_VALID` 或 `BUFFER_SILENT`）。
///
/// # Note 60 — catch_unwind 防御
///
/// `catch_unwind` 包裹所有实时处理逻辑。
///
/// **注意**：当 `panic = "abort"`（release 默认配置）时，`catch_unwind` 是空操作——
/// panic 触发后直接 abort，清理代码不会执行。
/// 此时真正的防护是 `telemetry/panic.rs` 的 hook + `panic = "abort"` 的进程终止。
///
/// `catch_unwind` 在以下场景发挥作用：
/// - Debug 构建临时切换为 `panic = "unwind"` 以获取回溯信息
/// - 测试场景中需要 unwind 来报告测试失败
pub fn apo_process(
    pipeline: &mut Pipeline,
    input_buffer: &[f32],
    output_buffer: &mut [f32],
    input_channels: usize,
    output_channels: usize,
    frame_count: usize,
    input_flags: u32,
    allow_silent_buffer: bool,
    log_ring: &Ring1K<u8>,
) -> u32 {
    // ── catch_unwind 包裹（Note 60） ────────────────────────────────────────

    // 将可变引用包装为 UnsafeCell，以便在 catch_unwind 闭包中使用
    let output_ptr = output_buffer.as_mut_ptr();
    let output_len = output_buffer.len();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pipeline.process(
            input_buffer,
            // SAFETY: output_buffer 由调用方传入，生命周期覆盖整个函数。
            // catch_unwind 闭包在函数返回前结束，不会超出 buffer 生命周期。
            unsafe { std::slice::from_raw_parts_mut(output_ptr, output_len) },
            frame_count,
            input_channels,
            output_channels,
            input_flags,
            allow_silent_buffer,
        )
    }));

    match result {
        Ok(flags) => flags,
        Err(panic_info) => {
            // ── panic 捕获（仅 unwind 模式有效） ────────────────────────────
            //
            // 1. 记录 panic 信息到 ring logger
            let panic_msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                s.as_bytes()
            } else if let Some(s) = panic_info.downcast_ref::<String>() {
                s.as_bytes()
            } else {
                b"unknown panic in APOProcess"
            };

            // 写入 ring logger（RT 安全，无分配）
            let _ = log_ring.push_slice(b"PANIC: ");
            let _ = log_ring.push_slice(panic_msg);
            let _ = log_ring.push_slice(b"\n");

            // 2. 清零输出缓冲区
            for v in output_buffer.iter_mut() {
                *v = 0.0;
            }

            // 3. 设置输出标志为 SILENT
            BUFFER_SILENT
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// GetLatency 辅助
// ══════════════════════════════════════════════════════════════════════════════

/// 获取 APO 延迟（`GetLatency` 的实现）。
///
/// 汇总过滤器链中所有过滤器的延迟。
/// 返回值以 `REFERENCE_TIME`（100 纳秒）为单位。
pub fn get_latency(pipeline: &Pipeline, sample_rate: u32) -> i64 {
    let chain_latency_samples = pipeline
        .swap_controller()
        .current_chain()
        .map(|c| c.total_latency())
        .unwrap_or(0);

    if sample_rate == 0 || chain_latency_samples == 0 {
        return 0;
    }

    chain_latency_samples as i64 * 10_000_000 / sample_rate as i64
}

// ══════════════════════════════════════════════════════════════════════════════
// CalcInputFrames / CalcOutputFrames（APO RT 接口）
// ══════════════════════════════════════════════════════════════════════════════

/// 给定输出帧数，计算需要的输入帧数。
///
/// VxAPO 的延迟 = 过滤器链延迟。输入帧数 = 输出帧数 + 延迟。
pub fn calc_input_frames(pipeline: &Pipeline, output_frames: u32) -> u32 {
    let latency = pipeline
        .swap_controller()
        .current_chain()
        .map(|c| c.total_latency())
        .unwrap_or(0);

    output_frames.saturating_add(latency)
}

/// 给定输入帧数，计算可产生的输出帧数。
///
/// 输出帧数 = 输入帧数 - 延迟（最少为 0）。
pub fn calc_output_frames(pipeline: &Pipeline, input_frames: u32) -> u32 {
    let latency = pipeline
        .swap_controller()
        .current_chain()
        .map(|c| c.total_latency())
        .unwrap_or(0);

    input_frames.saturating_sub(latency)
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::com::apo_abi::BUFFER_VALID;
    use crate::pipeline::stream::chain::{Chain, FilterInfo};
    use crate::dsp::filter::{Filter, PassthroughFilter};

    fn stereo_names() -> Vec<String> {
        vec!["L".to_owned(), "R".to_owned()]
    }

    // ── apo_process 基础 ────────────────────────────────────────────────────

    #[test]
    fn apo_process_no_chain_passthrough() {
        let log_ring = Ring1K::<u8>::new();
        let mut pipeline = Pipeline::new(480, 480, 2);
        let input = vec![1.0f32, 2.0, 3.0, 4.0];
        let mut output = vec![0.0f32; 4];

        let flags = apo_process(
            &mut pipeline, &input, &mut output,
            2, 2, 2, BUFFER_VALID, false, &log_ring,
        );

        assert_eq!(flags, BUFFER_VALID);
        assert_eq!(output, input);
    }

    #[test]
    fn apo_process_with_chain() {
        let log_ring = Ring1K::<u8>::new();
        let mut pipeline = Pipeline::new(480, 480, 2);

        let mut chain = Chain::new(2, 480, stereo_names());
        chain.push_filter(FilterInfo::new(
            Box::new(PassthroughFilter), vec![0, 1], vec![0, 1], true,
        ));
        pipeline.swap_controller_mut().submit_new_chain(chain);
        pipeline.swap_controller_mut().check_swap();

        let input = vec![1.0f32, 2.0, 3.0, 4.0];
        let mut output = vec![0.0f32; 4];

        let flags = apo_process(
            &mut pipeline, &input, &mut output,
            2, 2, 2, BUFFER_VALID, false, &log_ring,
        );

        assert_eq!(flags, BUFFER_VALID);
        assert_eq!(output, input);
    }

    #[test]
    fn apo_process_silent_buffer() {
        let log_ring = Ring1K::<u8>::new();
        let mut pipeline = Pipeline::new(480, 480, 2);
        let input = vec![0.0f32; 4];
        let mut output = vec![99.0f32; 4];

        let flags = apo_process(
            &mut pipeline, &input, &mut output,
            2, 2, 2, BUFFER_SILENT, false, &log_ring,
        );

        assert_eq!(flags, BUFFER_SILENT);
        assert!(output.iter().all(|&v| v == 0.0));
    }

    // ── get_latency ─────────────────────────────────────────────────────────

    #[test]
    fn get_latency_no_chain() {
        let pipeline = Pipeline::new(480, 480, 2);
        assert_eq!(get_latency(&pipeline, 48000), 0);
    }

    #[test]
    fn get_latency_zero_sample_rate() {
        let mut pipeline = Pipeline::new(480, 480, 2);
        let chain = Chain::new(2, 480, stereo_names());
        pipeline.swap_controller_mut().submit_new_chain(chain);
        pipeline.swap_controller_mut().check_swap();
        assert_eq!(get_latency(&pipeline, 0), 0);
    }

    // ── calc_input_frames / calc_output_frames ──────────────────────────────

    #[test]
    fn calc_frames_no_latency() {
        let pipeline = Pipeline::new(480, 480, 2);
        assert_eq!(calc_input_frames(&pipeline, 480), 480);
        assert_eq!(calc_output_frames(&pipeline, 480), 480);
    }

    #[test]
    fn calc_frames_with_latency() {
        let mut pipeline = Pipeline::new(480, 480, 2);
        let mut chain = Chain::new(2, 480, stereo_names());

        // 添加一个有延迟的 mock 过滤器
        #[derive(Debug)]
        struct DelayFilter;
        impl Filter for DelayFilter {
            fn initialize(&mut self, _: u32, _: &[String]) -> Option<Vec<String>> { None }
            fn process(&mut self, _: &mut [Vec<f32>], _: usize) {}
            fn latency(&self) -> u32 { 128 }
        }

        chain.push_filter(FilterInfo::new(
            Box::new(DelayFilter), vec![0, 1], vec![0, 1], true,
        ));
        pipeline.swap_controller_mut().submit_new_chain(chain);
        pipeline.swap_controller_mut().check_swap();

        assert_eq!(calc_input_frames(&pipeline, 480), 608); // 480 + 128
        assert_eq!(calc_output_frames(&pipeline, 608), 480); // 608 - 128
    }

    #[test]
    fn calc_output_frames_saturates() {
        let mut pipeline = Pipeline::new(480, 480, 2);
        let mut chain = Chain::new(2, 480, stereo_names());

        #[derive(Debug)]
        struct BigDelayFilter;
        impl Filter for BigDelayFilter {
            fn initialize(&mut self, _: u32, _: &[String]) -> Option<Vec<String>> { None }
            fn process(&mut self, _: &mut [Vec<f32>], _: usize) {}
            fn latency(&self) -> u32 { 1000 }
        }

        chain.push_filter(FilterInfo::new(
            Box::new(BigDelayFilter), vec![0, 1], vec![0, 1], true,
        ));
        pipeline.swap_controller_mut().submit_new_chain(chain);
        pipeline.swap_controller_mut().check_swap();

        // 输入帧数 < 延迟 → saturating_sub 返回 0
        assert_eq!(calc_output_frames(&pipeline, 100), 0);
    }

    // ── catch_unwind 测试（仅 unwind 模式有效） ────────────────────────────

    #[test]
    #[cfg(debug_assertions)] // debug 模式下 panic = "unwind"
    fn apo_process_catches_panic() {
        let log_ring = Ring1K::<u8>::new();
        let mut pipeline = Pipeline::new(480, 480, 2);

        // 创建一个会 panic 的过滤器
        #[derive(Debug)]
        struct PanicFilter;
        impl Filter for PanicFilter {
            fn initialize(&mut self, _: u32, _: &[String]) -> Option<Vec<String>> { None }
            fn process(&mut self, _: &mut [Vec<f32>], _: usize) {
                panic!("intentional test panic");
            }
        }

        let mut chain = Chain::new(2, 480, stereo_names());
        chain.push_filter(FilterInfo::new(
            Box::new(PanicFilter), vec![0, 1], vec![0, 1], true,
        ));
        pipeline.swap_controller_mut().submit_new_chain(chain);
        pipeline.swap_controller_mut().check_swap();

        let input = vec![1.0f32; 4];
        let mut output = vec![99.0f32; 4];

        let flags = apo_process(
            &mut pipeline, &input, &mut output,
            2, 2, 2, BUFFER_VALID, false, &log_ring,
        );

        // panic 被捕获：输出清零，返回 SILENT
        assert_eq!(flags, BUFFER_SILENT);
        assert!(output.iter().all(|&v| v == 0.0));

        // 日志中应包含 panic 信息
        let mut log_buf = vec![0u8; 256];
        let n = log_ring.pop_slice(&mut log_buf);
        let log_str = String::from_utf8_lossy(&log_buf[..n]);
        assert!(log_str.contains("PANIC"), "log should contain PANIC: {log_str}");
        assert!(log_str.contains("intentional test panic"));
    }
}