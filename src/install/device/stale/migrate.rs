//! install/device/stale/migrate.rs — 旧 GUID 安装迁移
//!
//! 配置/快照选择与搬运、记录键值合并、身份落盘刷新，以及迁移后的端点重启。
//! 共享常量与公开类型见父模块 `install/device/stale.rs`。

use super::*;
use super::acl::*;
use super::detect::*;
use super::matching::*;

/// 把旧 GUID 安装迁移到新 GUID。
///
/// `config_from` / `snapshot_from` 为显式来源；缺省按「最新 config、最早 snapshot」
/// 在同设备实例的旧记录与目标现有文件之间选择。
/// 目标端点安装状态需修复时，通过 `repair_install` 回调写入安装配置。
pub fn migrate_install(
    old_guid: &str,
    new_guid: &str,
    config_from: Option<&str>,
    snapshot_from: Option<&str>,
    repair_install: RepairInstallFn<'_>,
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
        repair_install(new_guid, &target.name, mode)?;
        // 槽位改写要**重启端点**才生效：引擎缓存端点 APO 链，只改注册表不会
        // 立刻重载（新起的流仍加载旧 APO，直到端点/服务重建）。
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
pub(super) struct FileSource {
    guid: String,
    path: PathBuf,
    mtime: u64,
}

pub(super) fn choose_config_source(
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

pub(super) fn choose_snapshot_source(
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

pub(super) fn copy_values(src: &RegKey, dst: &RegKey, overwrite: bool) -> Result<()> {
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

/// 取第一个非空字符串（活跃值优先，回退已落盘值）。
pub(super) fn pick(primary: &str, fallback: &str) -> String {
    if primary.trim().is_empty() {
        fallback.to_string()
    } else {
        primary.to_string()
    }
}

pub(super) fn copy_file(src: &Path, dst: &Path) -> Result<()> {
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
pub(super) fn rename_with_retry(tmp: &Path, dst: &Path) -> std::io::Result<()> {
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
pub(super) fn is_transient_sharing_error(e: &std::io::Error) -> bool {
    match e.raw_os_error() {
        Some(5) | Some(32) | Some(33) => true,
        _ => e.kind() == std::io::ErrorKind::PermissionDenied,
    }
}

pub(super) fn archive_target_file(target_guid: &str, name: &str) -> Result<()> {
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
