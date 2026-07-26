//! device/slots.rs — APO 槽位管理（Note 25/26/46）
//!
//! 管理 Windows 音频端点 FxProperties 注册表键下的 5 个 APO GUID 槽位，
//! 提供安装模式选择与原始 APO GUID 回退查询。
//!
//! 5 个槽位（Note 25）：
//! ```text
//! 索引  名称  角色
//! 0     LFX   Legacy PreMix（Win8.1+）
//! 1     GFX   Legacy PostMix（Win8.1+）
//! 2     SFX   Side-effect PreMix（Win10+）
//! 3     MFX   Mixed-effect PostMix（Win11 蓝牙）
//! 4     EFX   Endpoint-effect PostMix（默认）
//! ```
//!
//! 每个槽位有三种特殊值状态：
//! - `NoKey`：FxProperties 键不存在（设备未配置任何 APO）
//! - `NoValue`：值为空或已被其他 APO 占据（该槽位无自定义 APO）
//! - `Guid(GUID)`：具体的 APO CLSID
//!
//! 3 种安装模式（Note 26）：
//! | 模式     | PreMix 槽位 | PostMix 槽位 | 适用场景       |
//! |----------|-------------|--------------|----------------|
//! | LfxGfx   | LFX(0)      | GFX(1)       | Win8.1+ Legacy |
//! | SfxMfx   | SFX(2)      | MFX(3)       | Win11 蓝牙     |
//! | SfxEfx   | SFX(2)      | EFX(4)       | 默认           |
//!
//! GUID 回退逻辑（Note 46）：
//! - `get_original_pre_mix()`：当前模式槽位为 `NoValue` 时回退到同组另一槽位
//! - `get_original_post_mix()`：类似，涉及 GFX / MFX / EFX 三槽位
//! - `NoKey` 或无回退目标时返回空字符串
//!
//! 依赖：
//! - `utils/reg_read`：注册表只读操作（Note 48）
//! - `utils/error`：统一错误类型（Note 36）
//! - `log` crate：日志记录
//!
//! 此模块只做查询，不修改任何系统状态（Note 23）。实际操作委托 `installation/`。

use crate::utils::reg_read::RegKey;

// ══════════════════════════════════════════════════════════════════════════════
// 常量
// ══════════════════════════════════════════════════════════════════════════════

/// FxProperties 子键名称。
pub const FX_PROPERTIES_KEY: &str = "FxProperties";

/// 安装版本号（Note 24）。
pub const INSTALL_VERSION: &str = "2";

/// Legacy 安装版本号。
pub const INSTALL_VERSION_LEGACY: &str = "1";

/// 默认处理模式 GUID（Note 26）。
///
/// `{C18E2F7E-933D-4965-B7D1-1EEF228D2AF3}`
///
/// 切换安装模式时写入，告诉 Windows 该端点使用默认 APO 处理流程。
/// 对应 `KSDATAFORMAT_SUBTYPE_DEFAULT_PROCESSMODE`（Note 15 / L6）。
pub const DEFAULT_PROCESSMODE_GUID_STR: &str = "{C18E2F7E-933D-4965-B7D1-1EEF228D2AF3}";

/// FxProperties 中的 APO 注册属性 GUID。
///
/// Windows 使用 `{d04e05a6-594b-4fb6-a80d-01af5eed7d1d}` 作为 APO 注册属性集的标识。
/// 各槽位通过属性索引区分。
const APO_FX_PROPERTY_GUID: &str = "d04e05a6-594b-4fb6-a80d-01af5eed7d1d";

// ══════════════════════════════════════════════════════════════════════════════
// ApoSlot — 5 个 APO 槽位（Note 25）
// ══════════════════════════════════════════════════════════════════════════════

/// APO 槽位索引。
///
/// 对应 Windows 音频端点 FxProperties 下的 5 个 APO 注册位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ApoSlot {
    /// 索引 0：Legacy PreMix（Win8.1+ 时代）。
    Lfx = 0,
    /// 索引 1：Legacy PostMix（Win8.1+ 时代）。
    Gfx = 1,
    /// 索引 2：Side-effect PreMix（Win10+ 默认）。
    Sfx = 2,
    /// 索引 3：Mixed-effect PostMix（Win11 蓝牙场景）。
    Mfx = 3,
    /// 索引 4：Endpoint-effect PostMix（Win10+ 默认）。
    Efx = 4,
}

impl ApoSlot {
    /// 所有 5 个槽位（用于遍历）。
    pub const ALL: [ApoSlot; 5] = [
        ApoSlot::Lfx,
        ApoSlot::Gfx,
        ApoSlot::Sfx,
        ApoSlot::Mfx,
        ApoSlot::Efx,
    ];

    /// 槽位索引（0–4）。
    pub fn index(self) -> u8 {
        self as u8
    }

    /// 槽位的注册表值名称。
    ///
    /// 格式：`{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},{index}`
    pub fn value_name(self) -> String {
        format!("{{{}}},{}", APO_FX_PROPERTY_GUID, self.index())
    }

    /// 是否为 PreMix 类槽位。
    pub fn is_premix(self) -> bool {
        matches!(self, ApoSlot::Lfx | ApoSlot::Sfx)
    }

    /// 是否为 PostMix 类槽位。
    pub fn is_postmix(self) -> bool {
        matches!(self, ApoSlot::Gfx | ApoSlot::Mfx | ApoSlot::Efx)
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// InstallMode — 3 种安装模式（Note 26）
// ══════════════════════════════════════════════════════════════════════════════

/// 安装模式。
///
/// 决定 VxAPO 使用哪两个槽位注册 PreMix 和 PostMix APO。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMode {
    /// Win8.1+ Legacy 模式：PreMix = LFX(0)，PostMix = GFX(1)。
    LfxGfx,
    /// Win11 蓝牙模式：PreMix = SFX(2)，PostMix = MFX(3)。
    SfxMfx,
    /// **默认模式**：PreMix = SFX(2)，PostMix = EFX(4)。
    SfxEfx,
}

impl InstallMode {
    /// 当前模式的 PreMix 槽位。
    pub fn premix_slot(self) -> ApoSlot {
        match self {
            InstallMode::LfxGfx => ApoSlot::Lfx,
            InstallMode::SfxMfx | InstallMode::SfxEfx => ApoSlot::Sfx,
        }
    }

    /// 当前模式的 PostMix 槽位。
    pub fn postmix_slot(self) -> ApoSlot {
        match self {
            InstallMode::LfxGfx => ApoSlot::Gfx,
            InstallMode::SfxMfx => ApoSlot::Mfx,
            InstallMode::SfxEfx => ApoSlot::Efx,
        }
    }

    /// 默认安装模式。
    pub fn default_mode() -> InstallMode {
        InstallMode::SfxEfx
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// SlotValue — 槽位值状态（Note 25）
// ══════════════════════════════════════════════════════════════════════════════

/// APO 槽位值。
///
/// 表示 FxProperties 键下某个槽位的三种状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotValue {
    /// FxProperties 键不存在（设备未配置任何 APO）。
    ///
    /// 对应 Note 6 中的 `APOGUID_NOKEY`。
    NoKey,
    /// 值为空或已被其他 APO 占据（该槽位无自定义 APO）。
    ///
    /// 对应 Note 6 中的 `APOGUID_NOVALUE`。
    NoValue,
    /// 具体的 APO CLSID。
    Guid(windows::core::GUID),
}

impl SlotValue {
    /// 是否为具体 GUID。
    pub fn is_guid(&self) -> bool {
        matches!(self, SlotValue::Guid(_))
    }

    /// 是否为空（NoKey 或 NoValue）。
    pub fn is_empty(&self) -> bool {
        matches!(self, SlotValue::NoKey | SlotValue::NoValue)
    }

    /// 提取 GUID，NoKey/NoValue 时返回 None。
    pub fn as_guid(&self) -> Option<windows::core::GUID> {
        match self {
            SlotValue::Guid(g) => Some(*g),
            _ => None,
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 公开 API — 槽位读取
// ══════════════════════════════════════════════════════════════════════════════

/// 从端点 FxProperties 读取指定槽位的 APO GUID。
///
/// # 参数
///
/// - `fx_key`：已打开的 FxProperties 注册表键。
/// - `slot`：目标槽位。
///
/// # 返回
///
/// - `SlotValue::Guid(guid)`：该槽位有已注册的 APO。
/// - `SlotValue::NoValue`：FxProperties 键存在但该槽位值不存在。
///
/// 外层应先检查 FxProperties 键是否存在，不存在时返回 `SlotValue::NoKey`。
pub fn read_slot_value(fx_key: &RegKey, slot: ApoSlot) -> SlotValue {
    let value_name = slot.value_name();

    match fx_key.read_binary_value(&value_name) {
        Ok(raw) if raw.len() >= 16 => {
            // 二进制值的前 16 字节是 GUID（little-endian）。
            let guid = parse_guid_from_bytes(&raw);
            SlotValue::Guid(guid)
        }
        Ok(_) => {
            // 值存在但长度不足 → 空值。
            SlotValue::NoValue
        }
        Err(_) => {
            // 值不存在。
            SlotValue::NoValue
        }
    }
}

/// 从端点根键读取所有 5 个槽位的值。
///
/// 尝试打开 `FxProperties` 子键：
/// - 成功：遍历 5 个槽位，返回每个槽位的值。
/// - 失败（子键不存在）：所有槽位返回 `NoKey`。
///
/// # 返回
///
/// 5 个 `SlotValue` 的数组，索引与 `ApoSlot` 一致。
pub fn read_all_slots(endpoint_key: &RegKey) -> [SlotValue; 5] {
    let fx_key = match endpoint_key.open_sub_key(FX_PROPERTIES_KEY) {
        Ok(k) => k,
        Err(_) => {
            // FxProperties 键不存在 → 所有槽位 NoKey。
            return [SlotValue::NoKey, SlotValue::NoKey, SlotValue::NoKey, SlotValue::NoKey, SlotValue::NoKey];
        }
    };

    let mut result = [SlotValue::NoKey; 5];
    for (i, slot) in ApoSlot::ALL.iter().enumerate() {
        result[i] = read_slot_value(&fx_key, *slot);
    }
    result
}

// ══════════════════════════════════════════════════════════════════════════════
// 公开 API — 原始 APO GUID 回退（Note 46）
// ══════════════════════════════════════════════════════════════════════════════

/// 获取原始 PreMix APO GUID（带回退）。
///
/// # 回退规则（Note 46）
///
/// 1. 按安装模式取对应 PreMix 槽位（LFX 或 SFX）
/// 2. 若为 `NoValue` 且**同组另一槽位也是 `NoValue`**，回退到另一模式的 PreMix 槽位
/// 3. `NoKey` 或无回退时返回空字符串
///
/// # 参数
///
/// - `slots`：5 个槽位值（由 `read_all_slots` 获取）。
/// - `mode`：当前安装模式。
///
/// # 返回
///
/// - GUID 字符串：找到有效 GUID。
/// - 空字符串：未找到（`NoKey` 或所有候选槽位均为空）。
pub fn get_original_pre_mix(slots: &[SlotValue; 5], mode: InstallMode) -> String {
    let primary = mode.premix_slot();

    // 情况 1：主槽位有 GUID → 直接返回。
    if let SlotValue::Guid(g) = slots[primary.index() as usize] {
        return format_guid(g);
    }

    // 情况 2：主槽位是 NoKey → 无法回退。
    if matches!(slots[primary.index() as usize], SlotValue::NoKey) {
        return String::new();
    }

    // 情况 3：主槽位是 NoValue → 检查回退条件。
    // "同组另一槽位也是 NoValue"：PreMix 的同组指另一模式的 PreMix 槽位。
    let fallback_slot = other_premix_slot(mode);

    match slots[fallback_slot.index() as usize] {
        SlotValue::Guid(g) => format_guid(g),
        _ => String::new(),
    }
}

/// 获取原始 PostMix APO GUID（带回退）。
///
/// # 回退规则（Note 46）
///
/// 1. 按安装模式取对应 PostMix 槽位（GFX / MFX / EFX）
/// 2. 若为 `NoValue`，按优先级尝试其他 PostMix 槽位：
///    - SfxEfx 模式：EFX → MFX → GFX
///    - SfxMfx 模式：MFX → EFX → GFX
///    - LfxGfx 模式：GFX → EFX → MFX
/// 3. `NoKey` 或无回退时返回空字符串
///
/// # 参数
///
/// - `slots`：5 个槽位值（由 `read_all_slots` 获取）。
/// - `mode`：当前安装模式。
///
/// # 返回
///
/// - GUID 字符串：找到有效 GUID。
/// - 空字符串：未找到。
pub fn get_original_post_mix(slots: &[SlotValue; 5], mode: InstallMode) -> String {
    let primary = mode.postmix_slot();

    // 情况 1：主槽位有 GUID → 直接返回。
    if let SlotValue::Guid(g) = slots[primary.index() as usize] {
        return format_guid(g);
    }

    // 情况 2：主槽位是 NoKey → 无法回退。
    if matches!(slots[primary.index() as usize], SlotValue::NoKey) {
        return String::new();
    }

    // 情况 3：主槽位是 NoValue → 按优先级回退到其他 PostMix 槽位。
    // Note 46: PostMix 涉及 GFX/MFX/EFX 三槽位。
    for fallback in postmix_fallback_order(mode) {
        if let SlotValue::Guid(g) = slots[fallback.index() as usize] {
            return format_guid(g);
        }
    }

    String::new()
}

// ══════════════════════════════════════════════════════════════════════════════
// 内部辅助
// ══════════════════════════════════════════════════════════════════════════════

/// PreMix 回退槽位：另一模式的 PreMix。
///
/// LfxGfx → Sfx，SfxMfx/SfxEfx → Lfx。
fn other_premix_slot(mode: InstallMode) -> ApoSlot {
    match mode {
        InstallMode::LfxGfx => ApoSlot::Sfx,
        InstallMode::SfxMfx | InstallMode::SfxEfx => ApoSlot::Lfx,
    }
}

/// PostMix 回退顺序（不含主槽位，已排除）。
///
/// Note 46: 涉及 GFX/MFX/EFX 三槽位。
fn postmix_fallback_order(mode: InstallMode) -> &'static [ApoSlot] {
    match mode {
        // EFX 主 → 尝试 MFX → GFX
        InstallMode::SfxEfx => &[ApoSlot::Mfx, ApoSlot::Gfx],
        // MFX 主 → 尝试 EFX → GFX
        InstallMode::SfxMfx => &[ApoSlot::Efx, ApoSlot::Gfx],
        // GFX 主 → 尝试 EFX → MFX
        InstallMode::LfxGfx => &[ApoSlot::Efx, ApoSlot::Mfx],
    }
}

/// 将 GUID 格式化为注册表兼容的字符串（带花括号）。
///
/// 格式：`{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}`
fn format_guid(g: windows::core::GUID) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        g.data1,
        g.data2,
        g.data3,
        g.data4[0],
        g.data4[1],
        g.data4[2],
        g.data4[3],
        g.data4[4],
        g.data4[5],
        g.data4[6],
        g.data4[7],
    )
}

/// 从 16 字节小端字节数组解析 GUID。
///
/// # 布局
///
/// ```text
/// 偏移  大小  字段
/// 0     4     data1 (LE)
/// 4     2     data2 (LE)
/// 6     2     data3 (LE)
/// 8     8     data4 (BE，原样拷贝)
/// ```
fn parse_guid_from_bytes(bytes: &[u8]) -> windows::core::GUID {
    let data1 = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let data2 = u16::from_le_bytes([bytes[4], bytes[5]]);
    let data3 = u16::from_le_bytes([bytes[6], bytes[7]]);
    let mut data4 = [0u8; 8];
    data4.copy_from_slice(&bytes[8..16]);

    windows::core::GUID { data1, data2, data3, data4 }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试（Note 41）
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── ApoSlot ───────────────────────────────────────────────────────────

    #[test]
    fn slot_indices() {
        assert_eq!(ApoSlot::Lfx.index(), 0);
        assert_eq!(ApoSlot::Gfx.index(), 1);
        assert_eq!(ApoSlot::Sfx.index(), 2);
        assert_eq!(ApoSlot::Mfx.index(), 3);
        assert_eq!(ApoSlot::Efx.index(), 4);
    }

    #[test]
    fn slot_all_contains_5() {
        assert_eq!(ApoSlot::ALL.len(), 5);
    }

    #[test]
    fn slot_all_unique() {
        let mut indices: Vec<u8> = ApoSlot::ALL.iter().map(|s| s.index()).collect();
        indices.sort();
        assert_eq!(indices, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn slot_value_names_format() {
        for slot in ApoSlot::ALL {
            let name = slot.value_name();
            assert!(name.starts_with('{'), "value_name should start with '{{': {}", name);
            assert!(name.contains(APO_FX_PROPERTY_GUID));
            assert!(name.ends_with(&format!(",{}", slot.index())));
        }
    }

    #[test]
    fn slot_value_names_distinct() {
        let names: Vec<String> = ApoSlot::ALL.iter().map(|s| s.value_name()).collect();
        let unique_count = names.iter().collect::<std::collections::HashSet<_>>().len();
        assert_eq!(unique_count, 5);
    }

    #[test]
    fn slot_is_premix() {
        assert!(ApoSlot::Lfx.is_premix());
        assert!(ApoSlot::Sfx.is_premix());
        assert!(!ApoSlot::Gfx.is_premix());
        assert!(!ApoSlot::Mfx.is_premix());
        assert!(!ApoSlot::Efx.is_premix());
    }

    #[test]
    fn slot_is_postmix() {
        assert!(!ApoSlot::Lfx.is_postmix());
        assert!(!ApoSlot::Sfx.is_postmix());
        assert!(ApoSlot::Gfx.is_postmix());
        assert!(ApoSlot::Mfx.is_postmix());
        assert!(ApoSlot::Efx.is_postmix());
    }

    // ── InstallMode ───────────────────────────────────────────────────────

    #[test]
    fn mode_default_is_sfx_efx() {
        assert_eq!(InstallMode::default_mode(), InstallMode::SfxEfx);
    }

    #[test]
    fn mode_premix_slots() {
        assert_eq!(InstallMode::LfxGfx.premix_slot(), ApoSlot::Lfx);
        assert_eq!(InstallMode::SfxMfx.premix_slot(), ApoSlot::Sfx);
        assert_eq!(InstallMode::SfxEfx.premix_slot(), ApoSlot::Sfx);
    }

    #[test]
    fn mode_postmix_slots() {
        assert_eq!(InstallMode::LfxGfx.postmix_slot(), ApoSlot::Gfx);
        assert_eq!(InstallMode::SfxMfx.postmix_slot(), ApoSlot::Mfx);
        assert_eq!(InstallMode::SfxEfx.postmix_slot(), ApoSlot::Efx);
    }

    #[test]
    fn mode_premix_slots_are_premix_type() {
        for mode in [InstallMode::LfxGfx, InstallMode::SfxMfx, InstallMode::SfxEfx] {
            assert!(mode.premix_slot().is_premix(), "{:?} premix should be premix type", mode);
        }
    }

    #[test]
    fn mode_postmix_slots_are_postmix_type() {
        for mode in [InstallMode::LfxGfx, InstallMode::SfxMfx, InstallMode::SfxEfx] {
            assert!(mode.postmix_slot().is_postmix(), "{:?} postmix should be postmix type", mode);
        }
    }

    // ── SlotValue ─────────────────────────────────────────────────────────

    #[test]
    fn slot_value_is_guid() {
        let g = windows::core::GUID::zeroed();
        assert!(SlotValue::Guid(g).is_guid());
        assert!(!SlotValue::NoKey.is_guid());
        assert!(!SlotValue::NoValue.is_guid());
    }

    #[test]
    fn slot_value_is_empty() {
        assert!(SlotValue::NoKey.is_empty());
        assert!(SlotValue::NoValue.is_empty());
        assert!(!SlotValue::Guid(windows::core::GUID::zeroed()).is_empty());
    }

    #[test]
    fn slot_value_as_guid() {
        let g = windows::core::GUID::zeroed();
        assert_eq!(SlotValue::Guid(g).as_guid(), Some(g));
        assert_eq!(SlotValue::NoKey.as_guid(), None);
        assert_eq!(SlotValue::NoValue.as_guid(), None);
    }

    #[test]
    fn slot_value_debug() {
        assert_eq!(format!("{:?}", SlotValue::NoKey), "NoKey");
        assert_eq!(format!("{:?}", SlotValue::NoValue), "NoValue");
        let dbg = format!("{:?}", SlotValue::Guid(windows::core::GUID::zeroed()));
        assert!(dbg.starts_with("Guid("));
    }

    #[test]
    fn slot_value_clone() {
        let v = SlotValue::Guid(windows::core::GUID::zeroed());
        let v2 = v.clone();
        assert_eq!(v, v2);
    }

    // ── format_guid ───────────────────────────────────────────────────────

    #[test]
    fn format_guid_zeroed() {
        let g = windows::core::GUID { data1: 0, data2: 0, data3: 0, data4: [0; 8] };
        let s = format_guid(g);
        assert_eq!(s, "{00000000-0000-0000-0000-000000000000}");
    }

    #[test]
    fn format_guid_known_value() {
        // {C18E2F7E-933D-4965-B7D1-1EEF228D2AF3}
        let g = windows::core::GUID {
            data1: 0xC18E2F7E,
            data2: 0x933D,
            data3: 0x4965,
            data4: [0xB7, 0xD1, 0x1E, 0xEF, 0x22, 0x8D, 0x2A, 0xF3],
        };
        assert_eq!(format_guid(g), "{C18E2F7E-933D-4965-B7D1-1EEF228D2AF3}");
    }

    #[test]
    fn format_guid_max_values() {
        let g = windows::core::GUID {
            data1: 0xFFFFFFFF,
            data2: 0xFFFF,
            data3: 0xFFFF,
            data4: [0xFF; 8],
        };
        assert_eq!(format_guid(g), "{FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF}");
    }

    #[test]
    fn format_guid_has_braces() {
        let g = windows::core::GUID::zeroed();
        let s = format_guid(g);
        assert!(s.starts_with('{'));
        assert!(s.ends_with('}'));
        assert_eq!(s.len(), 38); // {xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}
    }

    // ── parse_guid_from_bytes ─────────────────────────────────────────────

    #[test]
    fn parse_guid_roundtrip() {
        let original = windows::core::GUID {
            data1: 0xC18E2F7E,
            data2: 0x933D,
            data3: 0x4965,
            data4: [0xB7, 0xD1, 0x1E, 0xEF, 0x22, 0x8D, 0x2A, 0xF3],
        };

        // 序列化为 16 字节小端
        let mut bytes = Vec::with_capacity(16);
        bytes.extend_from_slice(&original.data1.to_le_bytes());
        bytes.extend_from_slice(&original.data2.to_le_bytes());
        bytes.extend_from_slice(&original.data3.to_le_bytes());
        bytes.extend_from_slice(&original.data4);

        let parsed = parse_guid_from_bytes(&bytes);
        assert_eq!(parsed.data1, original.data1);
        assert_eq!(parsed.data2, original.data2);
        assert_eq!(parsed.data3, original.data3);
        assert_eq!(parsed.data4, original.data4);
    }

    #[test]
    fn parse_guid_zeroed() {
        let bytes = [0u8; 16];
        let g = parse_guid_from_bytes(&bytes);
        assert_eq!(g.data1, 0);
        assert_eq!(g.data2, 0);
        assert_eq!(g.data3, 0);
        assert_eq!(g.data4, [0; 8]);
    }

    #[test]
    fn parse_guid_max() {
        let bytes = [0xFFu8; 16];
        let g = parse_guid_from_bytes(&bytes);
        assert_eq!(g.data1, 0xFFFFFFFF);
        assert_eq!(g.data2, 0xFFFF);
        assert_eq!(g.data3, 0xFFFF);
        assert_eq!(g.data4, [0xFF; 8]);
    }

    // ── 回退逻辑 — get_original_pre_mix（Note 46） ────────────────────────

    #[test]
    fn premix_primary_has_guid() {
        // SFX 有 GUID → 直接返回，不走回退
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(test_guid(1));
        assert_eq!(get_original_pre_mix(&slots, InstallMode::SfxEfx), format_guid(test_guid(1)));
    }

    #[test]
    fn premix_primary_nokey_returns_empty() {
        // SFX 是 NoKey → 无法回退
        let slots = empty_slots(); // 所有 NoKey
        assert_eq!(get_original_pre_mix(&slots, InstallMode::SfxEfx), "");
    }

    #[test]
    fn premix_novalue_fallback_to_lfx() {
        // SFX 是 NoValue，LFX 有 GUID → 回退到 LFX
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::NoValue;
        slots[ApoSlot::Lfx.index() as usize] = SlotValue::Guid(test_guid(2));
        assert_eq!(get_original_pre_mix(&slots, InstallMode::SfxEfx), format_guid(test_guid(2)));
    }

    #[test]
    fn premix_novalue_lfx_novalue_returns_empty() {
        // SFX 是 NoValue，LFX 也是 NoValue → 无回退
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::NoValue;
        slots[ApoSlot::Lfx.index() as usize] = SlotValue::NoValue;
        assert_eq!(get_original_pre_mix(&slots, InstallMode::SfxEfx), "");
    }

    #[test]
    fn premix_lfxgfx_mode_uses_lfx() {
        // LfxGfx 模式：主槽位 = LFX
        let mut slots = empty_slots();
        slots[ApoSlot::Lfx.index() as usize] = SlotValue::Guid(test_guid(3));
        assert_eq!(get_original_pre_mix(&slots, InstallMode::LfxGfx), format_guid(test_guid(3)));
    }

    #[test]
    fn premix_lfxgfx_novalue_fallback_to_sfx() {
        // LfxGfx 模式：LFX 是 NoValue → 回退到 SFX
        let mut slots = empty_slots();
        slots[ApoSlot::Lfx.index() as usize] = SlotValue::NoValue;
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(test_guid(4));
        assert_eq!(get_original_pre_mix(&slots, InstallMode::LfxGfx), format_guid(test_guid(4)));
    }

    #[test]
    fn premix_sfxmfx_mode_uses_sfx() {
        // SfxMfx 模式：主槽位 = SFX
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(test_guid(5));
        assert_eq!(get_original_pre_mix(&slots, InstallMode::SfxMfx), format_guid(test_guid(5)));
    }

    #[test]
    fn premix_novalue_fallback_skips_nokey() {
        // SFX 是 NoValue，LFX 是 NoKey → 回退到 LFX 但 LFX 是 NoKey → 返回空
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::NoValue;
        // LFX 默认是 NoKey（来自 empty_slots）
        assert_eq!(get_original_pre_mix(&slots, InstallMode::SfxEfx), "");
    }

    // ── 回退逻辑 — get_original_post_mix（Note 46） ───────────────────────

    #[test]
    fn postmix_primary_has_guid() {
        // EFX 有 GUID → 直接返回
        let mut slots = empty_slots();
        slots[ApoSlot::Efx.index() as usize] = SlotValue::Guid(test_guid(10));
        assert_eq!(get_original_post_mix(&slots, InstallMode::SfxEfx), format_guid(test_guid(10)));
    }

    #[test]
    fn postmix_primary_nokey_returns_empty() {
        // EFX 是 NoKey → 无法回退
        let slots = empty_slots();
        assert_eq!(get_original_post_mix(&slots, InstallMode::SfxEfx), "");
    }

    #[test]
    fn postmix_sfxefx_novalue_fallback_to_mfx() {
        // EFX 是 NoValue → 尝试 MFX
        let mut slots = empty_slots();
        slots[ApoSlot::Efx.index() as usize] = SlotValue::NoValue;
        slots[ApoSlot::Mfx.index() as usize] = SlotValue::Guid(test_guid(11));
        assert_eq!(get_original_post_mix(&slots, InstallMode::SfxEfx), format_guid(test_guid(11)));
    }

    #[test]
    fn postmix_sfxefx_novalue_fallback_to_gfx() {
        // EFX 是 NoValue，MFX 是 NoValue → 尝试 GFX
        let mut slots = empty_slots();
        slots[ApoSlot::Efx.index() as usize] = SlotValue::NoValue;
        slots[ApoSlot::Mfx.index() as usize] = SlotValue::NoValue;
        slots[ApoSlot::Gfx.index() as usize] = SlotValue::Guid(test_guid(12));
        assert_eq!(get_original_post_mix(&slots, InstallMode::SfxEfx), format_guid(test_guid(12)));
    }

    #[test]
    fn postmix_sfxefx_all_empty_returns_empty() {
        // 所有 PostMix 槽位都是空的
        let mut slots = empty_slots();
        slots[ApoSlot::Efx.index() as usize] = SlotValue::NoValue;
        slots[ApoSlot::Mfx.index() as usize] = SlotValue::NoValue;
        slots[ApoSlot::Gfx.index() as usize] = SlotValue::NoValue;
        assert_eq!(get_original_post_mix(&slots, InstallMode::SfxEfx), "");
    }

    #[test]
    fn postmix_sfxmfx_novalue_fallback_to_efx() {
        // SfxMfx 模式：MFX 是 NoValue → 尝试 EFX
        let mut slots = empty_slots();
        slots[ApoSlot::Mfx.index() as usize] = SlotValue::NoValue;
        slots[ApoSlot::Efx.index() as usize] = SlotValue::Guid(test_guid(13));
        assert_eq!(get_original_post_mix(&slots, InstallMode::SfxMfx), format_guid(test_guid(13)));
    }

    #[test]
    fn postmix_lfxgfx_novalue_fallback_to_efx() {
        // LfxGfx 模式：GFX 是 NoValue → 尝试 EFX
        let mut slots = empty_slots();
        slots[ApoSlot::Gfx.index() as usize] = SlotValue::NoValue;
        slots[ApoSlot::Efx.index() as usize] = SlotValue::Guid(test_guid(14));
        assert_eq!(get_original_post_mix(&slots, InstallMode::LfxGfx), format_guid(test_guid(14)));
    }

    #[test]
    fn postmix_lfxgfx_novalue_fallback_to_mfx() {
        // LfxGfx 模式：GFX 是 NoValue，EFX 是 NoValue → 尝试 MFX
        let mut slots = empty_slots();
        slots[ApoSlot::Gfx.index() as usize] = SlotValue::NoValue;
        slots[ApoSlot::Efx.index() as usize] = SlotValue::NoValue;
        slots[ApoSlot::Mfx.index() as usize] = SlotValue::Guid(test_guid(15));
        assert_eq!(get_original_post_mix(&slots, InstallMode::LfxGfx), format_guid(test_guid(15)));
    }

    // ── 回退顺序验证 ─────────────────────────────────────────────────────

    #[test]
    fn postmix_fallback_order_sfxefx() {
        assert_eq!(postmix_fallback_order(InstallMode::SfxEfx), &[ApoSlot::Mfx, ApoSlot::Gfx]);
    }

    #[test]
    fn postmix_fallback_order_sfxmfx() {
        assert_eq!(postmix_fallback_order(InstallMode::SfxMfx), &[ApoSlot::Efx, ApoSlot::Gfx]);
    }

    #[test]
    fn postmix_fallback_order_lfxgfx() {
        assert_eq!(postmix_fallback_order(InstallMode::LfxGfx), &[ApoSlot::Efx, ApoSlot::Mfx]);
    }

    // ── other_premix_slot ─────────────────────────────────────────────────

    #[test]
    fn other_premix_for_lfxgfx_is_sfx() {
        assert_eq!(other_premix_slot(InstallMode::LfxGfx), ApoSlot::Sfx);
    }

    #[test]
    fn other_premix_for_sfxefx_is_lfx() {
        assert_eq!(other_premix_slot(InstallMode::SfxEfx), ApoSlot::Lfx);
    }

    #[test]
    fn other_premix_for_sfxmfx_is_lfx() {
        assert_eq!(other_premix_slot(InstallMode::SfxMfx), ApoSlot::Lfx);
    }

    // ── 常量验证 ──────────────────────────────────────────────────────────

    #[test]
    fn default_processmode_guid_format() {
        assert_eq!(DEFAULT_PROCESSMODE_GUID_STR, "{C18E2F7E-933D-4965-B7D1-1EEF228D2AF3}");
    }

    #[test]
    fn install_version_constants() {
        assert_eq!(INSTALL_VERSION, "2");
        assert_eq!(INSTALL_VERSION_LEGACY, "1");
        assert_ne!(INSTALL_VERSION, INSTALL_VERSION_LEGACY);
    }

    #[test]
    fn apo_fx_property_guid_format() {
        // GUID 格式应该是 32 字符的十六进制（无花括号）
        assert_eq!(APO_FX_PROPERTY_GUID.len(), 36); // xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx
        assert!(!APO_FX_PROPERTY_GUID.contains('{'));
        assert!(!APO_FX_PROPERTY_GUID.contains('}'));
    }

    // ── 辅助函数 ──────────────────────────────────────────────────────────

    /// 创建全 NoKey 的空槽位数组。
    fn empty_slots() -> [SlotValue; 5] {
        [SlotValue::NoKey; 5]
    }

    /// 创建测试用 GUID，不同编号产生不同的 GUID。
    fn test_guid(n: u32) -> windows::core::GUID {
        windows::core::GUID {
            data1: 0xA000_0000 + n,
            data2: 0xB000 + n as u16,
            data3: 0xC000 + n as u16,
            data4: [0xD0, 0xE0, 0xF0, n as u8, (n >> 8) as u8, (n >> 16) as u8, (n >> 24) as u8, 0xFF],
        }
    }
}