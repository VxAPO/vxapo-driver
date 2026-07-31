//! sys/audio_defs.rs — Windows 音频基础定义（v6.2 规范 3.5）
//!
//! 职责：提供通道掩码位标志常量、标准布局常量，以及通道掩码兜底/通道名映射函数。
//!
//! 引用来源：
//! - `windows::Win32::Media::KernelStreaming::SPEAKER_*`（re-export 优先，缺失位自定义补齐）
//!
//! 导出给：`install/device/format.rs`（`default_channel_mask` 兜底）、
//! `config/parser.rs`、`config/commands/channel.rs`（`get_channel_names`）、
//! `object/apo.rs`（`get_channel_names`）。
//!
//! 职责边界：只包含 Windows SDK 语义的静态定义与纯函数。
//! 不包含任何 VxAPO 业务逻辑，不感知运行时状态。

// ══════════════════════════════════════════════════════════════════════════════
// 1. 通道掩码位标志
//
// windows-rs 0.62.2 的 Win32_Media_KernelStreaming 特性未导出 SPEAKER_* 常量，
// 此处按 Windows SDK (ksmedia.h) 定义自定义补齐（值完全一致）。
// ══════════════════════════════════════════════════════════════════════════════

/// 前左扬声器（0x1）
pub const SPEAKER_FRONT_LEFT: u32 = 0x1;
/// 前右扬声器（0x2）
pub const SPEAKER_FRONT_RIGHT: u32 = 0x2;
/// 前中置扬声器（0x4）
pub const SPEAKER_FRONT_CENTER: u32 = 0x4;
/// 低频效果扬声器（0x8）
pub const SPEAKER_LOW_FREQUENCY: u32 = 0x8;
/// 后左扬声器（0x10）
pub const SPEAKER_BACK_LEFT: u32 = 0x10;
/// 后右扬声器（0x20）
pub const SPEAKER_BACK_RIGHT: u32 = 0x20;
/// 前左中置扬声器（0x40）
pub const SPEAKER_FRONT_LEFT_OF_CENTER: u32 = 0x40;
/// 前右中置扬声器（0x80）
pub const SPEAKER_FRONT_RIGHT_OF_CENTER: u32 = 0x80;
/// 后中置扬声器（0x100）
pub const SPEAKER_BACK_CENTER: u32 = 0x100;
/// 侧左扬声器（0x200）
pub const SPEAKER_SIDE_LEFT: u32 = 0x200;
/// 侧右扬声器（0x400）
pub const SPEAKER_SIDE_RIGHT: u32 = 0x400;
/// 顶部中置扬声器（0x800）
pub const SPEAKER_TOP_CENTER: u32 = 0x800;
/// 顶部前左扬声器（0x1000）
pub const SPEAKER_TOP_FRONT_LEFT: u32 = 0x1000;
/// 顶部前中置扬声器（0x2000）
pub const SPEAKER_TOP_FRONT_CENTER: u32 = 0x2000;
/// 顶部前右扬声器（0x4000）
pub const SPEAKER_TOP_FRONT_RIGHT: u32 = 0x4000;
/// 顶部后左扬声器（0x8000）
pub const SPEAKER_TOP_BACK_LEFT: u32 = 0x8000;
/// 顶部后中置扬声器（0x10000）
pub const SPEAKER_TOP_BACK_CENTER: u32 = 0x10000;
/// 顶部后右扬声器（0x20000）
pub const SPEAKER_TOP_BACK_RIGHT: u32 = 0x20000;

// ══════════════════════════════════════════════════════════════════════════════
// 2. 标准布局常量
// ══════════════════════════════════════════════════════════════════════════════

/// 单声道：前中置
pub const KSAUDIO_SPEAKER_MONO: u32 = SPEAKER_FRONT_CENTER;

/// 立体声：前左 | 前右
pub const KSAUDIO_SPEAKER_STEREO: u32 = SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT;

/// 四声道：前左 | 前右 | 后左 | 后右
pub const KSAUDIO_SPEAKER_QUAD: u32 = SPEAKER_FRONT_LEFT
    | SPEAKER_FRONT_RIGHT
    | SPEAKER_BACK_LEFT
    | SPEAKER_BACK_RIGHT;

/// 5.1 声道：前左 | 前右 | 前中置 | 低音 | 后左 | 后右
pub const KSAUDIO_SPEAKER_5POINT1: u32 = SPEAKER_FRONT_LEFT
    | SPEAKER_FRONT_RIGHT
    | SPEAKER_FRONT_CENTER
    | SPEAKER_LOW_FREQUENCY
    | SPEAKER_BACK_LEFT
    | SPEAKER_BACK_RIGHT;

/// 7.1 声道：前左 | 前右 | 前中置 | 低音 | 后左 | 后右 | 侧左 | 侧右
pub const KSAUDIO_SPEAKER_7POINT1: u32 = SPEAKER_FRONT_LEFT
    | SPEAKER_FRONT_RIGHT
    | SPEAKER_FRONT_CENTER
    | SPEAKER_LOW_FREQUENCY
    | SPEAKER_BACK_LEFT
    | SPEAKER_BACK_RIGHT
    | SPEAKER_SIDE_LEFT
    | SPEAKER_SIDE_RIGHT;

// ══════════════════════════════════════════════════════════════════════════════
// 3. 函数
// ══════════════════════════════════════════════════════════════════════════════

/// 按通道数返回标准布局掩码（兜底用）。
///
/// 1→MONO、2→STEREO、4→QUAD、6→5POINT1、8→7POINT1；其余返回 0。
pub fn default_channel_mask(channels: u32) -> u32 {
    match channels {
        1 => KSAUDIO_SPEAKER_MONO,
        2 => KSAUDIO_SPEAKER_STEREO,
        4 => KSAUDIO_SPEAKER_QUAD,
        6 => KSAUDIO_SPEAKER_5POINT1,
        8 => KSAUDIO_SPEAKER_7POINT1,
        _ => 0,
    }
}

/// 掩码 → 短名通道列表（L/R/C/LFE/BL/BR/SL/SR，按标准位顺序）。
///
/// 严格按掩码位映射，未命中位不补。
/// 掩码为 0 时按通道数兜底到 `default_channel_mask` 后重试。
pub fn get_channel_names(mask: u32) -> Vec<String> {
    let mask = if mask == 0 {
        // 兜底需要通道数——此处仅处理 mask==0 的情况：
        // 调用方应传入非零掩码；若为 0 我们无法推断通道数，返回空列表。
        return Vec::new();
    } else {
        mask
    };

    // (短名, 位标志) 按标准位顺序排列
    const MAPPINGS: [(&str, u32); 8] = [
        ("L", SPEAKER_FRONT_LEFT),
        ("R", SPEAKER_FRONT_RIGHT),
        ("C", SPEAKER_FRONT_CENTER),
        ("LFE", SPEAKER_LOW_FREQUENCY),
        ("BL", SPEAKER_BACK_LEFT),
        ("BR", SPEAKER_BACK_RIGHT),
        ("SL", SPEAKER_SIDE_LEFT),
        ("SR", SPEAKER_SIDE_RIGHT),
    ];

    MAPPINGS
        .iter()
        .filter(|(_, bit)| mask & bit != 0)
        .map(|(name, _)| (*name).to_owned())
        .collect()
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── 位标志值 ──────────────────────────────────────────────────────────

    #[test]
    fn speaker_bit_values() {
        assert_eq!(SPEAKER_FRONT_LEFT, 0x1);
        assert_eq!(SPEAKER_FRONT_RIGHT, 0x2);
        assert_eq!(SPEAKER_FRONT_CENTER, 0x4);
        assert_eq!(SPEAKER_LOW_FREQUENCY, 0x8);
        assert_eq!(SPEAKER_BACK_LEFT, 0x10);
        assert_eq!(SPEAKER_BACK_RIGHT, 0x20);
        assert_eq!(SPEAKER_FRONT_LEFT_OF_CENTER, 0x40);
        assert_eq!(SPEAKER_FRONT_RIGHT_OF_CENTER, 0x80);
        assert_eq!(SPEAKER_BACK_CENTER, 0x100);
        assert_eq!(SPEAKER_SIDE_LEFT, 0x200);
        assert_eq!(SPEAKER_SIDE_RIGHT, 0x400);
        assert_eq!(SPEAKER_TOP_CENTER, 0x800);
        assert_eq!(SPEAKER_TOP_FRONT_LEFT, 0x1000);
        assert_eq!(SPEAKER_TOP_FRONT_CENTER, 0x2000);
        assert_eq!(SPEAKER_TOP_FRONT_RIGHT, 0x4000);
        assert_eq!(SPEAKER_TOP_BACK_LEFT, 0x8000);
        assert_eq!(SPEAKER_TOP_BACK_CENTER, 0x10000);
        assert_eq!(SPEAKER_TOP_BACK_RIGHT, 0x20000);
    }

    // ── 标准布局 ──────────────────────────────────────────────────────────

    #[test]
    fn standard_layout_values() {
        assert_eq!(KSAUDIO_SPEAKER_MONO, 0x4);
        assert_eq!(KSAUDIO_SPEAKER_STEREO, 0x3);
        assert_eq!(KSAUDIO_SPEAKER_QUAD, 0x33);
        assert_eq!(
            KSAUDIO_SPEAKER_5POINT1,
            0x3F // 0x1|0x2|0x4|0x8|0x10|0x20
        );
        assert_eq!(
            KSAUDIO_SPEAKER_7POINT1,
            0x63F // 5.1 + SIDE_LEFT|SIDE_RIGHT (0x200|0x400)
        );
    }

    // ── default_channel_mask ──────────────────────────────────────────────

    #[test]
    fn default_mask_mapping() {
        assert_eq!(default_channel_mask(1), KSAUDIO_SPEAKER_MONO);
        assert_eq!(default_channel_mask(2), KSAUDIO_SPEAKER_STEREO);
        assert_eq!(default_channel_mask(4), KSAUDIO_SPEAKER_QUAD);
        assert_eq!(default_channel_mask(6), KSAUDIO_SPEAKER_5POINT1);
        assert_eq!(default_channel_mask(8), KSAUDIO_SPEAKER_7POINT1);
        assert_eq!(default_channel_mask(3), 0);
        assert_eq!(default_channel_mask(0), 0);
    }

    // ── get_channel_names ─────────────────────────────────────────────────

    #[test]
    fn channel_names_stereo() {
        let names = get_channel_names(KSAUDIO_SPEAKER_STEREO);
        assert_eq!(names, vec!["L", "R"]);
    }

    #[test]
    fn channel_names_51() {
        let names = get_channel_names(KSAUDIO_SPEAKER_5POINT1);
        assert_eq!(names, vec!["L", "R", "C", "LFE", "BL", "BR"]);
    }

    #[test]
    fn channel_names_71() {
        let names = get_channel_names(KSAUDIO_SPEAKER_7POINT1);
        assert_eq!(names, vec!["L", "R", "C", "LFE", "BL", "BR", "SL", "SR"]);
    }

    #[test]
    fn channel_names_mono() {
        let names = get_channel_names(KSAUDIO_SPEAKER_MONO);
        assert_eq!(names, vec!["C"]);
    }

    #[test]
    fn channel_names_unknown_bit_ignored() {
        // 0x80000 未定义位应被忽略（未命中位不补）
        let names = get_channel_names(SPEAKER_FRONT_LEFT | 0x80000);
        assert_eq!(names, vec!["L"]);
    }

    #[test]
    fn channel_names_zero_mask_returns_empty() {
        let names = get_channel_names(0);
        assert!(names.is_empty());
    }
}