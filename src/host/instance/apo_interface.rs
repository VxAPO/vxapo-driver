//! host/instance/apo_interface.rs — IAudioProcessingObject 实现
//!
//! 实现 `IAudioProcessingObject` 接口：
//! - `Reset`：重置内部处理状态
//! - `GetLatency`：返回 APO 引入的延迟
//! - `GetRegistrationProperties`：返回 APO 注册属性（`host/instance/reg_props.rs`，Note 42）
//! - `IsInputFormatSupported` / `IsOutputFormatSupported`：格式协商（Note 8）
//! - `GetInputChannelCount`：返回输入通道数
//!
//! Phase 4 初期基于 Phase 2 的 `ApoObject` 最小 COM 对象扩展。
//! vtable 由 `#[implement]` 宏自动生成，聚合模式遵循 Note 3。
//!
//! 此模块实现 `IAudioProcessingObject` 的接口层逻辑，
//! 实时处理委托 `host/instance/apo_rt.rs`（Note 60），
//! 配置管理委托 `host/instance/apo_conf.rs`（Note 9）。

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::{GUID, HRESULT, IUnknown};
use windows_core::Interface;

use crate::sys::com::prelude;
use crate::sys::com::apo_abi::{APO_REG_PROPERTIES, REFERENCE_TIME};
use crate::host::instance::reg_props::{props_for_clsid};
use crate::host::instance::object::ApoObjectState;
use crate::host::instance::ref_count as inst_count;

// ══════════════════════════════════════════════════════════════════════════════
// ApoObject — Phase 4 COM 对象（扩展 Phase 2 的最小实现）
// ══════════════════════════════════════════════════════════════════════════════

/// APO COM 对象。
///
/// 持有 `ApoObjectState` 和 COM 引用计数。
///
/// Phase 4 初期手动管理引用计数和 QI。
/// 后续切换到 `#[implement]` 宏自动管理（Note 3）。
pub struct ApoObject {
    /// APO 核心状态。
    pub state: ApoObjectState,
    /// 自身 COM 引用计数。
    ref_count: AtomicU32,
    /// 延迟（采样数），由 DSP 过滤器链汇总。
    latency_samples: u32,
    /// 子 APO（Phase 6，init.rs 创建后设置）。
    pub child_apo: Option<crate::host::instance::apo_child::ChildApo>,
}

impl ApoObject {
    /// 创建新的 APO 对象，引用计数初始化为 1（Note 3）。
    pub fn new(clsid: GUID) -> Self {
        inst_count::increment();
        Self {
            state: ApoObjectState::new(clsid),
            ref_count: AtomicU32::new(1),
            latency_samples: 0,
            child_apo: None, // Phase 6: init 阶段设置
        }
    }

    /// 获取注册属性（`GetRegistrationProperties` 的实现）。
    pub fn get_registration_properties(&self) -> Option<&'static APO_REG_PROPERTIES> {
        props_for_clsid(&self.state.clsid)
    }

    /// 重置内部状态（`Reset` 的实现）。
    pub fn reset(&mut self) {
        self.state.unlock_for_process();
        self.latency_samples = 0;
        // Phase 6: 重置子 APO
        if let Some(ref child) = self.child_apo {
            let _ = child.reset();
        }
    }

    /// 获取延迟（`GetLatency` 的实现）。
    ///
    /// 返回值以 `REFERENCE_TIME`（100 纳秒）为单位。
    /// 延迟来源：DSP 过滤器链的延迟汇总。
    pub fn get_latency(&self, sample_rate: u32) -> REFERENCE_TIME {
        // 采样数 → REFERENCE_TIME：
        // latency_ref_time = samples * 10_000_000 / sample_rate
        let own = if sample_rate == 0 || self.latency_samples == 0 {
            0
        } else {
            self.latency_samples as i64 * 10_000_000 / sample_rate as i64
        };

        // Phase 6: 叠加子 APO 延迟
        let child = self.child_apo
            .as_ref()
            .map(|c| c.get_latency())
            .unwrap_or(0);

        own + child
    }

    /// 设置延迟采样数（由 DSP 过滤器链汇总后调用）。
    pub fn set_latency_samples(&mut self, samples: u32) {
        self.latency_samples = samples;
    }

    /// 获取输入通道数（`GetInputChannelCount` 的实现）。
    pub fn get_input_channel_count(&self) -> u32 {
        if self.state.is_locked {
            self.state.input_channel_count
        } else {
            0
        }
    }

    // ── IUnknown（Phase 4 手动管理，Phase 6+ 切换 #[implement]） ────────────

    /// `QueryInterface` 实现。
    pub fn query_interface(
        &self,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> HRESULT {
        // SAFETY: riid 由调用方保证有效。
        let iid = unsafe { *riid };

        if iid == IUnknown::IID
            || iid == crate::sys::com::apo_abi::IID_IAPO
        {
            // SAFETY: ppv 输出参数，写入自身指针。
            unsafe {
                *ppv = self as *const Self as *mut c_void;
            }
            self.add_ref();
            prelude::S_OK
        } else if iid == crate::sys::com::apo_abi::IID_IAPO_RT
            || iid == crate::sys::com::apo_abi::IID_IAPO_CONFIG
        {
            // Phase 4 支持同一对象实现三个接口（COM 聚合模型）
            unsafe {
                *ppv = self as *const Self as *mut c_void;
            }
            self.add_ref();
            prelude::S_OK
        } else {
            unsafe {
                *ppv = std::ptr::null_mut();
            }
            prelude::E_NOINTERFACE
        }
    }

    /// `AddRef` 实现。
    pub fn add_ref(&self) -> u32 {
        self.ref_count.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// `Release` 实现。
    ///
    /// 引用计数归零时返回 `true`，调用方负责释放内存。
    pub fn release(&self) -> (u32, bool) {
        let prev = self.ref_count.fetch_sub(1, Ordering::SeqCst);
        let new_count = prev - 1;
        (new_count, new_count == 0)
    }

    /// 当前引用计数。
    pub fn ref_count(&self) -> u32 {
        self.ref_count.load(Ordering::SeqCst)
    }
}

impl Drop for ApoObject {
    fn drop(&mut self) {
        inst_count::decrement();
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// IsInputFormatSupported / IsOutputFormatSupported 逻辑（Note 8）
// ══════════════════════════════════════════════════════════════════════════════

/// 格式协商结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatSupportResult {
    /// 完全支持（S_OK）
    Supported,
    /// 部分支持，返回替代格式（S_FALSE）
    ///
    /// 不支持多于 2 通道下混到较少通道时返回替代（Note 8）。
    UnsupportedWithAlternative,
    /// 完全不支持（E_NOTIMPL 或其他错误码）
    Unsupported,
}

/// 格式协商参数。
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

/// 检查格式是否受支持（Note 8）。
///
/// 规则：
/// - 采样率必须匹配
/// - 位深必须匹配
/// - 不支持多于 2 通道下混到较少通道
/// - 通道数 ≤ 2 时允许任何通道组合
pub fn check_format_support(neg: &FormatNegotiation) -> FormatSupportResult {
    // 采样率必须匹配
    if neg.input_sample_rate != neg.output_sample_rate {
        return FormatSupportResult::Unsupported;
    }

    // 位深必须匹配
    if neg.input_bits_per_sample != neg.output_bits_per_sample {
        return FormatSupportResult::Unsupported;
    }

    // 不支持多于 2 通道下混到较少通道（Note 8）
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
    use windows_core::Interface;
    use crate::sys::com::apo_abi::AUDIO_FLOW_TYPE;
    use crate::host::instance::reg_props::{CLSID_VXAPO_PRE_MIX, CLSID_VXAPO_POST_MIX};
    use super::*;

    // ── ApoObject 创建 ──────────────────────────────────────────────────────

    #[test]
    fn apo_object_new() {
        inst_count::reset_for_test();
        let obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        assert_eq!(obj.ref_count(), 1);
        assert!(obj.state.is_pre_mix());
        assert_eq!(inst_count::get(), 1);
        drop(obj);
        assert_eq!(inst_count::get(), 0);
    }

    #[test]
    fn apo_object_postmix() {
        inst_count::reset_for_test();
        let obj = ApoObject::new(CLSID_VXAPO_POST_MIX);
        assert!(obj.state.is_post_mix());
        drop(obj);
    }

    // ── 引用计数 ────────────────────────────────────────────────────────────

    #[test]
    fn ref_count_lifecycle() {
        inst_count::reset_for_test();
        let obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        assert_eq!(obj.ref_count(), 1);

        obj.add_ref();
        assert_eq!(obj.ref_count(), 2);

        let (count, should_drop) = obj.release();
        assert_eq!(count, 1);
        assert!(!should_drop);

        let (count, should_drop) = obj.release();
        assert_eq!(count, 0);
        assert!(should_drop);

        drop(obj);
        assert_eq!(inst_count::get(), 0);
    }

    // ── QueryInterface ──────────────────────────────────────────────────────

    #[test]
    fn qi_iunknown() {
        inst_count::reset_for_test();
        let obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        let mut ppv: *mut c_void = std::ptr::null_mut();
        let hr = obj.query_interface(&IUnknown::IID, &mut ppv);
        assert_eq!(hr, prelude::S_OK);
        assert!(!ppv.is_null());
        assert_eq!(obj.ref_count(), 2);
        obj.release();
        drop(obj);
    }

    #[test]
    fn qi_iapo() {
        inst_count::reset_for_test();
        let obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        let mut ppv: *mut c_void = std::ptr::null_mut();
        let hr = obj.query_interface(&crate::sys::com::apo_abi::IID_IAPO, &mut ppv);
        assert_eq!(hr, prelude::S_OK);
        assert!(!ppv.is_null());
        obj.release();
        drop(obj);
    }

    #[test]
    fn qi_iapo_rt() {
        inst_count::reset_for_test();
        let obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        let mut ppv: *mut c_void = std::ptr::null_mut();
        let hr = obj.query_interface(&crate::sys::com::apo_abi::IID_IAPO_RT, &mut ppv);
        assert_eq!(hr, prelude::S_OK);
        obj.release();
        drop(obj);
    }

    #[test]
    fn qi_iapo_config() {
        inst_count::reset_for_test();
        let obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        let mut ppv: *mut c_void = std::ptr::null_mut();
        let hr = obj.query_interface(&crate::sys::com::apo_abi::IID_IAPO_CONFIG, &mut ppv);
        assert_eq!(hr, prelude::S_OK);
        obj.release();
        drop(obj);
    }

    #[test]
    fn qi_unsupported() {
        inst_count::reset_for_test();
        let obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        let mut ppv: *mut c_void = std::ptr::null_mut();
        let unknown = GUID::from_values(0xDEADBEEF, 0x1234, 0x5678,
            [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]);
        let hr = obj.query_interface(&unknown, &mut ppv);
        assert_eq!(hr, prelude::E_NOINTERFACE);
        assert!(ppv.is_null());
        drop(obj);
    }

    // ── 注册属性 ────────────────────────────────────────────────────────────

    #[test]
    fn get_registration_properties_premix() {
        inst_count::reset_for_test();
        let obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        let props = obj.get_registration_properties().unwrap();
        assert_eq!(props.clsid, CLSID_VXAPO_PRE_MIX);
        assert_eq!(props.audio_flow_type, AUDIO_FLOW_TYPE::RENDER);
        drop(obj);
    }

    #[test]
    fn get_registration_properties_postmix() {
        inst_count::reset_for_test();
        let obj = ApoObject::new(CLSID_VXAPO_POST_MIX);
        let props = obj.get_registration_properties().unwrap();
        assert_eq!(props.clsid, CLSID_VXAPO_POST_MIX);
        drop(obj);
    }

    // ── Reset ───────────────────────────────────────────────────────────────

    #[test]
    fn reset_clears_state() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        obj.state.lock_for_process(48000, 2, 2, 0x3, 32);
        obj.set_latency_samples(256);
        assert!(obj.state.is_locked);
        assert_eq!(obj.latency_samples, 256);

        obj.reset();
        assert!(!obj.state.is_locked);
        assert_eq!(obj.latency_samples, 0);
        drop(obj);
    }

    // ── 延迟 ────────────────────────────────────────────────────────────────

    #[test]
    fn get_latency_zero() {
        inst_count::reset_for_test();
        let obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        assert_eq!(obj.get_latency(48000), 0);
        drop(obj);
    }

    #[test]
    fn get_latency_calculated() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        obj.set_latency_samples(480);

        // 480 samples @ 48000 Hz = 10ms = 100_000 REFERENCE_TIME (100ns units)
        let latency = obj.get_latency(48000);
        assert_eq!(latency, 100_000);
        drop(obj);
    }

    #[test]
    fn get_latency_96k() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        obj.set_latency_samples(960);
        // 960 samples @ 96000 Hz = 10ms = 100_000
        let latency = obj.get_latency(96000);
        assert_eq!(latency, 100_000);
        drop(obj);
    }

    #[test]
    fn get_latency_zero_sample_rate() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        obj.set_latency_samples(480);
        assert_eq!(obj.get_latency(0), 0);
        drop(obj);
    }

    // ── 输入通道数 ──────────────────────────────────────────────────────────

    #[test]
    fn get_input_channel_count_locked() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        obj.state.lock_for_process(48000, 6, 6, 0x3F, 32);
        assert_eq!(obj.get_input_channel_count(), 6);
        drop(obj);
    }

    #[test]
    fn get_input_channel_count_unlocked() {
        inst_count::reset_for_test();
        let obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        assert_eq!(obj.get_input_channel_count(), 0);
        drop(obj);
    }

    // ── 格式协商（Note 8） ──────────────────────────────────────────────────

    #[test]
    fn format_supported_stereo() {
        let neg = FormatNegotiation {
            input_channels: 2,
            output_channels: 2,
            input_sample_rate: 48000,
            output_sample_rate: 48000,
            input_bits_per_sample: 32,
            output_bits_per_sample: 32,
            input_channel_mask: 0x3,
            output_channel_mask: 0x3,
        };
        assert_eq!(check_format_support(&neg), FormatSupportResult::Supported);
    }

    #[test]
    fn format_supported_51() {
        let neg = FormatNegotiation {
            input_channels: 6,
            output_channels: 6,
            input_sample_rate: 48000,
            output_sample_rate: 48000,
            input_bits_per_sample: 32,
            output_bits_per_sample: 32,
            input_channel_mask: 0x3F,
            output_channel_mask: 0x3F,
        };
        assert_eq!(check_format_support(&neg), FormatSupportResult::Supported);
    }

    #[test]
    fn format_unsupported_rate_mismatch() {
        let neg = FormatNegotiation {
            input_channels: 2,
            output_channels: 2,
            input_sample_rate: 44100,
            output_sample_rate: 48000,
            input_bits_per_sample: 32,
            output_bits_per_sample: 32,
            input_channel_mask: 0x3,
            output_channel_mask: 0x3,
        };
        assert_eq!(check_format_support(&neg), FormatSupportResult::Unsupported);
    }

    #[test]
    fn format_unsupported_bits_mismatch() {
        let neg = FormatNegotiation {
            input_channels: 2,
            output_channels: 2,
            input_sample_rate: 48000,
            output_sample_rate: 48000,
            input_bits_per_sample: 16,
            output_bits_per_sample: 32,
            input_channel_mask: 0x3,
            output_channel_mask: 0x3,
        };
        assert_eq!(check_format_support(&neg), FormatSupportResult::Unsupported);
    }

    #[test]
    fn format_downmix_too_many_channels() {
        // Note 8：不支持多于 2 通道下混到较少通道
        let neg = FormatNegotiation {
            input_channels: 8,
            output_channels: 2,
            input_sample_rate: 48000,
            output_sample_rate: 48000,
            input_bits_per_sample: 32,
            output_bits_per_sample: 32,
            input_channel_mask: 0xFF,
            output_channel_mask: 0x3,
        };
        assert_eq!(
            check_format_support(&neg),
            FormatSupportResult::UnsupportedWithAlternative
        );
    }

    #[test]
    fn format_downmix_2_to_1_allowed() {
        // 2ch → 1ch 是允许的（不超过 2 通道的下混）
        let neg = FormatNegotiation {
            input_channels: 2,
            output_channels: 1,
            input_sample_rate: 48000,
            output_sample_rate: 48000,
            input_bits_per_sample: 32,
            output_bits_per_sample: 32,
            input_channel_mask: 0x3,
            output_channel_mask: 0x4,
        };
        assert_eq!(check_format_support(&neg), FormatSupportResult::Supported);
    }

    #[test]
    fn format_upmix_allowed() {
        // 上混不受限制
        let neg = FormatNegotiation {
            input_channels: 2,
            output_channels: 8,
            input_sample_rate: 48000,
            output_sample_rate: 48000,
            input_bits_per_sample: 32,
            output_bits_per_sample: 32,
            input_channel_mask: 0x3,
            output_channel_mask: 0xFF,
        };
        assert_eq!(check_format_support(&neg), FormatSupportResult::Supported);
    }

    // ── INST_COUNT 汇总 ─────────────────────────────────────────────────────

    #[test]
    fn inst_count_multiple_objects() {
        inst_count::reset_for_test();
        let obj1 = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        let obj2 = ApoObject::new(CLSID_VXAPO_POST_MIX);
        assert_eq!(inst_count::get(), 2);
        drop(obj1);
        assert_eq!(inst_count::get(), 1);
        drop(obj2);
        assert_eq!(inst_count::get(), 0);
    }

    #[test]
    fn get_latency_with_child() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        obj.set_latency_samples(480);
        // 无子 APO 时延迟只包含自身
        let latency_no_child = obj.get_latency(48000);
        assert_eq!(latency_no_child, 100_000); // 480 samples @ 48kHz = 10ms

        // child_apo 为 None 时不增加延迟
        assert!(obj.child_apo.is_none());
        drop(obj);
    }

    #[test]
    fn apo_object_has_no_child_by_default() {
        inst_count::reset_for_test();
        let obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        assert!(obj.child_apo.is_none());
        drop(obj);
    }
}