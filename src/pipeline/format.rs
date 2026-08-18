//! pipeline/format.rs — 从 IAudioMediaType 提取 WAVEFORMATEX 信息（规范 4.3）

use crate::sys::com::apo_interfaces::IAudioMediaType;
use crate::sys::com::apo_types::{WAVEFORMATEX, WAVEFORMATEXTENSIBLE};
use crate::sys::com::prelude::GUID;
use crate::utils::vx_error::{Result, VxApoError};

/// 提取的音频格式。
#[derive(Debug, Clone)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u32,
    pub bits_per_sample: u32,
    pub channel_mask: u32,
}

const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;

/// KSDATAFORMAT_SUBTYPE_IEEE_FLOAT（{00000003-0000-0010-8000-00aa00389b71}）。
const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: GUID = GUID::from_values(
    0x0000_0003,
    0x0000,
    0x0010,
    [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71],
);

/// 从 WAVEFORMATEX（可能是指向 WAVEFORMATEXTENSIBLE 的前缀）提取逻辑格式。
pub(crate) fn format_from_wave_format(wf: &WAVEFORMATEX) -> AudioFormat {
    let channels = wf.nChannels as u32;
    let sample_rate = wf.nSamplesPerSec;
    let bits_per_sample = wf.wBitsPerSample as u32;
    let channel_mask = if wf.wFormatTag == WAVE_FORMAT_EXTENSIBLE && wf.cbSize >= 22 {
        // SAFETY: GetAudioFormat 返回指向 WAVEFORMATEX 的指针；当 cbSize>=22 时
        // 它实际是 WAVEFORMATEXTENSIBLE 前缀，可安全扩展读取。
        let ext = wf as *const WAVEFORMATEX as *const WAVEFORMATEXTENSIBLE;
        unsafe { std::ptr::read_unaligned(std::ptr::addr_of!((*ext).dwChannelMask)) }
    } else {
        crate::sys::audio_defs::default_channel_mask(channels)
    };
    AudioFormat { sample_rate, channels, bits_per_sample, channel_mask }
}

/// 判断 WAVEFORMATEX 是否表示 IEEE float。
///
/// 现代音频引擎常用 WAVEFORMATEXTENSIBLE（wFormatTag=0xFFFE），真实格式在
/// SubFormat GUID 中；只判断 wFormatTag==3 会把 float extensible 误判为不支持。
pub(crate) fn is_float_wave_format(wf: &WAVEFORMATEX) -> bool {
    if wf.wFormatTag == WAVE_FORMAT_IEEE_FLOAT {
        return true;
    }
    if wf.wFormatTag == WAVE_FORMAT_EXTENSIBLE && wf.cbSize >= 22 {
        // SAFETY: 同上，cbSize>=22 表示可安全读取 WAVEFORMATEXTENSIBLE 的 SubFormat。
        let ext = wf as *const WAVEFORMATEX as *const WAVEFORMATEXTENSIBLE;
        let sub: GUID =
            unsafe { std::ptr::read_unaligned(std::ptr::addr_of!((*ext).SubFormat)) };
        return sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
    }
    false
}

/// 从 IAudioMediaType 提取格式信息。
///
/// # Safety
/// `media_type` 必须指向有效的 IAudioMediaType COM 对象。
pub unsafe fn extract_format(media_type: *mut IAudioMediaType) -> Result<AudioFormat> {
    if media_type.is_null() {
        return Err(VxApoError::format("media_type is null"));
    }
    let wfx = media_type.as_ref().unwrap().GetAudioFormat();
    if wfx.is_null() {
        return Err(VxApoError::format("GetAudioFormat returned null"));
    }
    let fmt = (wfx as *const WAVEFORMATEX).as_ref().unwrap();
    Ok(format_from_wave_format(fmt))
}

/// 检查是否为浮点格式（WAVE_FORMAT_IEEE_FLOAT = 3）。
///
/// # Safety
/// `media_type` 必须指向有效的 IAudioMediaType COM 对象。
pub unsafe fn is_float_format(media_type: *mut IAudioMediaType) -> bool {
    if media_type.is_null() {
        return false;
    }
    let wfx = media_type.as_ref().unwrap().GetAudioFormat();
    if wfx.is_null() {
        return false;
    }
    let fmt = (wfx as *const WAVEFORMATEX).as_ref().unwrap();
    is_float_wave_format(fmt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_ieee_float() -> WAVEFORMATEX {
        WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_IEEE_FLOAT,
            nChannels: 2,
            nSamplesPerSec: 48_000,
            nAvgBytesPerSec: 384_000,
            nBlockAlign: 8,
            wBitsPerSample: 32,
            cbSize: 0,
        }
    }

    #[test]
    fn base_ieee_float_is_float() {
        let wf = base_ieee_float();
        assert!(is_float_wave_format(&wf));
        let f = format_from_wave_format(&wf);
        assert_eq!(f.sample_rate, 48_000);
        assert_eq!(f.channels, 2);
        assert_eq!(f.bits_per_sample, 32);
        assert_eq!(f.channel_mask, 0x3);
    }

    #[test]
    fn base_pcm_is_not_float() {
        let wf = WAVEFORMATEX { wFormatTag: 1, ..base_ieee_float() };
        assert!(!is_float_wave_format(&wf));
    }

    #[test]
    fn extensible_ieee_float_is_float_and_reads_mask() {
        let mut ext: WAVEFORMATEXTENSIBLE = unsafe { std::mem::zeroed() };
        ext.Format.wFormatTag = WAVE_FORMAT_EXTENSIBLE;
        ext.Format.nChannels = 6;
        ext.Format.nSamplesPerSec = 48_000;
        ext.Format.nAvgBytesPerSec = 1_152_000;
        ext.Format.nBlockAlign = 24;
        ext.Format.wBitsPerSample = 32;
        ext.Format.cbSize = 22;
        ext.Samples.wValidBitsPerSample = 32;
        ext.dwChannelMask = 0x3F;
        ext.SubFormat = KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;

        let wf = unsafe { &*(&ext as *const WAVEFORMATEXTENSIBLE as *const WAVEFORMATEX) };
        assert!(is_float_wave_format(wf));
        let f = format_from_wave_format(wf);
        assert_eq!(f.channels, 6);
        assert_eq!(f.channel_mask, 0x3F);
    }

    #[test]
    fn extensible_pcm_is_not_float() {
        let mut ext: WAVEFORMATEXTENSIBLE = unsafe { std::mem::zeroed() };
        ext.Format.wFormatTag = WAVE_FORMAT_EXTENSIBLE;
        ext.Format.nChannels = 2;
        ext.Format.nSamplesPerSec = 48_000;
        ext.Format.wBitsPerSample = 16;
        ext.Format.cbSize = 22;
        ext.Samples.wValidBitsPerSample = 16;
        ext.dwChannelMask = 0x3;
        ext.SubFormat = GUID::from_values(
            0x0000_0001,
            0x0000,
            0x0010,
            [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71],
        );

        let wf = unsafe { &*(&ext as *const WAVEFORMATEXTENSIBLE as *const WAVEFORMATEX) };
        assert!(!is_float_wave_format(wf));
    }
}
