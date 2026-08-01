//! install/device/info.rs — 设备组合查询层（v6.3 规范 5.4）
//!
//! 组合 `endpoint`、`slots`、`format` 三个子模块，提供高层查询接口。
//! 只读，不修改系统状态。

use crate::install::device::endpoint::{query_endpoint, EndpointInfo};
use crate::install::device::format::{read_audio_format, AudioFormat};
use crate::install::device::slots::{
    read_all_slots, ApoSlot, InstallMode, SlotValue, FX_PROPERTIES_KEY, INSTALL_VERSION,
    INSTALL_VERSION_LEGACY,
};
use crate::object::vx_reg_props::{CLSID_VXAPO_POST_MIX, CLSID_VXAPO_PRE_MIX};
use crate::sys::registry::RegKey;
use crate::utils::vx_error::Result;
use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;

/// 存储版本号的注册表值名称。
const VERSION_VALUE_NAME: &str = "version";

/// MMDevices 渲染端点根路径。
const RENDER_PATH: &str =
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render";

/// MMDevices 采集端点根路径。
const CAPTURE_PATH: &str =
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture";

// ══════════════════════════════════════════════════════════════════════════════
// DeviceInfo — 组合查询结果
// ══════════════════════════════════════════════════════════════════════════════

/// 设备综合信息（查询结果，只读）。
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// 端点信息。
    pub endpoint: Option<EndpointInfo>,
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
    /// VxAPO 是否已安装。
    ///
    /// 判定：版本非空，且 PreMix 或 PostMix 槽位含 VxAPO CLSID。
    pub fn is_installed(&self) -> bool {
        if self.installed_version.is_empty() {
            return false;
        }

        let pre_slot = self.install_mode.premix_slot();
        let post_slot = self.install_mode.postmix_slot();

        is_vxapo_slot(&self.slots, pre_slot) || is_vxapo_slot(&self.slots, post_slot)
    }

    /// 是否可升级（已安装且版本低于当前 INSTALL_VERSION）。
    pub fn can_be_upgraded(&self) -> bool {
        if !self.is_installed() {
            return false;
        }
        self.installed_version != INSTALL_VERSION
    }

    /// 是否为实验性安装（LfxGfx 模式）。
    pub fn is_experimental(&self) -> bool {
        self.install_mode == InstallMode::LfxGfx
    }

    /// 音频增强是否被禁用。
    ///
    /// Windows 的 `DisableEnhancements` 值为非零时表示禁用。
    pub fn is_enhancements_disabled(&self, endpoint_key: &RegKey) -> bool {
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

    /// 当前安装版本是否为 Legacy（"1"）。
    pub fn is_legacy(&self) -> bool {
        self.installed_version == INSTALL_VERSION_LEGACY
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 公开 API
// ══════════════════════════════════════════════════════════════════════════════

/// 查询设备综合信息。
///
/// 组合 `endpoint`、`slots`、`format` 三个子模块的查询结果。
///
/// `endpoint_key`：已打开的端点根键。
pub fn query_device_info(endpoint_key: &RegKey) -> Result<Option<DeviceInfo>> {
    // ── 端点信息 ──────────────────────────────────────────────────────────

    let endpoint = query_endpoint(endpoint_key)?;

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

/// 设备枚举（遍历 MMDevices\\Audio\\Render 和 Capture 下所有端点）。
///
/// 当前版本受限于 sys/registry 无键枚举 API——返回空列表。
/// 上层可通过已知设备 GUID 直接调用 `query_device_info` / `install_endpoint`。
pub fn enumerate_devices() -> Result<Vec<DeviceInfo>> {
    let result = Vec::new();

    // TODO(v6.2): sys/registry 增加键枚举能力后，遍历 Render/Capture 下的
    // `{GUID}` 子键并逐一点调用 query_device_info。
    let _ = RENDER_PATH;
    let _ = CAPTURE_PATH;
    let _ = HKEY_LOCAL_MACHINE;

    Ok(result)
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
        SlotValue::Guid(g) => *g == CLSID_VXAPO_PRE_MIX || *g == CLSID_VXAPO_POST_MIX,
        _ => false,
    }
}

/// 检测当前安装模式。
///
/// 逻辑（规范 5.4）：
/// 1. SFX 有 GUID + MFX 有 GUID → SfxMfx
/// 2. SFX 有 GUID + MFX 无 GUID → SfxEfx
/// 3. LFX 有 GUID + SFX 无 GUID → LfxGfx
/// 4. 都没有 → SfxEfx（默认）
fn detect_install_mode(slots: &[SlotValue; 5]) -> InstallMode {
    let sfx_has_guid = slots[ApoSlot::Sfx.index() as usize].is_guid();
    let lfx_has_guid = slots[ApoSlot::Lfx.index() as usize].is_guid();
    let mfx_has_guid = slots[ApoSlot::Mfx.index() as usize].is_guid();

    match (lfx_has_guid, sfx_has_guid, mfx_has_guid) {
        (_, true, true) => InstallMode::SfxMfx,
        (_, true, false) => InstallMode::SfxEfx,
        (true, false, _) => InstallMode::LfxGfx,
        _ => InstallMode::default_mode(),
    }
}

/// 从端点键读取音频格式。
///
/// 尝试从 Properties 子键读取 PKEY 格式值（WAVEFORMATEX 二进制）。
fn read_format_for_endpoint(endpoint_key: &RegKey) -> Result<Option<AudioFormat>> {
    let props_key = match endpoint_key.open_sub_key("Properties") {
        Ok(k) => k,
        Err(_) => return Ok(None),
    };

    // 常见值名：`{PKEY_GUID},PID=0`。
    // 简化：尝试设备格式 PKEY（{f19f064d-...} 由 Windows 定义），
    // 具体值名由调用方传入。
    let device_format_name = "{f19f064d-82c0-4e00-bce4-7f12f211c8f2},0";
    let channel_mask_name = "{1da5d803-d492-4edd-8c23-e0c0ffee7f0e},0";

    read_audio_format(&props_key, device_format_name, Some(channel_mask_name))
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
        Ok(crate::sys::registry::RegValue::Sz(s)) => s,
        _ => String::new(),
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试（Note 41）
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::device::endpoint::{EndpointState, Flow};

    fn empty_slots() -> [SlotValue; 5] {
        [SlotValue::NoKey; 5]
    }

    fn vxapo_pre_guid() -> windows::core::GUID {
        CLSID_VXAPO_PRE_MIX
    }

    fn vxapo_post_guid() -> windows::core::GUID {
        CLSID_VXAPO_POST_MIX
    }

    fn make_device_info(slots: [SlotValue; 5], mode: InstallMode, version: &str) -> DeviceInfo {
        DeviceInfo {
            endpoint: Some(EndpointInfo {
                device_id: "test".to_string(),
                friendly_name: "Test".to_string(),
                state: EndpointState::Active,
                flow: Flow::Render,
                endpoint_guid: String::new(),
            }),
            install_mode: mode,
            slots,
            format: None,
            installed_version: version.to_string(),
        }
    }

    #[test]
    fn detect_mode_when_all_empty() {
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
    fn not_installed_when_version_empty() {
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(vxapo_pre_guid());
        let info = make_device_info(slots, InstallMode::SfxEfx, "");
        assert!(!info.is_installed());
    }

    #[test]
    fn not_installed_when_no_vxapo_guid() {
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(windows::core::GUID::zeroed());
        let info = make_device_info(slots, InstallMode::SfxEfx, "2");
        assert!(!info.is_installed());
    }

    #[test]
    fn can_upgrade_from_v1() {
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(vxapo_pre_guid());
        let info = make_device_info(slots, InstallMode::SfxEfx, "1");
        assert!(info.can_be_upgraded());
        assert!(info.is_legacy());
    }

    #[test]
    fn no_upgrade_when_current_version() {
        let mut slots = empty_slots();
        slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(vxapo_pre_guid());
        let info = make_device_info(slots, InstallMode::SfxEfx, "2");
        assert!(!info.can_be_upgraded());
        assert!(!info.is_legacy());
    }

    #[test]
    fn lfxgfx_is_experimental() {
        let info = make_device_info(empty_slots(), InstallMode::LfxGfx, "2");
        assert!(info.is_experimental());
    }

    #[test]
    fn sfxefx_not_experimental() {
        let info = make_device_info(empty_slots(), InstallMode::SfxEfx, "2");
        assert!(!info.is_experimental());
    }

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

    #[test]
    fn enumerate_devices_returns_empty_for_now() {
        assert!(enumerate_devices().unwrap().is_empty());
    }
}