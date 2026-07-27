//! host/instance/apo_child.rs — 子 APO 管理（Note 6/7）
//!
//! 子 APO COM 生命周期管理：
//! - `CoCreateInstance` 创建子 APO 实例
//! - `QueryInterface` 获取三个接口指针
//! - 延迟、重置、帧数计算委托给子 APO
//! - `Drop` 释放所有 COM 引用
//!
//! 子 APO 由 `init.rs` 在 `Initialize` 时创建（Note 7），
//! 存储在 `ApoObject` 中，供 `apo_rt.rs` 和 `apo_conf.rs` 委托调用。
//!
//! Phase 6 完整实现。

use std::ffi::c_void;

use windows::core::{GUID, HRESULT, IUnknown};

use crate::sys::com::apo_abi::{
    IID_IAPO, IID_IAPO_CONFIG, IID_IAPO_RT, REFERENCE_TIME,
    APO_CONNECTION_DESCRIPTOR,
};
use crate::sys::com::base;

// ══════════════════════════════════════════════════════════════════════════════
// COM vtable 布局常量
// ══════════════════════════════════════════════════════════════════════════════
//
// IUnknown:
//   [0] QueryInterface   [1] AddRef   [2] Release
//
// IAudioProcessingObject : IUnknown:
//   [3] Reset  [4] GetLatency  [5] GetRegistrationProperties
//   [6] IsInputFormatSupported  [7] IsOutputFormatSupported
//   [8] GetInputChannelCount
//
// IAudioProcessingObjectRT : IUnknown:
//   [3] CalcInputFrames  [4] CalcOutputFrames  [5] APOProcess
//
// IAudioProcessingObjectConfiguration : IUnknown:
//   [3] LockForProcess  [4] UnlockForProcess

/// IUnknown::Release 在 vtable 中的索引。
const VT_RELEASE: usize = 2;

// ══════════════════════════════════════════════════════════════════════════════
// ChildApo
// ══════════════════════════════════════════════════════════════════════════════

/// 子 APO COM 对象持有者。
///
/// 通过 `CoCreateInstance` 创建子 APO，`QueryInterface` 获取三个接口。
///
/// # COM 生命周期
///
/// - `create()`：`CoCreateInstance` → `IUnknown`（ref=1）→ QI×3（ref=4）→ drop `IUnknown`（ref=3）
/// - `Drop`：Release ×3（ref=0 → 对象销毁）
pub struct ChildApo {
    /// IAudioProcessingObject 接口指针。
    iapo_ptr: *mut c_void,
    /// IAudioProcessingObjectRT 接口指针。
    iapo_rt_ptr: *mut c_void,
    /// IAudioProcessingObjectConfiguration 接口指针。
    iapo_cfg_ptr: *mut c_void,
}

// SAFETY: COM 接口指针在 ChildApo 生命周期内有效。
// COM 引用计数手动管理（create 中 QI，Drop 中 Release）。
unsafe impl Send for ChildApo {}
unsafe impl Sync for ChildApo {}

impl ChildApo {
    /// 创建子 APO 实例。
    ///
    /// # Safety
    ///
    /// - COM 必须已初始化（`CoInitializeEx`）
    /// - `clsid` 必须指向有效的 APO CLSID
    pub unsafe fn create(clsid: &GUID) -> Result<Self, HRESULT> {
        use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

        // Step 1: CoCreateInstance → IUnknown
        let unknown: IUnknown = CoCreateInstance(clsid, None, CLSCTX_ALL)
            .map_err(|e| HRESULT::from(e))?;

        // Step 2: QI for each interface
        let mut iapo_ptr: *mut c_void = std::ptr::null_mut();
        let mut iapo_rt_ptr: *mut c_void = std::ptr::null_mut();
        let mut iapo_cfg_ptr: *mut c_void = std::ptr::null_mut();

        let hr = Self::qi(&unknown, &IID_IAPO, &mut iapo_ptr);
        if hr.is_err() {
            return Err(hr);
        }

        let hr = Self::qi(&unknown, &IID_IAPO_RT, &mut iapo_rt_ptr);
        if hr.is_err() {
            Self::release_raw(iapo_ptr);
            return Err(hr);
        }

        let hr = Self::qi(&unknown, &IID_IAPO_CONFIG, &mut iapo_cfg_ptr);
        if hr.is_err() {
            Self::release_raw(iapo_rt_ptr);
            Self::release_raw(iapo_ptr);
            return Err(hr);
        }

        // Step 3: Release original IUnknown（3 个 QI 结果保持对象存活）
        drop(unknown);

        Ok(Self {
            iapo_ptr,
            iapo_rt_ptr,
            iapo_cfg_ptr,
        })
    }

    /// 是否有效（所有接口指针非 null）。
    pub fn is_valid(&self) -> bool {
        !self.iapo_ptr.is_null()
            && !self.iapo_rt_ptr.is_null()
            && !self.iapo_cfg_ptr.is_null()
    }

    // ── IAudioProcessingObject 委托 ──────────────────────────────────────────

    /// 获取子 APO 延迟（`GetLatency`，vtable[4]）。
    ///
    /// 返回 `REFERENCE_TIME`（100 纳秒单位），失败返回 0。
    pub fn get_latency(&self) -> REFERENCE_TIME {
        if self.iapo_ptr.is_null() {
            return 0;
        }
        unsafe {
            let fn_ptr = self.vtbl_method(self.iapo_ptr, 4);
            let get_latency: unsafe extern "system" fn(
                *mut c_void,
                *mut REFERENCE_TIME,
            ) -> HRESULT = std::mem::transmute(fn_ptr);

            let mut latency: REFERENCE_TIME = 0;
            let _ = get_latency(self.iapo_ptr, &mut latency);
            latency
        }
    }

    /// 重置子 APO（`Reset`，vtable[3]）。
    pub fn reset(&self) -> HRESULT {
        if self.iapo_ptr.is_null() {
            return base::E_POINTER;
        }
        unsafe {
            let fn_ptr = self.vtbl_method(self.iapo_ptr, 3);
            let reset: unsafe extern "system" fn(*mut c_void) -> HRESULT =
                std::mem::transmute(fn_ptr);
            reset(self.iapo_ptr)
        }
    }

    // ── IAudioProcessingObjectRT 委托 ────────────────────────────────────────

    /// 子 APO 计算输入帧数（`CalcInputFrames`，vtable[3]）。
    pub fn calc_input_frames(&self, output_frames: u32) -> u32 {
        if self.iapo_rt_ptr.is_null() {
            return output_frames;
        }
        unsafe {
            let fn_ptr = self.vtbl_method(self.iapo_rt_ptr, 3);
            let calc: unsafe extern "system" fn(*mut c_void, u32, *mut u32) -> HRESULT =
                std::mem::transmute(fn_ptr);

            let mut result: u32 = output_frames;
            let _ = calc(self.iapo_rt_ptr, output_frames, &mut result);
            result
        }
    }

    /// 子 APO 计算输出帧数（`CalcOutputFrames`，vtable[4]）。
    pub fn calc_output_frames(&self, input_frames: u32) -> u32 {
        if self.iapo_rt_ptr.is_null() {
            return input_frames;
        }
        unsafe {
            let fn_ptr = self.vtbl_method(self.iapo_rt_ptr, 4);
            let calc: unsafe extern "system" fn(*mut c_void, u32, *mut u32) -> HRESULT =
                std::mem::transmute(fn_ptr);

            let mut result: u32 = input_frames;
            let _ = calc(self.iapo_rt_ptr, input_frames, &mut result);
            result
        }
    }

    // ── IAudioProcessingObjectConfiguration 委托 ─────────────────────────────

    /// 锁定子 APO（`LockForProcess`，vtable[3]）。
    ///
    /// # Safety
    ///
    /// `pp_inputs` / `pp_outputs` 必须指向有效的 `APO_CONNECTION_DESCRIPTOR` 指针数组，
    /// 且描述符中的 `format`（`IAudioMediaType*`）和 `buffer` 在调用期间保持有效。
    pub unsafe fn lock_for_process(
        &self,
        num_input: u32,
        pp_inputs: *mut *mut APO_CONNECTION_DESCRIPTOR,
        num_output: u32,
        pp_outputs: *mut *mut APO_CONNECTION_DESCRIPTOR,
    ) -> HRESULT {
        if self.iapo_cfg_ptr.is_null() {
            return base::E_POINTER;
        }
        unsafe {
            let fn_ptr = self.vtbl_method(self.iapo_cfg_ptr, 3);
            let lock_fn: unsafe extern "system" fn(
                *mut c_void,
                u32,
                *mut *mut APO_CONNECTION_DESCRIPTOR,
                u32,
                *mut *mut APO_CONNECTION_DESCRIPTOR,
            ) -> HRESULT = std::mem::transmute(fn_ptr);
            lock_fn(self.iapo_cfg_ptr, num_input, pp_inputs, num_output, pp_outputs)
        }
    }

    /// 解锁子 APO（`UnlockForProcess`，vtable[4]）。
    pub fn unlock_for_process(&self) -> HRESULT {
        if self.iapo_cfg_ptr.is_null() {
            return base::E_POINTER;
        }
        unsafe {
            let fn_ptr = self.vtbl_method(self.iapo_cfg_ptr, 4);
            let unlock: unsafe extern "system" fn(*mut c_void) -> HRESULT =
                std::mem::transmute(fn_ptr);
            unlock(self.iapo_cfg_ptr)
        }
    }

    // ── 内部辅助 ─────────────────────────────────────────────────────────────

    /// QueryInterface 封装。
    unsafe fn qi(
        unknown: &IUnknown,
        iid: &GUID,
        out: *mut *mut c_void,
    ) -> HRESULT {
        // SAFETY: IUnknown 的 COM 方法，iid 和 out 由调用方保证有效。
        let fn_ptr = self::ChildApo::vtbl_method_from_ref(unknown, 0);
        let qi: unsafe extern "system" fn(
            *mut c_void,
            *const GUID,
            *mut *mut c_void,
        ) -> HRESULT = std::mem::transmute(fn_ptr);

        // 获取 IUnknown 的原始指针
        let raw: *mut c_void = std::mem::transmute_copy(unknown);
        qi(raw, iid, out)
    }

    /// 释放 COM 接口指针（IUnknown::Release，vtable[2]）。
    unsafe fn release_raw(ptr: *mut c_void) {
        if ptr.is_null() {
            return;
        }
        let vtbl = *(ptr as *const *const usize);
        let release: unsafe extern "system" fn(*mut c_void) -> u32 =
            std::mem::transmute(*vtbl.add(VT_RELEASE));
        release(ptr);
    }

    /// 获取 vtable 中指定索引的方法指针。
    ///
    /// # Safety
    ///
    /// `iface_ptr` 必须是有效的 COM 接口指针。
    #[inline]
    unsafe fn vtbl_method(&self, iface_ptr: *mut c_void, index: usize) -> usize {
        let vtbl = *(iface_ptr as *const *const usize);
        *vtbl.add(index)
    }

    /// 从引用获取 vtable 方法（用于 QI 调用前没有 ChildApo 实例的场景）。
    #[inline]
    unsafe fn vtbl_method_from_ref<T>(obj: &T, index: usize) -> usize {
        let raw: *const c_void = std::mem::transmute(obj);
        let vtbl = *(raw as *const *const usize);
        *vtbl.add(index)
    }
}

impl Drop for ChildApo {
    fn drop(&mut self) {
        unsafe {
            // 释放顺序：cfg → rt → iapo
            Self::release_raw(self.iapo_cfg_ptr);
            Self::release_raw(self.iapo_rt_ptr);
            Self::release_raw(self.iapo_ptr);
        }
        self.iapo_cfg_ptr = std::ptr::null_mut();
        self.iapo_rt_ptr = std::ptr::null_mut();
        self.iapo_ptr = std::ptr::null_mut();
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造 null 指针的 ChildApo 用于测试防御性检查。
    ///
    /// # Safety
    ///
    /// 仅用于测试，不得调用任何 COM 方法。
    unsafe fn null_child_apo() -> ChildApo {
        ChildApo {
            iapo_ptr: std::ptr::null_mut(),
            iapo_rt_ptr: std::ptr::null_mut(),
            iapo_cfg_ptr: std::ptr::null_mut(),
        }
    }

    #[test]
    fn null_child_is_not_valid() {
        let apo = unsafe { null_child_apo() };
        assert!(!apo.is_valid());
    }

    #[test]
    fn null_child_get_latency_returns_zero() {
        let apo = unsafe { null_child_apo() };
        assert_eq!(apo.get_latency(), 0);
    }

    #[test]
    fn null_child_calc_input_frames_passthrough() {
        let apo = unsafe { null_child_apo() };
        assert_eq!(apo.calc_input_frames(480), 480);
    }

    #[test]
    fn null_child_calc_output_frames_passthrough() {
        let apo = unsafe { null_child_apo() };
        assert_eq!(apo.calc_output_frames(480), 480);
    }

    #[test]
    fn null_child_reset_returns_error() {
        let apo = unsafe { null_child_apo() };
        assert_eq!(apo.reset(), base::E_POINTER);
    }

    #[test]
    fn null_child_unlock_returns_error() {
        let apo = unsafe { null_child_apo() };
        assert_eq!(apo.unlock_for_process(), base::E_POINTER);
    }

    #[test]
    fn null_child_drop_safely() {
        let apo = unsafe { null_child_apo() };
        drop(apo); // 不 panic、不 access violation
    }

    // 注意：ChildApo::create 需要真实 COM 环境和已注册的 APO，
    // 无法在单元测试中运行。集成测试见 tests/integration_apo.rs。
}