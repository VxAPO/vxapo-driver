//! install/device/stale.rs — 旧 GUID 残留检测、迁移与清理。
//!
//! Windows 重新枚举音频端点后，MMDevices 下的端点 GUID 可能变化：
//! 旧端点的槽位键已消失，但 VxAPO 自己的 `Child APOs\{oldGuid}`、
//! `C:\ProgramData\VxAPO\{oldGuid}` 与 snapshots 仍会残留。
//!
//! 本模块以「设备实例 ID」为稳定身份，把旧记录匹配到当前活跃端点，
//! 并在用户确认后迁移配置/快照/子 APO 备份、修复新 GUID 安装状态。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Serialize;
use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;

use crate::install::device::endpoint::query_endpoint;
use crate::install::device::info::{detect_mode_for_guid, enumerate_devices};
use crate::install::device::slots::{
    child_apo_key_exists, ChildApoKind, InstallMode, CHILD_APO_PATH_ROOT, FX_PROPERTIES_KEY,
    INSTALL_VERSION,
};
use crate::install::device::sysfx::{decode_backups, SYSFX_BACKUP_VALUE};
use crate::install::selector::operation::{find_endpoint_path, write_install_config, InstallConfig};
use crate::sys::registry::{delete_tree, split_key, RegKey, RegValue};
use crate::utils::guid::parse_guid_string;
use crate::utils::vx_error::{Result, VxApoError};

const CONFIG_ROOT: &str = r"C:\ProgramData\VxAPO";
const SNAPSHOT_DIR: &str = r"C:\ProgramData\VxAPO\snapshots";
const MIGRATION_BACKUP_DIR: &str = r"C:\ProgramData\VxAPO\_migration_backup";
const CHILD_APO_ROOT_REL: &str = r"SOFTWARE\VxAPO\Child APOs";
const BACKUP_PREMIX_SLOT: &str = "PreMixSlot";
const BACKUP_POSTMIX_SLOT: &str = "PostMixSlot";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct StaleInstall {
    pub guid: String,
    pub device_instance_id: String,
    pub display_name: String,
    pub config_path: Option<String>,
    pub config_mtime_ms: Option<u64>,
    pub snapshot_path: Option<String>,
    pub snapshot_mtime_ms: Option<u64>,
    pub premix_slot: Option<String>,
    pub postmix_slot: Option<String>,
    pub inferred_mode: String,
    pub has_child_backup: bool,
    pub has_sysfx_backup: bool,
    pub target_guid: Option<String>,
    pub target_name: Option<String>,
    pub target_state: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct MigrationReport {
    pub success: bool,
    pub target_guid: String,
    pub config_from: Option<String>,
    pub snapshot_from: Option<String>,
    pub config_migrated: bool,
    pub snapshot_migrated: bool,
    pub install_repaired: bool,
    pub removed_guids: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct StaleRecord {
    guid: String,
    device_instance_id: String,
    config_path: Option<PathBuf>,
    snapshot_path: Option<PathBuf>,
    info_values: HashMap<String, RegValue>,
}

/// 扫描旧 GUID 安装记录，并匹配当前活跃端点。
pub fn list_stale_installs() -> Result<Vec<StaleInstall>> {
    let active = active_endpoints()?;
    let active_guids: HashSet<String> = active
        .iter()
        .map(|(guid, _, _)| guid.to_ascii_uppercase())
        .collect();
    let active_by_identity: HashMap<String, (String, String)> = active
        .iter()
        .filter_map(|(guid, name, identity)| {
            let key = normalize_device_id(identity);
            (!key.is_empty()).then(|| (key, (guid.clone(), name.clone())))
        })
        .collect();

    let mut result = Vec::new();
    for record in collect_stale_records(&active_guids)? {
        let target = active_by_identity
            .get(&normalize_device_id(&record.device_instance_id))
            .cloned();
        let (target_guid, target_name, target_state) = match target {
            Some((guid, name)) => {
                let healthy = target_health(&guid).unwrap_or(false);
                (
                    Some(guid),
                    Some(name),
                    if healthy {
                        "matched_healthy"
                    } else {
                        "matched_partial"
                    },
                )
            }
            None => (None, None, "unmatched"),
        };
        let mode = infer_mode(&record.info_values).unwrap_or(InstallMode::SfxEfx);
        result.push(StaleInstall {
            guid: record.guid.clone(),
            device_instance_id: record.device_instance_id.clone(),
            display_name: target_name
                .clone()
                .unwrap_or_else(|| record.guid.clone()),
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

/// 清理一个无法匹配到活跃端点的旧 GUID 记录。
pub fn cleanup_orphan(guid: &str) -> Result<()> {
    validate_guid(guid)?;
    archive_guid_files(guid)?;
    delete_child_apo_key(guid)?;
    Ok(())
}

/// 修复迁移后 config/snapshot 的 ACL：给交互用户授予 Modify。
pub fn fix_config_acl(guid: &str) -> Result<()> {
    validate_guid(guid)?;
    grant_interactive_modify(&Path::new(CONFIG_ROOT).join(guid));
    grant_interactive_modify(&Path::new(SNAPSHOT_DIR).join(format!("{guid}.json")));
    Ok(())
}

/// 把旧 GUID 安装迁移到新 GUID。
///
/// `config_from` / `snapshot_from` 为显式来源；缺省按「最新 config、最早 snapshot」
/// 在同设备实例的旧记录与目标现有文件之间选择。
pub fn migrate_install(
    old_guid: &str,
    new_guid: &str,
    config_from: Option<&str>,
    snapshot_from: Option<&str>,
) -> Result<MigrationReport> {
    validate_guid(old_guid)?;
    validate_guid(new_guid)?;
    if old_guid.eq_ignore_ascii_case(new_guid) {
        return Err(VxApoError::internal("旧 GUID 与目标 GUID 相同，无需迁移"));
    }

    find_endpoint_path(new_guid)?;
    let active = active_endpoints()?;
    let target = active
        .iter()
        .find(|(guid, _, _)| guid.eq_ignore_ascii_case(new_guid))
        .ok_or_else(|| VxApoError::device_not_found(new_guid))?;
    let target_identity = normalize_device_id(&target.2);

    let records = collect_stale_records(&HashSet::new())?;
    let primary = records
        .iter()
        .find(|r| r.guid.eq_ignore_ascii_case(old_guid))
        .ok_or_else(|| VxApoError::internal(format!("未找到旧安装记录：{old_guid}")))?;
    let primary_identity = normalize_device_id(&primary.device_instance_id);
    if !primary_identity.is_empty()
        && !target_identity.is_empty()
        && primary_identity != target_identity
    {
        return Err(VxApoError::internal(format!(
            "旧 GUID {old_guid} 与目标端点 {new_guid} 不是同一设备，拒绝迁移"
        )));
    }

    let mut group: Vec<&StaleRecord> = records
        .iter()
        .filter(|r| {
            !r.guid.eq_ignore_ascii_case(new_guid)
                && (r.guid.eq_ignore_ascii_case(old_guid)
                    || (!primary_identity.is_empty()
                        && normalize_device_id(&r.device_instance_id) == primary_identity))
        })
        .collect();
    group.sort_by(|a, b| a.guid.cmp(&b.guid));

    let config_source = choose_config_source(&group, new_guid, config_from)?;
    let snapshot_source = choose_snapshot_source(&group, new_guid, snapshot_from)?;

    let repaired = !target_health(new_guid).unwrap_or(false);
    let mut warnings = Vec::new();
    let mut config_migrated = false;
    let mut snapshot_migrated = false;

    if let Some(source) = &config_source {
        if !source.guid.eq_ignore_ascii_case(new_guid) {
            archive_target_file(new_guid, "config.toml")?;
            copy_file(
                &source.path,
                &Path::new(CONFIG_ROOT)
                    .join(new_guid)
                    .join("config.toml"),
            )?;
            config_migrated = true;
        }
    }
    if let Some(source) = &snapshot_source {
        if !source.guid.eq_ignore_ascii_case(new_guid) {
            archive_target_file(new_guid, "snapshot.json")?;
            copy_file(
                &source.path,
                &Path::new(SNAPSHOT_DIR).join(format!("{new_guid}.json")),
            )?;
            snapshot_migrated = true;
        }
    }

    let install_repaired = if repaired {
        let mode = primary
            .info_values
            .get(BACKUP_PREMIX_SLOT)
            .and_then(|v| read_string(v))
            .zip(
                primary
                    .info_values
                    .get(BACKUP_POSTMIX_SLOT)
                    .and_then(|v| read_string(v)),
            )
            .and_then(|(pre, post)| mode_from_slots(&pre, &post))
            .or_else(|| Some(detect_mode_for_guid(new_guid)))
            .unwrap_or(InstallMode::SfxEfx);
        let config = InstallConfig {
            install_premix: true,
            install_postmix: true,
            install_mode: mode,
            use_original_apo_premix: false,
            use_original_apo_postmix: false,
            allow_silent_buffer: true,
            auto_adjust: false,
        };
        write_install_config(new_guid, &target.1, "", &config)?;
        true
    } else {
        false
    };

    // 修复后目标信息区是我们刚写入的（可能把 VxAPO 自身当成原槽位备份）；
    // 迁移旧记录里的子 APO / 原始槽位 / SysFx 备份，修复路径下覆盖，
    // 健康路径下只补缺失值。
    let target_key = ensure_child_apo_key(new_guid)?;
    for source in group.iter().rev() {
        if source.guid.eq_ignore_ascii_case(new_guid) {
            continue;
        }
        if let Ok(src_key) = open_child_apo_key(&source.guid) {
            if let Err(e) = copy_values(&src_key, &target_key, repaired) {
                warnings.push(format!("合并旧记录 {} 失败：{e}", source.guid));
            }
        }
    }

    if !target_health(new_guid).unwrap_or(false) {
        return Err(VxApoError::internal(
            "迁移后目标端点的 VxAPO 安装状态仍不完整（version / Child APOs 缺失）",
        ));
    }

    // 迁移由提权 CLI 执行，复制出来的 config.toml 会继承“管理员可写、
    // 交互用户只读”的 ACL；App 以普通用户写配置会报权限不足。
    // 这里显式给交互用户 Modify（目录递归 + 快照文件）。
    let target_dir = Path::new(CONFIG_ROOT).join(new_guid);
    grant_interactive_modify(&target_dir);
    grant_interactive_modify(&Path::new(SNAPSHOT_DIR).join(format!("{new_guid}.json")));

    let mut removed_guids = Vec::new();
    for source in &group {
        if source.guid.eq_ignore_ascii_case(new_guid) {
            continue;
        }
        if let Err(e) = archive_guid_files(&source.guid) {
            warnings.push(format!("归档旧 GUID {} 失败：{e}", source.guid));
        }
        if let Err(e) = delete_child_apo_key(&source.guid) {
            warnings.push(format!("删除旧信息区 {} 失败：{e}", source.guid));
        }
        removed_guids.push(source.guid.clone());
    }

    Ok(MigrationReport {
        success: true,
        target_guid: new_guid.to_string(),
        config_from: config_source.map(|s| s.guid),
        snapshot_from: snapshot_source.map(|s| s.guid),
        config_migrated,
        snapshot_migrated,
        install_repaired,
        removed_guids,
        warnings,
    })
}

#[derive(Debug)]
struct FileSource {
    guid: String,
    path: PathBuf,
    mtime: u64,
}

fn choose_config_source(
    group: &[&StaleRecord],
    new_guid: &str,
    forced: Option<&str>,
) -> Result<Option<FileSource>> {
    let mut candidates = Vec::new();
    for record in group {
        if let Some(path) = &record.config_path {
            if let Some(mtime) = file_mtime_ms(path) {
                candidates.push(FileSource {
                    guid: record.guid.clone(),
                    path: path.clone(),
                    mtime,
                });
            }
        }
    }
    let target_config = Path::new(CONFIG_ROOT).join(new_guid).join("config.toml");
    if let Some(mtime) = file_mtime_ms(&target_config) {
        // 新 GUID 刚出现时 App/驱动可能自动写过 28 字节默认 passthrough；
        // 它不应盖掉旧 GUID 里的真实调音配置。
        if config_is_meaningful(&target_config) {
            candidates.push(FileSource {
                guid: new_guid.to_string(),
                path: target_config,
                mtime,
            });
        }
    }
    if let Some(forced) = forced {
        return Ok(candidates.into_iter().find(|c| c.guid.eq_ignore_ascii_case(forced)));
    }
    Ok(candidates.into_iter().max_by_key(|c| c.mtime))
}

fn choose_snapshot_source(
    group: &[&StaleRecord],
    new_guid: &str,
    forced: Option<&str>,
) -> Result<Option<FileSource>> {
    let mut candidates = Vec::new();
    for record in group {
        if let Some(path) = &record.snapshot_path {
            if let Some(mtime) = file_mtime_ms(path) {
                candidates.push(FileSource {
                    guid: record.guid.clone(),
                    path: path.clone(),
                    mtime,
                });
            }
        }
    }
    let target_snapshot = Path::new(SNAPSHOT_DIR).join(format!("{new_guid}.json"));
    if let Some(mtime) = file_mtime_ms(&target_snapshot) {
        candidates.push(FileSource {
            guid: new_guid.to_string(),
            path: target_snapshot,
            mtime,
        });
    }
    if let Some(forced) = forced {
        return Ok(candidates.into_iter().find(|c| c.guid.eq_ignore_ascii_case(forced)));
    }
    Ok(candidates.into_iter().min_by_key(|c| c.mtime))
}

fn collect_stale_records(active_guids: &HashSet<String>) -> Result<Vec<StaleRecord>> {
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

fn active_endpoints() -> Result<Vec<(String, String, String)>> {
    let devices = enumerate_devices()?;
    Ok(devices
        .into_iter()
        .filter_map(|d| {
            let endpoint = d.endpoint?;
            Some((
                endpoint.endpoint_guid,
                endpoint.friendly_name,
                endpoint.device_id,
            ))
        })
        .collect())
}

/// 从旧 GUID 的 MMDevices 端点键读设备实例 ID（端点未启用/未插入也可读）。
fn endpoint_device_instance_id(guid: &str) -> Option<String> {
    for root in [
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render",
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture",
    ] {
        let path = format!("{root}\\{guid}");
        if let Ok(key) = RegKey::open(HKEY_LOCAL_MACHINE, &path) {
            if let Ok(Some(info)) = query_endpoint(&key) {
                if !info.device_id.is_empty() {
                    return Some(normalize_device_id(&info.device_id));
                }
            }
        }
    }
    None
}

fn read_info_values(guid: &str) -> Result<HashMap<String, RegValue>> {
    let key = open_child_apo_key(guid)?;
    let mut values = HashMap::new();
    for name in key.enum_values()? {
        if let Ok(value) = key.read_value(&name) {
            values.insert(name, value);
        }
    }
    Ok(values)
}

fn open_child_apo_key(guid: &str) -> Result<RegKey> {
    RegKey::open(HKEY_LOCAL_MACHINE, &format!("{CHILD_APO_ROOT_REL}\\{guid}"))
        .map_err(|e| VxApoError::internal(format!("打开旧信息区失败：{e}")))
}

fn ensure_child_apo_key(guid: &str) -> Result<RegKey> {
    RegKey::create(HKEY_LOCAL_MACHINE, &format!("{CHILD_APO_ROOT_REL}\\{guid}"))
        .map_err(|e| VxApoError::internal(format!("创建/打开目标信息区失败：{e}")))
}

fn delete_child_apo_key(guid: &str) -> Result<()> {
    let (root, sub) = split_key(CHILD_APO_PATH_ROOT)?;
    if sub.is_empty() {
        return Ok(());
    }
    let path = format!("{sub}\\{guid}");
    delete_tree(root, &path).map_err(|e| VxApoError::internal(format!("删除旧信息区失败：{e}")))
}

fn copy_values(src: &RegKey, dst: &RegKey, overwrite: bool) -> Result<()> {
    for name in src.enum_values()? {
        if !overwrite && dst.value_exists(&name).unwrap_or(false) {
            continue;
        }
        let value = src.read_value(&name)?;
        match value {
            RegValue::Sz(v) => dst.write_sz(&name, &v)?,
            RegValue::Dword(v) => dst.write_dword(&name, v)?,
            RegValue::Qword(v) => dst.write_qword(&name, v)?,
            RegValue::Binary(v) => dst.write_binary(&name, &v)?,
            RegValue::MultiSz(v) => dst.write_multi_value(&name, &v)?,
        }
    }
    Ok(())
}

fn target_health(guid: &str) -> Result<bool> {
    let endpoint_path = find_endpoint_path(guid)?;
    let fx_path = format!("{endpoint_path}\\{FX_PROPERTIES_KEY}");
    let version_ok = RegKey::open(HKEY_LOCAL_MACHINE, &fx_path)
        .ok()
        .and_then(|k| k.read_sz("version"))
        .map(|v| v == INSTALL_VERSION)
        .unwrap_or(false);
    Ok(version_ok && child_apo_key_exists(guid))
}

fn infer_mode(values: &HashMap<String, RegValue>) -> Option<InstallMode> {
    let pre = values
        .get(BACKUP_PREMIX_SLOT)
        .and_then(read_string)?;
    let post = values
        .get(BACKUP_POSTMIX_SLOT)
        .and_then(read_string)?;
    mode_from_slots(&pre, &post)
}

fn mode_from_slots(pre: &str, post: &str) -> Option<InstallMode> {
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

fn mode_name(mode: InstallMode) -> &'static str {
    match mode {
        InstallMode::LfxGfx => "LfxGfx",
        InstallMode::SfxMfx => "SfxMfx",
        InstallMode::SfxEfx => "SfxEfx",
    }
}

fn identity_from_sysfx(value: &RegValue) -> Option<String> {
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
fn extract_device_instance_id(path: &str) -> Option<String> {
    let marker = path.find("##?#")?;
    let rest = &path[marker + 4..];
    let end = rest.find("#{").unwrap_or(rest.len());
    let raw = &rest[..end];
    if raw.is_empty() {
        return None;
    }
    Some(normalize_device_id(&raw.replace('#', "\\")))
}

fn normalize_device_id(value: &str) -> String {
    let mut s = value.trim().replace('#', "\\").to_ascii_uppercase();
    while s.starts_with("\\\\?\\") {
        s = s[4..].to_string();
    }
    if let Some(rest) = s.strip_prefix("{1}.") {
        s = rest.to_string();
    }
    if let Some(rest) = s.strip_prefix("{2}.") {
        s = rest.to_string();
    }
    s.trim_matches('\\').to_string()
}

fn read_sz(values: &HashMap<String, RegValue>, name: &str) -> Option<String> {
    values.get(name).and_then(read_string)
}

fn read_string(value: &RegValue) -> Option<String> {
    match value {
        RegValue::Sz(v) => Some(v.clone()),
        RegValue::MultiSz(v) => v.first().cloned(),
        _ => None,
    }
}

fn file_mtime_ms(path: &PathBuf) -> Option<u64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}

fn config_is_meaningful(path: &Path) -> bool {
    fs::metadata(path)
        .map(|m| m.len() > 64)
        .unwrap_or(false)
}

/// 给交互登录用户授予 Modify 权限（icacls SID `*S-1-5-4`）。
///
/// 目录使用 `(OI)(CI)M /T` 递归；文件直接 `M`。失败只记录告警，
/// 不影响迁移结果（App 侧仍可提示用户以管理员运行一次修复）。
fn grant_interactive_modify(path: &Path) {
    if !path.exists() {
        return;
    }
    let mut cmd = std::process::Command::new("icacls");
    cmd.arg(path);
    if path.is_dir() {
        cmd.args(["/grant", "*S-1-5-4:(OI)(CI)M", "/T", "/C", "/Q"]);
    } else {
        cmd.args(["/grant", "*S-1-5-4:M", "/C", "/Q"]);
    }
    let _ = cmd.output();
}

fn copy_file(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| VxApoError::internal(format!("创建目录失败：{e}")))?;
    }
    let tmp = dst.with_extension("migration.tmp");
    fs::copy(src, &tmp).map_err(|e| VxApoError::internal(format!("复制文件失败：{e}")))?;
    fs::rename(&tmp, dst).map_err(|e| VxApoError::internal(format!("替换文件失败：{e}")))?;
    Ok(())
}

fn archive_target_file(target_guid: &str, name: &str) -> Result<()> {
    let dir = Path::new(MIGRATION_BACKUP_DIR).join(target_guid).join("target");
    fs::create_dir_all(&dir).map_err(|e| VxApoError::internal(format!("创建备份目录失败：{e}")))?;
    let src = match name {
        "config.toml" => Path::new(CONFIG_ROOT).join(target_guid).join(name),
        _ => Path::new(SNAPSHOT_DIR).join(format!("{target_guid}.json")),
    };
    if src.exists() {
        let file_name = src
            .file_name()
            .map(|s| s.to_os_string())
            .unwrap_or_else(|| name.into());
        let _ = fs::copy(&src, dir.join(file_name));
    }
    Ok(())
}

fn archive_guid_files(guid: &str) -> Result<()> {
    let backup = Path::new(MIGRATION_BACKUP_DIR).join(guid);
    fs::create_dir_all(&backup).map_err(|e| VxApoError::internal(format!("创建归档目录失败：{e}")))?;
    let config_dir = Path::new(CONFIG_ROOT).join(guid);
    if config_dir.exists() {
        let dst = backup.join("config_dir");
        let _ = fs::create_dir_all(&dst);
        for entry in fs::read_dir(&config_dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_file() {
                let _ = fs::copy(&path, dst.join(entry.file_name()));
            }
        }
        let _ = fs::remove_dir_all(&config_dir);
    }
    let snapshot = Path::new(SNAPSHOT_DIR).join(format!("{guid}.json"));
    if snapshot.exists() {
        let _ = fs::copy(&snapshot, backup.join("snapshot.json"));
        let _ = fs::remove_file(&snapshot);
    }
    Ok(())
}

fn validate_guid(guid: &str) -> Result<()> {
    parse_guid_string(guid)
        .map(|_| ())
        .ok_or_else(|| VxApoError::internal(format!("无效的端点 GUID：{guid}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_device_instance_from_sysfx_path() {
        let path = r"SYSTEM\CurrentControlSet\Control\DeviceClasses\{65E8773E-8F56-11D0-A3B9-00A0C9223196}\##?#USB#VID_2D99&PID_A037&MI_00#6&20be7186&2&0000#{65e8773e-8f56-11d0-a3b9-00a0c9223196}\#GLOBAL\Device Parameters\MSFX\0";
        assert_eq!(
            extract_device_instance_id(path).as_deref(),
            Some(r"USB\VID_2D99&PID_A037&MI_00\6&20BE7186&2&0000")
        );
    }

    #[test]
    fn mode_inference_covers_three_modes() {
        let n = "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},";
        assert_eq!(
            mode_from_slots(&format!("{n}5"), &format!("{n}7")),
            Some(InstallMode::SfxEfx)
        );
        assert_eq!(
            mode_from_slots(&format!("{n}5"), &format!("{n}6")),
            Some(InstallMode::SfxMfx)
        );
        assert_eq!(
            mode_from_slots(&format!("{n}0"), &format!("{n}3")),
            Some(InstallMode::LfxGfx)
        );
    }
}
