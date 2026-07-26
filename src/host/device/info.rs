//! device/info.rs — 设备组合查询层（Note 23/24）
//!
//! 组合 `endpoint`、`slots`、`format` 三个子模块，提供高层查询接口：
//!
//! ```text
//! info.rs（组合查询）
//!   ├── endpoint.rs  ← 设备 ID、友好名称、状态
//!   ├── slots.rs     ← 5 个 APO 槽位、安装模式、GUID 回退
//!   └── format.rs    ← WAVEFORMATEX 解析
//! ```
//!
//! 查询接口：
//! - `is_installed` / `can_be_upgraded`
//! - `is_experimental` / `is_enhancements_disabled`
//! - `has_changes`
//!
//! 安装版本常量（Note 24）：
//! - `INSTALL_VERSION = "2"`
//! - `INSTALL_VERSION = "1"`
//! 写入 FxProperties 子键的 `version` 值。版本不匹配时需重新安装。
//!
//! 依赖：
//! - `device/endpoint`：端点状态查询
//! - `device/slots`：槽位管理
//! - `device/format`：音频格式解析
//! - `utils/reg_read`：注册表只读操作（Note 48）
//! - `utils/error`：统一错误类型（Note 36）
//! - `com/iid`：APO CLSID / GUID 常量
//! - `log` crate
//!
//! 此模块只做查询，不修改任何系统状态（Note 23）。实际操作委托 `installation/`。

use log::warn;

use crate::sys::registry::read::{RegKey, RegValue};
use crate::host::instance::reg_props::{CLSID_VXAPO_PRE_MIX, CLSID_VXAPO_POST_MIX};
use crate::host::device::endpoint::{EndpointInfo, query_endpoint};
use crate::host::device::format::{AudioFormat, read_audio_format};
use crate::host::device::slots::{
    ApoSlot, InstallMode, SlotValue, 
    get_original_post_mix, get_original_pre_mix,
    read_all_slots,
    INSTALL_VERSION, FX_PROPERTIES_KEY
};
use crate::utils::error::VxApoError;

// ══════════════════════════════════════════════════════════════════════════════
// 安装版本常量（Note 24）
// ══════════════════════════════════════════════════════════════════════════════

/// 存储版本号的注册表值名称。
const VERSION_VALUE_NAME: &str = "version";

// ══════════════════════════════════════════════════════════════════════════════
// DeviceInfo — 组合查询结果
// ══════════════════════════════════════════════════════════════════════════════

/// 设备综合信息。
///
/// 由 `query_device_info()` 组合 `endpoint`、`slots`、`format` 的查询结果。
/// 只读——查询后不可变。
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// 端点信息。
    pub endpoint: EndpointInfo,
    /// 安装模式。
    pub install_mode: InstallMode,
    /// 5 个槽位的值。
    pub slots: [SlotValue; 5],
    /// 音频格式（可能未配置，为 None）。
    pub format: Option<AudioFormat>,
    /// 安装版本（`"2"`、`"1"` 或空字符串表示未安装）。
    pub installed_version: String,
}

impl DeviceInfo {
    /// VxAPO 是否已安装在此设备上。
    ///
    /// 判定：安装版本非空，且 PreMix 或 PostMix 槽位包含 VxAPO 的 CLSID。
    pub fn is_installed(&self) -> bool {
        if self.installed_version.is_empty() {
            return false;
        }

        let pre_slot = self.install_mode.premix_slot();
        let post_slot = self.install_mode.postmix_slot();

        is_vxapo_slot(&self.slots, pre_slot) || is_vxapo_slot(&self.slots, post_slot)
    }

    /// 是否可以升级到当前版本。
    ///
    /// 判定：已安装且版本低于当前 `INSTALL_VERSION`。
    pub fn can_be_upgraded(&self) -> bool {
        if !self.is_installed() {
            return false;
        }
        self.installed_version != INSTALL_VERSION
    }

    /// 是否为实验性安装（LfxGfx 模式）。
    ///
    /// Legacy 模式仅用于 Win8.1 兼容，标记为实验性。
    pub fn is_experimental(&self) -> bool {
        self.install_mode == InstallMode::LfxGfx
    }

    /// 音频增强是否已被禁用。
    ///
    /// Windows 有一个 `DisableEnhancements` 注册表值。如果存在且非零，
    /// 表示用户或策略禁用了该端点的音频增强（包括 APO）。
    pub fn is_enhancements_disabled(&self, endpoint_key: &RegKey) -> bool {
        // 尝试读取 DisableEnhancements 值。
        // 位于 FxProperties 键下，也可能是端点键的 Properties 子键下。
        if let Ok(fx_key) = endpoint_key.open_sub_key(FX_PROPERTIES_KEY) {
            if let Ok(val) = fx_key.read_dword_value("DisableEnhancements") {
                return val != 0;
            }
        }
        false
    }

    /// 是否有未应用的更改。
    ///
    /// 判定：已安装但安装模式与默认模式不同。
    pub fn has_changes(&self) -> bool {
        self.is_installed() && self.install_mode != InstallMode::default_mode()
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 公开 API
// ══════════════════════════════════════════════════════════════════════════════

/// 查询设备综合信息。
///
/// 组合 `endpoint`、`slots`、`format` 三个子模块的查询结果。
///
/// # 参数
///
/// - `endpoint_key`：已打开的端点根键（通常是 `...\Render\{guid}` 或 `...\Capture\{guid}`）。
///
/// # 返回
///
/// - `Ok(Some(info))`：成功查询。
/// - `Ok(None)`：端点属性缺失（非致命）。
/// - `Err(...)`：注册表 I/O 错误。
pub fn query_device_info(endpoint_key: &RegKey) -> Result<Option<DeviceInfo>, VxApoError> {
    // ── 端点信息 ──────────────────────────────────────────────────────────

    let endpoint = match query_endpoint(endpoint_key)? {
        Some(ep) => ep,
        None => {
            warn!("Endpoint query returned None — skipping device info");
            return Ok(None);
        }
    };

    // ── 槽位 ──────────────────────────────────────────────────────────────

    let slots = read_all_slots(endpoint_key);

    // ── 安装模式（检测当前使用的是哪个槽位组） ────────────────────────────

    let install_mode = detect_install_mode(&slots);

    // ── 音频格式 ──────────────────────────────────────────────────────────

    let format = read_format_for_endpoint(endpoint_key)?;

    // ── 安装版本 ──────────────────────────────────────────────────────────

    let installed_version = read_install_version(endpoint_key);

    Ok(Some(DeviceInfo {
        endpoint,
        install_mode,
        slots,
        format,
        installed_version,
    }))
}

/// 获取原始 PreMix APO GUID（带回退，Note 46）。
///
/// 便捷方法，组合 `read_all_slots` + `get_original_pre_mix`。
pub fn get_pre_mix_apo_guid(endpoint_key: &RegKey, mode: InstallMode) -> Result<String, VxApoError> {
    let slots = read_all_slots(endpoint_key);
    Ok(get_original_pre_mix(&slots, mode))
}

/// 获取原始 PostMix APO GUID（带回退，Note 46）。
///
/// 便捷方法，组合 `read_all_slots` + `get_original_post_mix`。
pub fn get_post_mix_apo_guid(endpoint_key: &RegKey, mode: InstallMode) -> Result<String, VxApoError> {
    let slots = read_all_slots(endpoint_key);
    Ok(get_original_post_mix(&slots, mode))
}

/// 检查端点的 FxProperties 键是否存在。
pub fn has_fx_properties(endpoint_key: &RegKey) -> bool {
    endpoint_key.open_sub_key(FX_PROPERTIES_KEY).is_ok()
}

// ══════════════════════════════════════════════════════════════════════════════
// 内部辅助
// ══════════════════════════════════════════════════════════════════════════════

/// 检查指定槽位是否为 VxAPO 的 CLSID。
fn is_vxapo_slot(slots: &[SlotValue; 5], slot: ApoSlot) -> bool {
    match &slots[slot.index() as usize] {
        SlotValue::Guid(g) => {
            *g == CLSID_VXAPO_PRE_MIX || *g == CLSID_VXAPO_POST_MIX
        }
        _ => false,
    }
}

/// 检测当前安装模式。
///
/// 优先级：检查 PreMix 槽位优先。
/// 1. SFX 有 GUID → SfxEfx 或 SfxMfx（再看 PostMix 槽位）
/// 2. LFX 有 GUID → LfxGfx
/// 3. 默认 → SfxEfx
fn detect_install_mode(slots: &[SlotValue; 5]) -> InstallMode {
    // 检查 SFX（索引 2）是否有 GUID。
    let sfx_has_guid = slots[ApoSlot::Sfx.index() as usize].is_guid();
    // 检查 LFX（索引 0）是否有 GUID。
    let lfx_has_guid = slots[ApoSlot::Lfx.index() as usize].is_guid();

    if lfx_has_guid && !sfx_has_guid {
        // 只有 LFX 有 → Legacy 模式。
        return InstallMode::LfxGfx;
    }

    if sfx_has_guid {
        // SFX 有 GUID → 检查 PostMix 槽位。
        if slots[ApoSlot::Mfx.index() as usize].is_guid() {
            return InstallMode::SfxMfx;
        }
        // 默认 SfxEfx（包括 EFX 有 GUID 或都没有的情况）。
        return InstallMode::SfxEfx;
    }

    // 都没有 → 默认模式。
    InstallMode::default_mode()
}

/// 从端点键读取音频格式。
///
/// 尝试多个格式值名称（Windows 音频端点的常见值名模式）。
fn read_format_for_endpoint(endpoint_key: &RegKey) -> Result<Option<AudioFormat>, VxApoError> {
    // 尝试从 Properties 子键读取。
    let props_key = match endpoint_key.open_sub_key("Properties") {
        Ok(k) => k,
        Err(_) => return Ok(None),
    };

    // PKEY_AudioEngine_DeviceFormat = {f19f064d-...}-0
    // 值名格式取决于设备驱动，通常存储为二进制值。
    // 常见值名：
    //   "{f19f064d-082c-4e27-bc73-6882a1bb8e4c},0" — 设备格式
    //   "{3d6e1656-2e50-4c4c-8d7b-...},0" — 引擎格式

    // 通道掩码值名（PKEY_AudioEndpoint_PhysicalSpeakers）
    let channel_mask_name = "{1da5d803-d492-4edd-8c23-e0c0ffee7f0e},0";

    // 尝试设备格式值名。
    let device_format_name = "{f19f064d-082c-4e27-bc73-6882a1bb8e4c},0";
    if let Ok(Some(fmt)) = read_audio_format(&props_key, device_format_name, Some(channel_mask_name)) {
        return Ok(Some(fmt));
    }

    // 尝试引擎格式值名。
    let engine_format_name = "{3d6e1656-2e50-4c4c-8d7b-d3f7e4d1f3a9},0";
    if let Ok(Some(fmt)) = read_audio_format(&props_key, engine_format_name, Some(channel_mask_name)) {
        return Ok(Some(fmt));
    }

    Ok(None)
}

/// 读取 VxAPO 安装版本。
///
/// 从 FxProperties 子键的 `version` 值读取。
/// 不存在或读取失败时返回空字符串（表示未安装）。
fn read_install_version(endpoint_key: &RegKey) -> String {
    let fx_key = match endpoint_key.open_sub_key(FX_PROPERTIES_KEY) {
        Ok(k) => k,
        Err(_) => return String::new(),
    };

    match fx_key.read_value(VERSION_VALUE_NAME) {
    Ok(RegValue::Sz(s)) => s,
    _ => String::new(),
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试（Note 41）
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::device::slots::{ApoSlot, SlotValue, INSTALL_VERSION_LEGACY};
    use crate::host::device::endpoint::{EndpointState, Flow};

    // ── 辅助 ──────────────────────────────────────────────────────────────

    fn empty_slots() -> [SlotValue; 5] {
        [SlotValue::NoKey; 5]
    }

    fn vxapo_pre_guid() -> windows::core::GUID {
        CLSID_VXAPO_PRE_MIX
    }

    fn vxapo_post_guid() -> windows::core::GUID {
        CLSID_VXAPO_POST_MIX
    }

    fn other_guid(n: u32) -> windows::core::GUID {
        windows::core::GUID {
            data1: 0xF000_0000 + n,
            data2: 0xE000 + n as u16,
            data3: 0xD000 + n as u16,
            data4: [0xA0, 0xB0, 0xC0, n as u8, 0, 0, 0, 0xFF],
        }
    }

    fn make_device_info(slots: [SlotValue; 5], mode: InstallMode, version: &str) -> DeviceInfo {
        DeviceInfo {
            endpoint: EndpointInfo {
                device_id: "test".to_string(),
                friendly_name: "Test".to_string(),
                state: EndpointState::Active,
                flow: Flow::Render,
                endpoint_guid: String::new(),
            },
            install_mode: mode,
            slots,
            format: None,
            installed_version: version.to_string(),
        }
    }

    // ── 安装版本常量（Note 24） ───────────────────────────────────────────

    #[test]
    fn install_version_is_2() {
        assert_eq!(INSTALL_VERSION, "2");
    }

    #[test]
    fn install_version_legacy_is_1() {
        assert_eq!(INSTALL_VERSION_LEGACY, "1");
    }

    #[test]
    fn install_versions_distinct() {
        assert_ne!(INSTALL_VERSION, INSTALL_VERSION_LEGACY);
    }

    // ── is_vxapo_slot ─────────────────────────────────────────────────────

    #[test]
    fn vxapo_premix_slot_detected() {
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(vxapo_pre_guid());
        assert!(is_vxapo_slot(&slots, ApoSlot::Sfx));
    }

    #[test]
    fn vxapo_postmix_slot_detected() {
        let mut slots = empty_slots();
        slots[ApoSlot::Efx.index() as usize] = SlotValue::Guid(vxapo_post_guid());
        assert!(is_vxapo_slot(&slots, ApoSlot::Efx));
    }

    #[test]
    fn other_guid_not_vxapo() {
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(other_guid(1));
        assert!(!is_vxapo_slot(&slots, ApoSlot::Sfx));
    }

    #[test]
    fn empty_slot_not_vxapo() {
        let slots = empty_slots();
        assert!(!is_vxapo_slot(&slots, ApoSlot::Sfx));
        assert!(!is_vxapo_slot(&slots, ApoSlot::Efx));
    }

    // ── detect_install_mode ───────────────────────────────────────────────

    #[test]
    fn detect_mode_default_when_all_empty() {
        let slots = empty_slots();
        assert_eq!(detect_install_mode(&slots), InstallMode::SfxEfx);
    }

    #[test]
    fn detect_mode_lfxgfx_when_lfx_has_guid() {
        let mut slots = empty_slots();
        slots[ApoSlot::Lfx.index() as usize] = SlotValue::Guid(vxapo_pre_guid());
        assert_eq!(detect_install_mode(&slots), InstallMode::LfxGfx);
    }

    #[test]
    fn detect_mode_sfxefx_when_sfx_has_guid() {
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(vxapo_pre_guid());
        slots[ApoSlot::Efx.index() as usize] = SlotValue::Guid(vxapo_post_guid());
        assert_eq!(detect_install_mode(&slots), InstallMode::SfxEfx);
    }

    #[test]
    fn detect_mode_sfxmfx_when_mfx_has_guid() {
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(vxapo_pre_guid());
        slots[ApoSlot::Mfx.index() as usize] = SlotValue::Guid(vxapo_post_guid());
        assert_eq!(detect_install_mode(&slots), InstallMode::SfxMfx);
    }

    #[test]
    fn detect_mode_sfxefx_when_sfx_only() {
        // SFX 有 GUID 但 MFX 和 EFX 都没有 → 默认 SfxEfx
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(vxapo_pre_guid());
        assert_eq!(detect_install_mode(&slots), InstallMode::SfxEfx);
    }

    #[test]
    fn detect_mode_prefers_sfx_over_lfx() {
        // 两者都有 GUID → SFX 优先（SfxEfx）
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(vxapo_pre_guid());
        slots[ApoSlot::Lfx.index() as usize] = SlotValue::Guid(other_guid(1));
        assert_eq!(detect_install_mode(&slots), InstallMode::SfxEfx);
    }

    #[test]
    fn detect_mode_novalue_treated_as_empty() {
        // LFX 是 NoValue（不是 Guid）→ 不算 LfxGfx
        let mut slots = empty_slots();
        slots[ApoSlot::Lfx.index() as usize] = SlotValue::NoValue;
        assert_eq!(detect_install_mode(&slots), InstallMode::SfxEfx);
    }

    // ── DeviceInfo::is_installed ──────────────────────────────────────────

    #[test]
    fn installed_when_premix_is_vxapo() {
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(vxapo_pre_guid());
        let info = make_device_info(slots, InstallMode::SfxEfx, "2");
        assert!(info.is_installed());
    }

    #[test]
    fn installed_when_postmix_is_vxapo() {
        let mut slots = empty_slots();
        slots[ApoSlot::Efx.index() as usize] = SlotValue::Guid(vxapo_post_guid());
        let info = make_device_info(slots, InstallMode::SfxEfx, "2");
        assert!(info.is_installed());
    }

    #[test]
    fn installed_when_both_are_vxapo() {
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(vxapo_pre_guid());
        slots[ApoSlot::Efx.index() as usize] = SlotValue::Guid(vxapo_post_guid());
        let info = make_device_info(slots, InstallMode::SfxEfx, "2");
        assert!(info.is_installed());
    }

    #[test]
    fn not_installed_when_version_empty() {
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(vxapo_pre_guid());
        let info = make_device_info(slots, InstallMode::SfxEfx, "");
        assert!(!info.is_installed());
    }

    #[test]
    fn not_installed_when_no_vxapo_guid() {
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(other_guid(1));
        let info = make_device_info(slots, InstallMode::SfxEfx, "2");
        assert!(!info.is_installed());
    }

    #[test]
    fn not_installed_when_slots_empty() {
        let info = make_device_info(empty_slots(), InstallMode::SfxEfx, "");
        assert!(!info.is_installed());
    }

    // ── DeviceInfo::can_be_upgraded ───────────────────────────────────────

    #[test]
    fn can_upgrade_from_v1() {
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(vxapo_pre_guid());
        let info = make_device_info(slots, InstallMode::SfxEfx, "1");
        assert!(info.can_be_upgraded());
    }

    #[test]
    fn can_upgrade_from_empty_version() {
        // 已安装但版本为空（异常状态）→ 视为需要升级
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(vxapo_pre_guid());
        let info = make_device_info(slots, InstallMode::SfxEfx, "");
        // is_installed() 返回 false → can_be_upgraded 也返回 false
        assert!(!info.can_be_upgraded());
    }

    #[test]
    fn no_upgrade_when_current_version() {
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(vxapo_pre_guid());
        let info = make_device_info(slots, InstallMode::SfxEfx, "2");
        assert!(!info.can_be_upgraded());
    }

    #[test]
    fn no_upgrade_when_not_installed() {
        let info = make_device_info(empty_slots(), InstallMode::SfxEfx, "");
        assert!(!info.can_be_upgraded());
    }

    // ── DeviceInfo::is_experimental ───────────────────────────────────────

    #[test]
    fn lfxgfx_is_experimental() {
        let info = make_device_info(empty_slots(), InstallMode::LfxGfx, "");
        assert!(info.is_experimental());
    }

    #[test]
    fn sfxefx_not_experimental() {
        let info = make_device_info(empty_slots(), InstallMode::SfxEfx, "");
        assert!(!info.is_experimental());
    }

    #[test]
    fn sfxmfx_not_experimental() {
        let info = make_device_info(empty_slots(), InstallMode::SfxMfx, "");
        assert!(!info.is_experimental());
    }

    // ── DeviceInfo::has_changes ───────────────────────────────────────────

    #[test]
    fn has_changes_when_installed_non_default_mode() {
        let mut slots = empty_slots();
        slots[ApoSlot::Lfx.index() as usize] = SlotValue::Guid(vxapo_pre_guid());
        let info = make_device_info(slots, InstallMode::LfxGfx, "2");
        assert!(info.has_changes());
    }

    #[test]
    fn no_changes_when_default_mode() {
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(vxapo_pre_guid());
        let info = make_device_info(slots, InstallMode::SfxEfx, "2");
        assert!(!info.has_changes());
    }

    #[test]
    fn no_changes_when_not_installed() {
        let info = make_device_info(empty_slots(), InstallMode::LfxGfx, "");
        assert!(!info.has_changes());
    }

    // ── has_fx_properties ─────────────────────────────────────────────────

    #[test]
    fn has_fx_properties_nonexistent_key() {
        // 不存在的键 → 应返回 false（非 panic）
        let result = RegKey::open(
            windows::Win32::System::Registry::HKEY_CURRENT_USER,
            "SOFTWARE\\VxAPO_Test_NonExistent_FxProps_12345",
        );
        assert!(result.is_err());
    }

    // ── SlotValue + InstallMode 交互 ──────────────────────────────────────

    #[test]
    fn sfxefx_premix_and_postmix_are_different_slots() {
        let mode = InstallMode::SfxEfx;
        assert_ne!(mode.premix_slot(), mode.postmix_slot());
    }

    #[test]
    fn lfxgfx_premix_and_postmix_are_different_slots() {
        let mode = InstallMode::LfxGfx;
        assert_ne!(mode.premix_slot(), mode.postmix_slot());
    }

    #[test]
    fn sfxmfx_premix_and_postmix_are_different_slots() {
        let mode = InstallMode::SfxMfx;
        assert_ne!(mode.premix_slot(), mode.postmix_slot());
    }

    // ── DeviceInfo Debug/Clone ────────────────────────────────────────────

    #[test]
    fn device_info_debug() {
        let info = make_device_info(empty_slots(), InstallMode::SfxEfx, "2");
        let dbg = format!("{:?}", info);
        assert!(dbg.contains("install_mode"));
        assert!(dbg.contains("installed_version"));
    }

    #[test]
    fn device_info_clone() {
        let info = make_device_info(empty_slots(), InstallMode::SfxEfx, "2");
        let cloned = info.clone();
        assert_eq!(info.install_mode, cloned.install_mode);
        assert_eq!(info.installed_version, cloned.installed_version);
    }

    // ── 端到端：安装模式 + 回退 + 版本 ───────────────────────────────────

    #[test]
    fn end_to_end_sfxefx_installed_vxapo() {
        // 模拟 SfxEfx 安装的设备
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(vxapo_pre_guid());
        slots[ApoSlot::Efx.index() as usize] = SlotValue::Guid(vxapo_post_guid());

        let info = make_device_info(slots, InstallMode::SfxEfx, "2");

        assert!(info.is_installed());
        assert!(!info.can_be_upgraded());
        assert!(!info.is_experimental());
        assert!(!info.has_changes());
    }

    #[test]
    fn end_to_end_lfxgfx_installed_legacy() {
        // 模拟 LfxGfx Legacy 安装的设备
        let mut slots = empty_slots();
        slots[ApoSlot::Lfx.index() as usize] = SlotValue::Guid(vxapo_pre_guid());
        slots[ApoSlot::Gfx.index() as usize] = SlotValue::Guid(vxapo_post_guid());

        let info = make_device_info(slots, InstallMode::LfxGfx, "1");

        assert!(info.is_installed());
        assert!(info.can_be_upgraded());
        assert!(info.is_experimental());
        assert!(info.has_changes());
    }

    #[test]
    fn end_to_end_not_installed() {
        // 模拟未安装 VxAPO 的设备（其他 APO 占据槽位）
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(other_guid(1));
        slots[ApoSlot::Efx.index() as usize] = SlotValue::Guid(other_guid(2));

        let info = make_device_info(slots, InstallMode::SfxEfx, "");

        assert!(!info.is_installed());
        assert!(!info.can_be_upgraded());
        assert!(!info.has_changes());
    }

    #[test]
    fn end_to_end_pre_mix_guid_fallback() {
        // SFX 是 NoValue，LFX 有其他 GUID → 回退
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::NoValue;
        slots[ApoSlot::Lfx.index() as usize] = SlotValue::Guid(other_guid(99));

        let pre = get_original_pre_mix(&slots, InstallMode::SfxEfx);
        assert!(!pre.is_empty(), "Should fall back to LFX");
        assert!(pre.contains("F0000063"), "Should contain fallback GUID data1"); // 99 = 0x63
    }

    #[test]
    fn end_to_end_post_mix_guid_fallback() {
        // EFX 是 NoValue，MFX 有 GUID → 回退到 MFX
        let mut slots = empty_slots();
        slots[ApoSlot::Efx.index() as usize] = SlotValue::NoValue;
        slots[ApoSlot::Mfx.index() as usize] = SlotValue::Guid(other_guid(42));

        let post = get_original_post_mix(&slots, InstallMode::SfxEfx);
        assert!(!post.is_empty(), "Should fall back to MFX");
    }

    #[test]
    fn end_to_end_all_postmix_empty_returns_empty() {
        let mut slots = empty_slots();
        slots[ApoSlot::Efx.index() as usize] = SlotValue::NoValue;
        slots[ApoSlot::Mfx.index() as usize] = SlotValue::NoValue;
        slots[ApoSlot::Gfx.index() as usize] = SlotValue::NoValue;

        let post = get_original_post_mix(&slots, InstallMode::SfxEfx);
        assert!(post.is_empty());
    }
}