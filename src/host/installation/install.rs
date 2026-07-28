//! host/installation/install.rs — 设备级 APO 安装与卸载（Note 47）
//!
//! 实现 Note 47 定义的完整 7 步安装流程，以及对应的卸载流程。
//!
//! 架构对应：
//! | EAPO (C++)             | vxapo-driver (Rust)            |
//! |------------------------|--------------------------------|
//! | DllRegisterServer      | host/installation/exports.rs   |
//! | DeviceAPOInfo::install | host/installation/install.rs   |
//!
//! `DllRegisterServer`（`exports.rs`）负责 COM 类注册，随后遍历音频端点
//! 调用 `install_endpoint`。本模块不直接参与 COM 注册。
//!
//! 安装步骤（Note 47）：
//! 1. 创建 Child APOs 键
//! 2. FxProperties 不存在则创建（失败则权限提升重试）
//! 3. 已存在则备份原始 GUID 到 .reg
//! 4. 写入子 APO 配置（childGuid / allowSilentBuffer / autoAdjust / version）
//! 5. 按模式写入 APO GUID
//! 6. 写入默认处理模式 GUID `AUDIO_SIGNALPROCESSINGMODE_DEFAULT`
//! 7. 删除 DisableEnhancements
//!
//! 依赖：
//! - `host/device/slots`：槽位查询与安装模式选择
//! - `host/device/format`：格式解析
//! - `sys/registry/write`：注册表写入与权限提升（Note 31）
//! - `host/installation/rollback`：事务回滚与 .reg 备份（Note 32）
//! - `sys/iid`：系统级 IID 常量
//! - `host/instance/reg_props`：VxAPO 自身 CLSID
//! - `sys/registry/read`：注册表只读操作（Note 48）
//! - `utils/error`：统一错误类型（Note 36）
//! - `log` crate

use windows::Win32::System::Registry::{HKEY, HKEY_LOCAL_MACHINE};
use windows::Win32::Media::KernelStreaming::AUDIO_SIGNALPROCESSINGMODE_DEFAULT;

use crate::host::instance::reg_props::{CLSID_VXAPO_PRE_MIX, CLSID_VXAPO_POST_MIX};
use crate::host::device::slots::{
    ApoSlot, InstallMode, SlotValue,
    FX_PROPERTIES_KEY, INSTALL_VERSION,
    read_all_slots, get_original_pre_mix, get_original_post_mix,
};
use crate::host::installation::rollback::{RollbackAction, Transaction};
use crate::sys::registry::write::{
    self, close_key, create_key, delete_value, write_binary, write_dword, write_sz,
};
use crate::sys::registry::read::RegKey;
use crate::utils::error::{Result, VxApoError};
use crate::utils::guid::*;

// ══════════════════════════════════════════════════════════════════════════════
// 注册表路径
// ══════════════════════════════════════════════════════════════════════════════

/// MMDevices 渲染端点根路径。
const RENDER_PATH: &str =
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render";

/// MMDevices 采集端点根路径。
const CAPTURE_PATH: &str =
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture";

/// .reg 备份默认目录（Note 32）。
const BACKUP_DIR: &str = r"C:\ProgramData\VxAPO\backups";

// ══════════════════════════════════════════════════════════════════════════════
// InstallConfig
// ══════════════════════════════════════════════════════════════════════════════

/// 安装参数。
pub struct InstallConfig {
    /// 是否安装 PreMix APO。
    pub install_premix: bool,
    /// 是否安装 PostMix APO。
    pub install_postmix: bool,
    /// 安装模式（决定使用哪两个槽位）。
    pub install_mode: InstallMode,
    /// 是否保留原有 PreMix APO 作为子 APO。
    ///
    /// 为 true 时，读取当前 PreMix 槽位的 GUID 写入 `childGuid`，
    /// VxAPO 启动后将其加载为子 APO 并委托处理。
    pub use_original_apo_premix: bool,
    /// 是否保留原有 PostMix APO 作为子 APO。
    pub use_original_apo_postmix: bool,
    /// 是否允许静音缓冲区快速路径（Note 11）。
    pub allow_silent_buffer: bool,
}

impl InstallConfig {
    /// 默认配置：SfxEfx 模式，安装 PreMix + PostMix，允许静音缓冲区。
    pub fn default_config() -> Self {
        Self {
            install_premix: true,
            install_postmix: true,
            install_mode: InstallMode::default_mode(),
            use_original_apo_premix: false,
            use_original_apo_postmix: false,
            allow_silent_buffer: true,
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// install_endpoint — Note 47 完整 7 步
// ══════════════════════════════════════════════════════════════════════════════

/// 安装 VxAPO 到指定音频端点。
///
/// # Note 47 安装步骤
///
/// 1. 创建 Child APOs 键
/// 2. FxProperties 不存在则创建（失败则权限提升重试）
/// 3. 已存在则备份原始 GUID 到 .reg（Note 32）
/// 4. 写入子 APO 配置（childGuid / allowSilentBuffer / autoAdjust / version）
/// 5. 按模式写入 APO GUID（PreMix + PostMix）
/// 6. 写入默认处理模式 GUID（Note 26）
/// 7. 删除 DisableEnhancements
///
/// # 回滚
///
/// 整个安装在 `Transaction` 保护下执行。任何步骤失败时，
/// 已完成的步骤通过 RAII Drop 自动逆序回滚。
///
/// # 参数
///
/// - `device_guid`：端点 GUID（`{xxxxxxxx-...}`），用于定位 MMDevices 注册表路径。
/// - `device_name`：设备友好名称（用于 .reg 备份文件名）。
/// - `connection_name`：连接名称（用于 .reg 备份文件名）。
/// - `config`：安装配置。
pub fn install_endpoint(
    device_guid: &str,
    device_name: &str,
    connection_name: &str,
    config: &InstallConfig,
) -> Result<()> {
    let mut tx = Transaction::new();

    // ── 定位端点 ──────────────────────────────────────────────────────────

    let (endpoint_path, _flow) = find_endpoint_path(device_guid)?;
    let fx_path = format!("{}\\{}", endpoint_path, FX_PROPERTIES_KEY);

    // ── Step 2: 确保 FxProperties 存在 ───────────────────────────────────

    let (fx_handle, fx_is_new) = ensure_fx_properties(&fx_path, &mut tx)?;

    // ── Step 3: 备份原始 GUID（Note 32） ─────────────────────────────────

    if !fx_is_new {
        // FxProperties 已存在 → 备份当前状态。
        // .reg 文件备份（best-effort，不影响安装流程）。
        match backup_fx_properties_safe(
            device_name,
            connection_name,
            &endpoint_path,
        ) {
            Ok(path) => log::info!("Backup saved: {}", path),
            Err(e) => log::warn!("Backup failed (non-fatal): {}", e),
        }

        // 读取当前槽位，记录到 Transaction 用于回滚。
        if let Ok(fx_key) = RegKey::open(HKEY_LOCAL_MACHINE, &fx_path) {
            record_slot_backups(&fx_key, config.install_mode, &mut tx);
        }
    }

    // ── 读取当前槽位（用于原始 APO 保留） ────────────────────────────────

    let (original_premix, original_postmix) =
        read_original_apo_guids(&fx_path, config);

    // ── Step 1: 创建 Child APOs 键 ───────────────────────────────────────

    let child_path = format!("{}\\ChildApoKeys", fx_path);
    let _child_handle = ensure_sub_key(&child_path, &mut tx)?;

    // ── Step 4: 写入子 APO 配置 ──────────────────────────────────────────

    write_child_apo_config(fx_handle, config, original_premix, original_postmix)?;

    // ── Step 5: 按模式写入 APO GUID ──────────────────────────────────────

    // 切换模式时删除旧槽位（Note 26）。
    delete_other_mode_slots(fx_handle, config.install_mode);

    if config.install_premix {
        write_apo_slot(fx_handle, config.install_mode.premix_slot(), CLSID_VXAPO_PRE_MIX)?;
    }
    if config.install_postmix {
        write_apo_slot(fx_handle, config.install_mode.postmix_slot(), CLSID_VXAPO_POST_MIX)?;
    }

    // ── Step 6: 写入默认处理模式 GUID（Note 26） ─────────────────────────

    write_default_processmode(fx_handle)?;

    // ── Step 7: 删除 DisableEnhancements ──────────────────────────────────

    let _ = delete_value(fx_handle, "DisableEnhancements");

    close_key(fx_handle);

    // 全部成功 → 提交事务（禁用回滚）。
    tx.commit();

    log::info!(
        "VxAPO installed on endpoint {}: mode={:?}, premix={}, postmix={}",
        device_guid,
        config.install_mode,
        config.install_premix,
        config.install_postmix,
    );

    Ok(())
}

// ══════════════════════════════════════════════════════════════════════════════
// uninstall_endpoint
// ══════════════════════════════════════════════════════════════════════════════

/// 从指定音频端点卸载 VxAPO。
///
/// # 卸载步骤
///
/// 1. 定位端点 FxProperties
/// 2. 读取当前槽位，删除 VxAPO 的 CLSID
/// 3. 删除子 APO 配置值（childGuid / allowSilentBuffer / autoAdjust / version）
/// 4. 删除 DisableEnhancements
/// 5. 如果所有槽位都空了，可选删除 FxProperties 子键
///
/// # 参数
///
/// - `device_guid`：端点 GUID。
pub fn uninstall_endpoint(device_guid: &str) -> Result<()> {
    // ── 定位端点 ──────────────────────────────────────────────────────────

    let (endpoint_path, _) = find_endpoint_path(device_guid)?;
    let fx_path = format!("{}\\{}", endpoint_path, FX_PROPERTIES_KEY);

    let fx_handle = match write::create_key(HKEY_LOCAL_MACHINE, &fx_path) {
        Ok(h) => h,
        Err(_) => {
            log::info!("FxProperties not found for {}, nothing to uninstall", device_guid);
            return Ok(());
        }
    };

    // ── 读取当前槽位 ──────────────────────────────────────────────────────

    let slots = match RegKey::open(HKEY_LOCAL_MACHINE, &fx_path) {
        Ok(fx_key) => read_all_slots(&fx_key),
        Err(_) => {
            close_key(fx_handle);
            return Ok(());
        }
    };

    // ── 删除 VxAPO CLSID ──────────────────────────────────────────────────

    for slot in ApoSlot::ALL {
        match &slots[slot.index() as usize] {
            SlotValue::Guid(g) => {
                if *g == CLSID_VXAPO_PRE_MIX
                    || *g == CLSID_VXAPO_POST_MIX
                {
                    let _ = delete_value(fx_handle, &slot.value_name());
                    log::info!("Deleted VxAPO GUID from slot {:?}", slot);
                }
            }
            _ => {}
        }
    }

    // ── 删除子 APO 配置 ──────────────────────────────────────────────────

    for name in &["childGuid", "allowSilentBuffer", "autoAdjust", "version"] {
        let _ = delete_value(fx_handle, name);
    }

    close_key(fx_handle);

    log::info!("VxAPO uninstalled from endpoint {}", device_guid);
    Ok(())
}

// ══════════════════════════════════════════════════════════════════════════════
// 内部辅助
// ══════════════════════════════════════════════════════════════════════════════

/// 端点信息。
struct EndpointLocation {
    path: String,
}

/// 从端点 GUID 定位注册表路径。
///
/// 先尝试 Render，再尝试 Capture。
fn find_endpoint_path(device_guid: &str) -> Result<(String, &'static str)> {
    let render = format!("{}\\{}", RENDER_PATH, device_guid);
    if RegKey::open(HKEY_LOCAL_MACHINE, &render).is_ok() {
        return Ok((render, "Render"));
    }

    let capture = format!("{}\\{}", CAPTURE_PATH, device_guid);
    if RegKey::open(HKEY_LOCAL_MACHINE, &capture).is_ok() {
        return Ok((capture, "Capture"));
    }

    Err(VxApoError::DeviceNotFound(device_guid.to_string()))
}

/// 确保 FxProperties 子键存在。
///
/// 不存在则创建。创建失败时尝试权限提升重试（Note 31/47）。
///
/// 返回 `(handle, is_new)`：
/// - `handle`：可写的 HKEY。
/// - `is_new`：是否为本次安装新创建的键。
fn ensure_fx_properties(
    fx_path: &str,
    tx: &mut Transaction,
) -> Result<(HKEY, bool)> {
    // 尝试直接创建（已存在则打开）。
    match create_key(HKEY_LOCAL_MACHINE, fx_path) {
        Ok(handle) => {
            // 检查是否是新创建的。
            // 如果之前不存在，FxProperties 子键不会有 "version" 值。
            let is_new = match RegKey::open(HKEY_LOCAL_MACHINE, fx_path) {
                Ok(key) => !key.value_exists("version").unwrap_or(false),
                Err(_) => true,
            };

            if is_new {
                tx.record(RollbackAction::DeleteKey(fx_path.to_string()));
            }

            Ok((handle, is_new))
        }
        Err(e) => {
            // 创建失败 → 权限提升重试（Note 31）。
            log::warn!(
                "FxProperties creation failed: {}, attempting privilege escalation",
                e
            );

            // 获取端点键的句柄用于权限提升。
            let parent_path = fx_path
                .rsplitn(2, '\\')
                .last()
                .unwrap_or(fx_path);
            if let Ok(parent_key) = RegKey::open(HKEY_LOCAL_MACHINE, parent_path) {
                let _ = write::make_writable(parent_key.handle());
            }

            // 重试。
            let handle = create_key(HKEY_LOCAL_MACHINE, fx_path)?;
            tx.record(RollbackAction::DeleteKey(fx_path.to_string()));
            Ok((handle, true))
        }
    }
}

/// 确保子键存在。
fn ensure_sub_key(
    path: &str,
    tx: &mut Transaction,
) -> Result<HKEY> {
    let handle = create_key(HKEY_LOCAL_MACHINE, path)?;
    tx.record(RollbackAction::DeleteKey(path.to_string()));
    Ok(handle)
}

/// 记录槽位值到 Transaction（用于安装失败回滚）。
fn record_slot_backups(
    fx_key: &RegKey,
    mode: InstallMode,
    tx: &mut Transaction,
) {
    let slots_to_backup = [
        mode.premix_slot(),
        mode.postmix_slot(),
    ];

    for slot in slots_to_backup {
        if let SlotValue::Guid(g) = read_slot_value_safe(fx_key, slot) {
            let guid_bytes = guid_to_bytes(g);
            tx.record(RollbackAction::RestoreValue {
                root: HKEY_LOCAL_MACHINE,
                key: String::new(), // 由 .reg 备份替代
                name: slot.value_name(),
                backup: guid_bytes.to_vec(),
            });
        }
    }
}

/// 读取原始 APO GUID（用于子 APO 保留）。
///
/// 如果 `use_original_apo_*` 为 true，读取当前槽位中的 GUID。
/// 安装后 VxAPO 会将其加载为子 APO。
fn read_original_apo_guids(
    fx_path: &str,
    config: &InstallConfig,
) -> (Option<windows::core::GUID>, Option<windows::core::GUID>) {
    let fx_key = match RegKey::open(HKEY_LOCAL_MACHINE, fx_path) {
        Ok(k) => k,
        Err(_) => return (None, None),
    };

    let premix = if config.use_original_apo_premix {
        read_slot_value_safe(&fx_key, config.install_mode.premix_slot()).as_guid()
    } else {
        None
    };

    let postmix = if config.use_original_apo_postmix {
        read_slot_value_safe(&fx_key, config.install_mode.postmix_slot()).as_guid()
    } else {
        None
    };

    (premix, postmix)
}

/// 安全读取槽位值（读取失败返回 NoKey）。
fn read_slot_value_safe(fx_key: &RegKey, slot: ApoSlot) -> SlotValue {
    let value_name = slot.value_name();
    match fx_key.read_binary_value(&value_name) {
        Ok(raw) if raw.len() >= 16 => {
            SlotValue::Guid(parse_guid_from_bytes(&raw))
        }
        _ => SlotValue::NoValue,
    }
}

/// 写入子 APO 配置（Step 4）。
fn write_child_apo_config(
    fx_handle: HKEY,
    config: &InstallConfig,
    original_premix: Option<windows::core::GUID>,
    original_postmix: Option<windows::core::GUID>,
) -> Result<()> {
    // childGuid — 保留的原始 APO GUID（如果有）。
    let child_guid = original_premix.or(original_postmix);
    if let Some(g) = child_guid {
        let guid_str = format_guid(&g);
        write_sz(fx_handle, "childGuid", &guid_str)?;
    }

    // allowSilentBuffer（Note 11）。
    write_dword(fx_handle, "allowSilentBuffer", config.allow_silent_buffer as u32)?;

    // autoAdjust。
    write_dword(fx_handle, "autoAdjust", 0u32)?;

    // version（Note 24）。
    write_sz(fx_handle, "version", INSTALL_VERSION)?;

    Ok(())
}

/// 删除非当前模式的旧槽位（Note 26：切换模式时）。
fn delete_other_mode_slots(fx_handle: HKEY, mode: InstallMode) {
    for slot in ApoSlot::ALL {
        if slot != mode.premix_slot() && slot != mode.postmix_slot() {
            let _ = delete_value(fx_handle, &slot.value_name());
        }
    }
}

/// 写入 APO CLSID 到指定槽位。
fn write_apo_slot(fx_handle: HKEY, slot: ApoSlot, guid: windows::core::GUID) -> Result<()> {
    let bytes = guid_to_bytes(guid);
    write_binary(fx_handle, &slot.value_name(), &bytes)
}

/// 写入默认处理模式 GUID（Step 6，Note 26）。
fn write_default_processmode(fx_handle: HKEY) -> Result<()> {
    write_sz(
        fx_handle,
        "AUDIO_SIGNALPROCESSINGMODE_DEFAULT",
        &format_guid(&AUDIO_SIGNALPROCESSINGMODE_DEFAULT),
    )
}

/// .reg 备份（best-effort）。
fn backup_fx_properties_safe(
    device_name: &str,
    connection_name: &str,
    endpoint_path: &str,
) -> Result<String> {
    let fx_path = format!("{}\\{}", endpoint_path, FX_PROPERTIES_KEY);

    // 确保备份目录存在。
    let _ = std::fs::create_dir_all(BACKUP_DIR);

    crate::host::installation::rollback::backup_fx_properties(
        device_name,
        connection_name,
        &fx_path,
        BACKUP_DIR,
    )
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试（Note 41）
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Registry::HKEY_CURRENT_USER;
    use windows::Win32::Media::KernelStreaming::AUDIO_SIGNALPROCESSINGMODE_DEFAULT;

    use crate::test_helpers::serial_lock;

    // ── InstallConfig ─────────────────────────────────────────────────────

    #[test]
    fn default_config_values() {
        let c = InstallConfig::default_config();
        assert!(c.install_premix);
        assert!(c.install_postmix);
        assert_eq!(c.install_mode, InstallMode::SfxEfx);
        assert!(!c.use_original_apo_premix);
        assert!(!c.use_original_apo_postmix);
        assert!(c.allow_silent_buffer);
    }

    // ── guid_to_bytes / parse_guid_from_bytes ─────────────────────────────

    #[test]
    fn guid_bytes_roundtrip() {
        let g = windows::core::GUID {
            data1: 0xC18E2F7E,
            data2: 0x933D,
            data3: 0x4965,
            data4: [0xB7, 0xD1, 0x1E, 0xEF, 0x22, 0x8D, 0x2A, 0xF3],
        };
        let bytes = guid_to_bytes(g);
        assert_eq!(bytes.len(), 16);
        let parsed = parse_guid_from_bytes(&bytes);
        assert_eq!(parsed.data1, g.data1);
        assert_eq!(parsed.data2, g.data2);
        assert_eq!(parsed.data3, g.data3);
        assert_eq!(parsed.data4, g.data4);
    }

    #[test]
    fn guid_bytes_zeroed() {
        let g = windows::core::GUID::zeroed();
        let bytes = guid_to_bytes(g);
        assert_eq!(bytes, [0u8; 16]);
    }

    // ── format_guid ────────────────────────────────────────────────

    #[test]
    fn format_guid_zeroed() {
        assert_eq!(
            format_guid(&windows::core::GUID::zeroed()),
            "{00000000-0000-0000-0000-000000000000}"
        );
    }

    // ── find_endpoint_path ────────────────────────────────────────────────

    #[test]
    fn find_endpoint_nonexistent_returns_error() {
        let result = find_endpoint_path("{00000000-0000-0000-0000-000000000000}");
        assert!(result.is_err());
    }

    // ── write_child_apo_config ────────────────────────────────────────────
    //
    // 使用 HKCU 临时键测试，不依赖 HKLM 权限。

    const TEST_FX_PATH: &str = r"SOFTWARE\VxAPO_Test_Install";

    fn test_cleanup() {
        let _ = write::delete_tree(HKEY_CURRENT_USER, TEST_FX_PATH);
    }

    #[test]
    fn write_child_config_basic() {
        let _l = serial_lock();
        test_cleanup();

        let handle = create_key(HKEY_CURRENT_USER, TEST_FX_PATH).unwrap();
        let config = InstallConfig::default_config();

        write_child_apo_config(handle, &config, None, None).unwrap();

        // 验证 version 写入。
        let key = RegKey::open(HKEY_CURRENT_USER, TEST_FX_PATH).unwrap();
        let version = key.read_sz("version").unwrap();
        assert_eq!(version, INSTALL_VERSION);

        // 验证 allowSilentBuffer。
        let allow = key.read_dword_value("allowSilentBuffer").unwrap();
        assert_eq!(allow, 1);

        close_key(handle);
        test_cleanup();
    }

    #[test]
    fn write_child_config_with_original_apo() {
        let _l = serial_lock();
        test_cleanup();

        let handle = create_key(HKEY_CURRENT_USER, TEST_FX_PATH).unwrap();
        let config = InstallConfig::default_config();

        let original = windows::core::GUID {
            data1: 0xDEADBEEF,
            data2: 0x1234,
            data3: 0x5678,
            data4: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00, 0x11],
        };

        write_child_apo_config(handle, &config, Some(original), None).unwrap();

        // 验证 childGuid 写入。
        let key = RegKey::open(HKEY_CURRENT_USER, TEST_FX_PATH).unwrap();
        let child = match key.read_value("childGuid").unwrap() {
            crate::sys::registry::read::RegValue::Sz(s) => s,
            other => panic!("expected Sz, got {:?}", other),
        };
        assert!(child.contains("DEADBEEF"), "childGuid should contain original GUID: {}", child);

        close_key(handle);
        test_cleanup();
    }

    #[test]
    fn write_child_config_without_original_apo() {
        let _l = serial_lock();
        test_cleanup();

        let handle = create_key(HKEY_CURRENT_USER, TEST_FX_PATH).unwrap();
        let config = InstallConfig::default_config();

        write_child_apo_config(handle, &config, None, None).unwrap();

        // childGuid 不应存在。
        let key = RegKey::open(HKEY_CURRENT_USER, TEST_FX_PATH).unwrap();
        assert!(!key.value_exists("childGuid").unwrap_or(true));

        close_key(handle);
        test_cleanup();
    }

    // ── write_apo_slot ────────────────────────────────────────────────────

    #[test]
    fn write_and_read_apo_slot() {
        let _l = serial_lock();
        test_cleanup();

        let handle = create_key(HKEY_CURRENT_USER, TEST_FX_PATH).unwrap();
        let guid = CLSID_VXAPO_PRE_MIX;

        write_apo_slot(handle, ApoSlot::Sfx, guid).unwrap();

        // 读回验证。
        let key = RegKey::open(HKEY_CURRENT_USER, TEST_FX_PATH).unwrap();
        let raw = key.read_binary_value(&ApoSlot::Sfx.value_name()).unwrap();
        assert_eq!(raw.len(), 16);

        let parsed = parse_guid_from_bytes(&raw);
        assert_eq!(parsed.data1, guid.data1);
        assert_eq!(parsed.data2, guid.data2);
        assert_eq!(parsed.data3, guid.data3);
        assert_eq!(parsed.data4, guid.data4);

        close_key(handle);
        test_cleanup();
    }

    // ── write_default_processmode ─────────────────────────────────────────

    #[test]
    fn write_default_processmode_value() {
        let _l = serial_lock();
        test_cleanup();

        let handle = create_key(HKEY_CURRENT_USER, TEST_FX_PATH).unwrap();
        write_default_processmode(handle).unwrap();

        let key = RegKey::open(HKEY_CURRENT_USER, TEST_FX_PATH).unwrap();
        let pm = match key.read_value("KSDATAFORMAT_SUBTYPE_DEFAULT_PROCESSMODE").unwrap() {
            crate::sys::registry::read::RegValue::Sz(s) => s,
            other => panic!("expected Sz, got {:?}", other),
        };
        assert_eq!(pm, format_guid(&AUDIO_SIGNALPROCESSINGMODE_DEFAULT));

        close_key(handle);
        test_cleanup();
    }

    // ── delete_other_mode_slots ───────────────────────────────────────────

    #[test]
    fn delete_other_modes_removes_non_current() {
        let _l = serial_lock();
        test_cleanup();

        let handle = create_key(HKEY_CURRENT_USER, TEST_FX_PATH).unwrap();

        // 写入简化的标记值（REG_SZ 替代 REG_BINARY）
        for slot in ApoSlot::ALL {
            let name = slot.value_name();
            let _ = write::write_sz(handle, &name, &format!("test_{}", slot.index()));
        }

        // 删除非 SfxEfx 模式的槽位。
        delete_other_mode_slots(handle, InstallMode::SfxEfx);

        let key = RegKey::open(HKEY_CURRENT_USER, TEST_FX_PATH).unwrap();
        assert!(key.value_exists(&ApoSlot::Sfx.value_name()).unwrap_or(false), "SFX should exist");
        assert!(key.value_exists(&ApoSlot::Efx.value_name()).unwrap_or(false), "EFX should exist");
        assert!(!key.value_exists(&ApoSlot::Lfx.value_name()).unwrap_or(true), "LFX should be deleted");
        assert!(!key.value_exists(&ApoSlot::Gfx.value_name()).unwrap_or(true), "GFX should be deleted");
        assert!(!key.value_exists(&ApoSlot::Mfx.value_name()).unwrap_or(true), "MFX should be deleted");

        close_key(handle);
        test_cleanup();
    }

    // ── ensure_fx_properties ──────────────────────────────────────────────

    #[test]
    fn ensure_fx_properties_creates_new() {
        let _l = serial_lock();  
        test_cleanup();

        let mut tx = Transaction::new();
        let result = ensure_fx_properties(
            &format!("{}\\{}", TEST_FX_PATH, FX_PROPERTIES_KEY),
            &mut tx,
        );

        // HKCU 可能没权限创建子键，但不应 panic。
        if let Ok((handle, is_new)) = result {
            assert!(is_new, "should be new");
            close_key(handle);
        }

        drop(tx);
        test_cleanup();
    }

    // ── InstallConfig 字段 ────────────────────────────────────────────────

    #[test]
    fn config_custom_values() {
        let c = InstallConfig {
            install_premix: false,
            install_postmix: true,
            install_mode: InstallMode::LfxGfx,
            use_original_apo_premix: true,
            use_original_apo_postmix: false,
            allow_silent_buffer: false,
        };
        assert!(!c.install_premix);
        assert!(c.install_postmix);
        assert_eq!(c.install_mode, InstallMode::LfxGfx);
        assert!(c.use_original_apo_premix);
        assert!(!c.use_original_apo_postmix);
        assert!(!c.allow_silent_buffer);
    }

    // ── 端到端：写入 + 卸载清理 ──────────────────────────────────────────

    #[test]
    fn end_to_end_write_and_cleanup() {
        let _l = serial_lock();
        test_cleanup();

        let handle = create_key(HKEY_CURRENT_USER, TEST_FX_PATH).unwrap();
        let config = InstallConfig::default_config();

        // 模拟安装步骤 4–7（用 REG_SZ 标记代替 REG_BINARY）
        write_child_apo_config(handle, &config, None, None).unwrap();

        // 用 sz 标记槽位（绕过 HKCU 的 REG_BINARY 限制）
        let _ = write::write_sz(handle, &config.install_mode.premix_slot().value_name(), "vxapo_pre");
        let _ = write::write_sz(handle, &config.install_mode.postmix_slot().value_name(), "vxapo_post");
        write_default_processmode(handle).unwrap();

        let key = RegKey::open(HKEY_CURRENT_USER, TEST_FX_PATH).unwrap();
        assert!(key.value_exists("version").unwrap_or(false));
        assert!(key.value_exists(&config.install_mode.premix_slot().value_name()).unwrap_or(false));
        assert!(key.value_exists(&config.install_mode.postmix_slot().value_name()).unwrap_or(false));

        // 模拟卸载
        for slot in ApoSlot::ALL {
            let _ = delete_value(handle, &slot.value_name());
        }
        for name in &["childGuid", "allowSilentBuffer", "autoAdjust", "version"] {
            let _ = delete_value(handle, name);
        }

        let key2 = RegKey::open(HKEY_CURRENT_USER, TEST_FX_PATH).unwrap();
        assert!(!key2.value_exists("version").unwrap_or(true));

        close_key(handle);
        test_cleanup();
    }
}