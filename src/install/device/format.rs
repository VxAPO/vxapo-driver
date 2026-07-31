//! host/device/format.rs — WAVEFORMATEX / EXTENSIBLE 解析（Note 27）
//!
//! 从注册表二进制值解析音频格式，提取通道数、采样率、位深和通道掩码。
//!
//! 通道掩码兜底链（Note 27）：
//! 1. `WAVEFORMATEXTENSIBLE` 的 `dwChannelMask`
//! 2. 注册表 `channelMaskValueName`（DWORD）
//! 3. `pipeline::stream::channel::default_channel_mask`（Note 17）
//!
//! 此模块只做查询，不执行任何写操作（Note 23）。

use crate::utils::error::VxApoError;
use crate::sys::registry::read::RegKey;

// ══════════════════════════════════════════════════════════════════════════════
// 常量
// ══════════════════════════════════════════════════════════════════════════════

/// WAVEFORMATEX 结构体最小长度（18 字节，含 cbSize）。
const WAVEFORMATEX_MIN_SIZE: usize = 18;

/// WAVEFORMATEXTENSIBLE 额外长度（22 字节：cbSize 值 = wValidBitsPerSample(2) + dwChannelMask(4) + SubFormat(16)）。
/// 总结构体 = WAVEFORMATEX(18) + 额外(22) = 40 字节。
const WAVEFORMATEXTENSIBLE_EXTRA_SIZE: usize = 22;

/// WAVE_FORMAT_PCM（标准 PCM 格式）。
const WAVE_FORMAT_PCM: u16 = 0x0001;

/// WAVE_FORMAT_EXTENSIBLE（可扩展格式，含通道掩码和子格式 GUID）。
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

// ══════════════════════════════════════════════════════════════════════════════
// 数据结构
// ══════════════════════════════════════════════════════════════════════════════

/// 解析后的音频格式信息。
///
/// 从注册表二进制值解析，供 device/ 其他模块使用。
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

/// 从原始字节解析音频格式。
///
/// # 解析流程（Note 27）
///
/// 1. 验证长度 >= 18 字节（WAVEFORMATEX 最小尺寸）
/// 2. 解析基本头（wFormatTag / nChannels / nSamplesPerSec / wBitsPerSample）
/// 3. 若 wFormatTag == WAVE_FORMAT_EXTENSIBLE 且长度 >= 40 字节：
///    从 WAVEFORMATEXTENSIBLE 扩展部分提取 dwChannelMask（bytes[20..24]）
/// 4. 否则 dwChannelMask = 0，由调用方执行兜底链
///
/// # 参数
///
/// - `bytes`：注册表二进制值原始字节
/// - `channel_mask_override`：外部提供的兜底通道掩码（来自注册表 channelMaskValueName 或 None）
///
/// # 返回
///
/// - `Some(AudioFormat)`：成功解析
/// - `None`：长度不足（< 18 字节）
pub fn parse_audio_format(bytes: &[u8], channel_mask_override: Option<u32>) -> Option<AudioFormat> {
    // Step 1: 最小长度校验
    if bytes.len() < WAVEFORMATEX_MIN_SIZE {
        return None;
    }

    // Step 2: 解析 WAVEFORMATEX 基本头（Little-Endian）
    let format_tag = u16::from_le_bytes([bytes[0], bytes[1]]);
    let channels = u16::from_le_bytes([bytes[2], bytes[3]]);
    let sample_rate = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    let bits_per_sample = u16::from_le_bytes([bytes[14], bytes[15]]);

    // Step 3: WAVEFORMATEXTENSIBLE 通道掩码（仅当 tag == EXTENSIBLE 且数据充足时提取）
    let extensible_mask = if format_tag == WAVE_FORMAT_EXTENSIBLE
        && bytes.len() >= WAVEFORMATEX_MIN_SIZE + WAVEFORMATEXTENSIBLE_EXTRA_SIZE
    {
        u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]])
    } else {
        0
    };

    // Step 4: 通道掩码兜底链（Note 27 第 3-4 步）
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
// 通道掩码解析链（Note 27 第 3-4 步 + Note 17 兜底）
// ══════════════════════════════════════════════════════════════════════════════

/// 按优先级解析通道掩码。
///
/// 1. 若 `extensible_mask != 0`，使用 EXTENSIBLE 中的值
/// 2. 否则若 `registry_mask` 有值且非零，使用注册表中的值
/// 3. 否则调用 `default_channel_mask`（Note 17）
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

    // Note 17: 掩码为 0 时生成默认值
    crate::pipeline::stream::channel::default_channel_mask(channels as u32)
}

// ══════════════════════════════════════════════════════════════════════════════
// 注册表读取入口（Note 27 完整流程 + Note 48 读取操作）
// ══════════════════════════════════════════════════════════════════════════════

/// 从注册表键读取并解析音频格式。
///
/// # 解析流程（Note 27 完整链路）
///
/// 1. 读取 `format_value_name` 二进制值（`RegKey::read_binary_value`，Note 48）
/// 2. 值不存在时返回 `Ok(None)`（端点无格式信息，非致命）
/// 3. 调用 `parse_audio_format` 解析 WAVEFORMATEX / EXTENSIBLE
/// 4. 若 EXTENSIBLE 中 dwChannelMask == 0 且提供了 `channel_mask_value_name`：
///    读取注册表 DWORD 值作为兜底
/// 5. 仍为 0 时由 `parse_audio_format` 内部调用 `default_channel_mask`（Note 17）
///
/// # 返回
///
/// - `Ok(Some(AudioFormat))`：成功解析
/// - `Ok(None)`：值不存在或长度不足（非致命，跳过该端点）
/// - `Err(VxApoError)`：注册表 I/O 错误
pub fn read_audio_format(
    key: &RegKey,
    format_value_name: &str,
    channel_mask_value_name: Option<&str>,
) -> Result<Option<AudioFormat>, VxApoError> {
    // Step 1: 读取二进制值（Note 48：readBinaryValue 在 reg_read.rs 实现）
    let raw = match key.read_binary_value(format_value_name) {
        Ok(v) => v,
        Err(_) => return Ok(None), // 值不存在或 I/O 错误，跳过
    };

    // Step 2: 若需要，先读取注册表中的通道掩码（兜底链第 2 级）
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
        // WAVEFORMATEX 头（cbSize = 22）
        buf.extend_from_slice(&WAVE_FORMAT_EXTENSIBLE.to_le_bytes());
        buf.extend_from_slice(&channels.to_le_bytes());
        buf.extend_from_slice(&sample_rate.to_le_bytes());
        buf.extend_from_slice(&(sample_rate * channels as u32 * bits as u32 / 8).to_le_bytes());
        buf.extend_from_slice(&(channels * bits / 8).to_le_bytes());
        buf.extend_from_slice(&bits.to_le_bytes());
        buf.extend_from_slice(&22u16.to_le_bytes()); // cbSize = 22
        // WAVEFORMATEXTENSIBLE 扩展部分
        buf.extend_from_slice(&bits.to_le_bytes()); // wValidBitsPerSample
        buf.extend_from_slice(&channel_mask.to_le_bytes()); // dwChannelMask
        buf.extend_from_slice(&[0u8; 16]); // SubFormat GUID（测试中不关心）
        buf
    }

    // ── 长度校验（Note 27 第 1 步）──

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

    // ── 基本解析（Note 27 第 2 步）──

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

    // ── EXTENSIBLE 解析（Note 27 第 3 步）──

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
        assert_eq!(fmt.channel_mask, 0x0003); // SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT
    }

    // ── 通道掩码兜底链（Note 27 第 3-4 步 + Note 17）──

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
        // Note 17: 掩码为 0 时生成默认值
        let expected = crate::pipeline::stream::channel::default_channel_mask(2);
        assert_eq!(resolve_channel_mask(0, None, 2), expected);
    }

    #[test]
    fn mask_from_default_when_registry_zero() {
        let expected = crate::pipeline::stream::channel::default_channel_mask(8);
        assert_eq!(resolve_channel_mask(0, Some(0), 8), expected);
    }

    // ── 非 EXTENSIBLE 格式的 cbSize（不提取通道掩码）──

    #[test]
    fn pcm_with_extra_cb_size_ignored() {
        // cbSize = 8（非 EXTENSIBLE 格式），不应提取通道掩码
        let mut bytes = build_waveformatex(WAVE_FORMAT_PCM, 2, 48000, 16);
        bytes[16] = 8; // cbSize = 8
        bytes.extend_from_slice(&[0xFFu8; 8]); // 额外数据（不应解析）

        let fmt = parse_audio_format(&bytes, None).unwrap();
        assert!(!fmt.is_extensible);
        // 通道掩码来自 default_channel_mask（因为无 EXTENSIBLE、无 override）
        let expected = crate::pipeline::stream::channel::default_channel_mask(2);
        assert_eq!(fmt.channel_mask, expected);
    }

    // ── parse_audio_format 配合 override ──

    #[test]
    fn pcm_with_registry_override() {
        let bytes = build_waveformatex(WAVE_FORMAT_PCM, 8, 48000, 32);
        let fmt = parse_audio_format(&bytes, Some(0x063F)).unwrap();

        // PCM 非 EXTENSIBLE，通道掩码来自 registry override
        assert_eq!(fmt.channel_mask, 0x063F);
    }

    #[test]
    fn extensible_takes_precedence_over_override() {
        let bytes = build_waveformatextensible(8, 48000, 32, 0x063F);
        let fmt = parse_audio_format(&bytes, Some(0x00FF)).unwrap();

        // EXTENSIBLE 的 dwChannelMask 优先于注册表 override
        assert_eq!(fmt.channel_mask, 0x063F);
    }

    // ── 边界值 ──

    #[test]
    fn max_sample_rate() {
        let bytes = build_waveformatex(WAVE_FORMAT_PCM, 2, 384000, 32);
        let fmt = parse_audio_format(&bytes, None).unwrap();
        assert_eq!(fmt.sample_rate, 384000);
    }

    #[test]
    fn parse_24bit_pcm() {
        let bytes = build_waveformatex(WAVE_FORMAT_PCM, 2, 96000, 24);
        let fmt = parse_audio_format(&bytes, None).unwrap();
        assert_eq!(fmt.bits_per_sample, 24);
    }

    // ── resolve_channel_mask 单元测试 ──

    #[test]
    fn resolve_extensible_nonzero_wins() {
        // EXTENSIBLE 有值时忽略 override
        assert_eq!(resolve_channel_mask(0x0003, Some(0x063F), 2), 0x0003);
    }

    #[test]
    fn resolve_fallback_to_51_default() {
        let expected = crate::pipeline::stream::channel::default_channel_mask(6);
        assert_eq!(resolve_channel_mask(0, None, 6), expected);
    }
}