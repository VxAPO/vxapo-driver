//! install/device/slots/types.rs — 槽位/模式/值类型与常量

//! 共享导入见父模块 install/device/slots.rs。

use super::*;

/// FxProperties 子键名称。
pub const FX_PROPERTIES_KEY: &str = "FxProperties";

/// 安装版本号。
pub const INSTALL_VERSION: &str = "2";

/// Legacy 安装版本号。
pub const INSTALL_VERSION_LEGACY: &str = "1";

/// FxProperties 中的 APO 注册属性 GUID。
///
/// Windows 使用 `{d04e05a6-594b-4fb6-a80d-01af5eed7d1d}` 作为 APO 注册属性集的标识。
/// 各槽位通过属性索引区分。
pub(super) const APO_FX_PROPERTY_GUID: &str = "d04e05a6-594b-4fb6-a80d-01af5eed7d1d";

// Windows 真实注册表槽位属性 ID（PID）， reg query 实证。
pub(super) const PID_LFX: u8 = 0;
pub(super) const PID_GFX: u8 = 3;
pub(super) const PID_SFX: u8 = 5;
pub(super) const PID_MFX: u8 = 6;
pub(super) const PID_EFX: u8 = 7;

// ══════════════════════════════════════════════════════════════════════════════
// ApoSlot — 5 个 APO 槽位
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

/// Windows 真实注册表槽位属性 ID（PID）。
///
/// **实证（reg query）**：FxProperties 下 `{d04e05a6-...}` 各槽位
/// 的 PID 为 **0/3/5/6/7**（非连续 0-4）：
/// - LFX=0 / GFX=3 / SFX=5 / MFX=6 / EFX=7
/// - 值与旧 CLI `src/reg.rs` 常量一致（VAL_SFX=5 / VAL_MFX=6 / VAL_EFX=7）
pub fn registry_pid(self) -> u8 {
    match self {
        ApoSlot::Lfx => PID_LFX,
        ApoSlot::Gfx => PID_GFX,
        ApoSlot::Sfx => PID_SFX,
        ApoSlot::Mfx => PID_MFX,
        ApoSlot::Efx => PID_EFX,
    }
}

/// 槽位的注册表值名称。
///
/// 格式：`{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},{registry_pid}`
pub fn value_name(self) -> String {
    format!("{{{}}},{}", APO_FX_PROPERTY_GUID, self.registry_pid())
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
// InstallMode — 3 种安装模式
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
// SlotValue — 槽位值状态
// ══════════════════════════════════════════════════════════════════════════════

/// APO 槽位值。
///
/// 表示 FxProperties 键下某个槽位的三种状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotValue {
    /// FxProperties 键不存在（设备未配置任何 APO）。
    ///
    /// 对应 中的 `APOGUID_NOKEY`。
    NoKey,
    /// 值为空或已被其他 APO 占据（该槽位无自定义 APO）。
    ///
    /// 对应 中的 `APOGUID_NOVALUE`。
    NoValue,
    /// 具体的 APO CLSID。
    Guid(GUID),
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
    pub fn as_guid(&self) -> Option<GUID> {
        match self {
            SlotValue::Guid(g) => Some(*g),
            _ => None,
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 公开 API — 槽位读取
// ══════════════════════════════════════════════════════════════════════════════

