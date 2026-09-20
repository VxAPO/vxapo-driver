//! install/device/stale/acl.rs — 残留清理与配置目录 ACL 修复
//!
//! 一并清理无法匹配的旧记录（归档文件 + 删记录键），以及给交互用户
//! 授予 config/snapshot 的 Modify 权限。
//! 共享常量与公开类型见父模块 `install/device/stale.rs`。

use super::*;
use super::detect::*;

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

pub(super) fn delete_child_apo_key(guid: &str) -> Result<()> {
    let (root, sub) = split_key(CHILD_APO_PATH_ROOT)?;
    if sub.is_empty() {
        return Ok(());
    }
    let path = format!("{sub}\\{guid}");
    delete_tree(root, &path).map_err(|e| VxApoError::internal(format!("删除旧信息区失败：{e}")))
}

/// 给交互登录用户授予 Modify 权限（icacls SID `*S-1-5-4`）。
///
/// 目录使用 `(OI)(CI)M /T` 递归；文件直接 `M`。失败只记录告警，
/// 不影响迁移结果（App 侧仍可提示用户以管理员运行一次修复）。
pub(super) fn grant_interactive_modify(path: &Path) {
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

pub(super) fn archive_guid_files(guid: &str) -> Result<()> {
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
