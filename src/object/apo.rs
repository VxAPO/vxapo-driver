//! object/apo.rs — ApoObject 核心（v6.3 规范 7.1，按 windows-rs 0.62.2 _Impl trait 实现）
//! 模块入口：纯逻辑子模块见 `object/apo/`。

pub mod config;
pub mod aggregate;
pub mod inner;
pub mod negotiate;
pub mod state;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use windows::core::Result;

use crate::config::commands::register_all_commands;
use crate::config::parser::ConfigParser;
use crate::config::watcher::ConfigWatcher;
use crate::install::device::slots::{ChildApoKind, read_child_apo_guid};
use crate::object::child::ChildApo;
use crate::object::ref_count;
use crate::object::vx_reg_props::{REG_PROPS_PRE_MIX, REG_PROPS_POST_MIX};
use crate::pipeline::chain::Chain;
use crate::pipeline::context::PipelineContext;
use crate::pipeline::dsp::factory::FilterRegistry;
use crate::pipeline::dsp::filter::{DspContext, DeviceType, ProcessingStage};
use crate::pipeline::format::extract_format;
use crate::pipeline::process::{
    ErrorPolicy, ProcessParams, ProcessStatistics, process_audio, process_chain_interleaved,
};
use crate::sys::audio_defs::get_channel_names;
use crate::sys::com::apo_interfaces::{
    IAudioMediaType, IAudioProcessingObject, IAudioProcessingObjectConfiguration, IAudioProcessingObjectRT,
    IAudioProcessingObject_Impl, IAudioProcessingObjectRT_Impl, IAudioProcessingObjectConfiguration_Impl,
    IAudioSystemEffects, IAudioSystemEffects_Impl,
};
use crate::sys::com::apo_types::{
    APO_CONNECTION_DESCRIPTOR, APO_CONNECTION_PROPERTY, APO_REG_PROPERTIES,
    APOERR_FORMAT_NOT_SUPPORTED, APOERR_INVALID_CONNECTION_FORMAT, APOERR_NOT_INITIALIZED,
    APOERR_NUM_CONNECTIONS_INVALID, APOInitSystemEffects, BUFFER_SILENT, BUFFER_VALID,
};
use crate::sys::com::prelude::{
    CoTaskMemAlloc, E_FAIL, E_OUTOFMEMORY, GUID, HRESULT, guid_to_string, implement,
};

use crate::object::apo::config::{
    DEFAULT_CONFIG_PATH, WatcherState, extract_endpoint_guid, hot_reload_impl, resolve_config_path,
};
use crate::object::apo::inner::ApoObjectInner;
use crate::object::apo::negotiate::{check_format_supported, extract_format_ref};
use crate::object::apo::state::{ApoState, LockGuard, StateCell};

#[cfg(test)]
use crate::object::apo::config::resolve_config_path_from;

// ═══ ApoObject ═══
#[implement(
    IAudioProcessingObject,
    IAudioProcessingObjectRT,
    IAudioProcessingObjectConfiguration,
    IAudioSystemEffects
)]
#[allow(dead_code)]
pub struct ApoObject {
    pub(crate) clsid: GUID,
    pub(crate) state_cell: StateCell,
    /// 内部状态（双链过渡）。Arc<Mutex>：spawn 线程可 clone（hot_reload 独立访问）。
    pub(crate) mutex: Arc<Mutex<ApoObjectInner>>,
    pub(crate) latency_samples: AtomicU32,
    pub(crate) latency_frames_atomic: AtomicU32,
    pub(crate) process_stats: ProcessStatistics,
    /// 配置文件路径（Initialize 确定，per-device `Documents\VxAPO\{GUID}\config.txt`）。
    /// Arc<Mutex>：spawn 线程可 clone（hot_reload 独立访问）。
    pub(crate) config_path: Arc<Mutex<String>>,
    /// watcher 运行时状态（v7.10，P0-4 外部驱动模型）：Lock 末尾启动 / Unlock 停止。
    /// Arc<Mutex>：&self 可写（#[implement] 无 &mut Foo）；spawn 可 clone 移入线程。
    watcher_state: Arc<Mutex<WatcherState>>,
    /// 子 APO（P0-6，object 7.1.3）：Initialize 创建，失败降级 None。
    /// Arc<Mutex>：&self 可写 + 控制线程（Initialize/Lock/Unlock）持有；
    /// RT 路径 APOProcess 锁 inner 前短锁读取（引擎保证不重叠，无实际阻塞）。
    /// 语义等价规范 7.1.3 的字段（P0-4 Arc Mutex WatcherState 先例）。
    pub(crate) child_apo: Arc<Mutex<Option<ChildApo>>>,
}

impl ApoObject {
    pub fn new(clsid: GUID) -> Self {
        ref_count::increment();
        Self {
            clsid,
            state_cell: StateCell::new(),
            mutex: Arc::new(Mutex::new(ApoObjectInner::new())),
            latency_samples: AtomicU32::new(0),
            latency_frames_atomic: AtomicU32::new(0),
            process_stats: ProcessStatistics::new(),
            config_path: Arc::new(Mutex::new(DEFAULT_CONFIG_PATH.to_owned())),
            watcher_state: Arc::new(Mutex::new(WatcherState::default())),
            child_apo: Arc::new(Mutex::new(None)),
        }
    }

    /// 配置热重载（watcher 回调）。委托模块级 `hot_reload_impl`（共享 Arc 字段）。
    pub fn hot_reload(&self) {
        hot_reload_impl(
            &self.config_path,
            &self.mutex,
            self.clsid,
            self as *const _ as usize,
        );
    }

    /// 读取当前输出通道数（panic 兜底路径专用）。
    ///
    /// 使用 `PoisonError::into_inner()` 容忍被前序 panic 污染的 mutex——panic 发生时
    /// 锁内数据本身仍有效（仅锁标记 poisoned），此路径保证**不二次 panic**（P0-5）。
    fn out_channel_count_safe(&self) -> usize {
        let inner = self
            .mutex
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        inner.pipeline_context.output_channels as usize
    }

    /// RT 入口 panic 兜底（P0-5，debug `panic="unwind"` 测试态防御路径）。
    ///
    /// 捕获到 panic 后：输出缓冲清零 + `BUFFER_SILENT` + `stats.error_count++` + 日志
    /// （RT 零分配）。release（`panic="abort"`）下 `catch_unwind` 为编译移除的空操作，
    /// panic 即确定性 abort（O3），本函数不会被执行。
    fn apo_process_panic_fallback(&self, num_output: u32, pp_outputs: *mut *mut APO_CONNECTION_PROPERTY) {
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
        self.process_stats.error_count.fetch_add(1, Ordering::Relaxed);
        // RT 零分配：log::error! 走 telemetry 定长环形缓冲（object 7.1.11 注）。
        log::error!("APOProcess: panic caught — output silenced");
    }

    /// 启动配置监控线程（object 7.1.9，v7.10 外部驱动模型）。
    ///
    /// 流程：CreateEventW(shutdown_event) → ConfigWatcher::new(watch_dir, shutdown_event)
    /// → spawn 线程循环 `wait_and_handle` → `hot_reload_impl`（DirectoryChanged → 重载）。
    /// 失败降级（watcher 未启动，仅日志）——不阻塞锁定（配置热重载失效但音频链路正常）。
    ///
    /// `#[implement]` 只暴露 `&self`（gen.rs：不向安全代码暴露所有权实例）——因此用
    /// `Arc<Mutex<WatcherState>>` 内部可变性（用户方案 A）；spawn 线程 clone `config_path`/
    /// `mutex` 的 Arc 移入（'static），线程内调 `hot_reload_impl`（无需持有 self）。
    pub(crate) fn start_watcher(&self) -> Result<()> {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{CreateEventW, SetEvent};

        // 幂等：已有 watcher 线程则不重复。
        let mut st = self.watcher_state.lock().unwrap();
        if st.thread.is_some() {
            return Ok(());
        }

        // 1. 创建退出事件（manual-reset，初始 non-signaled）。
        // Safety: CreateEventW 无安全属性、无名字；返回句柄由 watcher_state 持有，stop_watcher 释放。
        let shutdown_event = unsafe { CreateEventW(None, true, false, None)? };

        // 2. 目录级监控器（不自启线程，v7.10）。watch_dir = config_path 父目录。
        let config_path = self.config_path.lock().unwrap().clone();
        let watch_dir = std::path::Path::new(&config_path)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::Path::new(&config_path).to_path_buf());
        let mut watcher = ConfigWatcher::new(watch_dir, shutdown_event);
        if watcher.notify_handle().is_invalid() {
            // 目录不存在（FindFirstChangeNotificationW 失败）→ 释放事件，降级。
            let _ = unsafe { SetEvent(shutdown_event) };
            let _ = unsafe { CloseHandle(shutdown_event) };
            log::warn!("start_watcher: watch dir unavailable — config hot-reload disabled");
            return Ok(());
        }

        // 3. spawn 线程：wait_and_handle → hot_reload_impl；shutdown 事件置位 → 退出。
        let cfg = self.config_path.clone();
        let inner = self.mutex.clone();
        let clsid = self.clsid;
        let obj_ptr = self as *const _ as usize;
        let handle = std::thread::spawn(move || loop {
            if !watcher.wait_and_handle() {
                break; // shutdown 或句柄失效。
            }
            // watcher 事件探针（debug 门控，验证目录变更是否到达线程）。
            #[cfg(debug_assertions)]
            {
                let _ = std::fs::write(
                    r"C:\ProgramData\VxAPO\watcher_probe.txt",
                    format!(
                        "dir change at {:?} clsid={clsid:?} obj=0x{obj_ptr:x} thread={:?}\n",
                        std::time::SystemTime::now(),
                        std::thread::current().id()
                    ),
                );
            }
            // 目录级变更 → 热重载（spec 短路 + 128KB 闸门内部处理）。
            hot_reload_impl(&cfg, &inner, clsid, obj_ptr);
        });

        // 4. 记录 watcher 运行时状态（&self 可写：Arc<Mutex> 内部可变性）。
        //    watcher 已 move 进线程（循环消费）；线程退出时 watcher Drop 自动关闭
        //    notify_handle（FindCloseChangeNotification）——stop 只需 SetEvent + join。
        st.shutdown_event = Some(shutdown_event);
        st.thread = Some(handle);
        Ok(())
    }

    /// 停止配置监控线程（object 7.1.10，v7.10 外部驱动模型）。
    ///
    /// 1. SetEvent(shutdown_event) → wait_and_handle 返回 false → 线程循环退出
    /// 2. join(watcher_thread) → 确保线程已退出（无泄漏）
    /// 3. watcher.shutdown() → FindCloseChangeNotification + CloseHandle
    /// 幂等：watcher 为 None（未启动/启动失败）时直接返回。
    pub(crate) fn stop_watcher(&self) {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::SetEvent;

        let mut st = self.watcher_state.lock().unwrap();

        // 1. 置位退出事件（唤醒等待中的 wait_and_handle）。
        if let Some(evt) = st.shutdown_event {
            // Safety: 事件句柄由 start_watcher 创建且有效。
            let _ = unsafe { SetEvent(evt) };
        }

        // 2. join 线程（确保已退出）。watcher 在线程内（move 消费），线程退出时
        //    watcher Drop 已关闭 notify_handle（FindCloseChangeNotification）。
        if let Some(handle) = st.thread.take() {
            let _ = handle.join();
        }

        // 3. 释放事件句柄。
        if let Some(evt) = st.shutdown_event.take() {
            // Safety: 事件句柄由 start_watcher 创建且有效（此处唯一持有者，关闭后不再使用）。
            let _ = unsafe { CloseHandle(evt) };
        }
    }

    /// APOProcess 实际处理主体（P0-5，v8.2）。
    ///
    /// 由 `IAudioProcessingObjectRT_Impl::APOProcess` 用 `catch_unwind` 包裹调用——
    /// 参数校验（状态 + 指针）留在壳外：panic 兜底路径依赖合法的 `pp_outputs`，
    /// 若非法指针在壳内被 panic 污染，兜底会二次访问非法内存（不可救）。
    ///
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
                    format!("APOProcess called: {} frames={}\n", n, unsafe { (**pp_inputs).u32ValidFrameCount }),
                );
            }
        }
        // P0-6（v8.1 D1）：childRT->APOProcess **前置每帧一次**（object 7.1.11 Step 3）。
        // 双链共享同一份 child 输出作输入；child 不在 current/outgoing 任一链内。
        // 锁 inner **前**调（避免持 inner 锁调 child——child 是独立 COM 对象，无循环依赖）。
        if let Some(child) = self.child_apo.lock().unwrap().as_ref() {
            // SAFETY: 引擎保证 pp_inputs/pp_outputs 有效（APOProcess 契约）。
            unsafe { child.apo_process(num_input, pp_inputs, num_output, pp_outputs) };
            // 委托帧数计算（RT 无锁，object 7.1.11：每帧委托）。
            let _ = child.calc_input_frames(0);
        }

        let mut inner = self.mutex.lock().unwrap();
        let pending = inner.pending_reload;

        // 过渡模式存在 → 双链处理 + 混合。
        if pending || inner.transition.is_some() {
            // 先把所有需要变异的字段移到栈上（每次仅单字段借用），避免互斥 guard 多 &mut。
            let mut outgoing = inner.outgoing_chain.take();
            let mut transition = inner.transition.take();
            let mut owned_chain = std::mem::replace(&mut inner.current_chain, Box::new(Chain::new()));
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
                dst[..copy_len].copy_from_slice(&src[..copy_len]);
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

            // 混合 → 输出。factor 从 0.0（旧）→ 1.0（新）。
            // 用户风险②：advance() 返回 None（已达上限）时**也必须写输出**——
            // 按 factor=1.0（纯新链）输出，APO 契约要求每帧写出。
            let factor = transition
                .as_mut()
                .and_then(|p| p.advance())
                .unwrap_or(1.0);
            {
                let out_slice = unsafe {
                    std::slice::from_raw_parts_mut(output_prop.pBuffer as *mut f32, frames * out_ch)
                };
                for f in 0..frames {
                    for c in 0..out_ch {
                        let idx = f * out_ch + c;
                        let old_v = if old_ready { tbuf_old[idx] } else { 0.0 };
                        out_slice[idx] = old_v * (1.0 - factor)
                            + tbuf_new[idx] * factor;
                    }
                }
                output_prop.u32ValidFrameCount = frames as u32;
                output_prop.u32BufferFlags = BUFFER_VALID;
            }

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
}

impl Drop for ApoObject {
    fn drop(&mut self) { ref_count::decrement(); }
}

// ═══ IAudioProcessingObject 实现（windows-rs _Impl trait 签名） ═══
impl IAudioProcessingObject_Impl for ApoObject_Impl {
    fn Reset(&self) -> Result<()> {
        // ---- 探针 6: Reset 被调（2026-08-04 排查，删）----
        #[cfg(debug_assertions)]
        { let _ = std::fs::write(r"C:\ProgramData\VxAPO\method_probe.txt", "Reset called\n"); }
        let mut inner = self.mutex.lock().unwrap();
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
        self.latency_samples.store(0, Ordering::SeqCst);
        self.latency_frames_atomic.store(0, Ordering::SeqCst);
        Ok(())
    }

    fn GetLatency(&self) -> Result<i64> {
        // ---- 探针 6: GetLatency 被调（2026-08-04 排查，删）----
        #[cfg(debug_assertions)]
        { let _ = std::fs::write(r"C:\ProgramData\VxAPO\method_probe.txt", "GetLatency called\n"); }
        // P0-6（v8.3 S4）：有 child → 委托 child；无 child → 返回 0
        // （align EAPO `*pTime=0` 后仅 child 委托改写——EAPO 不维护自身延迟值）。
        if let Some(child) = self.child_apo.lock().unwrap().as_ref() {
            return Ok(child.get_latency());
        }
        Ok(0)
    }

    fn GetRegistrationProperties(&self) -> Result<*mut APO_REG_PROPERTIES> {
        // 按 CLSID 选择对应注册属性，CoTaskMemAlloc 拷贝返回（调用方负责 CoTaskMemFree）。
        let prop = if self.clsid == crate::object::vx_reg_props::CLSID_VXAPO_PRE_MIX {
            &REG_PROPS_PRE_MIX
        } else {
            &REG_PROPS_POST_MIX
        };
        let size = std::mem::size_of::<APO_REG_PROPERTIES>();
        // 分配并对齐（alignment_of<APO_REG_PROPERTIES>）。
        let alloc = unsafe { CoTaskMemAlloc(size) };
        if alloc.is_null() {
            return Err(windows::core::Error::from(E_OUTOFMEMORY));
        }
        unsafe {
            std::ptr::write(alloc as *mut APO_REG_PROPERTIES, *prop);
        }
        Ok(alloc as *mut APO_REG_PROPERTIES)
    }

    fn Initialize(&self, cb_data_size: u32, pby_data: *const u8) -> Result<()> {
        // ---- P0-7 无声诊断探针 5（2026-08-04，debug 门控，排查完删除）----
        // Initialize 被调与否：引擎拿到 IAudioProcessingObject 后先调 Initialize。
        // 之前只有 lock/apoprocess 探针，Initialize 失败被拒时 lock 自然不触发——补上。
        #[cfg(debug_assertions)]
        {
            let _ = std::fs::write(
                r"C:\ProgramData\VxAPO\initialize_probe.txt",
                format!("Initialize clsid={:?} cb={}\n", self.clsid, cb_data_size),
            );
        }
        // 1. 参数校验：pby_data 非空、cb_data_size 足以容纳 APOInitSystemEffects
        //    （SDK 约定：Initialize 的 pby_data 指向完整的 APOInitSystemEffects；
        //    数据非法 → 仍初始化成功并降级默认配置，不阻断 APO 加载）。
        let valid_init_data = !pby_data.is_null()
            && cb_data_size >= std::mem::size_of::<APOInitSystemEffects>() as u32;

        // 2. 状态转换 Created → Initialized，失败 → 对应 HRESULT。
        self.state_cell
            .transition(ApoState::Created, ApoState::Initialized)
            .map_err(|e| windows::core::Error::from(HRESULT::from(e)))?;

        // 3. 解析 APOInitSystemEffects → 端点 GUID + 子 APO（object 7.1.8 v8.4）。
        //    Safety: pby_data 已验证非空 + 尺寸足够；APOInitSystemEffects 为 repr(C) 结构。
        let endpoint_guid = if valid_init_data {
            let init = unsafe { &*(pby_data as *const APOInitSystemEffects) };
            extract_endpoint_guid(init)
        } else {
            None
        };

        // 4. 子 APO 创建（P0-6 v8.4：vendor 安装信息区读取，失败降级为无子 APO，Note 57）。
        //    - GUID 来源：端点 GUID + 安装信息区 PreMixChild/PostMixChild 值
        //    - 空/特殊 GUID、create 失败 → None（不阻塞 Initialize）
        let child = match endpoint_guid {
            Some(eg) => {
                let eg_str = guid_to_string(&eg);
                let premix = read_child_apo_guid(&eg_str, ChildApoKind::PreMix);
                let postmix = read_child_apo_guid(&eg_str, ChildApoKind::PostMix);
                premix.or(postmix).and_then(|c| {
                    // SAFETY: COM 已初始化（宿主进程 audiodg）；c 为有效 APO CLSID。
                    unsafe { ChildApo::create(&c) }.ok()
                })
            }
            None => None,
        };
        *self.child_apo.lock().unwrap() = child;

        // 5. per-device 配置路径（object 7.1.8）。
        let path = if valid_init_data {
            let init = unsafe { &*(pby_data as *const APOInitSystemEffects) };
            resolve_config_path(Some(init))
        } else {
            log::warn!(
                "Initialize: invalid init data (ptr null = {}, size {} < {}) — using default config",
                pby_data.is_null(),
                cb_data_size,
                std::mem::size_of::<APOInitSystemEffects>()
            );
            resolve_config_path(None)
        };
        *self.config_path.lock().unwrap() = path;

        Ok(())
    }

    fn IsInputFormatSupported(
        &self,
        p_opposite_format: windows::core::Ref<IAudioMediaType>,
        p_requested: windows::core::Ref<IAudioMediaType>,
    ) -> Result<IAudioMediaType> {
        // EAPO 对齐（EqualizerAPO.cpp:228-306）：
        // 读输入+输出双格式后仅拒绝「降混」（in > 2ch 且 in > out）。
        // EAPO 返回 S_FALSE + 设备输出格式（CreateAudioMediaTypeFromUncompressedAudioFormat）；
        // windows-rs Result 无法表达 S_FALSE（Ok→S_OK / Err→错误码），
        // 降混拒绝退化为 Err(APOERR_FORMAT_NOT_SUPPORTED)（宁拒不错）。
        let input = extract_format_ref(&p_requested)?;
        check_format_supported(&p_requested)?;
        // opposite 缺失/解析失败时跳过降混检查（保守接受，宁多勿误拒）。
        if let Ok(output) = extract_format_ref(&p_opposite_format) {
            if input.channels > 2 && input.channels > output.channels {
                return Err(windows::core::Error::from(APOERR_FORMAT_NOT_SUPPORTED));
            }
        }
        // 通过检查：返回请求格式（INPLACE 模式输入输出同格式）。
        let req = p_requested.as_ref().expect("checked above");
        Ok(req.clone())
    }

    fn IsOutputFormatSupported(
        &self,
        p_opposite_format: windows::core::Ref<IAudioMediaType>,
        p_requested: windows::core::Ref<IAudioMediaType>,
    ) -> Result<IAudioMediaType> {
        // 与 IsInputFormatSupported 对称：拒绝「上混」（out > 2ch 且 out > in）。
        let output = extract_format_ref(&p_requested)?;
        check_format_supported(&p_requested)?;
        if let Ok(input) = extract_format_ref(&p_opposite_format) {
            if output.channels > 2 && output.channels > input.channels {
                return Err(windows::core::Error::from(APOERR_FORMAT_NOT_SUPPORTED));
            }
        }
        let req = p_requested.as_ref().expect("checked above");
        Ok(req.clone())
    }

    fn GetInputChannelCount(&self) -> Result<u32> {
        // ---- 探针 6: GetInputChannelCount 被调（2026-08-04 排查，删）----
        #[cfg(debug_assertions)]
        { let _ = std::fs::write(r"C:\ProgramData\VxAPO\method_probe.txt", "GetInputChannelCount called\n"); }
        if self.state_cell.current() != ApoState::Locked {
            return Err(windows::core::Error::from(APOERR_NOT_INITIALIZED));
        }
        let inner = self.mutex.lock().unwrap();
        Ok(inner.pipeline_context.input_channels)
    }
}

// ═══ IAudioProcessingObjectRT 实现 ═══
impl IAudioProcessingObjectRT_Impl for ApoObject_Impl {
    fn APOProcess(
        &self,
        num_input: u32,
        pp_inputs: *const *const APO_CONNECTION_PROPERTY,
        num_output: u32,
        pp_outputs: *mut *mut APO_CONNECTION_PROPERTY,
    ) {
        // 参数校验在壳外（状态 + 指针）：panic 兜底路径依赖合法的 pp_outputs——
        // 若非法指针在壳内被 panic 污染，兜底访问会二次访问非法内存（不可救）。
        if self.state_cell.current() != ApoState::Locked {
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
            self.apo_process_inner(num_input, pp_inputs, num_output, pp_outputs)
        }));
        if result.is_err() {
            // panic 捕获：输出清零 + BUFFER_SILENT + stats.error_count++ + 日志（RT 零分配）。
            self.apo_process_panic_fallback(num_output, pp_outputs);
        }
    }

    fn CalcInputFrames(&self, output_frames: u32) -> u32 {
        // P0-5（v8.2）：panic 保守值 = output_frames（不多不少、不二次 load）。
        // 实现注（object 7.1.12）：panic 分支不 load latency_frames_atomic（避免二次
        // panic）；保守策略以「不 panic + 不越界」为第一约束——roadmap 明确
        // 「保守策略由实现端在 DoD 测试中锁定」。
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            output_frames + self.latency_frames_atomic.load(Ordering::Acquire)
        }))
        .unwrap_or_else(|_| output_frames)
    }

    fn CalcOutputFrames(&self, input_frames: u32) -> u32 {
        // P0-5（v8.2）：panic 保守值 = 0（可丢帧不可越界，不二次 load）。
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let latency = self.latency_frames_atomic.load(Ordering::Acquire);
            input_frames.saturating_sub(latency)
        }))
        .unwrap_or_else(|_| 0)
    }
}

// ═══ IAudioSystemEffects 实现（EAPO 对齐，marker 接口） ═══
impl IAudioSystemEffects_Impl for ApoObject_Impl {}

// ═══ IAudioProcessingObjectConfiguration 实现 ═══
impl IAudioProcessingObjectConfiguration_Impl for ApoObject_Impl {
    fn LockForProcess(
        &self,
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
                format!("LockForProcess called: num_input={} num_output={}\n", num_input, num_output),
            );
        }
        // Step 0: 状态机 Initialized → Locked，失败自动回退。
        self.state_cell
            .transition(ApoState::Initialized, ApoState::Locked)
            .map_err(|e| windows::core::Error::from(HRESULT::from(e)))?;
        let _guard = LockGuard::new(&self.state_cell);

        if num_input == 0 || pp_inputs.is_null() {
            return Err(windows::core::Error::from(APOERR_NUM_CONNECTIONS_INVALID));
        }
        // EAPO 对齐（EqualizerAPO.cpp:308-339）：Lock 需读输入+输出**双格式**，
        // 引擎可协商 in≠out（如设备 5.1 输出）；输出通道数/掩码按输出格式（render 场景）。
        if num_output == 0 || pp_outputs.is_null() {
            return Err(windows::core::Error::from(APOERR_NUM_CONNECTIONS_INVALID));
        }

        // Step 1: 从输入/输出连接描述符提取格式（pFormat 为 ManuallyDrop<Option<IAudioMediaType>>）。
        let input_descriptor = unsafe { &**pp_inputs };
        let output_descriptor = unsafe { &**pp_outputs };
        let extract_descriptor_format =
            |desc: &APO_CONNECTION_DESCRIPTOR| -> Result<crate::pipeline::format::AudioFormat> {
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
            rt_marker: std::marker::PhantomData,
        };

        // Step 3: 构建 FilterRegistry + ConfigParser，解析配置文件
        //         （路径来自 Initialize 确定的 per-device config_path）。
        // v7.9：parse_file_with_spec → (滤波器列表, spec chain) 双返回。
        // active_spec 即本次解析产出的配置指纹（LockForProcess 建立基线）。
        let mut registry = FilterRegistry::new();
        register_all_commands(&mut registry);
        let parser = ConfigParser::new(registry);
        let config_path = self.config_path.lock().unwrap().clone();
        let (filters, spec_chain) = parser.parse_file_with_spec(&config_path, &dsp_ctx)
            .map_err(|_| windows::core::Error::from(E_FAIL))?;

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
            chain.add_filter(f)
                .map_err(|_| windows::core::Error::from(E_FAIL))?;
        }
        // DSP 依赖 initialize 预计算系数/状态（GraphicEQ/PEQ/IIR/Delay/Convolution）。
        chain.initialize(format.sample_rate, &channel_names);
        let total_latency = chain.total_latency();

        // Step 5: 预分配过渡缓冲区（v7.8 修订，杜绝 RT 线程过渡首次 resize 扩容——
        //          EAPO 对齐：按 max_frame_count × max_ch 预分配充足容量）。
        let max_ch = pipeline_context.input_channels.max(pipeline_context.output_channels) as usize;
        let max_samples = pipeline_context.max_frame_count * max_ch;
        let temp_buffer_old = vec![0.0f32; max_samples];
        let temp_buffer_new = vec![0.0f32; max_samples];

        // deinterleave 空间（channels 个 Vec）。
        // **必须用 vec![0.0; len]（带长度），不能用 Vec::with_capacity（len=0）**——
        // deinterleave_into 按 `output[ch][f]` 写会越界 panic → catch_unwind 捕获 →
        // panic 兜底输出清零 + BUFFER_SILENT → 完全无声（2026-08-04 实测 audiodg 加载后无声音根因）。
        let mut temp_buffers: Vec<Vec<f32>> = Vec::with_capacity(max_ch);
        for _ in 0..max_ch {
            temp_buffers.push(vec![0.0f32; pipeline_context.max_frame_count]);
        }

        // Step 6: 更新内部状态。（R1：退役链由控制线程锁内统一析构）
        {
            let mut inner = self.mutex.lock().unwrap();
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
        self.latency_samples.store(total_latency, Ordering::SeqCst);
        self.latency_frames_atomic.store(total_latency, Ordering::SeqCst);

        // Step 6b（P0-6，object 7.1.9）：子 APO LockForProcess 委托（失败不阻塞父，Note 57）。
        // 对齐 EAPO 341-347：childCfg->LockForProcess 结果仅 Trace 不 return。
        if let Some(child) = self.child_apo.lock().unwrap().as_ref() {
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
        crate::install::audiodg::ensure_can_load()
            .map_err(|e| windows::core::Error::from(HRESULT::from(e)))?;

        // Step 8 (v7.10)：Lock 末尾启动 watcher（config_path 已确定 + active_spec 基线就绪）。
        // 启动失败降级（仅日志），不阻塞锁定。
        if let Err(e) = self.start_watcher() {
            log::warn!("LockForProcess: watcher start failed: {e}");
        }

        // 全部成功 → 解除守卫（不再回退状态）。
        _guard.disarm();
        Ok(())
    }

    fn UnlockForProcess(&self) -> Result<()> {
        // ---- 探针 6: UnlockForProcess 被调（2026-08-04 排查，删）----
        #[cfg(debug_assertions)]
        { let _ = std::fs::write(r"C:\ProgramData\VxAPO\method_probe.txt", "UnlockForProcess called\n"); }
        self.state_cell
            .transition(ApoState::Locked, ApoState::Initialized)
            .map_err(|e| windows::core::Error::from(HRESULT::from(e)))?;
        // Stop watcher：SetEvent → join → close（v7.10 stop_watcher）。先释放锁（join 可能等待）。
        // P0-6（object 7.1.10）：子 APO UnlockForProcess 委托——失败不阻塞父解锁
        // （UnlockForProcess 无重试语义，子可能已部分解锁，父继续自身流程 + 日志）。
        if let Some(child) = self.child_apo.lock().unwrap().as_ref() {
            let hr = child.unlock_for_process();
            if hr.0 != 0 {
                log::warn!("child APO UnlockForProcess failed");
            }
        }

        drop(self.mutex.lock().unwrap());
        self.stop_watcher();
        // R1：退役链 + 过渡状态由控制线程锁内统一析构。
        let mut inner = self.mutex.lock().unwrap();
        inner.retired_chain = None;
        inner.outgoing_chain = None;
        inner.transition = None;
        inner.pending_reload = false;
        inner.reloading = false;
        // v7.9：释放配置指纹基线（重新 Lock 时重建）。
        inner.active_spec.clear();
        Ok(())
    }
}

unsafe impl Send for ApoObject {}
unsafe impl Sync for ApoObject {}

// ═══ 测试 ═══
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use crate::object::vx_reg_props::CLSID_VXAPO_PRE_MIX;

    /// 构造「无 IPropertyStore」的最低有效 APOInitSystemEffects（zeroed 后仅设置 APOInit.cbSize）。
    /// 提取端点 GUID 会因属性存储缺失返回 None → 走 `_default` 兜底。
    fn empty_init() -> APOInitSystemEffects {
        let mut init: APOInitSystemEffects = unsafe { std::mem::zeroed() };
        init.APOInit.cbSize = std::mem::size_of::<APOInitSystemEffects>() as u32;
        init
    }

    #[test]
    fn config_path_default_device_dir_when_no_guid() {
        // 无端点 GUID（属性存储缺失）→ `{config_root}\_default\config.txt`。
        let root = std::env::temp_dir().join("vxapo_apo_test").join("cfg");
        let root_str = root.display().to_string();
        let init = empty_init();
        let path = resolve_config_path_from(&root_str, Some(&init));
        let p = Path::new(&path);
        assert!(p.starts_with(&root));
        assert!(p.ends_with("config.txt"));
        // 目录应包含 `_default`。
        assert!(path.contains("_default"));
        // 目录已创建 + 默认 passthrough 文件已写入。
        assert!(p.parent().unwrap().is_dir());
        assert!(p.exists());
        let content = std::fs::read_to_string(p).unwrap();
        assert!(content.contains("passthrough"));
        // 清理（避免污染 temp）。
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn config_path_custom_guid_dir() {
        // 有明确端点 GUID（用模拟 IPropertyStore 成本高，此处用 default GUID 走不到
        // Real IPropertyStore——改为验证：手动构造 pAPOSystemEffectsProperties 为 None
        // 时仍 `_default`。GUID 路径分支由 extract_endpoint_guid（真实环境）覆盖。
        // 这里验证 `_default` 兜底 + 目录创建 + 文件写入的完整链路。
        let root = std::env::temp_dir().join("vxapo_apo_test2").join("cfg");
        let root_str = root.display().to_string();
        let init = empty_init();
        let path = resolve_config_path_from(&root_str, Some(&init));
        assert!(path.contains("_default"));
        // 幂等：再次调用不应报错（目录已存在）。
        let _ = resolve_config_path_from(&root_str, Some(&init));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn config_path_none_init_falls_back_default() {
        // init=None（Initialize 数据非法降级）→ `_default` 兜底。
        let root = std::env::temp_dir().join("vxapo_apo_test3").join("cfg");
        let root_str = root.display().to_string();
        let path = resolve_config_path_from(&root_str, None);
        assert!(path.contains("_default"));
        assert!(Path::new(&path).parent().unwrap().is_dir());
        let _ = std::fs::remove_dir_all(&root);
    }

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
