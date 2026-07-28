//! sys/registry/delete.rs — 注册表删除操作
//!
//! 提供删除注册表键、值、树的底层工具。
//! 只封装 Windows Registry API，不包含任何业务逻辑。
//!
//! 只读操作位于 `sys/registry/read.rs`，写入操作位于 `sys/registry/write.rs`。

use std::io::{Error as IoError, ErrorKind};
use windows::Win32::System::Registry::{
    HKEY, RegDeleteKeyW, RegDeleteTreeW, RegDeleteValueW,
};
use windows::core::{HSTRING, Result};

// ══════════════════════════════════════════════════════════════════════════════
// 删除操作
// ══════════════════════════════════════════════════════════════════════════════

/// 删除注册表值。
///
/// # 参数
///
/// - `handle`：已打开的注册表键句柄
/// - `name`：要删除的值名称
///
/// # 行为
///
/// - 值不存在时返回 `Ok(())`（幂等）
pub fn delete_value(handle: HKEY, name: &str) -> Result<()> {
    let name_hstr = HSTRING::from(name);

    // SAFETY: name 是合法 HSTRING（UTF-16 null 结尾）。
    let err = unsafe { RegDeleteValueW(handle, &name_hstr) };

    if err.is_ok() {
        return Ok(());
    }

    // 值不存在不算错误（幂等）
    let code = err.to_hresult().0 as u32;
    if code == 2 || code == 3 {
        // ERROR_FILE_NOT_FOUND (2) 或 ERROR_PATH_NOT_FOUND (3)
        Ok(())
    } else {
        Err(IoError::new(
            ErrorKind::Other,
            format!("RegDeleteValueW({}) failed: {}", name, code)
        ).into())
    }
}

/// 删除注册表键（空键）。
///
/// # 参数
///
/// - `root`：根键（HKEY_LOCAL_MACHINE 等）
/// - `sub_key`：要删除的子键路径
///
/// # 注意
///
/// 此函数只能删除**空键**（无子键）。如需删除含子键的键，使用 `delete_tree`。
pub fn delete_key(root: HKEY, sub_key: &str) -> Result<()> {
    let name = HSTRING::from(sub_key);

    // SAFETY: name 是合法 HSTRING。
    let err = unsafe { RegDeleteKeyW(root, &name) };

    if err.is_ok() {
        Ok(())
    } else {
        let code = err.to_hresult().0 as u32;
        if code == 2 || code == 3 {
            // ERROR_FILE_NOT_FOUND (2) 或 ERROR_PATH_NOT_FOUND (3)
            Ok(())
        } else {
            Err(IoError::new(
                ErrorKind::Other,
                format!("RegDeleteKeyW({}) failed: {}", sub_key, code)
            ).into())
        }
    }
}

/// 递归删除注册表树（含所有子键和值）。
///
/// # 参数
///
/// - `root`：根键（HKEY_LOCAL_MACHINE 等）
/// - `sub_key`：要删除的子树路径
///
/// # 行为
///
/// - 键不存在时返回 `Ok(())`（幂等）
/// - 删除整个子树，所有子键和值一并删除
pub fn delete_tree(root: HKEY, sub_key: &str) -> Result<()> {
    let name = HSTRING::from(sub_key);

    // SAFETY: name 是合法 HSTRING。
    let err = unsafe { RegDeleteTreeW(root, &name) };

    if err.is_ok() {
        Ok(())
    } else {
        let code = err.to_hresult().0 as u32;
        if code == 2 || code == 3 {
            // ERROR_FILE_NOT_FOUND (2) 或 ERROR_PATH_NOT_FOUND (3)
            Ok(())
        } else {
            Err(IoError::new(
                ErrorKind::Other,
                format!("RegDeleteTreeW({}) failed: {}", sub_key, code)
            ).into())
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::read::RegKey;
    use super::super::write::{create_key, write_sz, close_key};
    use windows::Win32::System::Registry::HKEY_CURRENT_USER;
    use crate::test_helpers::serial_lock;

    const TEST_ROOT: HKEY = HKEY_CURRENT_USER;
    const TEST_KEY: &str = r"SOFTWARE\VxAPO_Test_Delete";

    fn cleanup() {
        let _ = delete_tree(TEST_ROOT, TEST_KEY);
    }

    // ── delete_value ────────────────────────────────────────────────────────

    #[test]
    fn delete_existing_value() {
        let _l = serial_lock();
        cleanup();

        let handle = create_key(TEST_ROOT, TEST_KEY).unwrap();
        write_sz(handle, "ToDelete", "value").unwrap();

        delete_value(handle, "ToDelete").unwrap();

        let key = RegKey::open(TEST_ROOT, TEST_KEY).unwrap();
        assert!(!key.value_exists("ToDelete").unwrap_or(true));

        close_key(handle);
        cleanup();
    }

    #[test]
    fn delete_nonexistent_value_ok() {
        let _l = serial_lock();
        cleanup();

        let handle = create_key(TEST_ROOT, TEST_KEY).unwrap();

        // 删除不存在的值应返回 Ok（幂等）
        let result = delete_value(handle, "NonExistentValue");
        assert!(result.is_ok());

        close_key(handle);
        cleanup();
    }

    // ── delete_tree ─────────────────────────────────────────────────────────

    #[test]
    fn delete_existing_tree() {
        let _l = serial_lock();
        cleanup();

        let handle = create_key(TEST_ROOT, TEST_KEY).unwrap();
        write_sz(handle, "Val", "data").unwrap();
        close_key(handle);

        assert!(RegKey::open(TEST_ROOT, TEST_KEY).is_ok());

        delete_tree(TEST_ROOT, TEST_KEY).unwrap();

        assert!(RegKey::open(TEST_ROOT, TEST_KEY).is_err());
        cleanup();
    }

    #[test]
    fn delete_tree_with_subkeys() {
        let _l = serial_lock();
        cleanup();

        // 创建嵌套结构
        let nested = format!("{}\\Sub1\\Sub2", TEST_KEY);
        let handle = create_key(TEST_ROOT, &nested).unwrap();
        write_sz(handle, "DeepVal", "data").unwrap();
        close_key(handle);

        // 删除整个树
        delete_tree(TEST_ROOT, TEST_KEY).unwrap();

        assert!(RegKey::open(TEST_ROOT, TEST_KEY).is_err());
        cleanup();
    }

    #[test]
    fn delete_nonexistent_tree_ok() {
        let _l = serial_lock();
        // 删除不存在的键应返回 Ok（幂等）
        let result = delete_tree(TEST_ROOT, r"SOFTWARE\VxAPO_Nonexistent_12345");
        assert!(result.is_ok());
    }

    // ── delete_key（空键）─────────────────────────────────────────────────

    #[test]
    fn delete_empty_key() {
        let _l = serial_lock();
        cleanup();

        // 创建空键（无子键）
        let handle = create_key(TEST_ROOT, TEST_KEY).unwrap();
        close_key(handle);

        // RegDeleteKeyW 只能删除空键
        delete_key(TEST_ROOT, TEST_KEY).unwrap();

        assert!(RegKey::open(TEST_ROOT, TEST_KEY).is_err());
        cleanup();
    }

    #[test]
    fn delete_key_with_subkeys_returns_error() {
        let _l = serial_lock();
        cleanup();

        // 创建带子键的键
        let nested = format!("{}\\Sub", TEST_KEY);
        let handle = create_key(TEST_ROOT, &nested).unwrap();
        close_key(handle);

        // 删除父键（非空）应失败
        let result = delete_key(TEST_ROOT, TEST_KEY);
        assert!(result.is_err());

        cleanup();
    }

    #[test]
    fn delete_nonexistent_key_ok() {
        let _l = serial_lock();
        let result = delete_key(TEST_ROOT, r"SOFTWARE\VxAPO_Nonexistent_Key");
        assert!(result.is_ok());
    }
}