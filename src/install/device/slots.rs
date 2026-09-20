//! install/device/slots.rs — APO 槽位管理（规范 5.3）
//!
//! 管理 Windows 音频端点 FxProperties 注册表键下的 5 个 APO GUID 槽位，
//! 提供安装模式选择与原始 APO GUID 回退查询。
//!
//! 5 个槽位：
//! ```text
//! 索引 名称 角色
//! 0 LFX Legacy PreMix（Win8.1+）
//! 1 GFX Legacy PostMix（Win8.1+）
//! 2 SFX Side-effect PreMix（Win10+）
//! 3 MFX Mixed-effect PostMix（Win11 蓝牙）
//! 4 EFX Endpoint-effect PostMix（默认）
//! ```
//!
//! 每个槽位有三种特殊值状态：
//! - `NoKey`：FxProperties 键不存在（设备未配置任何 APO）
//! - `NoValue`：值为空或已被其他 APO 占据（该槽位无自定义 APO）
//! - `Guid(GUID)`：具体的 APO CLSID
//!
//! 3 种安装模式：
//! | 模式 | PreMix 槽位 | PostMix 槽位 | 适用场景 |
//! |----------|-------------|--------------|----------------|
//! | LfxGfx | LFX(0) | GFX(1) | Win8.1+ Legacy |
//! | SfxMfx | SFX(2) | MFX(3) | Win11 蓝牙 |
//! | SfxEfx | SFX(2) | EFX(4) | 默认 |
//!
//! GUID 回退逻辑：
//! - `get_original_pre_mix()`：当前模式槽位为 `NoValue` 时回退到同组另一槽位
//! - `get_original_post_mix()`：类似，涉及 GFX / MFX / EFX 三槽位
//! - `NoKey` 或无回退目标时返回空字符串
//!
//! 此模块只做查询，不修改任何系统状态。实际操作委托 `install/install`。

use crate::sys::com::prelude::{GUID, guid_to_string};
use crate::sys::registry::RegKey;
use crate::utils::guid::{guid_from_bytes, is_zero_guid, parse_guid_string};

// ══════════════════════════════════════════════════════════════════════════════
// 常量
// ══════════════════════════════════════════════════════════════════════════════

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
const APO_FX_PROPERTY_GUID: &str = "d04e05a6-594b-4fb6-a80d-01af5eed7d1d";

// Windows 真实注册表槽位属性 ID（PID）， reg query 实证。
const PID_LFX: u8 = 0;
const PID_GFX: u8 = 3;
const PID_SFX: u8 = 5;
const PID_MFX: u8 = 6;
const PID_EFX: u8 = 7;

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

    // Windows 槽位值同时存在 REG_SZ（EAPO 等第三方写 GUID 字符串，实证）与
    // REG_BINARY（16 字节 LE，部分实现/VxAPO 旧写）两种格式——按真实类型自动识别。
    // 全零 GUID（{00000000-...}）是 Windows 的「无 APO」占位，归一为 NoValue——
    // 否则 detect_install_mode 会把全零槽位误判为已占用。
    match fx_key.read_value(&value_name) {
        Ok(crate::sys::registry::RegValue::Sz(s)) => {
            // REG_SZ：`{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}` 格式。
            match parse_guid_string(s.trim()) {
                Some(guid) if !is_zero_guid(&guid) => SlotValue::Guid(guid),
                _ => SlotValue::NoValue,
            }
        }
        Ok(crate::sys::registry::RegValue::Binary(raw)) if raw.len() >= 16 => {
            // 二进制值的前 16 字节是 GUID（little-endian）。
            match guid_from_bytes(&raw) {
                Some(guid) if !is_zero_guid(&guid) => SlotValue::Guid(guid),
                _ => SlotValue::NoValue,
            }
        }
        _ => {
            // 值不存在、类型不符或长度不足 → NoValue。
            SlotValue::NoValue
        }
    }
}

/// EAPO 三档安装模式探测（DeviceAPOInfo.cpp 396-413，C41-C44）。
///
/// 纯逻辑 API——调用方（APP/守护）负责准备输入，driver 只做判定：
///
/// | 优先级 | 条件 | 模式 |
/// |--------|------|------|
/// | 0 | Win < 8.1（不探测） | LfxGfx（Legacy 初始默认） |
/// | 1 | Win8.1+ 且 FxProperties **只有 LFX/GFX 值、SFX/MFX/EFX 全空** | LfxGfx（驱动仅支持 Legacy） |
/// | 2 | 端点实例 ID 以 BTHENUM/BTHLE 开头（蓝牙音频） | SfxMfx（Win11 蓝牙组合，EFX 无效） |
/// | 3 | 否则（现代驱动默认） | SfxEfx |
///
/// # 参数
///
/// - `is_windows_8_1_or_newer`：OS 版本判定（registry::is_windows_version_at_least(6,3,9600）)。
/// - `slots`：5 槽位值（LFX/GFX/SFX/MFX/EFX），来自端点 FxProperties。
/// - `has_bluetooth`：端点实例 ID 以 BTHENUM/BTHLE 开头（蓝牙音频设备）。
pub fn detect_install_mode(
    is_windows_8_1_or_newer: bool,
    slots: &[SlotValue; 5],
    has_bluetooth: bool,
) -> InstallMode {
    if !is_windows_8_1_or_newer {
        return InstallMode::LfxGfx;
    }

    // C41：只有 LFX/GFX 值、SFX/MFX/EFX 全空 → Legacy 独占。
    let has_lfx = matches!(slots[ApoSlot::Lfx.index() as usize], SlotValue::Guid(_));
    let has_gfx = matches!(slots[ApoSlot::Gfx.index() as usize], SlotValue::Guid(_));
    let has_sfx = matches!(slots[ApoSlot::Sfx.index() as usize], SlotValue::Guid(_));
    let has_mfx = matches!(slots[ApoSlot::Mfx.index() as usize], SlotValue::Guid(_));
    let has_efx = matches!(slots[ApoSlot::Efx.index() as usize], SlotValue::Guid(_));
    if (has_lfx || has_gfx) && !has_sfx && !has_mfx && !has_efx {
        return InstallMode::LfxGfx;
    }

    // C42：蓝牙组合设备容器 ID 存在 → SfxMfx。
    if has_bluetooth {
        return InstallMode::SfxMfx;
    }

    // C43：默认 SfxEfx。
    InstallMode::SfxEfx
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
// 公开 API — 原始 APO GUID 回退
// ══════════════════════════════════════════════════════════════════════════════

/// 获取原始 PreMix APO GUID（带回退）。
///
/// # 回退规则
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
        return guid_to_string(&g);
    }

    // 情况 2：主槽位是 NoKey → 无法回退。
    if matches!(slots[primary.index() as usize], SlotValue::NoKey) {
        return String::new();
    }

    // 情况 3：主槽位是 NoValue → 检查回退条件。
    // "同组另一槽位也是 NoValue"：PreMix 的同组指另一模式的 PreMix 槽位。
    let fallback_slot = other_premix_slot(mode);

    match slots[fallback_slot.index() as usize] {
        SlotValue::Guid(g) => guid_to_string(&g),
        _ => String::new(),
    }
}

/// 获取原始 PostMix APO GUID（带回退）。
///
/// # 回退规则
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
        return guid_to_string(&g);
    }

    // 情况 2：主槽位是 NoKey → 无法回退。
    if matches!(slots[primary.index() as usize], SlotValue::NoKey) {
        return String::new();
    }

    // 情况 3：主槽位是 NoValue → 按优先级回退到其他 PostMix 槽位。
    // PostMix 涉及 GFX/MFX/EFX 三槽位。
    for fallback in postmix_fallback_order(mode) {
        if let SlotValue::Guid(g) = slots[fallback.index() as usize] {
            return guid_to_string(&g);
        }
    }

    String::new()
}

// ══════════════════════════════════════════════════════════════════════════════
// VxAPO 独立安装信息区（子 APO GUID 来源）
// ══════════════════════════════════════════════════════════════════════════════

/// VxAPO 独立安装信息区键路径（install 5.3，全量判定依据）。
///
/// **路径隔离**：禁止读写 EAPO 的
/// `HKLM\SOFTWARE\EqualizerAPO\Child APOs`（EAPO childApoPath，RegistryHelper.h 33）——
/// VxAPO 用独立的 `HKLM\SOFTWARE\VxAPO` 根，避免污染 EAPO 安装信息区。
pub const CHILD_APO_PATH_ROOT: &str = r"HKLM\SOFTWARE\VxAPO\Child APOs";

/// 子 APO 类型（决定读取安装信息区中的哪个值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildApoKind {
    /// 前任 PreMix APO（PreMixChild 值）。
    PreMix,
    /// 前任 PostMix APO（PostMixChild 值）。
    PostMix,
}

impl ChildApoKind {
    /// 安装信息区中的值名。
    pub(crate) fn value_name(self) -> &'static str {
        match self {
            Self::PreMix => "PreMixChild",
            Self::PostMix => "PostMixChild",
        }
    }
}

/// 判断某设备是否有 VxAPO 安装信息区（全量/非全量判定的唯一依据，intent 七节）。
///
/// - 不存在 → 初始安装 / 完全卸载后安装 → `install_endpoint` 走**全量备份路径**；
/// - 存在 → 重装 / 失守重装 → 走**非全量路径**（槽位覆盖或保留旧 childapo）。
///
/// 私有路径保证：只由 install_endpoint 写、uninstall_endpoint 删（卸载必删整个键）；
/// 第三方 APO 软件不会写它（各软件只操作自己的私有路径）——存在性即充分判定。
pub fn child_apo_key_exists(device_guid: &str) -> bool {
    let key_path = format!("{}\\{}", CHILD_APO_PATH_ROOT, device_guid);
    // CHILD_APO_PATH_ROOT 含 HKLM\ 前缀（split_key 拆分为 root + 子键）。
    let (root, sub_key) = match split_path(&key_path) {
        Some(v) => v,
        None => return false,
    };
    RegKey::open(root, sub_key).is_ok()
}

/// 读取子 APO GUID（object 7.1.8， 三处矛盾消解）。
///
/// 运行期 `Initialize` 用端点 GUID 反查安装信息区：
/// `HKLM\SOFTWARE\VxAPO\Child APOs\{deviceGuid}\{PreMixChild|PostMixChild}`。
/// 返回 `None` = 键或值不存在 / 值为空 / 格式非法（降级为无子 APO）。
///
/// *注意*：与 FxProperties 槽位无关——这是 VxAPO 独立安装信息区
/// （install 5.3），非 `{d04e05a6-...},{index}` 槽位值。
pub fn read_child_apo_guid(device_guid: &str, kind: ChildApoKind) -> Option<GUID> {
    let key_path = format!("{}\\{}", CHILD_APO_PATH_ROOT, device_guid);
    let (root, sub_key) = split_path(&key_path)?;
    let key = RegKey::open(root, sub_key).ok()?;
    // 安装信息区存 GUID 字符串（带花括号的标准格式，guid_to_string 输出）。
    let s = key.read_sz(kind.value_name())?;
    parse_guid_string(&s)
}

/// 拆分 `HKLM\...` 完整路径为(root HKEY, 子键路径)。
///
/// 支持 `HKLM\` 前缀（CHILD_APO_PATH_ROOT 带根）。其他根（HKCU/HKCR/HKU）
/// 当前无使用点，返回 None（保守——不猜测不存在的调用场景）。
fn split_path(path: &str) -> Option<(windows::Win32::System::Registry::HKEY, &str)> {
    let (root_str, rest) = path.split_once('\\')?;
    match root_str.to_ascii_uppercase().as_str() {
        "HKLM" => Some((windows::Win32::System::Registry::HKEY_LOCAL_MACHINE, rest)),
        _ => None,
    }
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
/// 涉及 GFX/MFX/EFX 三槽位。
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

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests;
