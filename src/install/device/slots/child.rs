//! install/device/slots/child.rs — 原始 APO GUID 回退与 VxAPO 独立安装信息区

//! 共享导入见父模块 install/device/slots.rs。

use super::*;
// `guid_to_string` 仅被 cfg(test) 下的辅助函数使用。
#[cfg(test)]
use crate::sys::com::prelude::guid_to_string;

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
#[cfg(test)]
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
#[cfg(test)]
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
pub(super) fn split_path(path: &str) -> Option<(windows::Win32::System::Registry::HKEY, &str)> {
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
#[cfg(test)]
pub(super) fn other_premix_slot(mode: InstallMode) -> ApoSlot {
    match mode {
        InstallMode::LfxGfx => ApoSlot::Sfx,
        InstallMode::SfxMfx | InstallMode::SfxEfx => ApoSlot::Lfx,
    }
}

/// PostMix 回退顺序（不含主槽位，已排除）。
///
/// 涉及 GFX/MFX/EFX 三槽位。
#[cfg(test)]
pub(super) fn postmix_fallback_order(mode: InstallMode) -> &'static [ApoSlot] {
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

