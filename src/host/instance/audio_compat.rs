//! host/instance/audio_compat.rs — 音频格式约束检查（Note 8/9）
//!
//! 实现以下接口的格式验证逻辑：
//! - `IsInputFormatSupported` / `IsOutputFormatSupported`：采样率与位深匹配校验
//! - `LockForProcess`：通道数确定规则与掩码选择
//!
//! 关键约束（Note 8/9）：
//! - 不支持多于 2 通道下混到较少通道，检测到时返回输出格式替代（`S_FALSE`）
//! - 有子 APO 时使用输出通道数，无子 APO 时使用输入通道数
//! - 采集设备使用输入掩码，回放使用输出掩码，优先非零
//!
//! Phase 4 为核心格式检查占位，Phase 6 补全 `IAudioMediaType` 的完整解析。

/// 格式兼容性检查结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatCompatibility {
    /// 完全兼容。
    Compatible,
    /// 不兼容，但可提供替代格式。
    AlternativeAvailable,
    /// 完全不兼容。
    Incompatible,
}

/// 检查两个音频格式是否兼容。
///
/// 验证采样率、位深、通道数的基本约束。
///
/// Phase 4 只做基本检查。Phase 6 补全 WAVEFORMATEX 详细解析。
pub fn check_format_compatibility(
    input_rate: u32,
    output_rate: u32,
    input_bits: u32,
    output_bits: u32,
    input_channels: u32,
    output_channels: u32,
) -> FormatCompatibility {
    // 采样率必须匹配
    if input_rate != output_rate {
        return FormatCompatibility::Incompatible;
    }

    // 位深必须匹配
    if input_bits != output_bits {
        return FormatCompatibility::Incompatible;
    }

    // 不支持多于 2 通道下混到较少通道（Note 8）
    if input_channels > 2 && output_channels < input_channels {
        return FormatCompatibility::AlternativeAvailable;
    }

    FormatCompatibility::Compatible
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatible_stereo() {
        assert_eq!(
            check_format_compatibility(48000, 48000, 32, 32, 2, 2),
            FormatCompatibility::Compatible
        );
    }

    #[test]
    fn compatible_51() {
        assert_eq!(
            check_format_compatibility(48000, 48000, 32, 32, 6, 6),
            FormatCompatibility::Compatible
        );
    }

    #[test]
    fn incompatible_rate() {
        assert_eq!(
            check_format_compatibility(44100, 48000, 32, 32, 2, 2),
            FormatCompatibility::Incompatible
        );
    }

    #[test]
    fn incompatible_bits() {
        assert_eq!(
            check_format_compatibility(48000, 48000, 16, 32, 2, 2),
            FormatCompatibility::Incompatible
        );
    }

    #[test]
    fn alternative_8ch_to_2ch() {
        assert_eq!(
            check_format_compatibility(48000, 48000, 32, 32, 8, 2),
            FormatCompatibility::AlternativeAvailable
        );
    }

    #[test]
    fn compatible_2ch_to_1ch() {
        // 2→1 是允许的（不超过 2 通道的下混）
        assert_eq!(
            check_format_compatibility(48000, 48000, 32, 32, 2, 1),
            FormatCompatibility::Compatible
        );
    }

    #[test]
    fn compatible_upmix() {
        assert_eq!(
            check_format_compatibility(48000, 48000, 32, 32, 2, 8),
            FormatCompatibility::Compatible
        );
    }
}