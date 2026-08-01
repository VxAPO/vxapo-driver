//! object/apo.rs — ApoObject 核心（v6.3 规范 7.1，按 windows-rs 0.62.2 _Impl trait 实现）

use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::Mutex;
use windows::core::implement;
use windows::core::{Result};

use crate::object::ref_count;
use crate::pipeline::chain::Chain;
use crate::pipeline::context::PipelineContext;
use crate::pipeline::process::ProcessStatistics;
use crate::sys::com::apo_interfaces::{
    IAudioMediaType, IAudioProcessingObject, IAudioProcessingObjectConfiguration, IAudioProcessingObjectRT,
    IAudioProcessingObject_Impl, IAudioProcessingObjectRT_Impl, IAudioProcessingObjectConfiguration_Impl,
};
use windows::Win32::Media::Audio::Apo::{APO_CONNECTION_DESCRIPTOR, APO_CONNECTION_PROPERTY, APO_REG_PROPERTIES};

use crate::sys::com::apo_types::{APOERR_FORMAT_NOT_SUPPORTED, APOERR_NOT_INITIALIZED};

// ═══ 状态机 ═══
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ApoState { Created = 0, Initialized = 1, Locked = 2 }

pub struct StateCell { state: AtomicU8 }
impl StateCell {
    pub fn new() -> Self { Self { state: AtomicU8::new(ApoState::Created as u8) } }
    pub fn transition(&self, from: ApoState, to: ApoState) -> crate::utils::vx_error::Result<()> {
        self.state.compare_exchange(from as u8, to as u8, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ()).map_err(|_| crate::utils::vx_error::VxApoError::state("非法状态转换"))
    }
    pub fn current(&self) -> ApoState {
        match self.state.load(Ordering::Acquire) { 1 => ApoState::Initialized, 2 => ApoState::Locked, _ => ApoState::Created }
    }
}

// ═══ ApoObjectInner（双链过渡状态） ═══
pub struct ApoObjectInner {
    pub current_chain: Box<Chain>,
    pub outgoing_chain: Option<Box<Chain>>,
    pub pipeline_context: PipelineContext,
    pub temp_buffers: Vec<Vec<f32>>,
    pub temp_buffer_old: Vec<f32>,
    pub temp_buffer_new: Vec<f32>,
    pub pending_reload: bool,
}
impl ApoObjectInner {
    pub fn new() -> Self {
        Self {
            current_chain: Box::new(Chain::new()),
            outgoing_chain: None,
            pipeline_context: PipelineContext::new(),
            temp_buffers: Vec::new(),
            temp_buffer_old: Vec::new(),
            temp_buffer_new: Vec::new(),
            pending_reload: false,
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
        }
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
        inner.pipeline_context = PipelineContext::new();
        inner.temp_buffers.clear();
        inner.temp_buffer_old.clear();
        inner.temp_buffer_new.clear();
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
        // 骨架：返回 null，后续批次用 vx_reg_props 实现
        Ok(std::ptr::null_mut())
    }

    fn Initialize(&self, _cb_data_size: u32, _pby_data: *const u8) -> Result<()> {
        self.state_cell.transition(ApoState::Created, ApoState::Initialized)
            .map_err(|e| windows::core::Error::from(windows::core::HRESULT::from(e)))
    }

    fn IsInputFormatSupported(
        &self,
        _p_opposite_format: windows::core::Ref<IAudioMediaType>,
        _p_requested: windows::core::Ref<IAudioMediaType>,
    ) -> Result<IAudioMediaType> {
        Err(windows::core::Error::from(APOERR_FORMAT_NOT_SUPPORTED))
    }

    fn IsOutputFormatSupported(
        &self,
        _p_opposite_format: windows::core::Ref<IAudioMediaType>,
        _p_requested: windows::core::Ref<IAudioMediaType>,
    ) -> Result<IAudioMediaType> {
        Err(windows::core::Error::from(APOERR_FORMAT_NOT_SUPPORTED))
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
        _num_input: u32,
        _pp_inputs: *const *const APO_CONNECTION_PROPERTY,
        _num_output: u32,
        _pp_outputs: *mut *mut APO_CONNECTION_PROPERTY,
    ) {
        // 骨架：空实现，后续批次接 pipeline::process::process_audio
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
        _num_input: u32,
        _pp_inputs: *const *const APO_CONNECTION_DESCRIPTOR,
        _num_output: u32,
        _pp_outputs: *const *const APO_CONNECTION_DESCRIPTOR,
    ) -> Result<()> {
        Err(windows::core::Error::from(APOERR_FORMAT_NOT_SUPPORTED))
    }

    fn UnlockForProcess(&self) -> Result<()> {
        self.state_cell.transition(ApoState::Locked, ApoState::Initialized)
            .map_err(|e| windows::core::Error::from(windows::core::HRESULT::from(e)))
    }
}

unsafe impl Send for ApoObject {}
unsafe impl Sync for ApoObject {}