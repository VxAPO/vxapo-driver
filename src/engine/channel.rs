//! engine/channel.rs — 通道工具（Note 17）
//!
//! 提供通道名称生成与默认掩码回退：
//! - `get_channel_names(channel_count, channel_mask)`：根据通道数和掩码生成
//!   L / R / C / LFE / SL / SR 等标准通道名称
//! - `default_channel_mask(channel_count)`：掩码为 0 时生成对应通道数的默认掩码
//!
//! 本模块同时被 `device/` 与 `engine/` 引用——`device/format.rs` 通过
//! `crate::engine::channel` 导入，作为通道掩码兜底链的最后一级（Note 27）。
//!
//! 此模块为纯函数，无状态，可在任意线程安全调用。

// ══════════════════════════════════════════════════════════════════════════════
// Windows 通道掩码常量（KSAUDIO_SPEAKER_*）
//
// 来自 ksmedia.h，与 dwChannelMask 格式一致。
// 每个位代表一个物理扬声器位置。
// ══════════════════════════════════════════════════════════════════════════════

/// Front Left
pub const SPEAKER_FRONT_LEFT: u32 = 0x0000_0001;
/// Front Right
pub const SPEAKER_FRONT_RIGHT: u32 = 0x0000_0002;
/// Front Center
pub const SPEAKER_FRONT_CENTER: u32 = 0x0000_0004;
/// Low Frequency (LFE)
pub const SPEAKER_LOW_FREQUENCY: u32 = 0x0000_0008;
/// Back Left (Surround Left)
pub const SPEAKER_BACK_LEFT: u32 = 0x0000_0010;
/// Back Right (Surround Right)
pub const SPEAKER_BACK_RIGHT: u32 = 0x0000_0020;
/// Front Left of Center
pub const SPEAKER_FRONT_LEFT_OF_CENTER: u32 = 0x0000_0040;
/// Front Right of Center
pub const SPEAKER_FRONT_RIGHT_OF_CENTER: u32 = 0x0000_0080;
/// Back Center
pub const SPEAKER_BACK_CENTER: u32 = 0x0000_0100;
/// Side Left
pub const SPEAKER_SIDE_LEFT: u32 = 0x0000_0200;
/// Side Right
pub const SPEAKER_SIDE_RIGHT: u32 = 0x0000_0400;
/// Top Center
pub const SPEAKER_TOP_CENTER: u32 = 0x0000_0800;
/// Top Front Left
pub const SPEAKER_TOP_FRONT_LEFT: u32 = 0x0000_1000;
/// Top Front Center
pub const SPEAKER_TOP_FRONT_CENTER: u32 = 0x0000_2000;
/// Top Front Right
pub const SPEAKER_TOP_FRONT_RIGHT: u32 = 0x0000_4000;
/// Top Back Left
pub const SPEAKER_TOP_BACK_LEFT: u32 = 0x0000_8000;
/// Top Back Center
pub const SPEAKER_TOP_BACK_CENTER: u32 = 0x0001_0000;
/// Top Back Right
pub const SPEAKER_TOP_BACK_RIGHT: u32 = 0x0002_0000;

// 标准预定义组合
/// 5.1 环绕声 = FL | FR | FC | LFE | SL | SR
pub const SPEAKER_5POINT1: u32 =
    SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT | SPEAKER_FRONT_CENTER
    | SPEAKER_LOW_FREQUENCY | SPEAKER_BACK_LEFT | SPEAKER_BACK_RIGHT;

/// 7.1 环绕声 = 5.1 + Side Left + Side Right
pub const SPEAKER_7POINT1: u32 = SPEAKER_5POINT1 | SPEAKER_SIDE_LEFT | SPEAKER_SIDE_RIGHT;

// ══════════════════════════════════════════════════════════════════════════════
// 通道位 → 名称映射
// ══════════════════════════════════════════════════════════════════════════════

/// 通道掩码中每一位对应的名称，按位索引排列。
///
/// 索引 0 = bit 0 (Front Left)，索引 17 = bit 17 (Top Back Right)。
const CHANNEL_BIT_NAMES: [&str; 18] = [
    "L",       // bit 0  SPEAKER_FRONT_LEFT
    "R",       // bit 1  SPEAKER_FRONT_RIGHT
    "C",       // bit 2  SPEAKER_FRONT_CENTER
    "LFE",     // bit 3  SPEAKER_LOW_FREQUENCY
    "RL",      // bit 4  SPEAKER_BACK_LEFT
    "RR",      // bit 5  SPEAKER_BACK_RIGHT
    "FLC",     // bit 6  SPEAKER_FRONT_LEFT_OF_CENTER
    "FRC",     // bit 7  SPEAKER_FRONT_RIGHT_OF_CENTER
    "BC",      // bit 8  SPEAKER_BACK_CENTER
    "SL",      // bit 9  SPEAKER_SIDE_LEFT
    "SR",      // bit 10 SPEAKER_SIDE_RIGHT
    "TC",      // bit 11 SPEAKER_TOP_CENTER
    "TFL",     // bit 12 SPEAKER_TOP_FRONT_LEFT
    "TFC",     // bit 13 SPEAKER_TOP_FRONT_CENTER
    "TFR",     // bit 14 SPEAKER_TOP_FRONT_RIGHT
    "TBL",     // bit 15 SPEAKER_TOP_BACK_LEFT
    "TBC",     // bit 16 SPEAKER_TOP_BACK_CENTER
    "TBR",     // bit 17 SPEAKER_TOP_BACK_RIGHT
];

/// 常见通道布局的预设名称列表（用于掩码为 0 时的回退）。
const FALLBACK_NAMES: [&str; 8] = ["L", "R", "C", "LFE", "RL", "RR", "SL", "SR"];

// ══════════════════════════════════════════════════════════════════════════════
// get_channel_names
// ══════════════════════════════════════════════════════════════════════════════

/// 根据通道数和掩码生成通道名列表。
///
/// 逻辑：
/// 1. 遍历 `channel_mask` 中每个置位 bit，按 bit 索引取对应名称
/// 2. 如果遍历完置位 bit 数量 < `channel_count`，用 `"ChN"` 格式补齐
/// 3. 如果 `channel_mask == 0`，使用 FALLBACK_NAMES + `"ChN"` 格式补齐
///
/// # Example
///
/// ```
/// # use vxapo_driver::engine::channel::get_channel_names;
/// let names = get_channel_names(2, 0x3); // FL | FR
/// assert_eq!(names, vec!["L", "R"]);
///
/// let names = get_channel_names(6, 0x3F); // 5.1
/// assert_eq!(names, vec!["L", "R", "C", "LFE", "RL", "RR"]);
/// ```
pub fn get_channel_names(channel_count: u32, channel_mask: u32) -> Vec<String> {
    let count = channel_count as usize;

    if channel_mask == 0 {
        // 掩码为 0：使用预设名称或 "ChN" 格式
        return (0..count)
            .map(|i| {
                if i < FALLBACK_NAMES.len() {
                    FALLBACK_NAMES[i].to_owned()
                } else {
                    format!("Ch{i}")
                }
            })
            .collect();
    }

    let mut names = Vec::with_capacity(count);

    // 遍历每个置位 bit
    for bit in 0..18u32 {
        if channel_mask & (1 << bit) != 0 {
            let name = if (bit as usize) < CHANNEL_BIT_NAMES.len() {
                CHANNEL_BIT_NAMES[bit as usize].to_owned()
            } else {
                format!("Ch{bit}")
            };
            names.push(name);

            if names.len() >= count {
                break;
            }
        }
    }

    // 不够则补齐
    while names.len() < count {
        names.push(format!("Ch{}", names.len()));
    }

    names
}

// ══════════════════════════════════════════════════════════════════════════════
// default_channel_mask（Note 27）
// ══════════════════════════════════════════════════════════════════════════════

/// 根据通道数生成默认掩码。
///
/// 当 `dwChannelMask` 为 0 时调用（Note 27）：
/// - 1ch → 0x4 (Mono → Front Center)
/// - 2ch → 0x3 (Stereo → FL | FR)
/// - 4ch → 0x33 (Quad → FL | FR | BL | BR)
/// - 6ch → 0x3F (5.1)
/// - 8ch → 0xFF (7.1)
/// - 其他 → 连续低位掩码
pub fn default_channel_mask(channel_count: u32) -> u32 {
    match channel_count {
        1 => SPEAKER_FRONT_CENTER,
        2 => SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT,
        4 => SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT
            | SPEAKER_BACK_LEFT | SPEAKER_BACK_RIGHT,
        6 => SPEAKER_5POINT1,
        8 => SPEAKER_7POINT1,
        _ => {
            // 其他通道数：使用连续低位 bit
            if channel_count >= 32 {
                0xFFFF_FFFF
            } else {
                (1u32 << channel_count) - 1
            }
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 辅助查询
// ══════════════════════════════════════════════════════════════════════════════

/// 掩码中的有效通道数（置位 bit 数量）。
pub fn mask_channel_count(mask: u32) -> u32 {
    mask.count_ones()
}

/// 检查掩码是否包含指定通道 bit。
pub fn mask_has_channel(mask: u32, bit: u32) -> bool {
    bit < 32 && (mask & (1 << bit)) != 0
}

/// 将通道名映射回掩码 bit 位置。
///
/// 常见别名：
/// - `"L"` / `"FL"` → bit 0
/// - `"R"` / `"FR"` → bit 1
/// - `"C"` / `"FC"` → bit 2
/// - `"LFE"` / `"SUB"` → bit 3
/// - `"RL"` / `"BL"` → bit 4
/// - `"RR"` / `"BR"` → bit 5
/// - `"SL"` → bit 9
/// - `"SR"` → bit 10
///
/// 未识别的名称返回 `None`。
pub fn channel_name_to_bit(name: &str) -> Option<u32> {
    match name {
        "L" | "FL" | "FrontLeft" => Some(0),
        "R" | "FR" | "FrontRight" => Some(1),
        "C" | "FC" | "Center" | "FrontCenter" => Some(2),
        "LFE" | "SUB" | "Subwoofer" => Some(3),
        "RL" | "BL" | "RearLeft" | "BackLeft" => Some(4),
        "RR" | "BR" | "RearRight" | "BackRight" => Some(5),
        "FLC" | "FrontLeftOfCenter" => Some(6),
        "FRC" | "FrontRightOfCenter" => Some(7),
        "BC" | "BackCenter" => Some(8),
        "SL" | "SideLeft" => Some(9),
        "SR" | "SideRight" => Some(10),
        "TC" | "TopCenter" => Some(11),
        _ => None,
    }
}

/// 将通道名列表转为掩码。
///
/// 未识别的通道名被跳过。
pub fn channel_names_to_mask(names: &[&str]) -> u32 {
    let mut mask = 0u32;
    for name in names {
        if let Some(bit) = channel_name_to_bit(name) {
            mask |= 1 << bit;
        }
    }
    mask
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── get_channel_names ───────────────────────────────────────────────────

    #[test]
    fn stereo_names() {
        let names = get_channel_names(2, 0x3);
        assert_eq!(names, vec!["L", "R"]);
    }

    #[test]
    fn mono_names() {
        let names = get_channel_names(1, 0x4);
        assert_eq!(names, vec!["C"]);
    }

    #[test]
    fn surround_51_names() {
        let names = get_channel_names(6, 0x3F);
        assert_eq!(names, vec!["L", "R", "C", "LFE", "RL", "RR"]);
    }

    #[test]
    fn surround_71_names() {
        let names = get_channel_names(8, 0x063F);
        assert_eq!(names, ["L", "R", "C", "LFE", "RL", "RR", "SL", "SR"]);
    }

    // 额外补充旧格式兼容测试
    #[test]
    fn surround_71_rear_names() {
        let names = get_channel_names(8, 0x00FF);
        assert_eq!(names, ["L", "R", "C", "LFE", "RL", "RR", "FLC", "FRC"]);
    }

    #[test]
    fn mask_zero_uses_fallback() {
        let names = get_channel_names(2, 0);
        assert_eq!(names, vec!["L", "R"]);
    }

    #[test]
    fn mask_zero_8ch() {
        let names = get_channel_names(8, 0);
        assert_eq!(names, vec!["L", "R", "C", "LFE", "RL", "RR", "SL", "SR"]);
    }

    #[test]
    fn mask_zero_many_channels() {
        let names = get_channel_names(12, 0);
        assert_eq!(names.len(), 12);
        assert_eq!(names[0], "L");
        assert_eq!(names[7], "SR");
        assert_eq!(names[8], "Ch8");
        assert_eq!(names[11], "Ch11");
    }

    #[test]
    fn mask_with_fewer_bits_than_count() {
        // 掩码只有 2 个 bit，但要求 4 个通道 → 补齐
        let names = get_channel_names(4, 0x3);
        assert_eq!(names.len(), 4);
        assert_eq!(names[0], "L");
        assert_eq!(names[1], "R");
        assert_eq!(names[2], "Ch2");
        assert_eq!(names[3], "Ch3");
    }

    #[test]
    fn mask_with_more_bits_than_count() {
        // 掩码有 6 个 bit，但只要求 2 个通道 → 只取前 2 个
        let names = get_channel_names(2, 0x3F);
        assert_eq!(names, vec!["L", "R"]);
    }

    #[test]
    fn side_channels() {
        let names = get_channel_names(2, SPEAKER_SIDE_LEFT | SPEAKER_SIDE_RIGHT);
        assert_eq!(names, vec!["SL", "SR"]);
    }

    // ── default_channel_mask ────────────────────────────────────────────────

    #[test]
    fn default_mask_mono() {
        assert_eq!(default_channel_mask(1), 0x4);
    }

    #[test]
    fn default_mask_stereo() {
        assert_eq!(default_channel_mask(2), 0x3);
    }

    #[test]
    fn default_mask_quad() {
        assert_eq!(default_channel_mask(4), 0x33);
    }

    #[test]
    fn default_mask_51() {
        assert_eq!(default_channel_mask(6), 0x3F);
    }

    #[test]
    fn default_mask_71() {
        assert_eq!(default_channel_mask(8), 0x063F);  // 7.1 side (modern)
    }

    #[test]
    fn default_mask_3ch() {
        // 连续低位：0b111 = 0x7
        assert_eq!(default_channel_mask(3), 0x7);
    }

    #[test]
    fn default_mask_5ch() {
        // 连续低位：0b11111 = 0x1F
        assert_eq!(default_channel_mask(5), 0x1F);
    }

    #[test]
    fn default_mask_32ch() {
        assert_eq!(default_channel_mask(32), 0xFFFF_FFFF);
    }

    #[test]
    fn default_mask_64ch_saturates() {
        assert_eq!(default_channel_mask(64), 0xFFFF_FFFF);
    }

    // ── mask_channel_count ──────────────────────────────────────────────────

    #[test]
    fn mask_channel_count_stereo() {
        assert_eq!(mask_channel_count(0x3), 2);
    }

    #[test]
    fn mask_channel_count_51() {
        assert_eq!(mask_channel_count(0x3F), 6);
    }

    #[test]
    fn mask_channel_count_zero() {
        assert_eq!(mask_channel_count(0), 0);
    }

    #[test]
    fn mask_channel_count_71() {
        assert_eq!(mask_channel_count(0xFF), 8);
    }

    // ── mask_has_channel ────────────────────────────────────────────────────

    #[test]
    fn mask_has_channel_true() {
        assert!(mask_has_channel(0x3, 0)); // L
        assert!(mask_has_channel(0x3, 1)); // R
    }

    #[test]
    fn mask_has_channel_false() {
        assert!(!mask_has_channel(0x3, 2)); // C not in stereo
        assert!(!mask_has_channel(0x3, 9)); // SL not in stereo
    }

    #[test]
    fn mask_has_channel_out_of_range() {
        assert!(!mask_has_channel(0x3, 32));
        assert!(!mask_has_channel(0x3, 100));
    }

    // ── channel_name_to_bit ─────────────────────────────────────────────────

    #[test]
    fn name_to_bit_common() {
        assert_eq!(channel_name_to_bit("L"), Some(0));
        assert_eq!(channel_name_to_bit("R"), Some(1));
        assert_eq!(channel_name_to_bit("C"), Some(2));
        assert_eq!(channel_name_to_bit("LFE"), Some(3));
        assert_eq!(channel_name_to_bit("RL"), Some(4));
        assert_eq!(channel_name_to_bit("RR"), Some(5));
        assert_eq!(channel_name_to_bit("SL"), Some(9));
        assert_eq!(channel_name_to_bit("SR"), Some(10));
    }

    #[test]
    fn name_to_bit_aliases() {
        assert_eq!(channel_name_to_bit("FL"), Some(0));
        assert_eq!(channel_name_to_bit("FR"), Some(1));
        assert_eq!(channel_name_to_bit("FC"), Some(2));
        assert_eq!(channel_name_to_bit("SUB"), Some(3));
        assert_eq!(channel_name_to_bit("BL"), Some(4));
        assert_eq!(channel_name_to_bit("BR"), Some(5));
        assert_eq!(channel_name_to_bit("FrontLeft"), Some(0));
        assert_eq!(channel_name_to_bit("SideLeft"), Some(9));
    }

    #[test]
    fn name_to_bit_unknown() {
        assert_eq!(channel_name_to_bit("UNKNOWN"), None);
        assert_eq!(channel_name_to_bit(""), None);
        assert_eq!(channel_name_to_bit("ch0"), None);
    }

    // ── channel_names_to_mask ───────────────────────────────────────────────

    #[test]
    fn names_to_mask_stereo() {
        let mask = channel_names_to_mask(&["L", "R"]);
        assert_eq!(mask, 0x3);
    }

    #[test]
    fn names_to_mask_51() {
        let mask = channel_names_to_mask(&["L", "R", "C", "LFE", "RL", "RR"]);
        assert_eq!(mask, 0x3F);
    }

    #[test]
    fn names_to_mask_unknown_skipped() {
        let mask = channel_names_to_mask(&["L", "R", "UNKNOWN", "C"]);
        assert_eq!(mask, 0x7); // L | R | C
    }

    #[test]
    fn names_to_mask_empty() {
        let mask = channel_names_to_mask(&[]);
        assert_eq!(mask, 0);
    }

    #[test]
    fn names_to_mask_duplicates() {
        let mask = channel_names_to_mask(&["L", "L", "R"]);
        assert_eq!(mask, 0x3); // 重复不影响
    }

    // ── 掩码常量 ────────────────────────────────────────────────────────────

    #[test]
    fn speaker_constants() {
        assert_eq!(SPEAKER_FRONT_LEFT, 0x0001);
        assert_eq!(SPEAKER_FRONT_RIGHT, 0x0002);
        assert_eq!(SPEAKER_FRONT_CENTER, 0x0004);
        assert_eq!(SPEAKER_LOW_FREQUENCY, 0x0008);
        assert_eq!(SPEAKER_BACK_LEFT, 0x0010);
        assert_eq!(SPEAKER_BACK_RIGHT, 0x0020);
        assert_eq!(SPEAKER_SIDE_LEFT, 0x0200);
        assert_eq!(SPEAKER_SIDE_RIGHT, 0x0400);
    }

    #[test]
    fn speaker_51_composition() {
        assert_eq!(
            SPEAKER_5POINT1,
            SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT | SPEAKER_FRONT_CENTER
                | SPEAKER_LOW_FREQUENCY | SPEAKER_BACK_LEFT | SPEAKER_BACK_RIGHT
        );
    }

    #[test]
    fn speaker_71_composition() {
        assert_eq!(SPEAKER_7POINT1, SPEAKER_5POINT1 | SPEAKER_SIDE_LEFT | SPEAKER_SIDE_RIGHT);
    }

    // ── 往返测试：names ↔ mask ──────────────────────────────────────────────

    #[test]
    fn roundtrip_stereo() {
        let names = get_channel_names(2, 0x3);
        let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let mask = channel_names_to_mask(&refs);
        assert_eq!(mask, 0x3);
    }

    #[test]
    fn roundtrip_51() {
        let mask_orig = 0x3F;
        let names = get_channel_names(6, mask_orig);
        let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let mask_back = channel_names_to_mask(&refs);
        assert_eq!(mask_back, mask_orig);
    }

    #[test]
    fn roundtrip_71() {
        let mask_orig = 0xFF;
        let names = get_channel_names(8, mask_orig);
        let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let mask_back = channel_names_to_mask(&refs);
        assert_eq!(mask_back, mask_orig);
    }
}