//! pipeline/format.rs — 从 IAudioMediaType 提取 WAVEFORMATEX 信息（v6.2 规范 4.3）

use crate::utils::vx_error::{Result, VxApoError};
use windows::Win32::Media::Audio::Apo::IAudioMediaType;

/// 提取的音频格式。
#[derive(Debug, Clone)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u32,
    pub bits_per_sample: u32,
    pub channel_mask: u32,
}

/// WAVEFORMATEX 布局（只读提取用，字面匹配 SDK）。
#[repr(C)]
struct WaveFormatEx {
    w_format_tag: u16,
    n_channels: u16,
    n_samples_per_sec: u32,
    n_avg_bytes_per_sec: u32,
    n_block_align: u16,
    w_bits_per_sample: u16,
    cb_size: u16,
}

const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

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
    let fmt = (wfx as *const WaveFormatEx).as_ref().unwrap();
    let channels = fmt.n_channels as u32;
    let sample_rate = fmt.n_samples_per_sec;
    let bits_per_sample = fmt.w_bits_per_sample as u32;
    let channel_mask = if fmt.w_format_tag == WAVE_FORMAT_EXTENSIBLE && fmt.cb_size >= 22 {
        let ext_ptr = wfx as *const u8;
        std::ptr::read_unaligned(ext_ptr.add(34) as *const u32)
    } else {
        crate::sys::audio_defs::default_channel_mask(channels)
    };
    Ok(AudioFormat { sample_rate, channels, bits_per_sample, channel_mask })
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
    let fmt = (wfx as *const WaveFormatEx).as_ref().unwrap();
    fmt.w_format_tag == 3
}