//! install/selector/operation/sysfx.rs — CAPX 设备默认效果接管/还原

//! 共享导入见父模块 install/selector/operation.rs。

use super::*;
use super::execute::*;
use super::helpers::*;

/// 接管指定端点的 Windows“设备默认效果”。
///
/// 两部分：
/// 1. 设备接口 `MSFX\N` 模板：微软 StreamEffect → VxAPO PreMix，删除 ModeEffect；
/// 2. 端点 FxProperties `,6`（MFX）：非 SfxMfx 模式时删除微软 MFX，避免与
///    VxAPO PostMix（`,7`）重复处理。
///
/// 所有原始值写入事务，安装失败自动回滚；成功后再持久化到 VxAPO 信息区，
/// 供卸载恢复。
pub(super) fn take_over_sysfx(
    device_guid: &str,
    endpoint_path: &str,
    fx_key: &RegKey,
    config: &InstallConfig,
    tx: &mut Transaction,
) -> Result<()> {
    let endpoint_key = RegKey::open(HKEY_LOCAL_MACHINE, endpoint_path)?;
    let (device_id, node_type) = sysfx::endpoint_identity(&endpoint_key);
    let paths = sysfx::find_msfx_entries(device_id.as_deref(), node_type.as_deref())?;
    let mut changes =
        sysfx::plan_msfx_takeover(&paths, config.install_mode, config.install_premix, config.install_postmix)?;

    // 端点 FxProperties 上的微软 MFX：默认模式（SfxEfx）下必须删掉。
    if config.install_mode != InstallMode::SfxMfx {
        let fx_path = format!("{}\\{}", endpoint_path, FX_PROPERTIES_KEY);
        if let Ok(RegValue::Sz(mode)) = fx_key.read_value(sysfx::PKEY_FX_MODE_EFFECT_CLSID) {
            if sysfx::is_ms_mode_clsid(&mode) || sysfx::is_vxapo_clsid(&mode) {
                changes.push(sysfx::SysFxChange {
                    key_path: fx_path,
                    value_name: sysfx::PKEY_FX_MODE_EFFECT_CLSID.to_string(),
                    original: Some(mode),
                    target: None,
                });
            }
        }
    }

    if changes.is_empty() {
        return Ok(());
    }

    for change in &changes {
        if let Some(original) = &change.original {
            tx.record(RollbackAction::RestoreValue {
                key_path: change.key_path.clone(),
                name: change.value_name.clone(),
                backup: original.clone(),
            });
        }
        let key = RegKey::open_for_write(HKEY_LOCAL_MACHINE, &change.key_path)?;
        match &change.target {
            Some(target) => key.write_sz(&change.value_name, target)?,
            None => key.delete_value(&change.value_name)?,
        }
    }

    let backups = sysfx::changes_to_backups(&changes);
    if !backups.is_empty() {
        let info_key = format!("{}\\{}", CHILD_APO_PATH_ROOT, device_guid);
        let (root, sub_key) = split_hklm_path(&info_key)?;
        let info = RegKey::open_for_write(root, sub_key)?;
        info.write_multi_value(sysfx::SYSFX_BACKUP_VALUE, &sysfx::encode_backups(&backups))?;
    }

    Ok(())
}

/// 卸载时恢复微软默认效果（优先用信息区备份，无备份则按 CAPX 默认值兜底）。
pub(super) fn restore_sysfx(device_guid: &str, endpoint_path: &str) -> Result<()> {
    let info_key = format!("{}\\{}", CHILD_APO_PATH_ROOT, device_guid);
    let backups = match split_hklm_path(&info_key) {
        Ok((root, sub_key)) => RegKey::open(root, sub_key)
            .ok()
            .and_then(|k| k.read_multi_value(sysfx::SYSFX_BACKUP_VALUE).ok())
            .map(|values| sysfx::decode_backups(&values))
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    let changes = if !backups.is_empty() {
        sysfx::plan_msfx_restore(&backups)?
    } else {
        let endpoint_key = RegKey::open(HKEY_LOCAL_MACHINE, endpoint_path)?;
        let (device_id, node_type) = sysfx::endpoint_identity(&endpoint_key);
        let paths = sysfx::find_msfx_entries(device_id.as_deref(), node_type.as_deref())?;
        sysfx::plan_msfx_restore_defaults(&paths)?
    };

    for change in &changes {
        let key = RegKey::open_for_write(HKEY_LOCAL_MACHINE, &change.key_path)?;
        match &change.target {
            Some(target) => key.write_sz(&change.value_name, target)?,
            None => key.delete_value(&change.value_name)?,
        }
    }

    Ok(())
}

