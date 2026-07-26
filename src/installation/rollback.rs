//! installation/rollback.rs — 分层事务与 .reg 备份（Note 32）
//!
//! 安装前将原始 APO GUID 备份到 .reg 文件，用于卸载时回退。
//! 文件名格式：`backup_{设备名}_{连接名}.reg`
//!
//! 实际的注册表导出由 `utils/reg_read.rs::save_to_file` 执行（Note 48）。
//!
//! 此模块仅负责备份策略与文件命名，不直接操作注册表。

use crate::utils::error::Result;
use crate::utils::reg_read;
use crate::installation::reg_write;
use windows::Win32::System::Registry::{HKEY, HKEY_LOCAL_MACHINE};

// ══════════════════════════════════════════════════════════════════════════════
// 备份
// ══════════════════════════════════════════════════════════════════════════════

/// 备份设备 FxProperties 到 .reg 文件（Note 32）。
///
/// - `device_name`：设备友好名称
/// - `connection_name`：连接名称
/// - `fx_properties_path`：FxProperties 注册表路径
/// - `output_dir`：.reg 文件输出目录
///
/// 返回 .reg 文件路径。
pub fn backup_fx_properties(
    device_name: &str,
    connection_name: &str,
    fx_properties_path: &str,
    output_dir: &str,
) -> Result<String> {
    // 生成文件名（替换非法字符）
    let safe_device = sanitize_filename(device_name);
    let safe_conn = sanitize_filename(connection_name);
    let filename = format!("backup_{safe_device}_{safe_conn}.reg");
    let path = format!("{output_dir}\\{filename}");

    // 导出注册表
    reg_read::save_to_file(HKEY_LOCAL_MACHINE, fx_properties_path, &path)?;

    Ok(path)
}

/// 将字符串中的非法文件名字符替换为 `_`。
fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            // 原始列表（Note 32 的 .reg 文件名）
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect()
}

// ══════════════════════════════════════════════════════════════════════════════
// 回滚
// ══════════════════════════════════════════════════════════════════════════════

/// 安装操作的回滚项。
#[derive(Debug)]
pub enum RollbackAction {
    /// 删除注册表键
    DeleteKey(String),
    /// 恢复注册表值（从备份）
    RestoreValue {
        root: HKEY,
        key: String,
        name: String,
        backup: Vec<u8>
    },
    /// 注销 APO（调用 UnregisterAPO）
    UnregisterApo(windows::core::GUID),
    /// 删除 COM 类键
    DeleteClsidKey(String),
}

/// 分层事务管理器。
///
/// 安装过程中记录所有操作，失败时按逆序回滚（Note 29）。
#[derive(Debug)]
pub struct Transaction {
    actions: Vec<RollbackAction>,
    committed: bool,
}

impl Transaction {
    /// 创建新事务。
    pub fn new() -> Self {
        Self {
            actions: Vec::new(),
            committed: false,
        }
    }

    /// 记录一个回滚动作。
    pub fn record(&mut self, action: RollbackAction) {
        self.actions.push(action);
    }

    /// 提交事务（安装成功后调用，不执行回滚）。
    pub fn commit(&mut self) {
        self.committed = true;
    }

    /// 执行回滚（安装失败时调用，按逆序执行所有回滚动作）。
    pub fn rollback(&mut self) {
        if self.committed {
            return;
        }

        for action in self.actions.drain(..).rev() {
            match action {
                RollbackAction::DeleteKey(path) => {
                    let _ = reg_read::save_to_file(
                        HKEY_LOCAL_MACHINE,
                        &path,
                        &format!("{}.rollback_backup.reg", path.replace('\\', "_")),
                    );
                    let _ = crate::installation::reg_write::delete_tree(
                        HKEY_LOCAL_MACHINE,
                        &path,
                    );
                }
                RollbackAction::DeleteClsidKey(path) => {
                    let _ = crate::installation::reg_write::delete_tree(
                        windows::Win32::System::Registry::HKEY_CLASSES_ROOT,
                        &path,
                    );
                }
                RollbackAction::RestoreValue { root, ref key, ref name, ref backup } => {
                    // 用 create_key（KEY_ALL_ACCESS）获取可写句柄。
                    // RegKey::open 是 KEY_READ，无法写入恢复值。
                    // 如果 create_key 失败（权限不足），尝试 make_writable 重试（Note 31）。
                    let handle = match reg_write::create_key(root, key) {
                        Ok(h) => h,
                        Err(_) => {
                            // 尝试获取父键句柄用于权限提升。
                            if let Some((parent, _)) = key.rsplit_once('\\') {
                                if let Ok(parent_key) = crate::utils::reg_read::RegKey::open(root, parent) {
                                    let _ = reg_write::make_writable(parent_key.handle());
                                }
                            }
                            match reg_write::create_key(root, key) {
                                Ok(h) => h,
                                Err(e) => {
                                    log::warn!(
                                        "Rollback: cannot restore value {}\\{} — {}",
                                        key, name, e,
                                    );
                                    continue;
                                }
                            }
                        }
                    };

                    let _ = reg_write::write_binary(handle, name, backup);
                    reg_write::close_key(handle);
                    log::info!("Rollback: restored value {}\\{}", key, name);
                }
                RollbackAction::UnregisterApo(guid) => {
                    // Note 30: 注销 APO。
                    // UnregisterAPO 是 Windows APO 框架 API（mmdeviceapi）。
                    // 当前 windows crate feature 集未启用此 API，
                    // 记录 GUID 供手动清理或 Phase 6 补全。
                    log::warn!(
                        "Rollback: UnregisterAPO({}) — API not yet available, record for manual cleanup",
                        format_apo_guid(guid),
                    );
                }
            }
        }
    }

    /// 已记录的回滚动作数。
    pub fn action_count(&self) -> usize {
        self.actions.len()
    }

    /// 是否已提交。
    pub fn is_committed(&self) -> bool {
        self.committed
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        if !self.committed {
            self.rollback();
        }
    }
}

/// 格式化 APO GUID 用于日志输出。
fn format_apo_guid(g: windows::core::GUID) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        g.data1, g.data2, g.data3,
        g.data4[0], g.data4[1], g.data4[2], g.data4[3],
        g.data4[4], g.data4[5], g.data4[6], g.data4[7],
    )
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_filename_basic() {
        assert_eq!(sanitize_filename("Speakers"), "Speakers");
        assert_eq!(sanitize_filename("Realtek HD Audio"), "Realtek HD Audio");
    }

    #[test]
    fn sanitize_filename_special_chars() {
        assert_eq!(
            sanitize_filename(r"Headphones (USB\VID_1234)"),
            "Headphones (USB_VID_1234)"
        );
        assert_eq!(
            sanitize_filename(r"C:\Windows\System32"),
            "C__Windows_System32"
        );
    }

    #[test]
    fn sanitize_filename_all_special() {
        assert_eq!(
            sanitize_filename("\\/:*?\"<>|"),
            "_________"
        );
    }

    #[test]
    fn transaction_new_is_empty() {
        let tx = Transaction::new();
        assert_eq!(tx.action_count(), 0);
        assert!(!tx.is_committed());
    }

    #[test]
    fn transaction_record() {
        let mut tx = Transaction::new();
        tx.record(RollbackAction::DeleteKey("test".to_owned()));
        tx.record(RollbackAction::DeleteKey("test2".to_owned()));
        assert_eq!(tx.action_count(), 2);
    }

    #[test]
    fn transaction_commit_prevents_rollback() {
        let mut tx = Transaction::new();
        tx.record(RollbackAction::DeleteKey("test".to_owned()));
        tx.commit();
        assert!(tx.is_committed());
        tx.rollback(); // 不应执行
        assert!(tx.is_committed());
    }

    #[test]
    fn transaction_drop_triggers_rollback() {
        // 创建事务但不提交——drop 时自动回滚
        {
            let mut tx = Transaction::new();
            tx.record(RollbackAction::DeleteKey(
                r"SOFTWARE\VxAPO_Nonexistent_Test".to_owned(),
            ));
            // tx drop here → rollback()
        }
        // 不 panic = 通过
    }

    #[test]
    fn backup_fx_properties_sanitizes_names() {
        // sanitize 只处理 Note 32 中列出的文件名非法字符
        let safe = sanitize_filename("Speakers (Realtek)");
        assert!(safe.contains('('), "parentheses are valid in filenames");
        assert!(safe.contains(')'));
        assert!(!safe.contains('"'));
        assert!(!safe.contains('\\'));
    }

    #[test]
    fn rollback_restore_value() {
        use windows::Win32::System::Registry::HKEY_CURRENT_USER;
        let test_path = format!("SOFTWARE\\VxAPO_Rollback_Restore_{}", std::process::id());
        let root = HKEY_CURRENT_USER;

        let _ = crate::installation::reg_write::delete_tree(root, &test_path);

        // 写入原始值
        let handle = crate::installation::reg_write::create_key(root, &test_path).unwrap();
        crate::installation::reg_write::write_binary(handle, "TestValue", &[1u8, 2, 3]).unwrap();
        crate::installation::reg_write::close_key(handle);

        // 覆盖为新值
        let handle = crate::installation::reg_write::create_key(root, &test_path).unwrap();
        crate::installation::reg_write::write_binary(handle, "TestValue", &[10u8, 20, 30]).unwrap();
        crate::installation::reg_write::close_key(handle);

        // 创建事务 + 回滚
        {
            let mut tx = Transaction::new();
            tx.record(RollbackAction::RestoreValue {
                root,
                key: test_path.clone(),
                name: "TestValue".to_string(),
                backup: vec![1u8, 2, 3],
            });
            drop(tx);  // ← 显式 drop，确保回滚在此处执行
        }

        // 验证
        let key = crate::utils::reg_read::RegKey::open(root, &test_path).unwrap();
        let restored = key.read_binary_value("TestValue").unwrap();
        assert_eq!(restored, vec![1u8, 2, 3]);

        let _ = crate::installation::reg_write::delete_tree(root, &test_path);
    }

    #[test]
    fn rollback_unregister_apo_logs_warning() {
        // UnregisterAPO 对无效 GUID 应安全返回（不 panic）
        let bogus_guid = windows::core::GUID::zeroed();
        {
            let mut tx = Transaction::new();
            tx.record(RollbackAction::UnregisterApo(bogus_guid));
            // tx Drop → rollback() → log::warn
        }
        // 不 panic = 通过
    }
}