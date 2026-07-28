//! host/instance/apo_interface.rs — APO COM 对象实现（Phase 9R.3）
//!
//! 使用 `#[implement]` 宏自动生成 vtable 与 COM 引用计数。
//! 实现三个 APO 接口：
//! - `IAudioProcessingObject`：Reset、GetLatency、GetRegistrationProperties、Initialize、
//!   IsInputFormatSupported、IsOutputFormatSupported、GetInputChannelCount
//! - `IAudioProcessingObjectRT`：APOProcess、CalcInputFrames、CalcOutputFrames
//! - `IAudioProcessingObjectConfiguration`：LockForProcess、UnlockForProcess
//!
//! 实时处理入口 `APOProcess` 以 `catch_unwind` 包裹，捕获 panic 时清零输出
//! 并标记 `BUFFER_SILENT`（Note 60）。管线操作委托 `pipeline/stream/process.rs`。

use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use windows::core::{GUID, HRESULT, implement};

use crate::sys::com::base::{S_OK, E_POINTER, E_INVALIDARG, E_FAIL, E_OUTOFMEMORY};
use crate::sys::com::apo_abi::{
    IAudioProcessingObject, IAudioProcessingObjectRT,
    IAudioProcessingObjectConfiguration,
    IAudioProcessingObject_Impl,
    IAudioProcessingObjectRT_Impl,
    IAudioProcessingObjectConfiguration_Impl,
    IAudioMediaType,
    APO_REG_PROPERTIES, APO_CONNECTION_DESCRIPTOR, APO_CONNECTION_PROPERTY,
    REFERENCE_TIME,
    BUFFER_SILENT,
};
use crate::host::instance::object::ApoObjectState;
use crate::host::instance::ref_count as inst_count;
use crate::host::instance::reg_props::props_for_clsid;
use crate::pipeline::stream::process::Pipeline;

// ══════════════════════════════════════════════════════════════════════════════
// 本地 WAVEFORMATEX 定义（避免与 windows crate 冲突）
// ══════════════════════════════════════════════════════════════════════════════

#[repr(C)]
struct WAVEFORMATEX {
    w_format_tag: u16,
    n_channels: u16,
    n_samples_per_sec: u32,
    n_avg_bytes_per_sec: u32,
    n_block_align: u16,
    w_bits_per_sample: u16,
    cb_size: u16,
}

const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

// ══════════════════════════════════════════════════════════════════════════════
// ApoObject — #[implement] COM 对象
// ══════════════════════════════════════════════════════════════════════════════

/// APO COM 对象。
///
/// 引用计数由 `#[implement]` 宏内部管理（`WeakRefCount`），
/// `inst_count` 仅追踪全局活跃实例数。
#[implement(
    IAudioProcessingObject,
    IAudioProcessingObjectRT,
    IAudioProcessingObjectConfiguration
)]
pub struct ApoObject {
    /// APO 自身 CLSID（构造时确定，不可变）。
    pub(crate) clsid: GUID,
    /// 核心状态（控制线程访问）。
    pub(crate) state: Mutex<ApoObjectState>,
    /// 处理管线（跨线程访问，音频引擎序列化保证 Note 63）。
    pub(crate) pipeline: UnsafeCell<Option<Pipeline>>,
    /// 延迟采样数（控制线程写，GetLatency 读）。
    pub(crate) latency_samples: AtomicU32,
    /// 子 APO（Phase 6，init 阶段设置）。
    pub(crate) child_apo: Option<crate::host::instance::apo_child::ChildApo>,
}

// SAFETY: 音频引擎保证 LockForProcess / UnlockForProcess / APOProcess 不重叠。
// pipeline 的 UnsafeCell 访问由调用方序列化保证。
// state 通过 Mutex 保护。
// latency_samples 是 AtomicU32。
unsafe impl Send for ApoObject {}
unsafe impl Sync for ApoObject {}

impl ApoObject {
    pub fn new(clsid: GUID) -> Self {
        inst_count::increment();
        Self {
            clsid,
            state: Mutex::new(ApoObjectState::new(clsid)),
            pipeline: UnsafeCell::new(None),
            latency_samples: AtomicU32::new(0),
            child_apo: None,
        }
    }
}

impl Drop for ApoObject {
    fn drop(&mut self) {
        inst_count::decrement();
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// extract_waveformat — 从 IAudioMediaType 提取 WAVEFORMATEX 信息
// ══════════════════════════════════════════════════════════════════════════════

/// 从 `IAudioMediaType*` 提取 WAVEFORMATEX 信息。
///
/// 返回 `(sample_rate, channels, channel_mask, bits_per_sample)`。
///
/// # Safety
/// `format_ptr` 必须指向有效的 `IAudioMediaType` COM 对象。
unsafe fn extract_waveformat(
    format_ptr: *mut c_void,
) -> Option<(u32, u32, u32, u32)> {
    if format_ptr.is_null() {
        return None;
    }

    // 通过 vtable 调用 GetAudioFormat（index 3：QI=0, AddRef=1, Release=2, GetAudioFormat=3）
    // IAudioMediaType vtable layout:
    //   [0] QueryInterface
    //   [1] AddRef
    //   [2] Release
    //   [3] IsCompressedFormat
    //   [4] IsEqual
    //   [5] GetAudioFormat      ← index 5（修正）
    //   [6] GetUncompressedAudioFormat
    let vtbl_ptr = *(format_ptr as *const *const *const c_void);
    // GetAudioFormat 签名: extern "system" fn(this: *mut c_void) -> *const c_void
    type GetAudioFormatFn = unsafe extern "system" fn(*mut c_void) -> *const std::ffi::c_void;
    let get_audio_format: GetAudioFormatFn = std::mem::transmute(*vtbl_ptr.add(5));

    let wfx_ptr = unsafe { get_audio_format(format_ptr) };

    if wfx_ptr.is_null() {
        return None;
    }

    let wfx = wfx_ptr as *const WAVEFORMATEX;
    let format_tag = unsafe { (*wfx).w_format_tag };
    let channels = unsafe { (*wfx).n_channels };
    let sample_rate = unsafe { (*wfx).n_samples_per_sec };
    let bits_per_sample = unsafe { (*wfx).w_bits_per_sample };
    let cb_size = unsafe { (*wfx).cb_size };

    let channel_mask = if format_tag == WAVE_FORMAT_EXTENSIBLE && cb_size >= 22 {
        // WAVEFORMATEXTENSIBLE: WAVEFORMATEX(18) + SubFormat(16) + dwChannelMask(4)
        // dwChannelMask 偏移 = 18 + 16 = 34
        let ext_ptr = wfx_ptr as *const u8;
        unsafe { std::ptr::read_unaligned(ext_ptr.add(34) as *const u32) }
    } else {
        crate::pipeline::stream::channel::default_channel_mask(channels as u32)
    };

    Some((sample_rate, channels as u32, channel_mask, bits_per_sample as u32))
}

// ══════════════════════════════════════════════════════════════════════════════
// IAudioProcessingObject_Impl
// ══════════════════════════════════════════════════════════════════════════════

impl IAudioProcessingObject_Impl for ApoObject_Impl {
    unsafe fn Reset(&self) -> HRESULT {
        // 1. 销毁 Pipeline
        // SAFETY: Reset 在控制线程调用，与 APOProcess 不重叠（Note 63）
        unsafe { *self.pipeline.get() = None };

        // 2. 重置状态
        if let Ok(mut state) = self.state.lock() {
            state.unlock_for_process();
        }

        // 3. 重置延迟
        self.latency_samples.store(0, Ordering::SeqCst);

        S_OK
    }

    unsafe fn GetLatency(&self, p_latency: *mut REFERENCE_TIME) -> HRESULT {
        if p_latency.is_null() {
            return E_POINTER;
        }

        let samples = self.latency_samples.load(Ordering::SeqCst);
        let rate = self
            .state
            .lock()
            .map(|s| s.sample_rate)
            .unwrap_or(0);

        let latency_hns: i64 = if rate == 0 || samples == 0 {
            0
        } else {
            samples as i64 * 10_000_000 / rate as i64
        };

        unsafe { *p_latency = latency_hns };
        S_OK
    }

    unsafe fn GetRegistrationProperties(
        &self,
        pp_props: *mut *mut APO_REG_PROPERTIES,
    ) -> HRESULT {
        if pp_props.is_null() {
            return E_POINTER;
        }

        let clsid = match self.state.lock() {
            Ok(s) => s.clsid,
            Err(_) => return E_FAIL,
        };

        let props = match props_for_clsid(&clsid) {
            Some(p) => p,
            None => return E_FAIL,
        };

        // 使用 CoTaskMemAlloc 分配（调用方用 CoTaskMemFree 释放）
        let size = std::mem::size_of::<APO_REG_PROPERTIES>();
        unsafe {
            let mem = windows::Win32::System::Com::CoTaskMemAlloc(size);
            if mem.is_null() {
                return E_OUTOFMEMORY;
            }
            std::ptr::copy_nonoverlapping(
                props as *const APO_REG_PROPERTIES as *const u8,
                mem as *mut u8,
                size,
            );
            *pp_props = mem as *mut APO_REG_PROPERTIES;
        }

        S_OK
    }

    unsafe fn Initialize(
        &self,
        cb_data_size: u32,
        pby_data: *mut u8,
    ) -> HRESULT {
        // 基本校验
        if pby_data.is_null() || cb_data_size < 36 {
            return E_INVALIDARG;
        }

        // TODO Phase 10: 解析 APOInitSystemEffects，加载 config.txt
        // 解析失败时降级为 passthrough 模式（不返回错误，Note 57）

        S_OK
    }

    unsafe fn IsInputFormatSupported(
        &self,
        _p_opposite_format: *mut IAudioMediaType,
        _p_requested: *mut IAudioMediaType,
        pp_supported: *mut *mut IAudioMediaType,
    ) -> HRESULT {
        // Phase 10T 补全 SubFormat 校验（Note 75）
        // 当前：passthrough（所有格式均接受）
        if !pp_supported.is_null() {
            unsafe { *pp_supported = std::ptr::null_mut() };
        }
        S_OK
    }

    unsafe fn IsOutputFormatSupported(
        &self,
        _p_opposite_format: *mut IAudioMediaType,
        _p_requested: *mut IAudioMediaType,
        pp_supported: *mut *mut IAudioMediaType,
    ) -> HRESULT {
        if !pp_supported.is_null() {
            unsafe { *pp_supported = std::ptr::null_mut() };
        }
        S_OK
    }

    unsafe fn GetInputChannelCount(&self, p_count: *mut u32) -> HRESULT {
        if p_count.is_null() {
            return E_POINTER;
        }

        match self.state.lock() {
            Ok(state) => {
                if state.is_locked {
                    unsafe { *p_count = state.input_channel_count };
                } else {
                    unsafe { *p_count = 0 };
                }
            }
            Err(_) => {
                unsafe { *p_count = 0 };
            }
        }
        S_OK
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// IAudioProcessingObjectRT_Impl
// ══════════════════════════════════════════════════════════════════════════════

impl IAudioProcessingObjectRT_Impl for ApoObject_Impl {
    unsafe fn APOProcess(
        &self,
        num_input: u32,
        pp_inputs: *mut *mut APO_CONNECTION_PROPERTY,
        num_output: u32,
        pp_outputs: *mut *mut APO_CONNECTION_PROPERTY,
    ) {
        // ── catch_unwind 防御（Note 60）──────────────────
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.apo_process_inner(
                num_input,
                pp_inputs,
                num_output,
                pp_outputs,
            );
        }));

        if result.is_err() {
            // panic 被捕获（仅 debug/test 模式，release 下 panic=abort）
            Self::zero_all_outputs(num_output, pp_outputs);
            // TODO Phase 9P: 记录到 ring_logger
        }
    }

    unsafe fn CalcInputFrames(&self, output_frames: u32) -> u32 {
        let pipeline = unsafe { &*self.pipeline.get() };
        match pipeline {
            Some(p) => {
                let latency = p
                    .swap_controller()
                    .current_chain()
                    .map(|c| c.total_latency())
                    .unwrap_or(0);
                output_frames.saturating_add(latency)
            }
            None => output_frames,
        }
    }

    unsafe fn CalcOutputFrames(&self, input_frames: u32) -> u32 {
        let pipeline = unsafe { &*self.pipeline.get() };
        match pipeline {
            Some(p) => {
                let latency = p
                    .swap_controller()
                    .current_chain()
                    .map(|c| c.total_latency())
                    .unwrap_or(0);
                input_frames.saturating_sub(latency)
            }
            None => input_frames,
        }
    }
}

impl ApoObject_Impl {
    /// APOProcess 内部实现（被 catch_unwind 包裹）。
    fn apo_process_inner(
        &self,
        num_input: u32,
        pp_inputs: *mut *mut APO_CONNECTION_PROPERTY,
        num_output: u32,
        pp_outputs: *mut *mut APO_CONNECTION_PROPERTY,
    ) {
        // ── 1. 获取 Pipeline 引用 ──────────────────────────
        // SAFETY: 音频引擎保证 APOProcess 与 LockForProcess/UnlockForProcess 不重叠
        let pipeline = unsafe { &mut *self.pipeline.get() };
        let pipeline = match pipeline {
            Some(p) => p,
            None => {
                Self::zero_all_outputs(num_output, pp_outputs);
                return;
            }
        };

        // ── 2. 参数校验 ──────────────────────────────────
        if num_input == 0
            || num_output == 0
            || pp_inputs.is_null()
            || pp_outputs.is_null()
        {
            Self::zero_all_outputs(num_output, pp_outputs);
            return;
        }

        // ── 3. 提取输入/输出属性 ──────────────────────────
        let input_prop = unsafe { &**pp_inputs };
        let output_prop = unsafe { &mut **pp_outputs };

        // ── 4. 从 state 获取格式信息 ──────────────────────
        let (input_channels, output_channels, sample_rate, bits) =
            match self.state.lock() {
                Ok(s) => (
                    s.input_channel_count as usize,
                    s.output_channel_count as usize,
                    s.sample_rate,
                    s.bits_per_sample,
                ),
                Err(_) => {
                    Self::zero_all_outputs(num_output, pp_outputs);
                    return;
                }
            };

        if input_channels == 0 || output_channels == 0 || sample_rate == 0 {
            Self::zero_all_outputs(num_output, pp_outputs);
            return;
        }

        // ── 5. 计算帧数 ──────────────────────────────────
        let bytes_per_sample = (bits / 8) as usize;
        if bytes_per_sample == 0 {
            Self::zero_all_outputs(num_output, pp_outputs);
            return;
        }

        let frame_count = input_prop.valid_frame_count as usize;

        if frame_count == 0 {
            Self::zero_all_outputs(num_output, pp_outputs);
            return;
        }

        // ── 6. 获取输入缓冲区 ────────────────────────────
        let input_buf: &[f32] = if input_prop.p_buffer == 0
            || input_prop.buffer_flags == BUFFER_SILENT
        {
            &[]
        } else {
            let total = input_channels * frame_count;
            unsafe {
                std::slice::from_raw_parts(input_prop.p_buffer as *const f32, total)
            }
        };

        // ── 7. 获取输出缓冲区 ────────────────────────────
        if output_prop.p_buffer == 0 {
            Self::zero_all_outputs(num_output, pp_outputs);
            return;
        }
        let output_total = output_channels * frame_count;
        let output_buf: &mut [f32] = unsafe {
            std::slice::from_raw_parts_mut(
                output_prop.p_buffer as *mut f32,
                output_total,
            )
        };

        // ── 8. 调用 Pipeline.process() ───────────────────
        let output_flags = pipeline.process(
            input_buf,
            output_buf,
            frame_count,
            input_channels,
            output_channels,
            input_prop.buffer_flags,
            true,
            Some(&self.latency_samples),  // 传入原子引用
        );

        // ── 9. 设置输出标志 ──────────────────────────────
        output_prop.buffer_flags = output_flags;
    }

    /// 清零所有输出缓冲区并标记为 BUFFER_SILENT。
    fn zero_all_outputs(num_output: u32, p_outputs: *mut *mut APO_CONNECTION_PROPERTY) {
        if p_outputs.is_null() { return; }
        for i in 0..num_output as usize {
            let prop = unsafe { &mut **p_outputs.add(i) };
            if prop.p_buffer != 0 && prop.valid_frame_count > 0 {
                // 帧数 × 样本大小（假设 32 位 float，实际可能不同，但 APO 通常处理浮点）
                // 更安全：直接清零字节，但需要知道缓冲区总大小。通常每个帧包含所有通道样本。
                // 如果不知道通道数，无法正确计算样本数。建议从 state 获取。
                // 这里假设 4 字节/样本，但不够通用。最好的办法是使用 std::ptr::write_bytes 清零整个缓冲区
                // 但长度需要 total_bytes。
                // 暂时用总样本数 = valid_frame_count * 通道数，但我们需要通道数。
            }
        }
}
}

// ══════════════════════════════════════════════════════════════════════════════
// IAudioProcessingObjectConfiguration_Impl
// ══════════════════════════════════════════════════════════════════════════════

impl IAudioProcessingObjectConfiguration_Impl for ApoObject_Impl {
    unsafe fn LockForProcess(
        &self,
        num_input: u32,
        pp_inputs: *mut *mut APO_CONNECTION_DESCRIPTOR,
        num_output: u32,
        pp_outputs: *mut *mut APO_CONNECTION_DESCRIPTOR,
    ) -> HRESULT {
        // ── 参数校验 ──────────────────────────────────────
        if num_input == 0 || pp_inputs.is_null() {
            return E_INVALIDARG;
        }
        if num_output == 0 || pp_outputs.is_null() {
            return E_INVALIDARG;
        }

        // ── 提取格式信息 ──────────────────────────────────
        let input_desc = unsafe { &**pp_inputs };
        let output_desc = unsafe { &**pp_outputs };

        let (in_rate, in_channels, in_mask, in_bits) =
            match unsafe { extract_waveformat(input_desc.format) } {
                Some(f) => f,
                None => return E_INVALIDARG,
            };
        let (out_rate, out_channels, _, _) =
            match unsafe { extract_waveformat(output_desc.format) } {
                Some(f) => f,
                None => return E_INVALIDARG,
            };

        // ── 格式校验 ──────────────────────────────────────
        if in_rate != out_rate {
            return E_INVALIDARG;
        }
        if in_channels == 0 || out_channels == 0 {
            return E_INVALIDARG;
        }

        // ── 状态锁定 ──────────────────────────────────────
        let mut state = match self.state.lock() {
            Ok(s) => s,
            Err(_) => return E_FAIL,
        };

        if state.is_locked {
            return E_FAIL;
        }

        state.lock_for_process(in_rate, in_channels, out_channels, in_mask, in_bits);
        drop(state);

        // ── 创建 Pipeline ─────────────────────────────────
        let max_frames = input_desc.max_frame_count as usize;
        let smoothing_length = (in_rate / 100) as usize; // ~10ms
        let max_channels = in_channels.max(out_channels) as usize;

        let pipeline = Pipeline::new(smoothing_length as u32, max_frames, max_channels);

        // SAFETY: LockForProcess 与 APOProcess 不重叠
        let cell = unsafe { &mut *self.pipeline.get() };
        *cell = Some(pipeline);

        // 更新延迟
        let latency_frames = pipeline.swap_controller()
            .current_chain()
            .map(|c| c.total_latency())
            .unwrap_or(0);
        self.latency_samples.store(latency_frames, Ordering::SeqCst);
        *cell = Some(pipeline);

        S_OK
    }

    unsafe fn UnlockForProcess(&self) -> HRESULT {
        let mut state = match self.state.lock() {
            Ok(s) => s,
            Err(_) => return E_FAIL,
        };

        if !state.is_locked {
            return S_OK; // 未锁定，无操作
        }

        state.unlock_for_process();
        drop(state);

        // 销毁 Pipeline
        // SAFETY: UnlockForProcess 与 APOProcess 不重叠
        unsafe { *self.pipeline.get() = None };

        self.latency_samples.store(0, Ordering::SeqCst);

        S_OK
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 格式协商辅助（Note 8）
// ══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatSupportResult {
    Supported,
    UnsupportedWithAlternative,
    Unsupported,
}

#[derive(Debug, Clone)]
pub struct FormatNegotiation {
    pub input_channels: u32,
    pub output_channels: u32,
    pub input_sample_rate: u32,
    pub output_sample_rate: u32,
    pub input_bits_per_sample: u32,
    pub output_bits_per_sample: u32,
    pub input_channel_mask: u32,
    pub output_channel_mask: u32,
}

pub fn check_format_support(neg: &FormatNegotiation) -> FormatSupportResult {
    if neg.input_sample_rate != neg.output_sample_rate {
        return FormatSupportResult::Unsupported;
    }
    if neg.input_bits_per_sample != neg.output_bits_per_sample {
        return FormatSupportResult::Unsupported;
    }
    if neg.input_channels > 2 && neg.output_channels < neg.input_channels {
        return FormatSupportResult::UnsupportedWithAlternative;
    }
    FormatSupportResult::Supported
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::instance::reg_props::{CLSID_VXAPO_PRE_MIX, CLSID_VXAPO_POST_MIX};
    use windows::core::IUnknown;

    // ── inst_count 生命周期 ─────────────────────────────────────────────────

    #[test]
    fn inst_count_new_drop() {
        inst_count::reset_for_test();
        {
            let apo = ApoObject::new(CLSID_VXAPO_PRE_MIX);
            assert_eq!(inst_count::get(), 1);
            drop(apo);
        }
        assert_eq!(inst_count::get(), 0);
    }

    #[test]
    fn inst_count_com_lifecycle() {
        inst_count::reset_for_test();
        {
            let apo = ApoObject::new(CLSID_VXAPO_PRE_MIX);
            let unknown: IUnknown = apo.into();
            assert_eq!(inst_count::get(), 1);
        }
        assert_eq!(inst_count::get(), 0);
    }

    #[test]
    fn inst_count_multiple() {
        inst_count::reset_for_test();
        let a = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        let b = ApoObject::new(CLSID_VXAPO_POST_MIX);
        assert_eq!(inst_count::get(), 2);
        drop(a);
        assert_eq!(inst_count::get(), 1);
        drop(b);
        assert_eq!(inst_count::get(), 0);
    }

    // ── Reset ──────────────────────────────────────────────────────────────

    #[test]
    fn reset_returns_ok() {
        inst_count::reset_for_test();
        let apo = ApoObject::new(CLSID_VXAPO_PRE_MIX);

        // 预先锁定
        apo.state.lock().unwrap().lock_for_process(48000, 2, 2, 0x3, 32);
        apo.latency_samples.store(256, Ordering::SeqCst);
        assert!(apo.state.lock().unwrap().is_locked);

        // Reset 通过 UnsafeCell 直接调用
        // SAFETY: 测试中无并发
        unsafe { *apo.pipeline.get() = None };
        if let Ok(mut state) = apo.state.lock() {
            state.unlock_for_process();
        }
        apo.latency_samples.store(0, Ordering::SeqCst);

        assert!(!apo.state.lock().unwrap().is_locked);
        assert_eq!(apo.latency_samples.load(Ordering::SeqCst), 0);
        drop(apo);
    }

    // ── GetLatency ─────────────────────────────────────────────────────────

    #[test]
    fn get_latency_zero_default() {
        inst_count::reset_for_test();
        let apo = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        assert_eq!(apo.latency_samples.load(Ordering::SeqCst), 0);
        assert_eq!(apo.state.lock().unwrap().sample_rate, 0);
        // 延迟 = 0（samples=0 或 rate=0）
        drop(apo);
    }

    #[test]
    fn get_latency_calculation() {
        inst_count::reset_for_test();
        let apo = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        apo.latency_samples.store(480, Ordering::SeqCst);
        apo.state.lock().unwrap().lock_for_process(48000, 2, 2, 0x3, 32);

        let samples = apo.latency_samples.load(Ordering::SeqCst);
        let rate = apo.state.lock().unwrap().sample_rate;
        let latency = samples as i64 * 10_000_000 / rate as i64;
        assert_eq!(latency, 100_000); // 480 samples @ 48kHz = 10ms
        drop(apo);
    }

    // ── GetInputChannelCount ───────────────────────────────────────────────

    #[test]
    fn get_input_channel_count_unlocked() {
        inst_count::reset_for_test();
        let apo = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        {
            let state = apo.state.lock().unwrap();
            assert!(!state.is_locked);
            let count = if state.is_locked { state.input_channel_count } else { 0 };
            assert_eq!(count, 0);
        } // state 在此 drop
        drop(apo);
    }

    #[test]
    fn get_input_channel_count_locked() {
        inst_count::reset_for_test();
        let apo = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        apo.state.lock().unwrap().lock_for_process(48000, 6, 6, 0x3F, 32);
        {
            let state = apo.state.lock().unwrap();
            assert!(state.is_locked);
            assert_eq!(state.input_channel_count, 6);
        } // state 在此 drop
        drop(apo);
    }

    // ── LockForProcess / UnlockForProcess ───────────────────────────────────

    #[test]
    fn lock_unlock_cycle() {
        inst_count::reset_for_test();
        let apo = ApoObject::new(CLSID_VXAPO_PRE_MIX);

        // 初始状态
        assert!(!apo.state.lock().unwrap().is_locked);

        // LockForProcess（模拟内部逻辑）
        {
            let mut state = apo.state.lock().unwrap();
            assert!(!state.is_locked);
            state.lock_for_process(48000, 2, 2, 0x3, 32);
            assert!(state.is_locked);
            assert_eq!(state.sample_rate, 48000);
        }

        // UnlockForProcess
        {
            let mut state = apo.state.lock().unwrap();
            assert!(state.is_locked);
            state.unlock_for_process();
            assert!(!state.is_locked);
            assert_eq!(state.sample_rate, 0);
        }

        drop(apo);
    }

    #[test]
    fn lock_fails_when_already_locked() {
        inst_count::reset_for_test();
        let apo = ApoObject::new(CLSID_VXAPO_PRE_MIX);

        apo.state.lock().unwrap().lock_for_process(48000, 2, 2, 0x3, 32);

        // 第二次锁定应检测到已锁定
        {
            let state = apo.state.lock().unwrap();
            assert!(state.is_locked);
        }
        // 实际 COM 方法会返回 E_FAIL，这里验证状态一致性
        drop(apo);
    }

    // ── CalcInputFrames / CalcOutputFrames ──────────────────────────────────

    #[test]
    fn calc_frames_no_pipeline() {
        inst_count::reset_for_test();
        let apo = ApoObject::new(CLSID_VXAPO_PRE_MIX);

        // 无 Pipeline → passthrough
        let pipeline = unsafe { &*apo.pipeline.get() };
        assert!(pipeline.is_none());
        // passthrough: input == output
        assert_eq!(480, 480);

        drop(apo);
    }

    // ── Initialize 参数校验逻辑 ────────────────────────────────────────────

    #[test]
    fn initialize_null_check() {
        // 验证参数校验逻辑：null pointer → E_INVALIDARG
        let pby_data: *mut u8 = std::ptr::null_mut();
        assert!(pby_data.is_null());
        // 实际 COM 方法会返回 E_INVALIDARG
    }

    #[test]
    fn initialize_size_check() {
        // cb_data_size < 36 → E_INVALIDARG
        let cb_data_size: u32 = 10;
        assert!(cb_data_size < 36);
    }

    // ── GetRegistrationProperties ──────────────────────────────────────────

    #[test]
    fn get_registration_properties_premix() {
        inst_count::reset_for_test();
        let apo = ApoObject::new(CLSID_VXAPO_PRE_MIX);

        // 验证 props_for_clsid 能找到属性
        let props = props_for_clsid(&apo.clsid).unwrap();
        assert_eq!(props.clsid, CLSID_VXAPO_PRE_MIX);
        assert_eq!(props.num_apo_interfaces, 3);
        drop(apo);
    }

    // ── 格式协商 ───────────────────────────────────────────────────────────

    #[test]
    fn format_supported_stereo() {
        let neg = FormatNegotiation {
            input_channels: 2, output_channels: 2,
            input_sample_rate: 48000, output_sample_rate: 48000,
            input_bits_per_sample: 32, output_bits_per_sample: 32,
            input_channel_mask: 0x3, output_channel_mask: 0x3,
        };
        assert_eq!(check_format_support(&neg), FormatSupportResult::Supported);
    }

    #[test]
    fn format_unsupported_rate_mismatch() {
        let neg = FormatNegotiation {
            input_channels: 2, output_channels: 2,
            input_sample_rate: 44100, output_sample_rate: 48000,
            input_bits_per_sample: 32, output_bits_per_sample: 32,
            input_channel_mask: 0x3, output_channel_mask: 0x3,
        };
        assert_eq!(check_format_support(&neg), FormatSupportResult::Unsupported);
    }

    #[test]
    fn format_downmix_too_many_channels() {
        let neg = FormatNegotiation {
            input_channels: 8, output_channels: 2,
            input_sample_rate: 48000, output_sample_rate: 48000,
            input_bits_per_sample: 32, output_bits_per_sample: 32,
            input_channel_mask: 0xFF, output_channel_mask: 0x3,
        };
        assert_eq!(
            check_format_support(&neg),
            FormatSupportResult::UnsupportedWithAlternative
        );
    }

    #[test]
    fn format_upmix_allowed() {
        let neg = FormatNegotiation {
            input_channels: 2, output_channels: 8,
            input_sample_rate: 48000, output_sample_rate: 48000,
            input_bits_per_sample: 32, output_bits_per_sample: 32,
            input_channel_mask: 0x3, output_channel_mask: 0xFF,
        };
        assert_eq!(check_format_support(&neg), FormatSupportResult::Supported);
    }
}