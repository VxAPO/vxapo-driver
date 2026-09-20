//! install/device/stale.rs — 旧 GUID 残留检测、迁移与清理。
//!
//! Windows 重新枚举音频端点后，MMDevices 下的端点 GUID 可能变化：
//! 旧端点的槽位键已消失，但 VxAPO 自己的 `Child APOs\{oldGuid}`、
//! `C:\ProgramData\VxAPO\{oldGuid}` 与 snapshots 仍会残留。
//!
//! 本模块以「设备实例 ID」为稳定身份，把旧记录匹配到当前活跃端点，
//! 并在用户确认后迁移配置/快照/子 APO 备份、修复新 GUID 安装状态。
//!
//! **匹配分层（2026-09-16 修复）**：Windows 大版本更新会重排端点 GUID 并
//! **整体删除**老端点键，此时「老端点键读实例 ID」这条旧路径失效（实测两条
//! 记录退化为 unmatched，App 只剩清理出口）。现按优先级分层匹配：
//!
//! 1. `endpoint_history`：老 GUID 出现在活跃端点的端点历史属性里
//!    （`identity::PKEY_ENDPOINT_HISTORY`），或活跃 GUID 出现在记录已落盘的
//!    `EndpointHistory` 值里；
//! 2. `device_instance_id`：老端点键仍在时读到的实例 ID（旧路径保留）；
//! 3. `stored_identity`：记录键里安装/迁移时落盘的实例 ID；
//! 4. `hardware_id`：硬件 ID 相交且产品名一致，且**唯一**命中。
//!
//! 多候选（歧义）或全部未命中 → `unmatched`：只提供清理，不猜、不自动迁移。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Serialize;
use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;

use crate::install::device::endpoint::query_endpoint;
use crate::install::device::identity::{
    identity_from_key, identity_from_values, merge_endpoint_history, normalize_device_id,
    read_endpoint_identity, write_identity_values, EndpointIdentity,
};
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
    /// 命中来源（`endpoint_history` / `device_instance_id` / `stored_identity` /
    /// `hardware_id`）；未命中为 `None`。
    pub matched_by: Option<String>,
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
        .find(|ep| ep.guid.eq_ignore_ascii_case(new_guid))
        .ok_or_else(|| VxApoError::device_not_found(new_guid))?;
    let target_identity = target.identity.instance_id.clone();

    let records = collect_stale_records(&HashSet::new())?;
    let primary = records
        .iter()
        .find(|r| r.guid.eq_ignore_ascii_case(old_guid))
        .ok_or_else(|| VxApoError::internal(format!("未找到旧安装记录：{old_guid}")))?;
    let primary_identity = normalize_device_id(&primary.device_instance_id);
    let primary_stored = identity_from_values(&primary.info_values);
    if !primary_identity.is_empty()
        && !target_identity.is_empty()
        && primary_identity != target_identity
    {
        return Err(VxApoError::internal(format!(
            "旧 GUID {old_guid} 与目标端点 {new_guid} 不是同一设备，拒绝迁移"
        )));
    }
    // 端点历史联动（GUID 刷新后老端点键被删的情况）：老 GUID 出现在目标端点的
    // 历史里，或记录已落盘的 EndpointHistory 里出现目标 GUID。
    let history_linked = target
        .identity
        .endpoint_history
        .iter()
        .any(|g| g.eq_ignore_ascii_case(old_guid))
        || primary_stored
            .endpoint_history
            .iter()
            .any(|g| g.eq_ignore_ascii_case(new_guid));
    if !history_linked
        && primary_identity.is_empty()
        && target_identity.is_empty()
        && primary_stored.instance_id.is_empty()
    {
        // 显式请求（CLI/App 指定 --from/--to）仍继续，仅记录告警供排查。
        log::warn!(
            "stale migrate: {old_guid} → {new_guid} 缺少身份证据（老端点键已删除、端历史无联动），按显式请求继续"
        );
    }

    let mut group: Vec<&StaleRecord> = records
        .iter()
        .filter(|r| {
            if r.guid.eq_ignore_ascii_case(new_guid) {
                return false;
            }
            if r.guid.eq_ignore_ascii_case(old_guid) {
                return true;
            }
            // 同一设备（老端点键身份一致）。
            if !primary_identity.is_empty()
                && normalize_device_id(&r.device_instance_id) == primary_identity
            {
                return true;
            }
            // 同一设备（记录键落盘的实例 ID 一致）。
            let stored = identity_from_values(&r.info_values);
            if !primary_stored.instance_id.is_empty()
                && stored.instance_id == primary_stored.instance_id
            {
                return true;
            }
            // 同一设备（目标端点的端点历史包含该旧 GUID）。
            target
                .identity
                .endpoint_history
                .iter()
                .any(|g| g.eq_ignore_ascii_case(&r.guid))
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
        write_install_config(new_guid, &target.name, "", &config)?;
        // 槽位改写要**重启端点**才生效：引擎缓存端点 APO 链，只改注册表不会
        // 立刻重载（2026-09-16 实测：活动流上删掉 VxAPO 槽位值后，新起的流
        // 仍加载旧 APO，直到端点/服务重建）。
        // 注意：写/删 FxProperties 值本身**不需要**停服（只需 KEY_SET_VALUE 句柄，
        // `open_for_write` 即是）——停服在这里没有「解锁」作用，故不再停服。
        let repair_endpoint_path = find_endpoint_path(new_guid)?;
        let is_capture = repair_endpoint_path.contains("Capture");
        if let Err(e) = crate::install::audiodg::restart_endpoint_device(new_guid, is_capture) {
            log::warn!("stale migrate: 修复后端点重启失败（改动将在下次端点重建时生效）：{e}");
        }
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

    // ── 身份落盘刷新（GUID 刷新后仍能找到这台设备）──────────────────────
    // 用活跃端点的实时身份刷新目标记录，并把端点历史并集写回：
    // 已落盘历史 ∪ 活跃端点历史 ∪ 各旧 GUID ∪ 新 GUID（去重排序）。
    // 上次刷新后即便 Windows 再换 GUID，也能靠这组历史把新 GUID 认回同一设备。
    let stored_target = identity_from_key(&target_key);
    let live = read_endpoint_identity(&find_endpoint_path(new_guid)?);
    let refreshed = EndpointIdentity {
        instance_id: pick(&live.instance_id, &stored_target.instance_id),
        hardware_ids: if live.hardware_ids.is_empty() {
            stored_target.hardware_ids.clone()
        } else {
            live.hardware_ids.clone()
        },
        product_name: pick(&live.product_name, &stored_target.product_name),
        interface_name: pick(&live.interface_name, &stored_target.interface_name),
        endpoint_history: Vec::new(),
    };
    let mut history: Vec<Vec<String>> = vec![
        stored_target.endpoint_history.clone(),
        live.endpoint_history.clone(),
        vec![new_guid.to_string()],
    ];
    for source in &group {
        history.push(vec![source.guid.clone()]);
    }
    if let Err(e) = write_identity_values(&target_key, &refreshed, &merge_endpoint_history(&history))
    {
        warnings.push(format!("写入设备身份失败：{e}"));
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

/// 活跃端点 + 稳定身份（身份读取失败时为空值，不影响其它端点）。
struct ActiveEndpoint {
    guid: String,
    name: String,
    identity: EndpointIdentity,
}

fn active_endpoints() -> Result<Vec<ActiveEndpoint>> {
    let devices = enumerate_devices()?;
    Ok(devices
        .into_iter()
        .filter_map(|d| {
            let endpoint = d.endpoint?;
            let identity = find_endpoint_path(&endpoint.endpoint_guid)
                .ok()
                .map(|path| read_endpoint_identity(&path))
                .unwrap_or_default();
            Some(ActiveEndpoint {
                guid: endpoint.endpoint_guid,
                name: endpoint.friendly_name,
                identity,
            })
        })
        .collect())
}

/// 配对结果。
#[derive(Debug)]
enum MatchOutcome {
    /// 命中唯一活跃端点 + 命中来源。
    Matched(usize, &'static str),
    /// 命中多个候选：不猜，保持 unmatched。
    Ambiguous,
    /// 未命中。
    None,
}

/// 活跃端点索引：端点历史 / 实例 ID / 硬件 ID 三个倒排表。
struct MatchIndex {
    /// 活跃端点归一化产品名（与下标一一对应，供硬件 ID 兜底做产品名比较）。
    products: Vec<String>,
    /// 历史端点 GUID（小写）→ 活跃端点下标（可能多候选）。
    by_history: HashMap<String, Vec<usize>>,
    /// 活跃端点 GUID（小写）→ 下标。
    active_by_guid: HashMap<String, usize>,
    /// 归一化设备实例 ID → 活跃端点下标。
    by_instance: HashMap<String, Vec<usize>>,
    /// 归一化硬件 ID → 活跃端点下标。
    by_hardware: HashMap<String, Vec<usize>>,
}

impl MatchIndex {
    fn new(active: &[ActiveEndpoint]) -> Self {
        let mut index = MatchIndex {
            products: Vec::with_capacity(active.len()),
            by_history: HashMap::new(),
            active_by_guid: HashMap::new(),
            by_instance: HashMap::new(),
            by_hardware: HashMap::new(),
        };
        for (i, ep) in active.iter().enumerate() {
            index.products.push(ep.identity.product_key());
            index
                .active_by_guid
                .insert(ep.guid.to_ascii_lowercase(), i);
            for guid in &ep.identity.endpoint_history {
                push_unique(index.by_history.entry(guid.clone()).or_default(), i);
            }
            if !ep.identity.instance_id.is_empty() {
                push_unique(
                    index
                        .by_instance
                        .entry(ep.identity.instance_id.clone())
                        .or_default(),
                    i,
                );
            }
            for hw in &ep.identity.hardware_ids {
                push_unique(index.by_hardware.entry(hw.clone()).or_default(), i);
            }
        }
        index
    }

    /// 分层匹配置记录 → 活跃端点。
    fn resolve(&self, record: &StaleRecord, stored: &EndpointIdentity) -> MatchOutcome {
        // ① 端点历史：老 GUID 出现在某活跃端点的历史里（GUID 刷新后最可靠的现成线索）。
        if let Some(candidates) = self.by_history.get(&record.guid.to_ascii_lowercase()) {
            match unique_index(candidates.as_slice()) {
                Some(i) => return MatchOutcome::Matched(i, "endpoint_history"),
                None => return MatchOutcome::Ambiguous,
            }
        }
        // ①b 反方向：记录已落盘的 EndpointHistory 里出现活跃端点 GUID。
        let reverse: Vec<usize> = stored
            .endpoint_history
            .iter()
            .filter_map(|guid| self.active_by_guid.get(guid).copied())
            .collect();
        match unique_index(&reverse) {
            Some(i) => return MatchOutcome::Matched(i, "endpoint_history"),
            None if !reverse.is_empty() => return MatchOutcome::Ambiguous,
            None => {}
        }
        // ② 老端点键仍可读时的实例 ID（旧路径保留）。
        if !record.device_instance_id.is_empty() {
            if let Some(candidates) = self.by_instance.get(&record.device_instance_id) {
                match unique_index(candidates.as_slice()) {
                    Some(i) => return MatchOutcome::Matched(i, "device_instance_id"),
                    None => return MatchOutcome::Ambiguous,
                }
            }
        }
        // ③ 记录键里落盘的实例 ID。
        if !stored.instance_id.is_empty() {
            if let Some(candidates) = self.by_instance.get(&stored.instance_id) {
                match unique_index(candidates.as_slice()) {
                    Some(i) => return MatchOutcome::Matched(i, "stored_identity"),
                    None => return MatchOutcome::Ambiguous,
                }
            }
        }
        // ④ 硬件 ID 相交 + 产品名一致，且唯一候选（USB 换口导致实例 ID 变化时兜底）。
        let mut candidates: Vec<usize> = Vec::new();
        for hw in &stored.hardware_ids {
            if let Some(found) = self.by_hardware.get(hw) {
                for i in found {
                    push_unique(&mut candidates, *i);
                }
            }
        }
        if !stored.product_name.is_empty() {
            // 产品名一致优先：过滤后仍为空则退回硬件 ID 候选（部分设备产品名缺失/不同）。
            let product_key = stored.product_key();
            let filtered: Vec<usize> = candidates
                .iter()
                .copied()
                .filter(|i| self.products.get(*i).map(|p| *p == product_key).unwrap_or(false))
                .collect();
            if !filtered.is_empty() {
                candidates = filtered;
            }
        }
        match unique_index(&candidates) {
            Some(i) => MatchOutcome::Matched(i, "hardware_id"),
            None if candidates.len() > 1 => MatchOutcome::Ambiguous,
            None => MatchOutcome::None,
        }
    }
}

/// 下标去重追加。
fn push_unique(list: &mut Vec<usize>, value: usize) {
    if !list.contains(&value) {
        list.push(value);
    }
}

/// 候选唯一下标；`None` 表示 0 个或多个。
fn unique_index(candidates: &[usize]) -> Option<usize> {
    match candidates {
        [only] => Some(*only),
        _ => None,
    }
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

/// 取第一个非空字符串（活跃值优先，回退已落盘值）。
fn pick(primary: &str, fallback: &str) -> String {
    if primary.trim().is_empty() {
        fallback.to_string()
    } else {
        primary.to_string()
    }
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
    rename_with_retry(&tmp, dst)
        .map_err(|e| VxApoError::internal(format!("替换文件失败：{e}")))
}

/// `tmp → 目标` 原子替换，带有限重试。
///
/// 迁移全程不停 AudioSrv（config.toml 不会被"占用"：DLL 一次性 `fs::read` +
/// 目录通知热重载，App 自身一直在用 tmp+rename 改写），因此存在极端时间窗：
/// DLL 的 watcher 刚触发、`fs::read` 正在读 config.toml 时
/// `fs::rename`（MoveFileEx REPLACE_EXISTING）会拿到 ERROR_SHARING_VIOLATION。
/// 读窗口是微秒级，重试即可跨过，避免一次瞬时冲突让整次迁移硬失败。
/// 仅对"可能瞬时"的错误码重试（5 ACCESS_DENIED / 32 SHARING_VIOLATION /
/// 33 LOCK_VIOLATION 及 PermissionDenied），其余错误立即返回。
fn rename_with_retry(tmp: &Path, dst: &Path) -> std::io::Result<()> {
    const ATTEMPTS: usize = 5;
    const BACKOFF_MS: u64 = 50;
    let mut last: Option<std::io::Error> = None;
    for attempt in 0..ATTEMPTS {
        match fs::rename(tmp, dst) {
            Ok(()) => return Ok(()),
            Err(e) if is_transient_sharing_error(&e) => {
                last = Some(e);
                if attempt + 1 < ATTEMPTS {
                    std::thread::sleep(std::time::Duration::from_millis(BACKOFF_MS));
                }
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::Other, "rename retry exhausted")
    }))
}

/// 是否为可能瞬时消失的共享/锁冲突（可重试）。
fn is_transient_sharing_error(e: &std::io::Error) -> bool {
    match e.raw_os_error() {
        Some(5) | Some(32) | Some(33) => true,
        _ => e.kind() == std::io::ErrorKind::PermissionDenied,
    }
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

    #[test]
    fn transient_sharing_errors_are_retryable() {
        // 5 ACCESS_DENIED / 32 SHARING_VIOLATION / 33 LOCK_VIOLATION。
        for code in [5, 32, 33] {
            let e = std::io::Error::from_raw_os_error(code);
            assert!(is_transient_sharing_error(&e), "code {code} 应可重试");
        }
        // 路径不存在等确定性错误不重试（避免无意义等待）。
        let missing = std::io::Error::from_raw_os_error(2);
        assert!(!is_transient_sharing_error(&missing));
        let not_found = std::io::Error::new(std::io::ErrorKind::NotFound, "x");
        assert!(!is_transient_sharing_error(&not_found));
    }

    #[test]
    fn rename_with_retry_replaces_destination() {
        let dir = std::env::temp_dir().join("vxapo_stale_rename_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.tmp");
        let dst = dir.join("dst.toml");
        fs::write(&src, b"new-content").unwrap();
        fs::write(&dst, b"old-content").unwrap();
        rename_with_retry(&src, &dst).unwrap();
        assert_eq!(fs::read_to_string(&dst).unwrap(), "new-content");
        assert!(!src.exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
