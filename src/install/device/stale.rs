//! install/device/stale.rs — 旧 GUID 残留检测、迁移与清理。
//!
//! Windows 重新枚举音频端点后，MMDevices 下的端点 GUID 可能变化：
//! 旧端点的槽位键已消失，但 VxAPO 自己的 `Child APOs\{oldGuid}`、
//! `C:\ProgramData\VxAPO\{oldGuid}` 与 snapshots 仍会残留。
//!
//! 本模块以「设备实例 ID」为稳定身份，把旧记录匹配到当前活跃端点，
//! 并在用户确认后迁移配置/快照/子 APO 备份、修复新 GUID 安装状态。
//!
//! **匹配分层**：Windows 大版本更新会重排端点 GUID 并**整体删除**老端点键，
//! 此时「老端点键读实例 ID」这条路径失效，记录会退化为 unmatched。现按优先级
//! 分层匹配：
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
use crate::install::device::info::{detect_mode_for_guid, enumerate_devices, find_endpoint_path};
use crate::install::device::slots::{
    child_apo_key_exists, ChildApoKind, InstallMode, CHILD_APO_PATH_ROOT, FX_PROPERTIES_KEY,
    INSTALL_VERSION,
};
use crate::install::device::sysfx::{decode_backups, SYSFX_BACKUP_VALUE};
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

/// 迁移修复回调：构造并写入目标端点的安装配置。
///
/// 参数 `(device_guid, device_name, install_mode)`。实现由 selector 层提供——
/// `InstallConfig` / `write_install_config` 属 selector，不下沉到 device 层。
pub type RepairInstallFn<'a> = &'a dyn Fn(&str, &str, InstallMode) -> Result<()>;

// ── 子模块（按职责拆分；公开 API 经此处 re-export）──────────────────────────
mod acl;
mod detect;
mod matching;
mod migrate;

pub use acl::{cleanup_orphan, fix_config_acl};
pub use detect::list_stale_installs;
pub use migrate::migrate_install;
#[cfg(test)]
mod tests;
