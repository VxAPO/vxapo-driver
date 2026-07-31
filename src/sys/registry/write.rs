//! sys/registry/write.rs — 注册表写入与权限提升（Note 31）
//!
//! 提供注册表写入操作及权限提升功能：
//! - `makeWritable`：修改 DACL，为 Administrators 添加完全控制权限
//! - `takeOwnership`：获取注册表键所有权，需 `SE_TAKE_OWNERSHIP_NAME` 特权
//!
//! 只读操作位于 `sys/registry/read.rs`（Note 48），写入与权限操作集中在本模块。
//!
//! 所有 `unsafe` 块必须附带 `SAFETY` 注释，说明前提条件与安全保证（Note 40）。
//! CI 启用 `#![deny(clippy::undocumented_unsafe_blocks)]` 强制检查。

use windows::Win32::Foundation::{HLOCAL, LUID, LocalFree, CloseHandle};
use windows::Win32::System::Registry::{
    HKEY,
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW,
    RegGetKeySecurity, RegSetKeySecurity, RegSetValueExW,
    KEY_ALL_ACCESS, REG_BINARY, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION,
    GetSecurityDescriptorDacl,
    InitializeSecurityDescriptor, LookupAccountNameW,
    LookupPrivilegeValueW, NO_INHERITANCE,
    OWNER_SECURITY_INFORMATION, PSID, SID_NAME_USE,
    PSECURITY_DESCRIPTOR, SE_PRIVILEGE_ENABLED,
    SetSecurityDescriptorDacl, SetSecurityDescriptorOwner,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    AdjustTokenPrivileges, LUID_AND_ATTRIBUTES,
    SECURITY_DESCRIPTOR,
};
use windows::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE,
    SET_ACCESS, SetEntriesInAclW,
    TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
};
use windows::Win32::System::Threading::OpenProcessToken;
use windows_core::{HSTRING, PCWSTR, PWSTR, BOOL, Result, Error};
use windows::Win32::Foundation::E_FAIL;

use super::win32_ok;

/// 将 Rust 字符串转为注册表可用的 UTF-16 字节（含 null terminator）
fn to_registry_bytes(s: &str) -> Vec<u8> {
    let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2).to_vec()
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 注册表写入
// ══════════════════════════════════════════════════════════════════════════════

fn to_pcwstr(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 创建注册表键（如果不存在则创建，已存在则打开）。
///
/// 返回打开的键句柄。调用方负责关闭。
pub fn create_key(root: HKEY, sub_key: &str) -> Result<HKEY> {
    let name = to_pcwstr(sub_key);
    let mut handle = HKEY::default();

    // SAFETY: name 是合法 HSTRING，handle 初始化为默认值。
    // REG_OPTION_NON_VOLATILE：持久化键。
    let err = unsafe {
        RegCreateKeyExW(
            root,
            PCWSTR(name.as_ptr()),
            Some(0),
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_ALL_ACCESS,
            None,
            &mut handle,
            None,
        )
    };

    if err.is_err() {
        return Err(Error::new(
            E_FAIL,
            format!("create_key({}): RegCreateKeyExW failed", sub_key),
        ));
    }

    Ok(handle)
}

/// 写入 REG_SZ 值。
pub fn write_sz(handle: HKEY, name: &str, value: &str) -> Result<()> {
    let name_hstr = HSTRING::from(name);

    // SAFETY: name/value 是合法 HSTRING（UTF-16 null 结尾）。
    let err = unsafe {
        RegSetValueExW(
            handle,
            &name_hstr,
            Some(0),
            REG_SZ,
            Some(to_registry_bytes(value).as_slice()),
        )
    };

    if err.is_err() {
        Err(Error::new(
            E_FAIL,
            format!("write_sz({}): RegSetValueExW failed", name),
        ))
    } else {
        Ok(())
    }
}

/// 写入 REG_DWORD 值。
pub fn write_dword(handle: HKEY, name: &str, value: u32) -> Result<()> {
    let name_hstr = HSTRING::from(name);
    let data = value.to_le_bytes();

    // SAFETY: name 是合法 HSTRING，data 是 4 字节小端序。
    let err = unsafe {
        RegSetValueExW(
            handle,
            &name_hstr,
            Some(0),
            REG_DWORD,
            Some(&data),
        )
    };

    if err.is_err() {
        Err(Error::new(
            E_FAIL,
            format!("write_dword({}): RegSetValueExW failed", name),
        ))
    } else {
        Ok(())
    }
}

/// 写入 REG_BINARY 值。
pub fn write_binary(handle: HKEY, name: &str, data: &[u8]) -> Result<()> {
    let name_hstr = HSTRING::from(name);

    // SAFETY: name 是合法 HSTRING，data 是原始字节。
    let err = unsafe {
        RegSetValueExW(
            handle,
            &name_hstr,
            Some(0),
            REG_BINARY,
            Some(data),
        )
    };

    if err.is_err() {
        Err(Error::new(
            E_FAIL,
            format!("write_binary({}): RegSetValueExW failed", name),
        ))
    } else {
        Ok(())
    }
}

/// 删除注册表值。
pub fn delete_value(handle: HKEY, name: &str) -> Result<()> {
    let name_hstr = HSTRING::from(name);

    // SAFETY: name 是合法 HSTRING。
    let err = unsafe { RegDeleteValueW(handle, &name_hstr) };

    if err.is_ok() {
        return Ok(());
    }

    // 值或路径不存在不算错误
    let code = err.to_hresult().0 as u32;
    if code == 2 || code == 3 {
        // ERROR_FILE_NOT_FOUND (2) 或 ERROR_PATH_NOT_FOUND (3)
        Ok(())
    } else {
        Err(Error::new(
            E_FAIL,
            format!("delete_value({}): RegDeleteValueW failed", name),
        ))
    }
}

/// 删除注册表键及其所有子键。
pub fn delete_tree(root: HKEY, sub_key: &str) -> Result<()> {
    let name = HSTRING::from(sub_key);

    // SAFETY: name 是合法 HSTRING。
    let err = unsafe { RegDeleteTreeW(root, &name) };

    if err.is_ok() {
        Ok(())
    } else {
        // 不存在不算致命错误——可能是首次卸载
        let code = err.to_hresult().0 as u32;
        if code == 2 || code == 3 {
            Ok(())
        } else {
            Err(Error::new(
                E_FAIL,
                format!("delete_tree({}): RegDeleteTreeW failed", sub_key),
            ))
        }
    }
}

/// 关闭注册表键句柄。
///
/// 内部调用 `RegCloseKey`。handle 为 null 或已关闭时静默返回。
///
/// # Note 40
///
/// 原标记为 `pub unsafe fn`，但此函数本身是安全的——
/// unsafe 约束在于"调用方保证 handle 有效"，由 `RegKey` 的 RAII 保证。
pub fn close_key(handle: HKEY) {
    if !handle.is_invalid() {
        // SAFETY: handle 由调用方保证为有效注册表键句柄（通过 open 获得）。
        // RegCloseKey 对已关闭的 handle 是幂等的（返回 ERROR_INVALID_HANDLE，
        // 不会导致内存损坏）。null handle 被 is_invalid() 过滤。
        unsafe {
            let _ = RegCloseKey(handle);
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// makeWritable（Note 31）
//
// 修改 DACL 添加 Administrators 组的完全控制权限。
// 用于安装时写入受保护的注册表键（如 FxProperties）。
// ══════════════════════════════════════════════════════════════════════════════

/// 修改注册表键的 DACL，添加 Administrators 完全控制权限（Note 31）。
///
/// 用于 `install()` 中 FxProperties 创建失败时的权限提升重试。
///
/// # Safety
///
/// 涉及 Windows 安全 API，调用方需确保：
/// - 进程有 `WRITE_DAC` 权限（通常需要管理员权限）
/// - handle 有效
pub fn make_writable(handle: HKEY) -> Result<()> {
    // RAII 守卫：自动释放 LocalAlloc 分配的 DACL 内存
    struct DaclGuard(HLOCAL);

    impl Drop for DaclGuard {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                unsafe {
                    let _ = LocalFree(Some(self.0));
                }
            }
        }
    }

    // 获取当前 DACL
    let mut needed: u32 = 0;

    // SAFETY: 首次调用获取所需缓冲区大小。
    let err = unsafe { RegGetKeySecurity(handle, DACL_SECURITY_INFORMATION, None, &mut needed) };

    // ERROR_INSUFFICIENT_BUFFER (122) 是预期的
    if err.is_err() && needed == 0 {
        return Err(Error::new(E_FAIL, "make_writable: RegGetKeySecurity query failed"));
    }

    let mut security_buf = vec![0u8; needed as usize];
    let security_desc = PSECURITY_DESCRIPTOR(security_buf.as_mut_ptr() as *mut _);

    // SAFETY: 第二次调用获取实际安全描述符。
    unsafe {
        win32_ok(unsafe {RegGetKeySecurity(
            handle,
            DACL_SECURITY_INFORMATION,
            Some(security_desc),
            &mut needed,
        )}).map_err(|e| Error::new(E_FAIL, format!("make_writable: {}", e)))?;
    }

    // 构造 Administrators SID
    let admin_sid = create_administrators_sid()?;

    // 构造新的 ACE：允许 Administrators 完全控制
    let explicit_access = EXPLICIT_ACCESS_W {
        grfAccessPermissions: KEY_ALL_ACCESS.0,
        grfAccessMode: SET_ACCESS,
        grfInheritance: NO_INHERITANCE,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
            ptstrName: PWSTR(admin_sid.as_ptr() as *mut u16),
        },
    };

    // 获取当前 DACL
    let mut old_dacl: *mut ACL = std::ptr::null_mut();
    let mut dacl_defaulted = BOOL::default();
    let mut dacl_present = BOOL::default();

    // SAFETY: 从安全描述符中提取 DACL。
    unsafe {
        GetSecurityDescriptorDacl(
            security_desc,
            &mut dacl_present,
            &mut old_dacl,
            &mut dacl_defaulted,
        ).map_err(|e| Error::new(E_FAIL, format!("make_writable: {}", e)))?;
    }

    let old_dacl_opt = if dacl_present.as_bool() && !old_dacl.is_null() {
        Some(old_dacl as *const ACL)
    } else {
        None
    };

    // 合并新 ACE 到现有 DACL
    let mut new_dacl: *mut ACL = std::ptr::null_mut();

    // SAFETY: SetEntriesInAclW 合并新的 ACE 到 DACL。
    unsafe {
        win32_ok(unsafe {SetEntriesInAclW(
            Some(&[explicit_access]),
            old_dacl_opt,
            &mut new_dacl,
        )}).map_err(|e| Error::new(E_FAIL, format!("make_writable: {}", e)))?;
    }
    // 分配成功后立即用 RAII 守卫包装，之后任何提前返回都会自动释放
    let _dacl_guard = DaclGuard(HLOCAL(new_dacl as *mut _));

    // 设置新的安全描述符
    let mut new_security = unsafe { std::mem::zeroed::<SECURITY_DESCRIPTOR>() };

    // SAFETY: 初始化安全描述符并设置 DACL。
    unsafe {
        InitializeSecurityDescriptor(
            PSECURITY_DESCRIPTOR(&mut new_security as *mut _ as *mut _),
            1u32, // SECURITY_DESCRIPTOR_REVISION,
        ).map_err(|e| Error::new(E_FAIL, format!("make_writable: {}", e)))?;
        SetSecurityDescriptorDacl(
            PSECURITY_DESCRIPTOR(&mut new_security as *mut _ as *mut _),
            true,
            Some(new_dacl),
            false,
        ).map_err(|e| Error::new(E_FAIL, format!("make_writable: {}", e)))?;

        win32_ok(unsafe {RegSetKeySecurity(
            handle,
            DACL_SECURITY_INFORMATION,
            PSECURITY_DESCRIPTOR(&new_security as *const _ as *mut _),
        )}).map_err(|e| Error::new(E_FAIL, format!("make_writable: {}", e)))?;
    }

    Ok(())
}

/// 创建 Administrators 组的 SID。
fn create_administrators_sid() -> Result<Vec<u8>> {
    let mut sid_size: u32 = 0;
    let mut domain_size: u32 = 0;
    let mut use_type = SID_NAME_USE(0);

    // 首次调用获取大小
    // SAFETY: 查询 SID 大小。
    unsafe {
        LookupAccountNameW(
            PCWSTR::null(),
            &HSTRING::from("Administrators"),
            None,
            &mut sid_size,
            None,
            &mut domain_size,
            &mut use_type,
        );
    }

    let mut sid_buf = vec![0u8; sid_size as usize];
    let mut domain_buf = vec![0u16; domain_size as usize];

    // SAFETY: 第二次调用获取实际 SID。
    unsafe {
        LookupAccountNameW(
            PCWSTR::null(),
            &HSTRING::from("Administrators"),
            Some(PSID(sid_buf.as_mut_ptr() as *mut _)),
            &mut sid_size,
            Some(PWSTR(domain_buf.as_mut_ptr())),
            &mut domain_size,
            &mut use_type,
        ).map_err(|e| Error::new(E_FAIL, format!("create_administrators_sid: {}", e)))?;
    }

    Ok(sid_buf)
}

// ══════════════════════════════════════════════════════════════════════════════
// takeOwnership（Note 31）
//
// 获取键所有权需要 SE_TAKE_OWNERSHIP_NAME 特权。
// ══════════════════════════════════════════════════════════════════════════════

/// 特权恢复守卫（N1：最小权限原则）。
///
/// 构造时保存原始特权状态，Drop 时自动恢复。
pub struct PrivilegeGuard {
    token: windows::Win32::Foundation::HANDLE,
    original_privileges: TOKEN_PRIVILEGES,
}

impl Drop for PrivilegeGuard {
    fn drop(&mut self) {
        // 恢复原始特权状态
        // SAFETY: self.token 在 enable_take_ownership_privilege 中已验证有效。
        // AdjustTokenPrivileges 恢复原始状态是 Windows 标准操作。
        unsafe {
            let _ = AdjustTokenPrivileges(
                self.token,
                false,
                Some(&self.original_privileges),
                0,
                None,
                None,
            );
        }
        // 恢复失败仅记录日志（进程退出时特权自动回收）。
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(0) {
            log::warn!("PrivilegeGuard: failed to restore original privilege state");
        }

        // 关闭令牌句柄，防止句柄泄漏
        // SAFETY: self.token 是从 OpenProcessToken 获取的有效句柄。
        unsafe {
            let _ = CloseHandle(self.token);
        }
    }
}

/// 启用当前进程的 `SE_TAKE_OWNERSHIP_NAME` 特权。
///
/// 返回 `PrivilegeGuard`，Drop 时自动恢复原始特权状态（N1）。
///
/// # Safety
///
/// 修改进程令牌特权。需要管理员权限。
pub fn enable_take_ownership_privilege() -> Result<PrivilegeGuard> {
    // SAFETY: 获取当前进程令牌。
    let token = unsafe {
        let mut token_handle = windows::Win32::Foundation::HANDLE::default();
        OpenProcessToken(
            windows::Win32::System::Threading::GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token_handle,
        ).map_err(|e| Error::new(E_FAIL, format!("enable_take_ownership_privilege: {}", e)))?;
        token_handle
    };

    // 查找 SE_TAKE_OWNERSHIP_NAME 特权的 LUID
    let mut luid = LUID::default();

    // SAFETY: 查找特权 LUID。
    unsafe {
        LookupPrivilegeValueW(
            PCWSTR::null(),
            &HSTRING::from("SeTakeOwnershipPrivilege"),
            &mut luid,
        ).map_err(|e| Error::new(E_FAIL, format!("enable_take_ownership_privilege: {}", e)))?;
    }

    // ── 保存当前特权状态（用于恢复） ─────────────────────────────────────
    let mut original_privileges = TOKEN_PRIVILEGES::default();
    let mut return_length = 0u32;

    // SAFETY: 首次调用获取当前特权状态。
    unsafe {
        let _ = AdjustTokenPrivileges(
            token,
            false,
            None,
            0,
            Some(&mut original_privileges),
            Some(&mut return_length as *mut u32),
        );
    }

    // ── 启用特权 ─────────────────────────────────────────────────────────

    let new_privileges = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };

    // SAFETY: 调整进程令牌特权。
    unsafe {
        AdjustTokenPrivileges(
            token,
            false,
            Some(&new_privileges),
            0,
            None,
            None,
        ).map_err(|e| Error::new(E_FAIL, format!("enable_take_ownership_privilege: {}", e)))?;
    }

    Ok(PrivilegeGuard {
        token,
        original_privileges,
    })
}

/// 获取注册表键的所有权（Note 31 + N1）。
///
/// 将键的所有者设置为 Administrators 组。
/// 通过 `PrivilegeGuard` RAII 确保用完即恢复（N1：最小权限原则）。
pub fn take_ownership(handle: HKEY) -> Result<()> {
    // guard 在此作用域结束时自动恢复特权。
    let _guard = enable_take_ownership_privilege()?;

    let admin_sid = create_administrators_sid()?;

    // 获取当前安全描述符
    let mut needed: u32 = 0;
    unsafe {
        RegGetKeySecurity(handle, OWNER_SECURITY_INFORMATION, None, &mut needed);
    }

    let mut buf = vec![0u8; needed as usize];
    let desc = PSECURITY_DESCRIPTOR(buf.as_mut_ptr() as *mut _);

    // SAFETY: 获取键的安全描述符。
    unsafe {
        win32_ok(unsafe {RegGetKeySecurity(handle, OWNER_SECURITY_INFORMATION, Some(desc), &mut needed)})
            .map_err(|e| Error::new(E_FAIL, format!("take_ownership: {}", e)))?;
    }

    // 设置新所有者
    let mut new_security = unsafe { std::mem::zeroed::<SECURITY_DESCRIPTOR>() };

    // SAFETY: 初始化安全描述符并设置所有者。
    unsafe {
        InitializeSecurityDescriptor(
            PSECURITY_DESCRIPTOR(&mut new_security as *mut _ as *mut _),
            1u32,
        ).map_err(|e| Error::new(E_FAIL, format!("take_ownership: {}", e)))?;
        SetSecurityDescriptorOwner(
            PSECURITY_DESCRIPTOR(&mut new_security as *mut _ as *mut _),
            Some(PSID(admin_sid.as_ptr() as *mut _)),
            false,
        ).map_err(|e| Error::new(E_FAIL, format!("take_ownership: {}", e)))?;
        win32_ok(unsafe {RegSetKeySecurity(
            handle,
            OWNER_SECURITY_INFORMATION,
            PSECURITY_DESCRIPTOR(&new_security as *const _ as *mut _),
        )}).map_err(|e| Error::new(E_FAIL, format!("take_ownership: {}", e)))?;
    }

    Ok(())
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use windows::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER};

    use crate::test_helpers::serial_lock;
    use super::*;
    use super::super::read::RegKey;

    // 测试写入 HKCU 下的临时键，避免影响系统
    const TEST_ROOT: HKEY = HKEY_CURRENT_USER;
    const TEST_KEY: &str = r"SOFTWARE\VxAPO_Test_RegWrite";

    fn cleanup() {
        let _ = delete_tree(TEST_ROOT, TEST_KEY);
    }

    // ── create_key + write + read ───────────────────────────────────────────

    #[test]
    fn create_key_and_write_sz() {
        let _l = serial_lock();
        cleanup();
        let handle = create_key(TEST_ROOT, TEST_KEY).unwrap();
        write_sz(handle, "TestValue", "Hello VxAPO").unwrap();

        // 验证
        let key = RegKey::open(TEST_ROOT, TEST_KEY).unwrap();
        match key.read_value("TestValue").unwrap() {
            super::super::read::RegValue::Sz(s) => assert_eq!(s, "Hello VxAPO"),
            other => panic!("expected Sz, got {other:?}"),
        }

        close_key(handle);
        cleanup();
    }

    #[test]
    fn create_key_and_write_dword() {
        let _l = serial_lock();
        cleanup();
        let handle = create_key(TEST_ROOT, TEST_KEY).unwrap();
        write_dword(handle, "TestDword", 42).unwrap();

        let key = RegKey::open(TEST_ROOT, TEST_KEY).unwrap();
        assert_eq!(key.read_dword_value("TestDword").unwrap(), 42);

        close_key(handle);
        cleanup();
    }

    #[test]
    fn create_key_and_write_binary() {
        let _l = serial_lock();
        cleanup();
        let handle = create_key(TEST_ROOT, TEST_KEY).unwrap();
        let data = [0x01, 0x02, 0x03, 0x04];
        write_binary(handle, "TestBinary", &data).unwrap();

        let key = RegKey::open(TEST_ROOT, TEST_KEY).unwrap();
        assert_eq!(key.read_binary_value("TestBinary").unwrap(), data);

        close_key(handle);
        cleanup();
    }

    #[test]
    fn write_sz_overwrites() {
        let _l = serial_lock();
        cleanup();
        let handle = create_key(TEST_ROOT, TEST_KEY).unwrap();

        write_sz(handle, "Val", "first").unwrap();
        write_sz(handle, "Val", "second").unwrap();

        let key = RegKey::open(TEST_ROOT, TEST_KEY).unwrap();
        match key.read_value("Val").unwrap() {
            super::super::read::RegValue::Sz(s) => assert_eq!(s, "second"),
            other => panic!("expected Sz, got {other:?}"),
        }

        close_key(handle);
        cleanup();
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

    // ── delete_tree ─────────────────────────────────────────────────────────

    #[test]
    fn delete_tree_removes_key() {
        let _l = serial_lock();
        cleanup();
        let handle = create_key(TEST_ROOT, TEST_KEY).unwrap();
        write_sz(handle, "Val", "data").unwrap();
        close_key(handle);

        assert!(RegKey::open(TEST_ROOT, TEST_KEY).is_ok());

        delete_tree(TEST_ROOT, TEST_KEY).unwrap();

        assert!(RegKey::open(TEST_ROOT, TEST_KEY).is_err());
    }

    #[test]
    fn delete_tree_nonexistent_ok() {
        let result = delete_tree(TEST_ROOT, r"SOFTWARE\VxAPO_Nonexistent_12345");
        assert!(result.is_ok());
    }

    // ── 子键写入 ────────────────────────────────────────────────────────────

    #[test]
    fn create_nested_keys() {
        let _l = serial_lock();
        cleanup();
        let handle = create_key(TEST_ROOT, TEST_KEY).unwrap();
        close_key(handle);

        let nested = format!("{TEST_KEY}\\SubKey1\\SubKey2");
        let handle = create_key(TEST_ROOT, &nested).unwrap();
        write_sz(handle, "DeepValue", "found").unwrap();

        let key = RegKey::open(TEST_ROOT, &nested).unwrap();
        match key.read_value("DeepValue").unwrap() {
            super::super::read::RegValue::Sz(s) => assert_eq!(s, "found"),
            other => panic!("expected Sz, got {other:?}"),
        }

        close_key(handle);
        cleanup();
    }

    // ── Unicode 值 ──────────────────────────────────────────────────────────

    #[test]
    fn write_unicode_value() {
        let _l = serial_lock();
        cleanup();
        let handle = create_key(TEST_ROOT, TEST_KEY).unwrap();
        write_sz(handle, "中文值", "音频处理").unwrap();

        let key = RegKey::open(TEST_ROOT, TEST_KEY).unwrap();
        match key.read_value("中文值").unwrap() {
            super::super::read::RegValue::Sz(s) => assert_eq!(s, "音频处理"),
            other => panic!("expected Sz, got {other:?}"),
        }

        close_key(handle);
        cleanup();
    }

    // ── makeWritable / takeOwnership ─────────────────────────────────────────
    // 注意：这两个函数需要管理员权限，在普通测试中可能失败。
    // 只测试不 panic，不验证成功。

    #[test]
    fn make_writable_on_test_key() {
        let _l = serial_lock();
        cleanup();
        let handle = create_key(TEST_ROOT, TEST_KEY).unwrap();

        let _ = make_writable(handle);

        close_key(handle);
        cleanup();
    }

    #[test]
    fn take_ownership_on_test_key() {
        let _l = serial_lock();
        cleanup();
        let handle = create_key(TEST_ROOT, TEST_KEY).unwrap();

        let _ = take_ownership(handle);

        close_key(handle);
        cleanup();
    }

    // ── enable_take_ownership_privilege ──────────────────────────────────────

    #[test]
    fn enable_privilege_runs() {
        let _ = enable_take_ownership_privilege();
    }
}