//! install/device/slots/read.rs — 槽位读取与安装模式探测

//! 共享导入见父模块 install/device/slots.rs。

use super::*;
use super::types::*;

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

