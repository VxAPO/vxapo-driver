//! install/device/format.rs — WAVEFORMATEX / EXTENSIBLE 解析（规范 5.2）
//!
//! 从注册表二进制值解析音频格式，提取通道数、采样率、位深和通道掩码。
//! 只读，不修改系统状态。

use crate::sys::audio_defs::default_channel_mask;
use crate::sys::registry::RegKey;
use crate::utils::vx_error::Result;

// ══════════════════════════════════════════════════════════════════════════════
// 常量
// ══════════════════════════════════════════════════════════════════════════════

/// WAVEFORMATEX 结构体最小长度（18 字节，含 cbSize）。
const WAVEFORMATEX_MIN_SIZE: usize = 18;

/// WAVEFORMATEXTENSIBLE 额外长度（22 字节）。
const WAVEFORMATEXTENSIBLE_EXTRA_SIZE: usize = 22;

/// WAVE_FORMAT_PCM（标准 PCM 格式，仅在测试中使用）。
#[allow(dead_code)]
const WAVE_FORMAT_PCM: u16 = 0x0001;

/// WAVE_FORMAT_EXTENSIBLE（可扩展格式，含通道掩码和子格式 GUID）。
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// PROPVARIANT 头部长度：部分端点的格式值带 8 字节前缀，需按 offset 8 再试一次
/// （口径对齐 cli 侧 probe 的 `reg::parse_waveformatex`，实机存在这类端点）。
const PROPVARIANT_HEADER_SIZE: usize = 8;

/// 取值范围护栏（与 cli probe 一致）：越界视为畸形 blob 直接失败，
/// 避免产出 `sr=0 / ch=0` 这类"看似有效"的格式喂给安装选择。
const MAX_CHANNELS: u16 = 256;
const MAX_SAMPLE_RATE: u32 = 1_000_000;
const MAX_BITS_PER_SAMPLE: u16 = 64;

// ══════════════════════════════════════════════════════════════════════════════
// 数据结构
// ══════════════════════════════════════════════════════════════════════════════

/// 解析后的音频格式信息。
#[derive(Debug, Clone)]
pub struct AudioFormat {
    /// 格式标签（WAVE_FORMAT_PCM = 1, WAVE_FORMAT_EXTENSIBLE = 0xFFFE）。
    pub format_tag: u16,
    /// 通道数。
    pub channels: u16,
    /// 采样率（Hz）。
    pub sample_rate: u32,
    /// 每通道位深（bits）。
    pub bits_per_sample: u16,
    /// 通道掩码（从 EXTENSIBLE 或注册表获取，兜底用 default_channel_mask）。
    pub channel_mask: u32,
    /// 是否为 EXTENSIBLE 格式（wFormatTag == 0xFFFE）。
    pub is_extensible: bool,
}

// ══════════════════════════════════════════════════════════════════════════════
// 纯字节解析（无 I/O，可独立测试）
// ══════════════════════════════════════════════════════════════════════════════

/// 从原始字节解析音频格式：先按 offset 0 试，失败再按 PROPVARIANT 前缀（offset 8）试。
///
/// 两步容错与 cli 侧 probe 对齐（部分端点的格式值带 8 字节前缀，只按 offset 0 解析会失败）。
pub fn parse_audio_format(bytes: &[u8], channel_mask_override: Option<u32>) -> Option<AudioFormat> {
    parse_audio_format_at(bytes, 0, channel_mask_override)
        .or_else(|| parse_audio_format_at(bytes, PROPVARIANT_HEADER_SIZE, channel_mask_override))
}

/// 按指定偏移解析音频格式。
///
/// 1. 验证长度 >= offset + 18 字节（WAVEFORMATEX 最小尺寸）
/// 2. 解析基本头（wFormatTag / nChannels / nSamplesPerSec / wBitsPerSample）
/// 3. 取值范围校验（channels/rate/bits 任一越界即返回 None）
/// 4. 若 wFormatTag == WAVE_FORMAT_EXTENSIBLE 且长度 >= 40 字节：
///    从 WAVEFORMATEXTENSIBLE 扩展部分提取 dwChannelMask（bytes[20..24]）
/// 5. 否则 dwChannelMask = 0，由调用方执行兜底链
///
/// `channel_mask_override`：外部提供的兜底通道掩码（来自注册表 DWORD 值）。
fn parse_audio_format_at(
    bytes: &[u8],
    offset: usize,
    channel_mask_override: Option<u32>,
) -> Option<AudioFormat> {
    // Step 1: 最小长度校验
    if bytes.len() < offset + WAVEFORMATEX_MIN_SIZE {
        return None;
    }
    let d = &bytes[offset..];

    // Step 2: 解析 WAVEFORMATEX 基本头（Little-Endian）
    let format_tag = u16::from_le_bytes([d[0], d[1]]);
    let channels = u16::from_le_bytes([d[2], d[3]]);
    let sample_rate = u32::from_le_bytes([d[4], d[5], d[6], d[7]]);
    let bits_per_sample = u16::from_le_bytes([d[14], d[15]]);

    // Step 3: 取值范围校验（畸形 blob 不产出"看似有效"的格式）
    if channels == 0 || channels > MAX_CHANNELS {
        return None;
    }
    if sample_rate == 0 || sample_rate > MAX_SAMPLE_RATE {
        return None;
    }
    if bits_per_sample == 0 || bits_per_sample > MAX_BITS_PER_SAMPLE {
        return None;
    }

    // Step 4: WAVEFORMATEXTENSIBLE 通道掩码（仅当 tag == EXTENSIBLE 且数据充足时提取）
    let extensible_mask = if format_tag == WAVE_FORMAT_EXTENSIBLE
        && d.len() >= WAVEFORMATEX_MIN_SIZE + WAVEFORMATEXTENSIBLE_EXTRA_SIZE
    {
        u32::from_le_bytes([d[20], d[21], d[22], d[23]])
    } else {
        0
    };

    // Step 5: 通道掩码兜底链
    let channel_mask = resolve_channel_mask(extensible_mask, channel_mask_override, channels);

    Some(AudioFormat {
        format_tag,
        channels,
        sample_rate,
        bits_per_sample,
        channel_mask,
        is_extensible: format_tag == WAVE_FORMAT_EXTENSIBLE,
    })
}

// ══════════════════════════════════════════════════════════════════════════════
// 通道掩码解析链
// ══════════════════════════════════════════════════════════════════════════════

/// 按优先级解析通道掩码：
///
/// 1. 若 `extensible_mask != 0`，使用 EXTENSIBLE 中的值
/// 2. 否则若 `registry_mask` 有值且非零，使用注册表中的值
/// 3. 否则调用 `default_channel_mask`
pub fn resolve_channel_mask(
    extensible_mask: u32,
    registry_mask: Option<u32>,
    channels: u16,
) -> u32 {
    if extensible_mask != 0 {
        return extensible_mask;
    }

    if let Some(mask) = registry_mask {
        if mask != 0 {
            return mask;
        }
    }

    // 兜底：标准布局映射
    default_channel_mask(channels as u32)
}

// ══════════════════════════════════════════════════════════════════════════════
// 注册表读取入口
// ══════════════════════════════════════════════════════════════════════════════

/// 从注册表键读取并解析音频格式。
///
/// - `format_value_name`：二进制值的名称（WAVEFORMATEX 数据）。
/// - `channel_mask_value_name`：可选 DWORD 通道掩码值名称。
///
/// 值不存在时返回 `Ok(None)`（非致命）。
pub fn read_audio_format(
    key: &RegKey,
    format_value_name: &str,
    channel_mask_value_name: Option<&str>,
) -> Result<Option<AudioFormat>> {
    // Step 1: 读取二进制值
    let raw = match key.read_binary_value(format_value_name) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    // Step 2: 读取注册表中的通道掩码（兜底链第 2 级）
    let registry_mask = if let Some(mask_name) = channel_mask_value_name {
        key.read_dword_value(mask_name).ok()
    } else {
        None
    };

    // Step 3: 解析字节流
    Ok(parse_audio_format(&raw, registry_mask))
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造 WAVEFORMATEX 18 字节（Little-Endian）。
    fn build_waveformatex(format_tag: u16, channels: u16, sample_rate: u32, bits: u16) -> Vec<u8> {
        let mut buf = Vec::with_capacity(18);
        buf.extend_from_slice(&format_tag.to_le_bytes());
        buf.extend_from_slice(&channels.to_le_bytes());
        buf.extend_from_slice(&sample_rate.to_le_bytes());
        buf.extend_from_slice(&(sample_rate * channels as u32 * bits as u32 / 8).to_le_bytes());
        buf.extend_from_slice(&(channels * bits / 8).to_le_bytes());
        buf.extend_from_slice(&bits.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // cbSize
        buf
    }

    /// 构造 WAVEFORMATEXTENSIBLE 40 字节（含 dwChannelMask）。
    fn build_waveformatextensible(
        channels: u16,
        sample_rate: u32,
        bits: u16,
        channel_mask: u32,
    ) -> Vec<u8> {
        let mut buf = Vec::with_capacity(40);
        buf.extend_from_slice(&WAVE_FORMAT_EXTENSIBLE.to_le_bytes());
        buf.extend_from_slice(&channels.to_le_bytes());
        buf.extend_from_slice(&sample_rate.to_le_bytes());
        buf.extend_from_slice(&(sample_rate * channels as u32 * bits as u32 / 8).to_le_bytes());
        buf.extend_from_slice(&(channels * bits / 8).to_le_bytes());
        buf.extend_from_slice(&bits.to_le_bytes());
        buf.extend_from_slice(&22u16.to_le_bytes()); // cbSize = 22
        buf.extend_from_slice(&bits.to_le_bytes()); // wValidBitsPerSample
        buf.extend_from_slice(&channel_mask.to_le_bytes()); // dwChannelMask
        buf.extend_from_slice(&[0u8; 16]); // SubFormat GUID
        buf
    }

    #[test]
    fn empty_bytes_returns_none() {
        assert!(parse_audio_format(&[], None).is_none());
    }

    #[test]
    fn too_short_returns_none() {
        assert!(parse_audio_format(&[0u8; 17], None).is_none());
    }

    #[test]
    fn exactly_minimum_length_parses() {
        let bytes = build_waveformatex(WAVE_FORMAT_PCM, 2, 48000, 16);
        assert_eq!(bytes.len(), 18);
        assert!(parse_audio_format(&bytes, None).is_some());
    }

    #[test]
    fn parse_stereo_pcm_48k_16bit() {
        let bytes = build_waveformatex(WAVE_FORMAT_PCM, 2, 48000, 16);
        let fmt = parse_audio_format(&bytes, None).unwrap();
        assert_eq!(fmt.format_tag, WAVE_FORMAT_PCM);
        assert_eq!(fmt.channels, 2);
        assert_eq!(fmt.sample_rate, 48000);
        assert_eq!(fmt.bits_per_sample, 16);
        assert!(!fmt.is_extensible);
    }

    #[test]
    fn parse_mono_pcm_44100_16bit() {
        let bytes = build_waveformatex(WAVE_FORMAT_PCM, 1, 44100, 16);
        let fmt = parse_audio_format(&bytes, None).unwrap();
        assert_eq!(fmt.channels, 1);
        assert_eq!(fmt.sample_rate, 44100);
    }

    #[test]
    fn parse_8ch_pcm_96000_32bit() {
        let bytes = build_waveformatex(WAVE_FORMAT_PCM, 8, 96000, 32);
        let fmt = parse_audio_format(&bytes, None).unwrap();
        assert_eq!(fmt.channels, 8);
        assert_eq!(fmt.sample_rate, 96000);
        assert_eq!(fmt.bits_per_sample, 32);
    }

    #[test]
    fn extensible_71_48k_extracts_channel_mask() {
        let bytes = build_waveformatextensible(8, 48000, 32, 0x063F);
        let fmt = parse_audio_format(&bytes, None).unwrap();
        assert_eq!(fmt.format_tag, WAVE_FORMAT_EXTENSIBLE);
        assert!(fmt.is_extensible);
        assert_eq!(fmt.channels, 8);
        assert_eq!(fmt.channel_mask, 0x063F);
    }

    #[test]
    fn extensible_stereo_extracts_mask() {
        let bytes = build_waveformatextensible(2, 44100, 16, 0x0003);
        let fmt = parse_audio_format(&bytes, None).unwrap();
        assert!(fmt.is_extensible);
        assert_eq!(fmt.channel_mask, 0x0003);
    }

    /// PROPVARIANT 前缀（8 字节）端点：offset 0 解析失败后按 offset 8 回退成功。
    #[test]
    fn offset_prefixed_blob_parses() {
        let mut bytes = vec![0xAA_u8; PROPVARIANT_HEADER_SIZE];
        bytes.extend_from_slice(&build_waveformatex(WAVE_FORMAT_PCM, 2, 48000, 24));
        let fmt = parse_audio_format(&bytes, None).unwrap();
        assert_eq!(fmt.channels, 2);
        assert_eq!(fmt.sample_rate, 48000);
        assert_eq!(fmt.bits_per_sample, 24);
    }

    /// 两个偏移都不足最小长度 → None。
    #[test]
    fn offset_prefixed_too_short_returns_none() {
        let bytes = vec![0_u8; PROPVARIANT_HEADER_SIZE + WAVEFORMATEX_MIN_SIZE - 1];
        assert!(parse_audio_format(&bytes, None).is_none());
    }

    /// 通道数 0 → 畸形，拒绝（此前会产出 ch=0 的"看似有效"格式）。
    #[test]
    fn zero_channels_rejected() {
        let bytes = build_waveformatex(WAVE_FORMAT_PCM, 0, 48000, 16);
        assert!(parse_audio_format(&bytes, None).is_none());
    }

    /// 采样率越界（> 1 MHz）→ 畸形，拒绝。
    #[test]
    fn absurd_sample_rate_rejected() {
        let bytes = build_waveformatex(WAVE_FORMAT_PCM, 2, 2_000_000, 16);
        assert!(parse_audio_format(&bytes, None).is_none());
    }

    /// 位深越界（> 64）→ 畸形，拒绝。
    #[test]
    fn absurd_bits_rejected() {
        let bytes = build_waveformatex(WAVE_FORMAT_PCM, 2, 48000, 127);
        assert!(parse_audio_format(&bytes, None).is_none());
    }

    #[test]
    fn mask_from_extensible() {
        assert_eq!(resolve_channel_mask(0x063F, None, 8), 0x063F);
    }

    #[test]
    fn mask_from_registry_when_extensible_zero() {
        assert_eq!(resolve_channel_mask(0, Some(0x00FF), 8), 0x00FF);
    }

    #[test]
    fn mask_from_default_when_both_zero() {
        let expected = default_channel_mask(2);
        assert_eq!(resolve_channel_mask(0, None, 2), expected);
    }

    #[test]
    fn mask_from_default_when_registry_zero() {
        let expected = default_channel_mask(8);
        assert_eq!(resolve_channel_mask(0, Some(0), 8), expected);
    }

    #[test]
    fn pcm_with_registry_override() {
        let bytes = build_waveformatex(WAVE_FORMAT_PCM, 8, 48000, 32);
        let fmt = parse_audio_format(&bytes, Some(0x063F)).unwrap();
        assert_eq!(fmt.channel_mask, 0x063F);
    }

    #[test]
    fn extensible_takes_precedence_over_override() {
        let bytes = build_waveformatextensible(8, 48000, 32, 0x063F);
        let fmt = parse_audio_format(&bytes, Some(0x00FF)).unwrap();
        assert_eq!(fmt.channel_mask, 0x063F);
    }

    #[test]
    fn parse_24bit_pcm() {
        let bytes = build_waveformatex(WAVE_FORMAT_PCM, 2, 96000, 24);
        let fmt = parse_audio_format(&bytes, None).unwrap();
        assert_eq!(fmt.bits_per_sample, 24);
    }
}
