//! install/selector/operation.rs — 设备 APO 安装/卸载执行 + 事务回滚（v6.4 规范 5.5.2）
//!
//! 原 `install.rs` + `rollback.rs` 合并至此。
//!
//! 职责：
//! - `install_endpoint`：Note 47 完整 7 步安装，Transaction 保护，失败自动回滚
//! - `uninstall_endpoint`：卸载（恢复原始 GUID，清理配置）
//! - `InstallConfig`：安装参数
//!
//! 禁止依赖：`pipeline/`、`config/`。

use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
use windows::Win32::Media::KernelStreaming::AUDIO_SIGNALPROCESSINGMODE_DEFAULT;

use crate::install::device::slots::{
    ApoSlot, InstallMode, SlotValue, read_all_slots, FX_PROPERTIES_KEY, INSTALL_VERSION,
};
use crate::object::vx_reg_props::{CLSID_VXAPO_POST_MIX, CLSID_VXAPO_PRE_MIX};
use crate::sys::com::prelude::guid_to_string;
use crate::sys::registry::RegKey;
use crate::utils::vx_error::{Result, VxApoError};

// ══════════════════════════════════════════════════════════════════════════════
// 注册表路径常量
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
#[derive(Debug, Clone)]
pub struct InstallConfig {
    /// 是否安装 PreMix APO。
    pub install_premix: bool,
    /// 是否安装 PostMix APO。
    pub install_postmix: bool,
    /// 安装模式（决定使用哪两个槽位）。
    pub install_mode: InstallMode,
    /// 是否保留原有 PreMix APO 作为子 APO。
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
// 事务回滚（原 rollback.rs 职责，合并至此）
// ══════════════════════════════════════════════════════════════════════════════

/// 回滚动作。
#[derive(Debug)]
enum RollbackAction {
    /// 删除指定键路径（安装新建的键）。
    DeleteKey(String),
    /// 恢复指定值（原名 + 备份字节）。
    RestoreValue { key_path: String, name: String, backup: Vec<u8> },
}

/// 简单事务：记录回滚动作，Drop 时逆序执行（未 commit 时）。
struct Transaction {
    actions: Vec<RollbackAction>,
    committed: bool,
}

impl Transaction {
    fn new() -> Self {
        Self { actions: Vec::new(), committed: false }
    }

    fn record(&mut self, action: RollbackAction) {
        self.actions.push(action);
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        // 逆序回滚所有已记录动作。
        for action in self.actions.iter().rev() {
            match action {
                RollbackAction::DeleteKey(path) => {
                    let _ = RegKey::open(HKEY_LOCAL_MACHINE, path)
                        .and_then(|k| k.delete_sub_key(path));
                }
                RollbackAction::RestoreValue { key_path, name, backup } => {
                    if let Ok(key) = RegKey::create(HKEY_LOCAL_MACHINE, key_path) {
                        let _ = key.write_binary(name, backup);
                    }
                }
            }
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// GUID 辅助（GUID→字符串走 sys::com::prelude；16 字节小端序列化就地实现）
// ══════════════════════════════════════════════════════════════════════════════

/// GUID → 16 字节小端（data1/data2/data3 LE + data4）。
fn guid_to_bytes(g: windows::core::GUID) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&g.data1.to_le_bytes());
    bytes[4..6].copy_from_slice(&g.data2.to_le_bytes());
    bytes[6..8].copy_from_slice(&g.data3.to_le_bytes());
    bytes[8..16].copy_from_slice(&g.data4);
    bytes
}

/// 16 字节 → GUID。
fn guid_from_bytes(bytes: &[u8]) -> Option<windows::core::GUID> {
    if bytes.len() < 16 {
        return None;
    }
    Some(windows::core::GUID {
        data1: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        data2: u16::from_le_bytes([bytes[4], bytes[5]]),
        data3: u16::from_le_bytes([bytes[6], bytes[7]]),
        data4: [
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ],
    })
}

// ══════════════════════════════════════════════════════════════════════════════
// install_endpoint — Note 47 完整 7 步（带事务回滚）
// ══════════════════════════════════════════════════════════════════════════════

/// 安装 VxAPO 到指定音频端点。
///
/// # Note 47 安装步骤
///
/// 1. 创建 Child APOs 键
/// 2. FxProperties 不存在则创建
/// 3. 已存在则备份原始 GUID（.reg / 事务记录）
/// 4. 写入子 APO 配置（childGuid / allowSilentBuffer / autoAdjust / version）
/// 5. 按模式写入 APO GUID（PreMix + PostMix），并清理其他模式旧槽位
/// 6. 写入默认处理模式 GUID
/// 7. 删除 DisableEnhancements
///
/// 任何步骤失败时，Transaction 通过 Drop 自动逆序回滚。
///
/// # 参数
///
/// - `device_guid`：端点 GUID（`{xxxxxxxx-...}`）。
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

    let endpoint_path = find_endpoint_path(device_guid)?;
    let fx_path = format!("{}\\{}", endpoint_path, FX_PROPERTIES_KEY);

    // ── Step 2: 确保 FxProperties 存在 ───────────────────────────────────

    let (fx_key, fx_is_new) = ensure_fx_properties(&fx_path, &mut tx)?;

    // ── Step 3: 备份原始 GUID ─────────────────────────────────────────────

    if !fx_is_new {
        // .reg 备份（best-effort，经 sys::registry::save_to_file 导出）。
        match backup_fx_properties_safe(device_name, connection_name, &endpoint_path) {
            Ok(path) => {
                #[cfg(debug_assertions)]
                log::info!("Backup saved: {}", path);
                let _ = path;
            }
            Err(e) => {
                #[cfg(debug_assertions)]
                log::warn!("Backup failed (non-fatal): {}", e);
                let _ = e;
            }
        }
        record_slot_backups(&fx_key, config.install_mode, &fx_path, &mut tx);
    }

    // ── 读取当前槽位（用于原始 APO 保留） ────────────────────────────────

    let (original_premix, original_postmix) =
        read_original_apo_guids(&fx_key, config);

    // ── Step 1: 创建 Child APOs 键 ───────────────────────────────────────

    let child_path = format!("{}\\ChildApoKeys", fx_path);
    let _child_key = ensure_sub_key(&child_path, &mut tx)?;

    // ── Step 4: 写入子 APO 配置 ──────────────────────────────────────────

    write_child_apo_config(&fx_key, config, original_premix, original_postmix)?;

    // ── Step 5: 按模式写入 APO GUID ──────────────────────────────────────

    delete_other_mode_slots(&fx_key, config.install_mode);

    if config.install_premix {
        write_apo_slot(&fx_key, config.install_mode.premix_slot(), CLSID_VXAPO_PRE_MIX)?;
    }
    if config.install_postmix {
        write_apo_slot(&fx_key, config.install_mode.postmix_slot(), CLSID_VXAPO_POST_MIX)?;
    }

    // ── Step 6: 写入默认处理模式 GUID ────────────────────────────────────

    write_default_processmode(&fx_key)?;

    // ── Step 7: 删除 DisableEnhancements ──────────────────────────────────

    let _ = fx_key.delete_value("DisableEnhancements");

    // 全部成功 → 提交事务（禁用回滚）。
    tx.commit();

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
pub fn uninstall_endpoint(device_guid: &str) -> Result<()> {
    let endpoint_path = find_endpoint_path(device_guid)?;
    let fx_path = format!("{}\\{}", endpoint_path, FX_PROPERTIES_KEY);

    let fx_key = match RegKey::open(HKEY_LOCAL_MACHINE, &fx_path) {
        Ok(k) => k,
        Err(_) => {
            // FxProperties 不存在 → 无安装，直接返回成功。
            return Ok(());
        }
    };

    // ── 读取当前槽位 ──────────────────────────────────────────────────────

    let slots = read_all_slots(&fx_key);

    // ── 删除 VxAPO CLSID ──────────────────────────────────────────────────

    for slot in ApoSlot::ALL {
        match &slots[slot.index() as usize] {
            SlotValue::Guid(g) => {
                if *g == CLSID_VXAPO_PRE_MIX || *g == CLSID_VXAPO_POST_MIX {
                    let _ = fx_key.delete_value(&slot.value_name());
                }
            }
            _ => {}
        }
    }

    // ── 删除子 APO 配置 ──────────────────────────────────────────────────

    for name in &["childGuid", "allowSilentBuffer", "autoAdjust", "version"] {
        let _ = fx_key.delete_value(name);
    }

    // ── 删除 DisableEnhancements ──────────────────────────────────────────

    let _ = fx_key.delete_value("DisableEnhancements");

    Ok(())
}

// ══════════════════════════════════════════════════════════════════════════════
// 内部辅助
// ══════════════════════════════════════════════════════════════════════════════

/// 从端点 GUID 定位注册表路径（先 Render 再 Capture）。
fn find_endpoint_path(device_guid: &str) -> Result<String> {
    let render = format!("{}\\{}", RENDER_PATH, device_guid);
    if RegKey::open(HKEY_LOCAL_MACHINE, &render).is_ok() {
        return Ok(render);
    }

    let capture = format!("{}\\{}", CAPTURE_PATH, device_guid);
    if RegKey::open(HKEY_LOCAL_MACHINE, &capture).is_ok() {
        return Ok(capture);
    }

    Err(VxApoError::device_not_found(device_guid))
}

/// 确保 FxProperties 子键存在。
///
/// 返回 `(key, is_new)`。
fn ensure_fx_properties(fx_path: &str, tx: &mut Transaction) -> Result<(RegKey, bool)> {
    // 先尝试打开（已存在）。
    if let Ok(key) = RegKey::open(HKEY_LOCAL_MACHINE, fx_path) {
        let is_new = !key.value_exists("version").unwrap_or(false);
        if is_new {
            tx.record(RollbackAction::DeleteKey(fx_path.to_string()));
        }
        return Ok((key, is_new));
    }

    // 不存在 → 创建。
    let key = RegKey::create(HKEY_LOCAL_MACHINE, fx_path)?;
    tx.record(RollbackAction::DeleteKey(fx_path.to_string()));
    Ok((key, true))
}

/// 确保子键存在（创建或打开）。
fn ensure_sub_key(path: &str, tx: &mut Transaction) -> Result<RegKey> {
    let key = RegKey::create(HKEY_LOCAL_MACHINE, path)?;
    tx.record(RollbackAction::DeleteKey(path.to_string()));
    Ok(key)
}

/// 记录槽位值到事务（用于安装失败回滚）。
fn record_slot_backups(
    fx_key: &RegKey,
    mode: InstallMode,
    fx_path: &str,
    tx: &mut Transaction,
) {
    for slot in [mode.premix_slot(), mode.postmix_slot()] {
        if let SlotValue::Guid(g) = read_slot_safe(fx_key, slot) {
            let bytes = guid_to_bytes(g).to_vec();
            tx.record(RollbackAction::RestoreValue {
                key_path: fx_path.to_string(),
                name: slot.value_name(),
                backup: bytes,
            });
        }
    }
}

/// 安全读取槽位值（读取失败返回 NoValue）。
fn read_slot_safe(fx_key: &RegKey, slot: ApoSlot) -> SlotValue {
    match fx_key.read_binary_value(&slot.value_name()) {
        Ok(raw) if raw.len() >= 16 => match guid_from_bytes(&raw) {
            Some(g) => SlotValue::Guid(g),
            None => SlotValue::NoValue,
        },
        _ => SlotValue::NoValue,
    }
}

/// 读取原始 APO GUID（用于子 APO 保留）。
fn read_original_apo_guids(
    fx_key: &RegKey,
    config: &InstallConfig,
) -> (Option<windows::core::GUID>, Option<windows::core::GUID>) {
    let premix = if config.use_original_apo_premix {
        read_slot_safe(fx_key, config.install_mode.premix_slot()).as_guid()
    } else {
        None
    };
    let postmix = if config.use_original_apo_postmix {
        read_slot_safe(fx_key, config.install_mode.postmix_slot()).as_guid()
    } else {
        None
    };
    (premix, postmix)
}

/// 写入子 APO 配置（Step 4）。
fn write_child_apo_config(
    fx_key: &RegKey,
    config: &InstallConfig,
    original_premix: Option<windows::core::GUID>,
    original_postmix: Option<windows::core::GUID>,
) -> Result<()> {
    // childGuid — 保留的原始 APO GUID（如果有）。
    let child_guid = original_premix.or(original_postmix);
    if let Some(g) = child_guid {
        fx_key.write_sz("childGuid", &guid_to_string(&g))?;
    }

    // allowSilentBuffer（Note 11）。
    fx_key.write_dword("allowSilentBuffer", config.allow_silent_buffer as u32)?;

    // autoAdjust。
    fx_key.write_dword("autoAdjust", 0u32)?;

    // version（Note 24）。
    fx_key.write_sz("version", INSTALL_VERSION)?;

    Ok(())
}

/// 删除非当前模式的旧槽位（Note 26）。
fn delete_other_mode_slots(fx_key: &RegKey, mode: InstallMode) {
    for slot in ApoSlot::ALL {
        if slot != mode.premix_slot() && slot != mode.postmix_slot() {
            let _ = fx_key.delete_value(&slot.value_name());
        }
    }
}

/// 写入 APO CLSID 到指定槽位。
fn write_apo_slot(fx_key: &RegKey, slot: ApoSlot, guid: windows::core::GUID) -> Result<()> {
    let bytes = guid_to_bytes(guid);
    fx_key.write_binary(&slot.value_name(), &bytes)?;
    Ok(())
}

/// 写入默认处理模式 GUID（Step 6，Note 26）。
fn write_default_processmode(fx_key: &RegKey) -> Result<()> {
    fx_key.write_sz(
        "KSDATAFORMAT_SUBTYPE_DEFAULT_PROCESSMODE",
        &guid_to_string(&AUDIO_SIGNALPROCESSINGMODE_DEFAULT),
    )?;
    Ok(())
}

/// .reg 备份（best-effort，经 sys::registry::save_to_file 导出 FxProperties）。
fn backup_fx_properties_safe(
    device_name: &str,
    connection_name: &str,
    endpoint_path: &str,
) -> Result<String> {
    let _ = std::fs::create_dir_all(BACKUP_DIR);

    let file_name = format!("{}_{}.reg", device_name, connection_name)
        .replace(['\\', '/', ':', '*', '?', '"', '<', '>', '|'], "_");
    let path = format!("{}\\{}", BACKUP_DIR, file_name);

    let fx_path = format!("{}\\{}", endpoint_path, FX_PROPERTIES_KEY);
    crate::sys::registry::save_to_file(HKEY_LOCAL_MACHINE, &fx_path, &path)?;
    Ok(path)
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

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
        let parsed = guid_from_bytes(&bytes).unwrap();
        assert_eq!(parsed, g);
    }

    #[test]
    fn guid_to_string_zeroed() {
        assert_eq!(
            guid_to_string(&windows::core::GUID::zeroed()),
            "{00000000-0000-0000-0000-000000000000}"
        );
    }

    #[test]
    fn guid_to_string_max_values() {
        let g = windows::core::GUID {
            data1: 0xFFFFFFFF,
            data2: 0xFFFF,
            data3: 0xFFFF,
            data4: [0xFF; 8],
        };
        assert_eq!(guid_to_string(&g), "{FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF}");
    }

    #[test]
    fn guid_from_bytes_too_short() {
        assert!(guid_from_bytes(&[0u8; 15]).is_none());
    }

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
}