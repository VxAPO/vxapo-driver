//! object/apo/negotiate.rs — APO 格式协商
//!
//! 职责：从 IAudioMediaType 提取格式、执行浮点/通道数/降混检查。
//! 不包含 COM 接口方法，由 `interfaces.rs` / `apo.rs` 调用。

use windows::core::Ref;

use crate::pipeline::format::{extract_format, is_float_format, AudioFormat};
use crate::sys::com::apo_interfaces::IAudioMediaType;
use crate::sys::com::apo_types::{
    APOERR_FORMAT_NOT_SUPPORTED, APOERR_INVALID_CONNECTION_FORMAT,
};
use windows::core::Result;

/// 格式协商独立属性检查（object 7.1.16，v7.7 修订）。
///
/// `IsInputFormatSupported`/`IsOutputFormatSupported` 由 Windows 引擎在**格式协商阶段**
/// 调用，**早于 LockForProcess**——此时 `pipeline_context` 为全零 `PipelineContext::new()`，
/// **禁止依赖 pipeline_context 做等值比较**。
pub(crate) fn extract_format_ref(
    media_ref: &Ref<IAudioMediaType>,
) -> Result<AudioFormat> {
    let Some(mt) = media_ref.as_ref() else {
        return Err(windows::core::Error::from(APOERR_INVALID_CONNECTION_FORMAT));
    };
    let mt_ptr = mt as *const IAudioMediaType as *mut IAudioMediaType;
    // Safety: mt 为有效 COM 对象（引擎传入的自窗口指针）。
    unsafe { extract_format(mt_ptr) }
        .map_err(|_| windows::core::Error::from(APOERR_FORMAT_NOT_SUPPORTED))
}

/// 基础格式检查（EAPO 对齐，EqualizerAPO.cpp IsInputFormatSupported）：
/// - 仅拒绝非 float（DSP 链为 f32 处理）
/// - 通道 1~8（DSP 链真实能力）
/// - **不设采样率限制**——EAPO 基类不限制采样率。
pub(crate) fn check_format_supported(
    p_requested: &Ref<IAudioMediaType>,
) -> Result<()> {
    let fmt = extract_format_ref(p_requested)?;
    let Some(req) = p_requested.as_ref() else {
        return Err(windows::core::Error::from(APOERR_INVALID_CONNECTION_FORMAT));
    };
    let mt_ptr = req as *const IAudioMediaType as *mut IAudioMediaType;
    // 浮点格式检查（WAVE_FORMAT_IEEE_FLOAT）。
    if !unsafe { is_float_format(mt_ptr) } {
        return Err(windows::core::Error::from(APOERR_FORMAT_NOT_SUPPORTED));
    }
    // 通道数范围：1 ~ 8。
    if fmt.channels == 0 || fmt.channels > 8 {
        return Err(windows::core::Error::from(APOERR_FORMAT_NOT_SUPPORTED));
    }
    Ok(())
}
