//! object/apo.rs — ApoObject 核心（v6.2 规范 7.1，骨架版）

use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::Mutex;
use windows::core::implement;
use windows::core::HRESULT;

use crate::object::ref_count;
use crate::pipeline::chain::Chain;
use crate::pipeline::context::PipelineContext;
use crate::pipeline::process::ProcessStatistics;
use crate::sys::com::apo_interfaces::{
    IAudioMediaType, IAudioProcessingObject, IAudioProcessingObjectConfiguration, IAudioProcessingObjectRT,
};
use crate::sys::com::apo_types::{
    APO_CONNECTION_DESCRIPTOR, APO_CONNECTION_PROPERTY, APO_REG_PROPERTIES, REFERENCE_TIME,
};

// ═══ 状态机 ═══
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ApoState { Created = 0, Initialized = 1, Locked = 2 }

pub struct StateCell { state: AtomicU8 }
impl StateCell {
    pub fn new() -> Self { Self { state: AtomicU8::new(ApoState::Created as u8) } }
    pub fn transition(&self, from: ApoState, to: ApoState) -> crate::utils::vx_error::Result<()> {
        self.state.compare_exchange(from as u8, to as u8, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ()).map_err(|_| crate::utils::vx_error::VxApoError::state("非法状态转换".into()))
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

// ═══ IAudioProcessingObject 实现 ═══
impl IAudioProcessingObject_Impl for ApoObject_Impl {
    unsafe fn Reset(&self) -> HRESULT { HRESULT(0) }
    unsafe fn GetLatency(&self, p_latency: *mut REFERENCE_TIME) -> HRESULT {
        if p_latency.is_null() { return windows::core::HRESULT(0x80004003u32 as i32); }
        *p_latency = 0; HRESULT(0)
    }
    unsafe fn GetRegistrationProperties(&self, _pp_props: *mut *mut APO_REG_PROPERTIES) -> HRESULT { HRESULT(0x80004005u32 as i32) }
    unsafe fn Initialize(&self, _cb_data_size: u32, _pby_data: *mut u8) -> HRESULT { HRESULT(0) }
    unsafe fn IsInputFormatSupported(&self, _a: *mut IAudioMediaType, _b: *mut IAudioMediaType, _c: *mut *mut IAudioMediaType) -> HRESULT { HRESULT(0) }
    unsafe fn IsOutputFormatSupported(&self, _a: *mut IAudioMediaType, _b: *mut IAudioMediaType, _c: *mut *mut IAudioMediaType) -> HRESULT { HRESULT(0) }
    unsafe fn GetInputChannelCount(&self, _p: *mut u32) -> HRESULT { HRESULT(0) }
}

impl IAudioProcessingObjectRT_Impl for ApoObject_Impl {
    unsafe fn APOProcess(&self, _a: u32, _b: *mut *mut APO_CONNECTION_PROPERTY, _c: u32, _d: *mut *mut APO_CONNECTION_PROPERTY) {}
    unsafe fn CalcInputFrames(&self, out: u32) -> u32 { out }
    unsafe fn CalcOutputFrames(&self, inp: u32) -> u32 { inp }
}

impl IAudioProcessingObjectConfiguration_Impl for ApoObject_Impl {
    unsafe fn LockForProcess(&self, _a: u32, _b: *mut *mut APO_CONNECTION_DESCRIPTOR, _c: u32, _d: *mut *mut APO_CONNECTION_DESCRIPTOR) -> HRESULT { HRESULT(0) }
    unsafe fn UnlockForProcess(&self) -> HRESULT { HRESULT(0) }
}

unsafe impl Send for ApoObject {}
unsafe impl Sync for ApoObject {}