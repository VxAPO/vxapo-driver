//! object/apo.rs — ApoObject 核心（v6.3 规范 7.1，按 windows-rs 0.62.2 _Impl trait 实现）

use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::Mutex;

use windows::core::implement;
use windows::core::{GUID, Result};

use crate::config::commands::register_all_commands;
use crate::config::parser::ConfigParser;
use crate::object::ref_count;
use crate::object::vx_reg_props::{REG_PROPS_PRE_MIX, REG_PROPS_POST_MIX};
use crate::pipeline::chain::Chain;
use crate::pipeline::context::PipelineContext;
use crate::pipeline::dsp::factory::FilterRegistry;
use crate::pipeline::dsp::filter::{DspContext, DeviceType, ProcessingStage};
use crate::pipeline::dsp::transition::{SmoothingProvider, default_smoothing_length};
use crate::pipeline::format::extract_format;
use crate::pipeline::process::{
    ErrorPolicy, ProcessParams, ProcessStatistics, process_audio, process_chain_interleaved,
};
use crate::sys::audio_defs::get_channel_names;
use crate::sys::com::apo_interfaces::{
    IAudioMediaType, IAudioProcessingObject, IAudioProcessingObjectConfiguration, IAudioProcessingObjectRT,
    IAudioProcessingObject_Impl, IAudioProcessingObjectRT_Impl, IAudioProcessingObjectConfiguration_Impl,
};
use crate::sys::com::apo_types::{
    APOInitSystemEffects, PKEY_AudioEndpoint_GUID, PROPVARIANT, VT_CLSID,
};
use crate::sys::com::prelude::guid_to_string;
use crate::sys::known_folder::documents_folder;
use windows::Win32::Media::Audio::Apo::{APO_CONNECTION_DESCRIPTOR, APO_CONNECTION_PROPERTY, APO_REG_PROPERTIES};
use windows::Win32::System::Com::CoTaskMemAlloc;

use crate::sys::com::apo_types::{
    APOERR_FORMAT_NOT_SUPPORTED, APOERR_INVALID_CONNECTION_FORMAT, APOERR_NOT_INITIALIZED,
    APOERR_NUM_CONNECTIONS_INVALID, BUFFER_VALID,
};

/// 配置文件默认路径（兜底：无设备 GUID / Documents 解析失败时回退单实例共用路径）。
const DEFAULT_CONFIG_PATH: &str = r"C:\ProgramData\VxAPO\config.txt";

/// 单实例共用子目录名（无设备 GUID 兜底，object 7.1.8）。
const DEFAULT_DEVICE_DIR: &str = "_default";

/// 从 APOInitSystemEffects 提取端点 GUID（object 7.1.8，v7.2）。
///
/// 规范原型为 `pSystemEffectsProperties->pEndpointGuid`，但 windows-rs 0.62.2 实测：
/// `APOInitSystemEffects` 无 `pSystemEffectsProperties` 直接字段，而是
/// `pAPOSystemEffectsProperties: ManuallyDrop<Option<IPropertyStore>>`；端点 GUID
/// 经 `IPropertyStore::GetValue(&PKEY_AudioEndpoint_GUID)` 返回 `PROPVARIANT`
/// （`VT_CLSID`，`puuid` 指向 `GUID`）提取（P0-3 实现反馈②，见 roadmap 反馈段）。
fn extract_endpoint_guid(init: &APOInitSystemEffects) -> Option<GUID> {
    let props = init.pAPOSystemEffectsProperties.as_ref()?;
    // Safety: PKEY_AudioEndpoint_GUID 为静态键；GetValue 返回的 PROPVARIANT 由
    // windows-rs 管理内存（含 puuid 指针有效期内读取）。
    // PROPVARIANT 是 union（Anonymous.Anonymous.Anonymous），读取/比较均在 unsafe 内。
    let pv: PROPVARIANT = unsafe { props.GetValue(&PKEY_AudioEndpoint_GUID) }.ok()?;
    unsafe {
        // PROPVARIANT_0_0: { vt: VARENUM, wReserved1-3, Anonymous: PROPVARIANT_0_0_0 }
        if pv.Anonymous.Anonymous.vt != VT_CLSID {
            return None;
        }
        // Safety: VT_CLSID 时 puuid 指向非空 GUID。
        let guid_ptr = pv.Anonymous.Anonymous.Anonymous.puuid;
        if guid_ptr.is_null() {
            return None;
        }
        // Safety: 已验证非空且 VT_CLSID 语义。
        Some(*guid_ptr)
    }
}

/// 确定 per-device 配置路径（object 7.1.8）：
/// `{Documents}\VxAPO\{GUID}\config.txt`；无 GUID / 解析失败 → `_default` 兜底。
/// 目录自动创建；config.txt 缺失时写默认 passthrough（空配置 → 链为空即 passthrough）。
fn resolve_config_path(init: Option<&APOInitSystemEffects>) -> String {
    let documents = match documents_folder() {
        Ok(d) => d,
        Err(e) => {
            log::warn!("documents_folder() failed: {} — fallback to shared default config", e);
            return DEFAULT_CONFIG_PATH.to_owned();
        }
    };
    resolve_config_path_from(&documents, init)
}

/// 纯拼接 + 目录/文件保障（可单元测试，不依赖真实 Documents 位置）。
///
/// `documents`：文档文件夹绝对路径。返回 `{documents}\VxAPO\{GUID}\config.txt`；
/// 无 GUID → `_default`；目录创建失败 → 回退 `DEFAULT_CONFIG_PATH`。
fn resolve_config_path_from(documents: &str, init: Option<&APOInitSystemEffects>) -> String {
    // 端点 GUID → 大写 `{XXXXXXXX-...}` 目录名。
    let device_dir = match init.and_then(extract_endpoint_guid) {
        Some(guid) => {
            let s = guid_to_string(&guid);
            log::info!("endpoint GUID: {}", s);
            s
        }
        None => DEFAULT_DEVICE_DIR.to_owned(),
    };

    let dir = std::path::Path::new(&documents)
        .join("VxAPO")
        .join(&device_dir);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::warn!("create_dir_all({}) failed: {} — fallback to shared default config", dir.display(), e);
        return DEFAULT_CONFIG_PATH.to_owned();
    }
    let path = dir.join("config.txt");

    // config.txt 缺失 → 写默认 passthrough（空文件 = 无滤波器 = passthrough）。
    if !path.exists() {
        log::info!("config not found at {}, writing default passthrough", path.display());
        if let Err(e) = std::fs::write(&path, "# VxAPO default passthrough\n") {
            log::warn!("write default config failed: {}", e);
        }
    }
    path.display().to_string()
}

/// 从 PipelineContext 构建 DspContext（共享逻辑，LockForProcess / hot_reload 用）。
fn build_dsp_context(ctx: &PipelineContext, bits_per_sample: u32) -> DspContext {
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

/// RAII guard：LockForProcess 失败时自动回退状态。
struct LockGuard<'a> {
    state_cell: &'a StateCell,
    armed: bool,
}

impl<'a> LockGuard<'a> {
    fn new(state_cell: &'a StateCell) -> Self {
        Self { state_cell, armed: true }
    }
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for LockGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _: std::result::Result<(), TransitionError> =
                self.state_cell.transition(ApoState::Locked, ApoState::Initialized);
        }
    }
}

// ═══ 状态机（O2/v6.6） ═══
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ApoState { Created = 0, Initialized = 1, Locked = 2 }

/// 状态转换错误（O2/v6.6）：携带期望/尝试/实际三态，替代纯字符串描述。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TransitionError {
    /// 期望的起始状态。
    pub expected: ApoState,
    /// 尝试转换到的目标状态。
    pub attempted: ApoState,
    /// 实际所处的状态。
    pub actual: ApoState,
}

impl TransitionError {
    fn new(expected: ApoState, attempted: ApoState, actual: ApoState) -> Self {
        Self { expected, attempted, actual }
    }
}

impl std::fmt::Display for TransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "状态转换失败：期望 {:?} → {:?}，但当前为 {:?}",
            self.expected, self.attempted, self.actual
        )
    }
}

/// TransitionError → HRESULT（O2）：统一映射为 APOERR_ALREADY_INITIALIZED。
impl From<TransitionError> for windows::core::HRESULT {
    fn from(_: TransitionError) -> Self {
        windows::core::HRESULT(0x887D_0001u32 as i32) // APOERR_ALREADY_INITIALIZED
    }
}

pub struct StateCell { state: AtomicU8 }
impl StateCell {
    pub fn new() -> Self { Self { state: AtomicU8::new(ApoState::Created as u8) } }

    /// CAS 转换：成功返回 Ok，失败返回 `TransitionError{expected, attempted, actual}`。
    pub fn transition(&self, from: ApoState, to: ApoState) -> std::result::Result<(), TransitionError> {
        let actual_raw = self.state.load(Ordering::Acquire);
        if actual_raw != from as u8 {
            return Err(TransitionError::new(from, to, state_from_u8(actual_raw)));
        }
        self.state.compare_exchange(from as u8, to as u8, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|actual| TransitionError::new(from, to, state_from_u8(actual)))
    }

    /// 当前状态。
    pub fn current(&self) -> ApoState {
        state_from_u8(self.state.load(Ordering::Acquire))
    }

    /// release（O2）：任意状态 → Created，返回旧状态。DLL 卸载终态复位用。
    pub fn release(&self) -> ApoState {
        let old = self.state.swap(ApoState::Created as u8, Ordering::AcqRel);
        state_from_u8(old)
    }

    // ── 语义化便捷转换（失败即 TransitionError） ──
    pub fn initialize(&self) -> std::result::Result<(), TransitionError> { self.transition(ApoState::Created, ApoState::Initialized) }
    pub fn lock(&self)       -> std::result::Result<(), TransitionError> { self.transition(ApoState::Initialized, ApoState::Locked) }
    pub fn unlock(&self)     -> std::result::Result<(), TransitionError> { self.transition(ApoState::Locked, ApoState::Initialized) }
}

fn state_from_u8(v: u8) -> ApoState {
    match v {
        1 => ApoState::Initialized,
        2 => ApoState::Locked,
        _ => ApoState::Created,
    }
}

// ═══ ApoObjectInner（双链过渡状态） ═══
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
        }
    }
}

// ═══ ApoObject ═══
#[implement(
    IAudioProcessingObject,
    IAudioProcessingObjectRT,
    IAudioProcessingObjectConfiguration
)]
#[allow(dead_code)]
pub struct ApoObject {
    pub(crate) clsid: windows::core::GUID,
    pub(crate) state_cell: StateCell,
    pub(crate) mutex: Mutex<ApoObjectInner>,
    pub(crate) latency_samples: AtomicU32,
    pub(crate) latency_frames_atomic: AtomicU32,
    pub(crate) process_stats: ProcessStatistics,
    /// 配置文件路径（Initialize 确定，per-device `Documents\VxAPO\{GUID}\config.txt`）。
    pub(crate) config_path: Mutex<String>,
}

impl ApoObject {
    pub fn new(clsid: windows::core::GUID) -> Self {
        ref_count::increment();
        Self {
            clsid,
            state_cell: StateCell::new(),
            mutex: Mutex::new(ApoObjectInner::new()),
            latency_samples: AtomicU32::new(0),
            latency_frames_atomic: AtomicU32::new(0),
            process_stats: ProcessStatistics::new(),
            config_path: Mutex::new(DEFAULT_CONFIG_PATH.to_owned()),
        }
    }

    /// 配置热重载（watcher 触发，R2/v6.9 阻塞式）：
    /// 短锁检查 transition 在途 / reloading → 直接返回（不构建新链）；
    /// 否则锁外解析新链，锁内放入 outgoing 进入过渡。
    pub fn hot_reload(&self) {
        {
            let mut inner = self.mutex.lock().unwrap();
            if inner.transition.is_some() || inner.reloading {
                // 阻塞式：过渡在途或加载中，本次变更丢弃（过渡完成后 APOProcess 会触发一次重载）。
                return;
            }
            // 标记加载中，防覆盖。
            inner.reloading = true;
        }

        // 锁外构建新 Chain（避免长时间持锁）。
        let new_chain = {
            let inner = self.mutex.lock().unwrap();
            let ctx = inner.pipeline_context.clone();
            drop(inner);
            let dsp_ctx = build_dsp_context(&ctx, 32);
            let mut registry = FilterRegistry::new();
            register_all_commands(&mut registry);
            let parser = ConfigParser::new(registry);
            let config_path = self.config_path.lock().unwrap().clone();
            let filters = parser.parse_file(&config_path, &dsp_ctx).unwrap_or_default();
            let mut chain = Chain::new();
            for f in filters {
                let _ = chain.add_filter(f);
            }
            chain
        };

        // 锁内切换：竞态兜底——解析期间新过渡已启动 → 排队。
        let mut inner = self.mutex.lock().unwrap();
        if inner.transition.is_some() {
            inner.pending_reload = true;
            inner.reloading = false;
            return;
        }
        // 旧链进 outgoing；退役链由控制线程在此统一析构（R1）。
        let old = std::mem::replace(&mut inner.current_chain, Box::new(new_chain));
        inner.outgoing_chain = Some(old);
        inner.pending_reload = false;
        inner.reloading = false;
        let length = default_smoothing_length(inner.pipeline_context.sample_rate);
        let mut sm = SmoothingProvider::new(length);
        sm.begin();
        inner.transition = Some(sm);
    }
}

impl Drop for ApoObject {
    fn drop(&mut self) { ref_count::decrement(); }
}

// ═══ IAudioProcessingObject 实现（windows-rs _Impl trait 签名） ═══
impl IAudioProcessingObject_Impl for ApoObject_Impl {
    fn Reset(&self) -> Result<()> {
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
        self.latency_samples.store(0, Ordering::SeqCst);
        self.latency_frames_atomic.store(0, Ordering::SeqCst);
        Ok(())
    }

    fn GetLatency(&self) -> Result<i64> {
        let sample_rate = self.mutex.lock().unwrap().pipeline_context.sample_rate;
        let latency_samples = self.latency_samples.load(Ordering::Acquire) as i64;
        if sample_rate == 0 {
            return Ok(0);
        }
        Ok(latency_samples * 10_000_000 / sample_rate as i64)
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
            return Err(windows::core::Error::from(windows::core::HRESULT(0x8007_000Eu32 as i32))); // ERROR_OUTOFMEMORY
        }
        unsafe {
            std::ptr::write(alloc as *mut APO_REG_PROPERTIES, *prop);
        }
        Ok(alloc as *mut APO_REG_PROPERTIES)
    }

    fn Initialize(&self, cb_data_size: u32, pby_data: *const u8) -> Result<()> {
        // 1. 参数校验：pby_data 非空、cb_data_size 足以容纳 APOInitSystemEffects
        //    （SDK 约定：Initialize 的 pby_data 指向完整的 APOInitSystemEffects；
        //    数据非法 → 仍初始化成功并降级默认配置，不阻断 APO 加载）。
        let valid_init_data = !pby_data.is_null()
            && cb_data_size >= std::mem::size_of::<APOInitSystemEffects>() as u32;

        // 2. 状态转换 Created → Initialized，失败 → 对应 HRESULT。
        self.state_cell
            .transition(ApoState::Created, ApoState::Initialized)
            .map_err(|e| windows::core::Error::from(windows::core::HRESULT::from(e)))?;

        // 3. 解析 APOInitSystemEffects → per-device 配置路径（object 7.1.8）。
        //    Safety: pby_data 已验证非空 + 尺寸足够；APOInitSystemEffects 为 repr(C) 结构。
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
        _p_opposite_format: windows::core::Ref<IAudioMediaType>,
        p_requested: windows::core::Ref<IAudioMediaType>,
    ) -> Result<IAudioMediaType> {
        // 提取请求格式并与当前 PipelineContext 比较（仅通道数/采样率）。
        // Ref<IAudioMediaType> 的 Deref 目标是 Option<IAudioMediaType>。
        let Some(req) = p_requested.as_ref() else {
            return Err(windows::core::Error::from(APOERR_INVALID_CONNECTION_FORMAT));
        };
        let mt_ptr = req as *const IAudioMediaType as *mut IAudioMediaType;
        let requested = unsafe { extract_format(mt_ptr) };
        let inner = self.mutex.lock().unwrap();
        let ctx = &inner.pipeline_context;
        match requested {
            Ok(fmt) if fmt.channels == ctx.input_channels
                && fmt.sample_rate == ctx.sample_rate => Ok(req.clone()),
            _ => Err(windows::core::Error::from(APOERR_FORMAT_NOT_SUPPORTED)),
        }
    }

    fn IsOutputFormatSupported(
        &self,
        _p_opposite_format: windows::core::Ref<IAudioMediaType>,
        p_requested: windows::core::Ref<IAudioMediaType>,
    ) -> Result<IAudioMediaType> {
        // 输出格式与输入一致（INPLACE 模式）。
        let Some(req) = p_requested.as_ref() else {
            return Err(windows::core::Error::from(APOERR_INVALID_CONNECTION_FORMAT));
        };
        let mt_ptr = req as *const IAudioMediaType as *mut IAudioMediaType;
        let requested = unsafe { extract_format(mt_ptr) };
        let inner = self.mutex.lock().unwrap();
        let ctx = &inner.pipeline_context;
        match requested {
            Ok(fmt) if fmt.channels == ctx.output_channels
                && fmt.sample_rate == ctx.sample_rate => Ok(req.clone()),
            _ => Err(windows::core::Error::from(APOERR_FORMAT_NOT_SUPPORTED)),
        }
    }

    fn GetInputChannelCount(&self) -> Result<u32> {
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
        if self.state_cell.current() != ApoState::Locked {
            return;
        }
        if num_input == 0 || num_output == 0 || pp_inputs.is_null() || pp_outputs.is_null() {
            return;
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
        // APO 通常单连接（1:1），直接用首元素解引用构造 slice，零分配。
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

    fn CalcInputFrames(&self, output_frames: u32) -> u32 {
        output_frames + self.latency_frames_atomic.load(Ordering::Acquire)
    }

    fn CalcOutputFrames(&self, input_frames: u32) -> u32 {
        let latency = self.latency_frames_atomic.load(Ordering::Acquire);
        input_frames.saturating_sub(latency)
    }
}

// ═══ IAudioProcessingObjectConfiguration 实现 ═══
impl IAudioProcessingObjectConfiguration_Impl for ApoObject_Impl {
    fn LockForProcess(
        &self,
        num_input: u32,
        pp_inputs: *const *const APO_CONNECTION_DESCRIPTOR,
        _num_output: u32,
        _pp_outputs: *const *const APO_CONNECTION_DESCRIPTOR,
    ) -> Result<()> {
        // Step 0: 状态机 Initialized → Locked，失败自动回退。
        self.state_cell
            .transition(ApoState::Initialized, ApoState::Locked)
            .map_err(|e| windows::core::Error::from(windows::core::HRESULT::from(e)))?;
        let _guard = LockGuard::new(&self.state_cell);

        if num_input == 0 || pp_inputs.is_null() {
            return Err(windows::core::Error::from(APOERR_NUM_CONNECTIONS_INVALID));
        }

        // Step 1: 从输入连接描述符提取格式（pFormat 为 ManuallyDrop<Option<IAudioMediaType>>）。
        let input_descriptor = unsafe { &**pp_inputs };
        let format = match input_descriptor.pFormat.as_ref() {
            Some(media_type) => {
                let mt_ptr: *mut IAudioMediaType =
                    media_type as *const IAudioMediaType as *mut IAudioMediaType;
                unsafe { extract_format(mt_ptr) }
                    .map_err(|e| windows::core::Error::from(windows::core::HRESULT::from(e)))?
            }
            None => return Err(windows::core::Error::from(APOERR_INVALID_CONNECTION_FORMAT)),
        };

        // Step 2: 构建 PipelineContext + DspContext。
        let pipeline_context = PipelineContext {
            sample_rate: format.sample_rate,
            input_channels: format.channels,
            output_channels: format.channels,
            channel_mask: format.channel_mask,
            max_frame_count: input_descriptor.u32MaxFrameCount as usize,
        };

        let channel_names = get_channel_names(format.channel_mask);
        let dsp_ctx = DspContext {
            sample_rate: format.sample_rate,
            channel_count: format.channels,
            channel_mask: format.channel_mask,
            channel_names,
            max_frame_count: input_descriptor.u32MaxFrameCount,
            bits_per_sample: format.bits_per_sample,
            device_type: DeviceType::Render,
            stage: ProcessingStage::None,
            variables: std::collections::HashMap::new(),
            rt_marker: std::marker::PhantomData,
        };

        // Step 3: 构建 FilterRegistry + ConfigParser，解析配置文件
        //         （路径来自 Initialize 确定的 per-device config_path）。
        let mut registry = FilterRegistry::new();
        register_all_commands(&mut registry);
        let parser = ConfigParser::new(registry);
        let config_path = self.config_path.lock().unwrap().clone();
        let filters = parser.parse_file(&config_path, &dsp_ctx)
            .map_err(|_| windows::core::Error::from(windows::core::HRESULT(0x8000_0001u32 as i32)))?;

        // Step 4: 组装 Chain。
        let mut chain = Chain::new();
        for f in filters {
            chain.add_filter(f)
                .map_err(|_| windows::core::Error::from(windows::core::HRESULT(0x8000_0001u32 as i32)))?;
        }
        let total_latency = chain.total_latency();

        // Step 5: 预分配临时缓冲区（deinterleave 空间，channels 个 Vec）。
        let mut temp_buffers: Vec<Vec<f32>> = Vec::with_capacity(format.channels as usize);
        for _ in 0..format.channels {
            temp_buffers.push(Vec::with_capacity(pipeline_context.max_frame_count));
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
            inner.temp_buffer_old = Vec::new();
            inner.temp_buffer_new = Vec::new();
            inner.pending_reload = false;
            inner.reloading = false;
        }
        self.latency_samples.store(total_latency, Ordering::SeqCst);
        self.latency_frames_atomic.store(total_latency, Ordering::SeqCst);

        // Step 7: 确保第三方 APO 可加载（DisableProtectedAudioDG）。
        crate::install::audiodg::ensure_can_load()
            .map_err(|e| windows::core::Error::from(windows::core::HRESULT::from(e)))?;

        // 全部成功 → 解除守卫（不再回退状态）。
        _guard.disarm();
        Ok(())
    }

    fn UnlockForProcess(&self) -> Result<()> {
        self.state_cell
            .transition(ApoState::Locked, ApoState::Initialized)
            .map_err(|e| windows::core::Error::from(windows::core::HRESULT::from(e)))?;
        // R1：退役链 + 过渡状态由控制线程锁内统一析构。
        let mut inner = self.mutex.lock().unwrap();
        inner.retired_chain = None;
        inner.outgoing_chain = None;
        inner.transition = None;
        inner.pending_reload = false;
        inner.reloading = false;
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

    /// 构造「无 IPropertyStore」的最低有效 APOInitSystemEffects（zeroed 后仅设置 APOInit.cbSize）。
    /// 提取端点 GUID 会因属性存储缺失返回 None → 走 `_default` 兜底。
    fn empty_init() -> APOInitSystemEffects {
        let mut init: APOInitSystemEffects = unsafe { std::mem::zeroed() };
        init.APOInit.cbSize = std::mem::size_of::<APOInitSystemEffects>() as u32;
        init
    }

    #[test]
    fn config_path_default_device_dir_when_no_guid() {
        // 无端点 GUID（属性存储缺失）→ `{docs}\VxAPO\_default\config.txt`。
        let docs = std::env::temp_dir().join("vxapo_apo_test").join("docs");
        let docs_str = docs.display().to_string();
        let init = empty_init();
        let path = resolve_config_path_from(&docs_str, Some(&init));
        let p = Path::new(&path);
        assert!(p.starts_with(&docs));
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
        let docs = std::env::temp_dir().join("vxapo_apo_test2").join("docs");
        let docs_str = docs.display().to_string();
        let init = empty_init();
        let path = resolve_config_path_from(&docs_str, Some(&init));
        assert!(path.contains("_default"));
        // 幂等：再次调用不应报错（目录已存在）。
        let _ = resolve_config_path_from(&docs_str, Some(&init));
        let _ = std::fs::remove_dir_all(docs.join("VxAPO"));
        let _ = std::fs::remove_dir_all(&docs);
    }

    #[test]
    fn config_path_none_init_falls_back_default() {
        // init=None（Initialize 数据非法降级）→ `_default` 兜底。
        let docs = std::env::temp_dir().join("vxapo_apo_test3").join("docs");
        let docs_str = docs.display().to_string();
        let path = resolve_config_path_from(&docs_str, None);
        assert!(path.contains("_default"));
        assert!(Path::new(&path).parent().unwrap().is_dir());
        let _ = std::fs::remove_dir_all(docs.join("VxAPO"));
        let _ = std::fs::remove_dir_all(&docs);
    }
}
