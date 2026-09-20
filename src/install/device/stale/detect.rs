//! install/device/stale/detect.rs — 残留记录检测与扫描
//!
//! 扫描 `MMDevices` 端点与 `HKLM\SOFTWARE\VxAPO\Child APOs` 记录，装配
//! `StaleInstall` 列表，并导出模式/身份/时间等纯读取辅助。
//! 共享常量与公开类型见父模块 `install/device/stale.rs`。

use super::*;
use super::matching::*;

#[derive(Debug, Clone)]
pub(super) struct StaleRecord {
    pub(super) guid: String,
    pub(super) device_instance_id: String,
    pub(super) config_path: Option<PathBuf>,
    pub(super) snapshot_path: Option<PathBuf>,
    pub(super) info_values: HashMap<String, RegValue>,
}

/// 扫描旧 GUID 安装记录，并匹配当前活跃端点。
pub fn list_stale_installs() -> Result<Vec<StaleInstall>> {
    let active = active_endpoints()?;
    let active_guids: HashSet<String> = active
        .iter()
        .map(|ep| ep.guid.to_ascii_uppercase())
        .collect();
    let index = MatchIndex::new(&active);

    let mut result = Vec::new();
    for record in collect_stale_records(&active_guids)? {
        let stored = identity_from_values(&record.info_values);
        let hit = index.resolve(&record, &stored);
        let (target_guid, target_name, target_state, matched_by) = match hit {
            MatchOutcome::Matched(i, source) => {
                let ep = &active[i];
                let healthy = target_health(&ep.guid).unwrap_or(false);
                (
                    Some(ep.guid.clone()),
                    Some(ep.name.clone()),
                    if healthy {
                        "matched_healthy"
                    } else {
                        "matched_partial"
                    },
                    Some(source.to_string()),
                )
            }
            MatchOutcome::Ambiguous => {
                log::warn!(
                    "stale record {} 命中多个活跃端点，保持 unmatched（不猜测目标）",
                    record.guid
                );
                (None, None, "unmatched", None)
            }
            MatchOutcome::None => (None, None, "unmatched", None),
        };
        let mode = infer_mode(&record.info_values).unwrap_or(InstallMode::SfxEfx);
        // 展示用身份：老端点键仍可读时优先，否则用记录里落盘的实例 ID。
        let device_instance_id = if record.device_instance_id.is_empty() {
            stored.instance_id.clone()
        } else {
            record.device_instance_id.clone()
        };
        result.push(StaleInstall {
            guid: record.guid.clone(),
            device_instance_id,
            display_name: target_name
                .clone()
                .unwrap_or_else(|| record.guid.clone()),
            matched_by,
            config_path: record
                .config_path
                .as_ref()
                .map(|p| p.display().to_string()),
            config_mtime_ms: record.config_path.as_ref().and_then(file_mtime_ms),
            snapshot_path: record
                .snapshot_path
                .as_ref()
                .map(|p| p.display().to_string()),
            snapshot_mtime_ms: record.snapshot_path.as_ref().and_then(file_mtime_ms),
            premix_slot: read_sz(&record.info_values, BACKUP_PREMIX_SLOT),
            postmix_slot: read_sz(&record.info_values, BACKUP_POSTMIX_SLOT),
            inferred_mode: mode_name(mode).to_string(),
            has_child_backup: [
                ChildApoKind::PreMix.value_name(),
                ChildApoKind::PostMix.value_name(),
            ]
            .iter()
            .any(|name| record.info_values.contains_key(*name)),
            has_sysfx_backup: record
                .info_values
                .get(SYSFX_BACKUP_VALUE)
                .map(|v| !matches!(v, RegValue::MultiSz(values) if values.is_empty()))
                .unwrap_or(false),
            target_guid,
            target_name,
            target_state: target_state.to_string(),
        });
    }
    Ok(result)
}

pub(super) fn collect_stale_records(active_guids: &HashSet<String>) -> Result<Vec<StaleRecord>> {
    let mut guids: BTreeMap<String, ()> = BTreeMap::new();
    if let Ok(key) = RegKey::open(HKEY_LOCAL_MACHINE, CHILD_APO_ROOT_REL) {
        for guid in key.enum_sub_keys().unwrap_or_default() {
            guids.insert(guid.to_ascii_uppercase(), ());
        }
    }
    if let Ok(entries) = fs::read_dir(CONFIG_ROOT) {
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if parse_guid_string(&name).is_some() {
                guids.insert(name.to_ascii_uppercase(), ());
            }
        }
    }
    if let Ok(entries) = fs::read_dir(SNAPSHOT_DIR) {
        for entry in entries.flatten() {
            let name = entry
                .path()
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if parse_guid_string(&name).is_some() {
                guids.insert(name.to_ascii_uppercase(), ());
            }
        }
    }

    let mut records = Vec::new();
    for (guid, _) in guids {
        if active_guids.contains(&guid) {
            continue;
        }
        let info_values = read_info_values(&guid).unwrap_or_default();
        let config_path = Path::new(CONFIG_ROOT).join(&guid).join("config.toml");
        let snapshot_path = Path::new(SNAPSHOT_DIR).join(format!("{guid}.json"));
        let has_info = !info_values.is_empty();
        // 只有软件信息区（Child APOs）里有记录的才算“安装残留”；
        // ProgramData 里可能只是暂时未插入设备的用户配置，不能当残留清理。
        if !has_info {
            continue;
        }
        let device_instance_id = endpoint_device_instance_id(&guid)
            .or_else(|| info_values.get(SYSFX_BACKUP_VALUE).and_then(identity_from_sysfx))
            .unwrap_or_default();
        records.push(StaleRecord {
            guid: guid.clone(),
            device_instance_id,
            config_path: config_path.exists().then_some(config_path),
            snapshot_path: snapshot_path.exists().then_some(snapshot_path),
            info_values,
        });
    }
    Ok(records)
}

/// 打开旧 GUID 的 VxAPO 记录键（`HKLM\SOFTWARE\VxAPO\Child APOs\{guid}`）。
pub(super) fn open_child_apo_key(guid: &str) -> Result<RegKey> {
    RegKey::open(HKEY_LOCAL_MACHINE, &format!("{CHILD_APO_ROOT_REL}\\{guid}"))
        .map_err(|e| VxApoError::internal(format!("打开旧信息区失败：{e}")))
}

/// 创建/打开目标 GUID 的记录键（迁移写入用）。
pub(super) fn ensure_child_apo_key(guid: &str) -> Result<RegKey> {
    RegKey::create(HKEY_LOCAL_MACHINE, &format!("{CHILD_APO_ROOT_REL}\\{guid}"))
        .map_err(|e| VxApoError::internal(format!("创建/打开目标信息区失败：{e}")))
}

pub(super) fn read_info_values(guid: &str) -> Result<HashMap<String, RegValue>> {
    let key = open_child_apo_key(guid)?;
    let mut values = HashMap::new();
    for name in key.enum_values()? {
        if let Ok(value) = key.read_value(&name) {
            values.insert(name, value);
        }
    }
    Ok(values)
}

pub(super) fn target_health(guid: &str) -> Result<bool> {
    let endpoint_path = find_endpoint_path(guid)?;
    let fx_path = format!("{endpoint_path}\\{FX_PROPERTIES_KEY}");
    let version_ok = RegKey::open(HKEY_LOCAL_MACHINE, &fx_path)
        .ok()
        .and_then(|k| k.read_sz("version"))
        .map(|v| v == INSTALL_VERSION)
        .unwrap_or(false);
    Ok(version_ok && child_apo_key_exists(guid))
}

pub(super) fn infer_mode(values: &HashMap<String, RegValue>) -> Option<InstallMode> {
    let pre = values
        .get(BACKUP_PREMIX_SLOT)
        .and_then(read_string)?;
    let post = values
        .get(BACKUP_POSTMIX_SLOT)
        .and_then(read_string)?;
    mode_from_slots(&pre, &post)
}

pub(super) fn mode_from_slots(pre: &str, post: &str) -> Option<InstallMode> {
    let pre = pre.trim().to_ascii_lowercase();
    let post = post.trim().to_ascii_lowercase();
    if pre.ends_with(",0") && post.ends_with(",3") {
        Some(InstallMode::LfxGfx)
    } else if pre.ends_with(",5") && post.ends_with(",6") {
        Some(InstallMode::SfxMfx)
    } else if pre.ends_with(",5") && post.ends_with(",7") {
        Some(InstallMode::SfxEfx)
    } else {
        None
    }
}

pub(super) fn mode_name(mode: InstallMode) -> &'static str {
    match mode {
        InstallMode::LfxGfx => "LfxGfx",
        InstallMode::SfxMfx => "SfxMfx",
        InstallMode::SfxEfx => "SfxEfx",
    }
}

pub(super) fn identity_from_sysfx(value: &RegValue) -> Option<String> {
    let RegValue::MultiSz(values) = value else {
        return None;
    };
    for backup in decode_backups(values) {
        if let Some(id) = extract_device_instance_id(&backup.key_path) {
            return Some(id);
        }
    }
    None
}

/// 从 DeviceClasses 键路径提取设备实例 ID。
pub(super) fn extract_device_instance_id(path: &str) -> Option<String> {
    let marker = path.find("##?#")?;
    let rest = &path[marker + 4..];
    let end = rest.find("#{").unwrap_or(rest.len());
    let raw = &rest[..end];
    if raw.is_empty() {
        return None;
    }
    Some(normalize_device_id(&raw.replace('#', "\\")))
}

pub(super) fn read_sz(values: &HashMap<String, RegValue>, name: &str) -> Option<String> {
    values.get(name).and_then(read_string)
}

pub(super) fn read_string(value: &RegValue) -> Option<String> {
    match value {
        RegValue::Sz(v) => Some(v.clone()),
        RegValue::MultiSz(v) => v.first().cloned(),
        _ => None,
    }
}

pub(super) fn file_mtime_ms(path: &PathBuf) -> Option<u64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}

pub(super) fn config_is_meaningful(path: &Path) -> bool {
    fs::metadata(path)
        .map(|m| m.len() > 64)
        .unwrap_or(false)
}

pub(super) fn validate_guid(guid: &str) -> Result<()> {
    parse_guid_string(guid)
        .map(|_| ())
        .ok_or_else(|| VxApoError::internal(format!("无效的端点 GUID：{guid}")))
}

