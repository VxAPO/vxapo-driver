//! host/instance/audio_compat.rs — 音频格式约束检查（Note 8/9）
//!
//! 实现以下接口的格式验证逻辑：
//! - `IsInputFormatSupported` / `IsOutputFormatSupported`：采样率与位深匹配校验
//! - `LockForProcess`：通道数确定规则与掩码选择

use windows::core::GUID;
use core::ffi::c_void;

// ══════════════════════════════════════════════════════════════════════════════
// 本地 WAVEFORMATEX 定义
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

// ══════════════════════════════════════════════════════════════════════════════
// 常量定义
// ══════════════════════════════════════════════════════════════════════════════

const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: GUID = GUID::from_values(
    0x00000003, 0x0000, 0x0010,
    [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71]
);

// ══════════════════════════════════════════════════════════════════════════════
// 公开接口
// ══════════════════════════════════════════════════════════════════════════════

/// 格式兼容性检查结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatCompatibility {
    Compatible,
    AlternativeAvailable,
    Incompatible,
}

/// 检查 WAVEFORMATEX 是否表示 IEEE 浮点格式。
pub fn is_ieee_float_format(wfx: *const c_void) -> bool {
    if wfx.is_null() { return false; }
    let wfx = wfx as *const WAVEFORMATEX;
    let tag = unsafe { (*wfx).w_format_tag };
    if tag == WAVE_FORMAT_IEEE_FLOAT {
        return true;
    }
    if tag == WAVE_FORMAT_EXTENSIBLE {
        let cb_size = unsafe { (*wfx).cb_size };
        if cb_size < 22 { return false; }
        let ext_ptr = wfx as *const u8;
        let subformat_ptr = unsafe { ext_ptr.add(18) } as *const GUID;
        unsafe { *subformat_ptr == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT }
    } else {
        false
    }
}

/// 提取 WAVEFORMATEX 的基本信息：采样率、通道数、位深。
pub fn extract_format_info(wfx: *const c_void) -> Option<(u32, u32, u32)> {
    if wfx.is_null() { return None; }
    let wfx = wfx as *const WAVEFORMATEX;
    unsafe {
        let rate = (*wfx).n_samples_per_sec;
        let channels = (*wfx).n_channels as u32;
        let bits = (*wfx).w_bits_per_sample as u32;
        Some((rate, channels, bits))
    }
}

/// 检查两个音频格式是否兼容。
pub fn check_format_compatibility(
    input_wfx: *const c_void,
    output_wfx: *const c_void,
) -> FormatCompatibility {
    if input_wfx.is_null() || output_wfx.is_null() {
        return FormatCompatibility::Incompatible;
    }
    if !is_ieee_float_format(input_wfx) || !is_ieee_float_format(output_wfx) {
        return FormatCompatibility::Incompatible;
    }

    let input_wfx = input_wfx as *const WAVEFORMATEX;
    let output_wfx = output_wfx as *const WAVEFORMATEX;

    let input_rate = unsafe { (*input_wfx).n_samples_per_sec };
    let output_rate = unsafe { (*output_wfx).n_samples_per_sec };
    if input_rate != output_rate {
        return FormatCompatibility::Incompatible;
    }

    let input_channels = unsafe { (*input_wfx).n_channels };
    let output_channels = unsafe { (*output_wfx).n_channels };
    if input_channels > 2 && output_channels < input_channels {
        return FormatCompatibility::AlternativeAvailable;
    }

    FormatCompatibility::Compatible
}