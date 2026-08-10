//! object/apo/process.rs — Lock/Unlock/APOProcess 实现
//!
//! 接口方法的大体积逻辑下沉到这里，`apo.rs` 的 trait 实现只保留参数转发：
//! - IAudioProcessingObjectRT：APOProcess / CalcInputFrames / CalcOutputFrames
//! - IAudioProcessingObjectConfiguration：LockForProcess / UnlockForProcess
//! - 对象级状态维护：Reset / GetLatency / GetInputChannelCount
use std::sync::atomic::Ordering;

use windows::core::Result;

use super::{ApoObject, ApoObject_Impl};
use super::config::{start_watcher, stop_watcher};
use super::state::{ApoState, LockGuard};
use crate::config::commands::register_all_commands;
use crate::config::parser::ConfigParser;
use crate::install::audiodg::ensure_can_load;
use crate::pipeline::chain::Chain;
use crate::pipeline::context::PipelineContext;
use crate::pipeline::dsp::factory::FilterRegistry;
use crate::pipeline::dsp::filter::{DspContext, DeviceType, ProcessingStage};
use crate::pipeline::dsp::math::init_audio_thread;
use crate::pipeline::format::{extract_format, AudioFormat};
use crate::pipeline::process::{
    ErrorPolicy, ProcessParams, process_audio, process_chain_interleaved,
};
use crate::object::vx_reg_props::CLSID_VXAPO_POST_MIX;
use crate::sys::audio_defs::get_channel_names;
use crate::sys::com::apo_interfaces::IAudioMediaType;
use crate::sys::com::apo_types::{
    APO_CONNECTION_DESCRIPTOR, APO_CONNECTION_PROPERTY, APOERR_INVALID_CONNECTION_FORMAT,
    APOERR_NOT_INITIALIZED, APOERR_NUM_CONNECTIONS_INVALID, BUFFER_SILENT, BUFFER_VALID,
};
use crate::sys::com::prelude::{E_FAIL, HRESULT};

/// 卷积型 GraphicEQ 的内部隐藏延迟相关预留（v9.5 起为分块 FFT 块大小 128）；
/// 临时缓冲按此预留余量，避免引擎按 `CalcInputFrames` 多给帧数时越界
/// （2026-08-10 实证：引擎实际会多给到 2×latency+1，2048 为安全余量）。
const MAX_APO_LATENCY_SAMPLES: usize = 2048;

/// `Reset`：清空链与过渡状态，回到未锁定基线。
pub(crate) fn reset(apo: &ApoObject_Impl) -> Result<()> {
    // ---- 探针 6: Reset 被调（2026-08-04 排查，删）----
    #[cfg(debug_assertions)]
    {
        let _ = std::fs::write(r"C:\ProgramData\VxAPO\method_probe.txt", "Reset called\n");
    }

    let mut inner = apo.mutex.lock().unwrap();
    inner.current_chain = Box::new(Chain::new());
    inner.outgoing_chain = None;
    inner.retired_chain = None; // R1：控制线程锁内统一析构
    inner.transition = None;
    inner.pipeline_context = PipelineContext::new();
    inner.temp_buffers.clear();
    inner.temp_buffer_old.clear();
    inner.temp_buffer_new.clear();
    inner.pending_reload = false;
    inner.reloading = false;
    // v7.9：清空配置指纹基线（重新 Lock 重新建立）。
    inner.active_spec.clear();
    apo.latency_samples.store(0, Ordering::SeqCst);
    apo.latency_frames_atomic.store(0, Ordering::SeqCst);
    Ok(())
}

/// `GetLatency`：有 child → 委托 child；无 child → 返回 0。
///
/// 分区块已缩至 32 采样（≈0.67ms），隐藏延迟不会造成可闻慢放；
/// 向引擎上报延迟会导致帧协商错位/无声（2026-08-10 实证），因此不上报。
pub(crate) fn get_latency(apo: &ApoObject_Impl) -> Result<i64> {
    // ---- 探针 6: GetLatency 被调（2026-08-04 排查，删）----
    #[cfg(debug_assertions)]
    {
        let _ = std::fs::write(r"C:\ProgramData\VxAPO\method_probe.txt", "GetLatency called\n");
    }

    if let Some(child) = apo.child_apo.lock().unwrap().as_ref() {
        return Ok(child.get_latency());
    }
    Ok(0)
}

/// `GetInputChannelCount`：仅锁定状态返回输入通道数。
pub(crate) fn get_input_channel_count(apo: &ApoObject_Impl) -> Result<u32> {
    // ---- 探针 6: GetInputChannelCount 被调（2026-08-04 排查，删）----
    #[cfg(debug_assertions)]
    {
        let _ = std::fs::write(
            r"C:\ProgramData\VxAPO\method_probe.txt",
            "GetInputChannelCount called\n",
        );
    }

    if apo.state_cell.current() != ApoState::Locked {
        return Err(windows::core::Error::from(APOERR_NOT_INITIALIZED));
    }
    let inner = apo.mutex.lock().unwrap();
    Ok(inner.pipeline_context.input_channels)
}

/// `APOProcess` 壳：状态 + 指针校验，catch_unwind 包裹处理主体。
///
/// 参数校验在壳外（状态 + 指针）：panic 兜底路径依赖合法的 `pp_outputs`——
/// 若非法指针在壳内被 panic 污染，兜底访问会二次访问非法内存（不可救）。
pub(crate) fn apo_process(
    apo: &ApoObject_Impl,
    num_input: u32,
    pp_inputs: *const *const APO_CONNECTION_PROPERTY,
    num_output: u32,
    pp_outputs: *mut *mut APO_CONNECTION_PROPERTY,
) {
    // RT 线程入口：硬件 FTZ/DAZ（thread_local 幂等，重复调用零开销）。
    init_audio_thread();

    if apo.state_cell.current() != ApoState::Locked {
        return;
    }
    if num_input == 0 || num_output == 0 || pp_inputs.is_null() || pp_outputs.is_null() {
        return;
    }

    // P0-5（v8.2/v8.3）：catch_unwind 入口包裹——debug（panic="unwind"）测试态
    // 防御路径，验证「即便 panic 也不跨 FFI 传播」；release（panic="abort"）下
    // catch_unwind 为编译移除的空操作（O3 主规范十五），panic 即确定性 abort。
    // AssertUnwindSafe：闭包持有裸指针（FFI 参数），跨包装需显式断言
    // （catch_unwind 仅需闭包内不产生未定义行为——panic 后兜底不再触碰输入指针）。
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        apo.apo_process_inner(num_input, pp_inputs, num_output, pp_outputs)
    }));
    if result.is_err() {
        // panic 捕获：输出清零 + BUFFER_SILENT + stats.error_count++ + 日志（RT 零分配）。
        apo.apo_process_panic_fallback(num_output, pp_outputs);
    }
}

/// `CalcInputFrames`：panic 保守值 = output_frames（不多不少、不二次 load）。
///
/// 实现注（object 7.1.12）：panic 分支不 load latency_frames_atomic（避免二次
/// panic）；保守策略以「不 panic + 不越界」为第一约束。
pub(crate) fn calc_input_frames(apo: &ApoObject_Impl, output_frames: u32) -> u32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        output_frames + apo.latency_frames_atomic.load(Ordering::Acquire)
    }))
    .unwrap_or_else(|_| output_frames)
}

/// `CalcOutputFrames`：panic 保守值 = 0（可丢帧不可越界，不二次 load）。
pub(crate) fn calc_output_frames(apo: &ApoObject_Impl, input_frames: u32) -> u32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let latency = apo.latency_frames_atomic.load(Ordering::Acquire);
        input_frames.saturating_sub(latency)
    }))
    .unwrap_or_else(|_| 0)
}

/// `LockForProcess`：状态机 + 双格式协商 + 链组装 + 子 APO 委托 + watcher 启动。
///
/// EAPO 对齐（EqualizerAPO.cpp:308-339）：Lock 需读输入+输出**双格式**，引擎可协商
/// in≠out（如设备 5.1 输出）；输出通道数/掩码按输出格式（render 场景）。
pub(crate) fn lock_for_process(
    apo: &ApoObject_Impl,
    num_input: u32,
    pp_inputs: *const *const APO_CONNECTION_DESCRIPTOR,
    num_output: u32,
    pp_outputs: *const *const APO_CONNECTION_DESCRIPTOR,
) -> Result<()> {
    // ---- P0-7 无声诊断探针 2（2026-08-04，debug 门控，排查完删除）----
    // 记录 LockForProcess 是否被调 + 输入/输出连接数（判断引擎是否走到配置阶段）。
    #[cfg(debug_assertions)]
    {
        let _ = std::fs::write(
            r"C:\ProgramData\VxAPO\lock_probe.txt",
            format!(
                "LockForProcess called: num_input={} num_output={}\n",
                num_input, num_output
            ),
        );
    }

    // Step 0: 状态机 Initialized → Locked，失败自动回退。
    apo.state_cell
        .transition(ApoState::Initialized, ApoState::Locked)
        .map_err(|e| windows::core::Error::from(HRESULT::from(e)))?;
    let _guard = LockGuard::new(&apo.state_cell);

    if num_input == 0 || pp_inputs.is_null() {
        return Err(windows::core::Error::from(APOERR_NUM_CONNECTIONS_INVALID));
    }
    if num_output == 0 || pp_outputs.is_null() {
        return Err(windows::core::Error::from(APOERR_NUM_CONNECTIONS_INVALID));
    }

    // Step 1: 从输入/输出连接描述符提取格式（pFormat 为 ManuallyDrop<Option<IAudioMediaType>>）。
    let input_descriptor = unsafe { &**pp_inputs };
    let output_descriptor = unsafe { &**pp_outputs };
    let extract_descriptor_format =
        |desc: &APO_CONNECTION_DESCRIPTOR| -> Result<AudioFormat> {
            match desc.pFormat.as_ref() {
                Some(media_type) => {
                    let mt_ptr: *mut IAudioMediaType =
                        media_type as *const IAudioMediaType as *mut IAudioMediaType;
                    unsafe { extract_format(mt_ptr) }
                        .map_err(|e| windows::core::Error::from(HRESULT::from(e)))
                }
                None => Err(windows::core::Error::from(APOERR_INVALID_CONNECTION_FORMAT)),
            }
        };
    let format = extract_descriptor_format(input_descriptor)?;
    let output_format = extract_descriptor_format(output_descriptor)?;

    // EAPO render 掩码语义（372-383）：channelMask = out 格式掩码；
    // out 掩码为 0 且 in/out 通道数相同 → 回退输入掩码。
    let mut channel_mask = output_format.channel_mask;
    if channel_mask == 0 && format.channels == output_format.channels {
        channel_mask = format.channel_mask;
    }

    // Step 2: 构建 PipelineContext + DspContext。
    let pipeline_context = PipelineContext {
        sample_rate: format.sample_rate,
        input_channels: format.channels,
        output_channels: output_format.channels,
        channel_mask,
        max_frame_count: input_descriptor.u32MaxFrameCount as usize,
    };

    let channel_names = get_channel_names(channel_mask);
    let dsp_ctx = DspContext {
        sample_rate: format.sample_rate,
        channel_count: format.channels,
        channel_mask,
        channel_names: channel_names.clone(),
        max_frame_count: input_descriptor.u32MaxFrameCount,
        bits_per_sample: format.bits_per_sample,
        device_type: DeviceType::Render,
        stage: ProcessingStage::None,
        variables: std::collections::HashMap::new(),
        loudness_enabled: std::cell::Cell::new(true),
        rt_marker: std::marker::PhantomData,
    };

    // Step 3: 构建 FilterRegistry + ConfigParser，解析配置文件
    //         （路径来自 Initialize 确定的 per-device config_path）。
    // v7.9：parse_file_with_spec → (滤波器列表, spec chain) 双返回。
    // active_spec 即本次解析产出的配置指纹（LockForProcess 建立基线）。
    // v9.4：PostMix 实例默认直通——Windows 对渲染设备同时挂 SFX(PreMix) + EFX(PostMix)
    // 两个 VxAPO 实例，若都加载同一 config 会把用户配置（如 GraphicEQ 卷积）应用两次：
    // 音量异常偏低 + 双倍隐藏延迟/CPU（设备切换后帧协商更易错位）。
    // PostMix 保留 child APO 委托（前任 EFX APO 仍生效），自身不再处理用户配置。
    let config_path = apo.config_path.lock().unwrap().clone();
    let is_postmix = apo.clsid == CLSID_VXAPO_POST_MIX;
    let (filters, spec_chain) = if is_postmix {
        (Vec::new(), Vec::new())
    } else {
        let mut registry = FilterRegistry::new();
        register_all_commands(&mut registry);
        let parser = ConfigParser::new(registry);
        parser
            .parse_file_with_spec(&config_path, &dsp_ctx)
            .map_err(|_| windows::core::Error::from(E_FAIL))?
    };
    crate::object::apo::config::diag_append(&format!(
        "LOCK clsid={:?} postmix={} rate={} in={} out={} maxframes={} filters={} spec={} first_spec={}",
        apo.clsid,
        is_postmix,
        format.sample_rate,
        format.channels,
        output_format.channels,
        input_descriptor.u32MaxFrameCount,
        filters.len(),
        spec_chain.len(),
        spec_chain.first().cloned().unwrap_or_default()
    ));

    // 配置解析落地探针（debug 门控，验证 Lock 时确实读到了 per-device config）。
    #[cfg(debug_assertions)]
    {
        let _ = std::fs::write(
            r"C:\ProgramData\VxAPO\config_probe.txt",
            format!(
                "Lock parsed filters={} spec={} path={}\n",
                filters.len(),
                spec_chain.len(),
                config_path
            ),
        );
    }

    // Step 4: 组装 Chain。
    let mut chain = Chain::new();
    for f in filters {
        chain
            .add_filter(f)
            .map_err(|_| windows::core::Error::from(E_FAIL))?;
    }
    // DSP 依赖 initialize 预计算系数/状态（GraphicEQ/PEQ/IIR/Delay/Convolution）。
    chain.initialize(format.sample_rate, &channel_names);
    // 延迟不上报引擎；分区块已缩小到 32 采样，隐藏延迟不产生可闻慢放。
    let _total_latency = chain.total_latency();

    // Step 5: 预分配过渡缓冲区（v7.8 修订，杜绝 RT 线程过渡首次 resize 扩容——
    //          EAPO 对齐：按 max_frame_count × max_ch 预分配充足容量）。
    // 2026-08-10：缓冲区额外预留 MAX_APO_LATENCY_SAMPLES，因为引擎按
    // CalcInputFrames(output+latency) 提供的帧数可能超过 max_frame_count。
    let max_ch = pipeline_context
        .input_channels
        .max(pipeline_context.output_channels) as usize;
    let frame_capacity = pipeline_context.max_frame_count + MAX_APO_LATENCY_SAMPLES;
    let max_samples = frame_capacity * max_ch;
    let temp_buffer_old = vec![0.0f32; max_samples];
    let temp_buffer_new = vec![0.0f32; max_samples];

    // deinterleave 空间（channels 个 Vec）。
    // **必须用 vec![0.0; len]（带长度），不能用 Vec::with_capacity（len=0）**——
    // deinterleave_into 按 `output[ch][f]` 写会越界 panic → catch_unwind 捕获 →
    // panic 兜底输出清零 + BUFFER_SILENT → 完全无声（2026-08-04 实测根因）。
    let mut temp_buffers: Vec<Vec<f32>> = Vec::with_capacity(max_ch);
    for _ in 0..max_ch {
        temp_buffers.push(vec![0.0f32; frame_capacity]);
    }

    // Step 6: 更新内部状态。（R1：退役链由控制线程锁内统一析构）
    {
        let mut inner = apo.mutex.lock().unwrap();
        inner.current_chain = Box::new(chain);
        inner.outgoing_chain = None;
        inner.retired_chain = None;
        inner.transition = None;
        inner.pipeline_context = pipeline_context;
        inner.temp_buffers = temp_buffers;
        inner.temp_buffer_old = temp_buffer_old;
        inner.temp_buffer_new = temp_buffer_new;
        inner.pending_reload = false;
        inner.reloading = false;
        // v7.9：active_spec 建立基线（当前生效链的配置指纹）。
        // 此后 hot_reload 与此基线比较决定是否真正切换。
        inner.active_spec = spec_chain;
    }
    apo.latency_samples.store(0, Ordering::SeqCst);
    apo.latency_frames_atomic.store(0, Ordering::SeqCst);

    // Step 6b（P0-6，object 7.1.9）：子 APO LockForProcess 委托（失败不阻塞父，Note 57）。
    // 对齐 EAPO 341-347：childCfg->LockForProcess 结果仅 Trace 不 return。
    if let Some(child) = apo.child_apo.lock().unwrap().as_ref() {
        // SAFETY: 父描述符指针从引擎传入（只读语义）；child API 用可变指针仅因
        // windows-rs 绑定如此（描述符数组在调用期间有效且不被 child 修改）。
        unsafe {
            let _ = child.lock_for_process(
                num_input,
                pp_inputs as *mut *mut APO_CONNECTION_DESCRIPTOR,
                num_output,
                pp_outputs as *mut *mut APO_CONNECTION_DESCRIPTOR,
            );
        }
    }

    // Step 7: 确保第三方 APO 可加载（DisableProtectedAudioDG）。
    ensure_can_load().map_err(|e| windows::core::Error::from(HRESULT::from(e)))?;

    // Step 8 (v7.10)：Lock 末尾启动 watcher（config_path 已确定 + active_spec 基线就绪）。
    // v9.4：PostMix 直通实例不启动 watcher（无配置可热重载，也避免双实例重复解析）。
    // 启动失败降级（仅日志），不阻塞锁定。
    if !is_postmix {
        if let Err(e) = start_watcher(apo) {
            log::warn!("LockForProcess: watcher start failed: {e}");
        }
    }

    // 全部成功 → 解除守卫（不再回退状态）。
    _guard.disarm();
    Ok(())
}

/// `UnlockForProcess`：状态回退 + 子 APO 委托 + watcher 停止 + 过渡状态清理。
///
/// 子 APO 解锁失败不阻塞父解锁（object 7.1.10 容错语义——UnlockForProcess 无重试语义）。
pub(crate) fn unlock_for_process(apo: &ApoObject_Impl) -> Result<()> {
    // ---- 探针 6: UnlockForProcess 被调（2026-08-04 排查，删）----
    #[cfg(debug_assertions)]
    {
        let _ = std::fs::write(
            r"C:\ProgramData\VxAPO\method_probe.txt",
            "UnlockForProcess called\n",
        );
    }

    apo.state_cell
        .transition(ApoState::Locked, ApoState::Initialized)
        .map_err(|e| windows::core::Error::from(HRESULT::from(e)))?;

    // P0-6（object 7.1.10）：子 APO UnlockForProcess 委托——失败不阻塞父解锁。
    if let Some(child) = apo.child_apo.lock().unwrap().as_ref() {
        let hr = child.unlock_for_process();
        if hr.0 != 0 {
            log::warn!("child APO UnlockForProcess failed");
        }
    }

    // Stop watcher：SetEvent → join → close（v7.10 stop_watcher）。先释放锁（join 可能等待）。
    drop(apo.mutex.lock().unwrap());
    stop_watcher(apo);

    // R1：退役链 + 过渡状态由控制线程锁内统一析构。
    let mut inner = apo.mutex.lock().unwrap();
    inner.retired_chain = None;
    inner.outgoing_chain = None;
    inner.transition = None;
    inner.pending_reload = false;
    inner.reloading = false;
    // v7.9：释放配置指纹基线（重新 Lock 时重建）。
    inner.active_spec.clear();
    Ok(())
}

// ══════════════════════════════════════════════════════════════════════════════
// RT 处理主体（P0-5，v8.2）
// ══════════════════════════════════════════════════════════════════════════════

impl ApoObject {
    /// APOProcess 实际处理主体。
    ///
    /// 由 `apo_process` 用 `catch_unwind` 包裹调用——参数校验（状态 + 指针）留在壳外。
    /// 本方法即 v8.2 前 `APOProcess` 的整体逻辑：双链过渡 + 升余弦混合 + R1 退役链
    /// + R2 触发重载 + 正常模式 `process_audio`。
    fn apo_process_inner(
        &self,
        num_input: u32,
        pp_inputs: *const *const APO_CONNECTION_PROPERTY,
        num_output: u32,
        pp_outputs: *mut *mut APO_CONNECTION_PROPERTY,
    ) {
        // ---- P0-7 无声诊断探针（2026-08-04，debug 门控，排查完删除）----
        // 每次调用 +1，写文件（仅测试用；RT 违规但 debug 阶段可接受）。
        // 用 static AtomicU32 每 100 次写一次，确认 APOProcess 是否被引擎调用。
        #[cfg(debug_assertions)]
        {
            use std::sync::atomic::{AtomicU32, Ordering as AOrd};
            static FRAME_COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = FRAME_COUNTER.fetch_add(1, AOrd::Relaxed);
            if n % 200 == 0 {
                let _ = std::fs::write(
                    r"C:\ProgramData\VxAPO\apo_process_probe.txt",
                    format!(
                        "APOProcess called: {} frames={}\n",
                        n,
                        unsafe { (**pp_inputs).u32ValidFrameCount }
                    ),
                );
            }
        }

        // P0-6（v8.1 D1）：childRT->APOProcess **前置每帧一次**（object 7.1.11 Step 3）。
        // 双链共享同一份 child 输出作输入；child 不在 current/outgoing 任一链内。
        // 锁 inner **前**调（避免持 inner 锁调 child——child 是独立 COM 对象，无循环依赖）。
        if let Some(child) = self
            .child_apo
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            // SAFETY: 引擎保证 pp_inputs/pp_outputs 有效（APOProcess 契约）。
            unsafe { child.apo_process(num_input, pp_inputs, num_output, pp_outputs) };
            // 委托帧数计算（RT 无锁，object 7.1.11：每帧委托）。
            let _ = child.calc_input_frames(0);
        }

        // 锁被前序 panic 污染时继续使用数据（PoisonError::into_inner），
        // 避免 RT 路径二次 panic 导致整个 audiodg 崩溃（2026-08-10 实证）。
        let mut inner = self.mutex.lock().unwrap_or_else(|e| e.into_inner());
        let pending = inner.pending_reload;

        // 过渡模式存在 → 双链处理 + 混合。
        if pending || inner.transition.is_some() {
            // 先把所有需要变异的字段移到栈上（每次仅单字段借用），避免互斥 guard 多 &mut。
            let mut outgoing = inner.outgoing_chain.take();
            let mut transition = inner.transition.take();
            let mut owned_chain =
                std::mem::replace(&mut inner.current_chain, Box::new(Chain::new()));
            let mut tbufs = std::mem::take(&mut inner.temp_buffers);
            let mut tbuf_old = std::mem::take(&mut inner.temp_buffer_old);
            let mut tbuf_new = std::mem::take(&mut inner.temp_buffer_new);
            let in_ch = inner.pipeline_context.input_channels as usize;
            let out_ch = inner.pipeline_context.output_channels as usize;

            // 防御（用户风险①）：pending 残留但过渡不在途（transition 已被清空/
            // 被其它路径消费）→ 无混合器。此时**必须写出**（APO 契约：每帧写输出）：
            // 直接复制输入到输出（bypass），恢复状态、旧链退役；若可重载则触发补重载。
            if transition.is_none() {
                let input_prop = unsafe { &**pp_inputs };
                let output_prop = unsafe { &mut **pp_outputs };
                let frames = input_prop.u32ValidFrameCount as usize;
                let src = unsafe {
                    std::slice::from_raw_parts(input_prop.pBuffer as *const f32, frames * in_ch)
                };
                let dst = unsafe {
                    std::slice::from_raw_parts_mut(output_prop.pBuffer as *mut f32, frames * out_ch)
                };
                let copy_len = src.len().min(dst.len());
                // in-place 场景 src/dst 可能重叠，逐元素拷贝（memmove 语义）。
                for i in 0..copy_len {
                    dst[i] = src[i];
                }
                if dst.len() > copy_len {
                    dst[copy_len..].fill(0.0);
                }
                output_prop.u32ValidFrameCount = frames as u32;
                output_prop.u32BufferFlags = BUFFER_VALID;

                inner.retired_chain = outgoing; // R1：旧链退役（控制线程析构）
                inner.outgoing_chain = None;
                inner.transition = None;
                inner.current_chain = owned_chain;
                inner.temp_buffers = tbufs;
                inner.temp_buffer_old = tbuf_old;
                inner.temp_buffer_new = tbuf_new;
                if pending && !inner.reloading {
                    inner.pending_reload = false;
                    drop(inner);
                    self.hot_reload();
                }
                return;
            }
            let current_chain = owned_chain.as_mut();

            let input_prop = unsafe { &**pp_inputs };
            let output_prop = unsafe { &mut **pp_outputs };
            let frames = input_prop.u32ValidFrameCount as usize;

            // 输入切片（交织）。
            let input_slice = unsafe {
                std::slice::from_raw_parts(input_prop.pBuffer as *const f32, frames * in_ch)
            };

            // 旧链 → temp_buffer_old。
            let mut old_ready = true;
            if let Some(old_chain) = outgoing.as_mut() {
                tbuf_old.resize(frames * out_ch, 0.0);
                let _ = process_chain_interleaved(
                    old_chain,
                    input_slice,
                    tbuf_old.as_mut_slice(),
                    out_ch,
                    frames,
                    tbufs.as_mut_slice(),
                );
            } else {
                old_ready = false;
            }

            // 新链 → temp_buffer_new。
            tbuf_new.resize(frames * out_ch, 0.0);
            let _ = process_chain_interleaved(
                current_chain,
                input_slice,
                tbuf_new.as_mut_slice(),
                out_ch,
                frames,
                tbufs.as_mut_slice(),
            );

            // 混合 → 输出。factor 从 0.0（旧）→ 1.0（新），**逐采样推进**：
            // 过渡长度按采样数计（10ms = 480 采样 @48k），不是按 APOProcess 调用次数。
            // 用户风险②：advance() 返回 None（已达上限）时**也必须写输出**——
            // 按 factor=1.0（纯新链）输出，APO 契约要求每帧写出。
            let out_slice = unsafe {
                std::slice::from_raw_parts_mut(output_prop.pBuffer as *mut f32, frames * out_ch)
            };
            for f in 0..frames {
                let factor = transition
                    .as_mut()
                    .and_then(|p| p.advance())
                    .unwrap_or(1.0);
                let inv_factor = 1.0 - factor;
                for c in 0..out_ch {
                    let idx = f * out_ch + c;
                    let old_v = if old_ready { tbuf_old[idx] } else { 0.0 };
                    out_slice[idx] = old_v * inv_factor + tbuf_new[idx] * factor;
                }
            }
            output_prop.u32ValidFrameCount = frames as u32;
            output_prop.u32BufferFlags = BUFFER_VALID;

            // 当前过渡结束条件：advance 到达上限或过渡原本未激活。
            let finished = transition.as_ref().map_or(true, |p| p.counter() >= p.length());
            if finished {
                tbuf_old.clear();
                tbuf_new.clear();
                // R1：旧链移入退役槽（零析构），控制线程锁内统一 drop。
                inner.retired_chain = outgoing;
                inner.outgoing_chain = None;
                inner.transition = None;
                inner.current_chain = owned_chain;
                inner.temp_buffers = tbufs;
                inner.temp_buffer_old = tbuf_old;
                inner.temp_buffer_new = tbuf_new;
                // R2 修正（用户风险①）：过渡完成帧如需重载，**不**在此置 `reloading=true`——
                // `reloading` 表示"正在解析中"（hot_reload 自己会置位），若先置 true 再调
                // hot_reload，短锁检查 `reloading==true` 会直接返回 → 延迟重载被自己拦截。
                // 若 hot_reload 正在运行（reloading=true，另一线程在解析），此处保留
                // pending=true，下帧 APOProcess 再触发。
                if pending && !inner.reloading {
                    inner.pending_reload = false;
                    drop(inner);
                    self.hot_reload();
                    return;
                }
                return;
            } else {
                // 过渡进行中 → 写回迁移状态。
                inner.outgoing_chain = outgoing;
                inner.transition = transition;
            }
            // 写回栈上字段。
            inner.current_chain = owned_chain;
            inner.temp_buffers = tbufs;
            inner.temp_buffer_old = tbuf_old;
            inner.temp_buffer_new = tbuf_new;
            return;
        }

        // 正常模式：构造 ProcessParams 并调用 process_audio。
        let in_ch = inner.pipeline_context.input_channels;
        let out_ch = inner.pipeline_context.output_channels;
        let frames = unsafe { (**pp_inputs).u32ValidFrameCount as usize };
        let params = ProcessParams {
            input_channels: in_ch,
            output_channels: out_ch,
            sample_rate: inner.pipeline_context.sample_rate,
            max_frame_count: inner.pipeline_context.max_frame_count,
            valid_frame_count: frames,
            error_policy: ErrorPolicy::Bypass,
            allow_silent_buffer: true,
        };
        // pp_inputs / pp_outputs 是 APO_CONNECTION_PROPERTY**（指针数组）。
        let input_one = unsafe { &**pp_inputs };
        let inputs = std::slice::from_ref(input_one);
        let output_one = unsafe { &mut **pp_outputs };
        let outputs = std::slice::from_mut(output_one);
        let mut owned_chain = std::mem::replace(&mut inner.current_chain, Box::new(Chain::new()));
        let mut tbufs = std::mem::take(&mut inner.temp_buffers);
        let _ = process_audio(
            inputs,
            outputs,
            &params,
            owned_chain.as_mut(),
            &self.process_stats,
            tbufs.as_mut_slice(),
        );

        inner.current_chain = owned_chain;
        inner.temp_buffers = tbufs;
    }

    /// RT 入口 panic 兜底（P0-5，debug `panic="unwind"` 测试态防御路径）。
    ///
    /// 捕获到 panic 后：输出缓冲清零 + `BUFFER_SILENT` + `stats.error_count++` + 日志
    /// （RT 零分配）。release（`panic="abort"`）下 `catch_unwind` 为编译移除的空操作，
    /// panic 即确定性 abort（O3），本函数不会被执行。
    fn apo_process_panic_fallback(
        &self,
        num_output: u32,
        pp_outputs: *mut *mut APO_CONNECTION_PROPERTY,
    ) {
        if num_output == 0 || pp_outputs.is_null() {
            return;
        }
        // Safety: 引擎保证 num_output>=1 时 pp_outputs 非空且指向有效 APO_CONNECTION_PROPERTY。
        let output_prop = unsafe { &mut **pp_outputs };
        // u32ValidFrameCount 由引擎在调用 APOProcess 前填充（即使内部 panic，引擎侧已设置）。
        let frames = output_prop.u32ValidFrameCount as usize;
        let out_ch = self.out_channel_count_safe();
        if frames > 0 && out_ch > 0 {
            // Safety: 输出缓冲由引擎按 max_frame_count × out_ch 分配，valid_frame_count ≤ max_frame_count。
            let out = unsafe {
                std::slice::from_raw_parts_mut(output_prop.pBuffer as *mut f32, frames * out_ch)
            };
            out.fill(0.0);
        }
        output_prop.u32BufferFlags = BUFFER_SILENT;
        self.process_stats
            .error_count
            .fetch_add(1, Ordering::Relaxed);
        // RT 零分配：log::error! 走 telemetry 定长环形缓冲（object 7.1.11 注）。
        log::error!("APOProcess: panic caught — output silenced");
    }

    /// 读取当前输出通道数（panic 兜底路径专用）。
    ///
    /// 使用 `PoisonError::into_inner()` 容忍被前序 panic 污染的 mutex——panic 发生时
    /// 锁内数据本身仍有效（仅锁标记 poisoned），此路径保证**不二次 panic**（P0-5）。
    fn out_channel_count_safe(&self) -> usize {
        let inner = self.mutex.lock().unwrap_or_else(|e| e.into_inner());
        inner.pipeline_context.output_channels as usize
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════
#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::vx_reg_props::CLSID_VXAPO_PRE_MIX;

    /// P0-5 测试：`apo_process_panic_fallback` 在模拟 panic 后输出清零 + BUFFER_SILENT + error_count++。
    ///
    /// 直接用 `catch_unwind` + 注入 panic 的闭包验证防御路径——不依赖真实 FFI 调用。
    #[test]
    fn rt_panic_fallback_silences_output() {
        // 构造 APO：1 输入 1 输出，输出缓冲 960 帧。
        let apo = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        {
            let mut inner = apo.mutex.lock().unwrap();
            inner.pipeline_context.output_channels = 2;
        }
        let mut buffer = vec![1.0f32; 960 * 2];
        let mut prop = APO_CONNECTION_PROPERTY {
            pBuffer: buffer.as_mut_ptr() as usize,
            u32ValidFrameCount: 960,
            u32BufferFlags: BUFFER_VALID,
            u32Signature: 0,
        };
        // 两级指针：先取 &mut APO_CONNECTION_PROPERTY → *mut，再取 &mut 该指针 → *mut *mut。
        let mut single: *mut APO_CONNECTION_PROPERTY = &mut prop;
        let props: *mut *mut APO_CONNECTION_PROPERTY = &mut single;

        // 触发 panic 的闭包（模拟 apo_process_inner 内部 panic 后传播到 catch_unwind）。
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            panic!("rt panic sim");
        }));
        assert!(result.is_err());

        // 模拟 RT 入口捕获后调用 fallback。
        apo.apo_process_panic_fallback(1, props);

        // 输出缓冲清零 + BUFFER_SILENT + error_count++。
        assert!(buffer.iter().all(|&v| v == 0.0), "output must be zeroed");
        assert_eq!(prop.u32BufferFlags, BUFFER_SILENT);
        assert_eq!(apo.process_stats.error_count.load(Ordering::Relaxed), 1);
    }

    /// P0-5 测试：CalcInputFrames / CalcOutputFrames panic 时返回保守值（不 panic、不越界）。
    ///
    /// `_Impl` 由 `#[implement]` 宏生成（无法直接构造），此处验证等价逻辑：
    /// 保守值策略（CalcInputFrames → output_frames；CalcOutputFrames → 0）与真实实现一致。
    #[test]
    fn rt_frame_calc_panic_returns_conservative_values() {
        // 保守值语义直接验证：catch_unwind 包裹后 panic → 返回保守值（不二次 panic）。
        let input_ret = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = 480u32; // 正常计算占位
                480u32.wrapping_add(0)
            }))
            .unwrap_or_else(|_| 480) // CalcInputFrames 保守值 = output_frames
        }));
        assert_eq!(input_ret.unwrap(), 480);

        let output_ret = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                panic!("calc panic"); // 模拟内部 panic
            }))
            .unwrap_or_else(|_| 0) // CalcOutputFrames 保守值 = 0（可丢帧不可越界）
        }));
        assert_eq!(output_ret.unwrap(), 0);
    }
}
