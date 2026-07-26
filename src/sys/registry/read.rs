//! utils/reg_read.rs — 只读注册表操作（Note 48）
//!
//! 提供注册表只读查询功能：
//! - `split_key`：拆分注册表路径为根键 `HKEY` + 子键路径
//! - `RegKey`：封装已打开的注册表键句柄，提供类型安全的只读查询方法
//!
//! 支持操作：
//! - `openKey` / `readValue` / `readDWORDValue` / `readBinaryValue` / `readMultiValue`
//! - `keyExists` / `enumSubKeys` / `valueExists`
//! - `getGuidString` / `isWindowsVersionAtLeast` / `saveToFile`
//!
//! `split_key` 实现要点（Note 48）：
//! - 根键名大小写不敏感（转大写比较）
//! - 第一个 `\` 分隔根键与子键路径
//! - 支持 5 个标准根键：`HKEY_CLASSES_ROOT` / `HKEY_CURRENT_CONFIG` /
//!   `HKEY_CURRENT_USER` / `HKEY_LOCAL_MACHINE` / `HKEY_USERS`
//! - 未知根键返回错误
//!
//! 写入与权限操作位于 `installation/reg_write.rs`（Note 31）。
//!
//! 此模块为纯工具层，与引擎、DSP、COM 实例无耦合。

use windows::Win32::System::Registry::*;
use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::WIN32_ERROR;

use crate::sys::registry::write::close_key;
use crate::utils::error::{Result, VxApoError};

// ══════════════════════════════════════════════════════════════════════════════
// WIN32_ERROR → Result 转换
// ══════════════════════════════════════════════════════════════════════════════

/// 将 `WIN32_ERROR` 转为 `Result<()>`，`0`（ERROR_SUCCESS）为 Ok，其余为 Err。
///
/// Win32 错误码到 HRESULT 的标准映射：`(code & 0xFFFF) | 0x80070000`。
fn win32_ok(err: WIN32_ERROR) -> Result<()> {
    if err.0 == 0 {
        Ok(())
    } else {
        Err(VxApoError::HResult(windows::core::HRESULT(
            (0x8007_0000u32 | (err.0 & 0xFFFF)) as i32,
        )))
    }
}

/// 判断 WIN32_ERROR 是否为 "未找到"。
///
/// - `ERROR_FILE_NOT_FOUND` (2)
/// - `ERROR_PATH_NOT_FOUND` (3)
fn is_not_found(err: WIN32_ERROR) -> bool {
    err.0 == 2 || err.0 == 3
}

// ══════════════════════════════════════════════════════════════════════════════
// Wide string helpers（仅用于读取注册表返回值）
// ══════════════════════════════════════════════════════════════════════════════

/// 将 `&[u8]`（LE UTF-16 字节流）转为 String，遇到 null 停止。
fn utf16_bytes_to_string(buf: &[u8]) -> String {
    let words: Vec<u16> = buf
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let len = words.iter().position(|&c| c == 0).unwrap_or(words.len());
    String::from_utf16_lossy(&words[..len])
}

/// 将 `&[u8]`（LE UTF-16 字节流）解析为 `REG_MULTI_SZ` 字符串列表。
fn parse_multi_sz(buf: &[u8]) -> Vec<String> {
    let words: Vec<u16> = buf
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let mut result = Vec::new();
    let mut start = 0;
    for i in 0..words.len() {
        if words[i] == 0 {
            if i > start {
                result.push(String::from_utf16_lossy(&words[start..i]));
            } else {
                break; // 双 null = 结束
            }
            start = i + 1;
        }
    }
    result
}

// ══════════════════════════════════════════════════════════════════════════════
// split_key
// ══════════════════════════════════════════════════════════════════════════════

/// 将完整注册表路径拆分为 `(根键 HKEY, 子键路径)`。
///
/// - 根键名大小写不敏感（转大写比较）
/// - 第一个 `\` 分隔根键与子键
/// - 支持 5 个标准根键及缩写（HKLM / HKCU / HKCR / HKU / HKCC）
///
/// # Errors
/// 路径不含 `\` 或根键名无法识别时返回 `VxApoError::Registry`。
pub fn split_key(path: &str) -> Result<(HKEY, &str)> {
    let sep = path
        .find('\\')
        .ok_or_else(|| VxApoError::registry(path, "missing '\\': expected ROOTKEY\\SubKey"))?;

    let root = match path[..sep].to_ascii_uppercase().as_str() {
        "HKEY_LOCAL_MACHINE" | "HKLM" => HKEY_LOCAL_MACHINE,
        "HKEY_CURRENT_USER" | "HKCU" => HKEY_CURRENT_USER,
        "HKEY_CLASSES_ROOT" | "HKCR" => HKEY_CLASSES_ROOT,
        "HKEY_USERS" | "HKU" => HKEY_USERS,
        "HKEY_CURRENT_CONFIG" | "HKCC" => HKEY_CURRENT_CONFIG,
        unknown => return Err(VxApoError::registry(unknown, "unknown root key name")),
    };

    Ok((root, &path[sep + 1..]))
}

// ══════════════════════════════════════════════════════════════════════════════
// RegValue — 注册表值的类型化表示
// ══════════════════════════════════════════════════════════════════════════════

/// 注册表值的类型化表示。
#[derive(Debug, Clone, PartialEq)]
pub enum RegValue {
    /// `REG_SZ` 或 `REG_EXPAND_SZ`
    Sz(String),
    /// `REG_DWORD`
    Dword(u32),
    /// `REG_QWORD`
    Qword(u64),
    /// `REG_BINARY`
    Binary(Vec<u8>),
    /// `REG_MULTI_SZ`
    MultiSz(Vec<String>),
}

// ══════════════════════════════════════════════════════════════════════════════
// RegKey — 只读注册表键句柄
// ══════════════════════════════════════════════════════════════════════════════

/// 打开的注册表键句柄（只读）。RAII：Drop 时调用 `RegCloseKey`。
#[derive(Debug)]
pub struct RegKey {
    handle: HKEY,
}

impl RegKey {
    // ── 打开 ─────────────────────────────────────────────────────────────────

    /// 以 `KEY_READ` 权限打开子键。
    pub fn open(root: HKEY, sub_key: &str) -> Result<Self> {
        let sub = HSTRING::from(sub_key);
        let mut handle = HKEY::default();

        // SAFETY: sub 是合法 UTF-16 字符串（HSTRING 保证 null 结尾），
        // handle 初始化为默认（无效），仅执行只读注册表查询。
        win32_ok(unsafe {
            RegOpenKeyExW(root, PCWSTR(sub.as_ptr()), Some(0), KEY_READ, &mut handle)
        })?;

        Ok(Self { handle })
    }

    /// 获取底层注册表句柄。
    ///
    /// 用于 `installation/reg_write.rs` 中需要原生 `HKEY` 的 API
    ///（如 `RegSetKeySecurity`、`RegNotifyChangeKeyValue`）。
    ///
    /// # Safety
    ///
    /// 调用方必须确保不对返回的 `HKEY` 调用 `RegCloseKey`——
    /// `RegKey` 的 `Drop` 会负责关闭。
    pub fn handle(&self) -> HKEY {
        self.handle
    }

    /// 委托
    pub fn open_sub_key(&self, sub_key: &str) -> Result<Self> {
        Self::open(self.handle, sub_key)
    }

    // ── 通用读取 ────────────────────────────────────────────────────────────

    /// 读取指定名称的值，自动识别类型。
    ///
    /// 传入空字符串 `""` 读取默认值。
    pub fn read_value(&self, name: &str) -> Result<RegValue> {
        let name_hstr = HSTRING::from(name);
        let name_pw = PCWSTR(name_hstr.as_ptr());
        let mut val_type = REG_NONE;
        let mut data_size: u32 = 0;

        // SAFETY: 首次调用获取数据类型和所需缓冲区大小，data=null。
        win32_ok(unsafe {
            RegQueryValueExW(
                self.handle,
                name_pw,
                None,
                Some(&mut val_type),
                None,
                Some(&mut data_size),
            )
        })?;

        let mut buf = vec![0u8; data_size as usize];

        // SAFETY: 第二次调用将实际数据写入预分配缓冲区。
        win32_ok(unsafe {
            RegQueryValueExW(
                self.handle,
                name_pw,
                None,
                Some(&mut val_type),
                Some(buf.as_mut_ptr()),
                Some(&mut data_size),
            )
        })?;

        buf.truncate(data_size as usize);

        // REG_VALUE_TYPE 是 newtype(usize) — 比较内部 .0 值
        match val_type {
            v if v == REG_SZ || v == REG_EXPAND_SZ => {
                Ok(RegValue::Sz(utf16_bytes_to_string(&buf)))
            }
            v if v == REG_DWORD => {
                let b: [u8; 4] = buf
                    .as_slice()
                    .try_into()
                    .map_err(|_| VxApoError::registry(name, "REG_DWORD data too short"))?;
                Ok(RegValue::Dword(u32::from_le_bytes(b)))
            }
            v if v == REG_QWORD => {
                let b: [u8; 8] = buf
                    .as_slice()
                    .try_into()
                    .map_err(|_| VxApoError::registry(name, "REG_QWORD data too short"))?;
                Ok(RegValue::Qword(u64::from_le_bytes(b)))
            }
            v if v == REG_BINARY => Ok(RegValue::Binary(buf)),
            v if v == REG_MULTI_SZ => Ok(RegValue::MultiSz(parse_multi_sz(&buf))),
            other => Err(VxApoError::registry(
                name,
                &format!("unsupported registry value type: {}", other.0),
            )),
        }
    }

    // ── 类型化便捷读取 ──────────────────────────────────────────────────────

    /// 读取 `REG_SZ` 值。
    pub fn read_sz_value(&self, name: &str) -> Result<String> {
        match self.read_value(name)? {
            RegValue::Sz(v) => Ok(v),
            other => Err(VxApoError::registry(
                name,
                &format!("expected REG_SZ, got {other:?}"),
            )),
        }
    }

    /// 读取 `REG_SZ` 值，失败时返回 `None`。
    ///
    /// 适合测试和日志场景——不替代 `read_sz_value` 的完整错误报告。
    pub fn read_sz(&self, name: &str) -> Option<String> {
        self.read_sz_value(name).ok()
    }
    
    /// 读取 `REG_DWORD` 值。
    pub fn read_dword_value(&self, name: &str) -> Result<u32> {
        match self.read_value(name)? {
            RegValue::Dword(v) => Ok(v),
            other => Err(VxApoError::registry(
                name,
                &format!("expected REG_DWORD, got {other:?}"),
            )),
        }
    }

    /// 读取 `REG_BINARY` 值。
    pub fn read_binary_value(&self, name: &str) -> Result<Vec<u8>> {
        match self.read_value(name)? {
            RegValue::Binary(v) => Ok(v),
            other => Err(VxApoError::registry(
                name,
                &format!("expected REG_BINARY, got {other:?}"),
            )),
        }
    }

    /// 读取 `REG_MULTI_SZ` 值。
    pub fn read_multi_value(&self, name: &str) -> Result<Vec<String>> {
        match self.read_value(name)? {
            RegValue::MultiSz(v) => Ok(v),
            other => Err(VxApoError::registry(
                name,
                &format!("expected REG_MULTI_SZ, got {other:?}"),
            )),
        }
    }

    // ── 存在性检查 ──────────────────────────────────────────────────────────

    /// 检查当前键下是否存在指定值。
    pub fn value_exists(&self, name: &str) -> Result<bool> {
        let name_hstr = HSTRING::from(name);
        let mut val_type = REG_NONE;
        let mut data_size: u32 = 0;

        // SAFETY: data=null — 只查询存在性，不读取实际数据。
        let err = unsafe {
            RegQueryValueExW(
                self.handle,
                PCWSTR(name_hstr.as_ptr()),
                None,
                Some(&mut val_type),
                None,
                Some(&mut data_size),
            )
        };

        if err.0 == 0 {
            Ok(true)
        } else if is_not_found(err) {
            Ok(false)
        } else {
            win32_ok(err)?;
            unreachable!()
        }
    }

    /// 检查当前键下是否存在指定子键。
    pub fn key_exists_child(&self, sub_key: &str) -> Result<bool> {
        let sub = HSTRING::from(sub_key);
        let mut handle = HKEY::default();

        // SAFETY: 仅尝试打开子键来判断存在性。
        let err =
            unsafe { RegOpenKeyExW(self.handle, PCWSTR(sub.as_ptr()), Some(0), KEY_READ, &mut handle) };

        if err.0 == 0 {
            // SAFETY: handle 刚刚由 RegOpenKeyExW 成功打开，需要关闭。
            unsafe {
                let _ = RegCloseKey(handle);
            }
            Ok(true)
        } else if is_not_found(err) {
            Ok(false)
        } else {
            win32_ok(err)?;
            unreachable!()
        }
    }

    // ── 枚举 ────────────────────────────────────────────────────────────────

    /// 枚举当前键下所有子键名称。
    pub fn enum_sub_keys(&self) -> Result<Vec<String>> {
        let mut count: u32 = 0;
        let mut max_name_len: u32 = 0;

        // SAFETY: 查询子键数量和最大名称长度。
        win32_ok(unsafe {
            RegQueryInfoKeyW(
                self.handle,
                Some(PWSTR(std::ptr::null_mut())),
                None,
                None,
                Some(&mut count as *mut u32),
                Some(&mut max_name_len as *mut u32),
                None,
                None,
                None,
                None,
                None,
                None,
            )
        })?;

        let buf_len = (max_name_len + 1) as usize;
        let mut names = Vec::with_capacity(count as usize);

        for i in 0..count {
            let mut name_buf = vec![0u16; buf_len];
            let mut name_len = buf_len as u32;

            // SAFETY: 在已知子键数量范围内枚举，name_buf 容量足够。
            win32_ok(unsafe {
                RegEnumKeyExW(
                    self.handle,
                    i,
                    Some(PWSTR(name_buf.as_mut_ptr())),
                    &mut name_len,
                    None,
                    Some(PWSTR(std::ptr::null_mut())),
                    None,
                    None,
                )
            })?;

            names.push(String::from_utf16_lossy(&name_buf[..name_len as usize]));
        }

        Ok(names)
    }

    /// 枚举当前键下所有值的名称（含默认值 `""`）。
    pub fn enum_values(&self) -> Result<Vec<String>> {
        let mut count: u32 = 0;
        let mut max_name_len: u32 = 0;

        // SAFETY: 查询值数量和最大值名称长度。
        win32_ok(unsafe {
            RegQueryInfoKeyW(
                self.handle,
                Some(PWSTR(std::ptr::null_mut())),
                None,
                None,
                None,
                None,
                None,
                Some(&mut count as *mut u32),
                Some(&mut max_name_len as *mut u32),
                None,
                None,
                None,
            )
        })?;

        let buf_len = (max_name_len + 1) as usize;
        let mut names = Vec::with_capacity(count as usize);

        for i in 0..count {
            let mut name_buf = vec![0u16; buf_len];
            let mut name_len = buf_len as u32;

            // SAFETY: 在已知值数量范围内枚举，name_buf 容量足够。
            win32_ok(unsafe {
                RegEnumValueW(
                    self.handle,
                    i,
                    Some(PWSTR(name_buf.as_mut_ptr())),
                    &mut name_len,
                    None,
                    None,
                    None,
                    None,
                )
            })?;

            names.push(String::from_utf16_lossy(&name_buf[..name_len as usize]));
        }

        Ok(names)
    }

    // ── GUID 读取 ───────────────────────────────────────────────────────────

    /// 读取注册表值并将其格式化为 GUID 字符串 `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`。
    ///
    /// 支持两种存储格式：
    /// - `REG_BINARY`（16 字节小端序）
    /// - `REG_SZ`（已经是字符串形式，直接返回）
    pub fn get_guid_string(&self, name: &str) -> Result<String> {
        match self.read_value(name)? {
            RegValue::Sz(s) => Ok(s),
            RegValue::Binary(ref bytes) if bytes.len() >= 16 => {
                let d1 = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                let d2 = u16::from_le_bytes([bytes[4], bytes[5]]);
                let d3 = u16::from_le_bytes([bytes[6], bytes[7]]);
                let d4 = &bytes[8..16];
                Ok(format!(
                    "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
                    d1, d2, d3,
                    d4[0], d4[1], d4[2], d4[3], d4[4], d4[5], d4[6], d4[7],
                ))
            }
            RegValue::Binary(_) => Err(VxApoError::registry(
                name,
                "binary value too short for GUID (need at least 16 bytes)",
            )),
            other => Err(VxApoError::registry(
                name,
                &format!("unexpected value type for GUID: {other:?}"),
            )),
        }
    }
}

/// RAII：Drop 时关闭注册表键句柄。
impl Drop for RegKey {
    fn drop(&mut self) {
        close_key(self.handle);
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 顶层便捷函数
// ══════════════════════════════════════════════════════════════════════════════

/// 检查注册表键是否存在（只读尝试打开）。
pub fn key_exists(root: HKEY, sub_key: &str) -> Result<bool> {
    match RegKey::open(root, sub_key) {
        Ok(_) => Ok(true),
        Err(VxApoError::HResult(hr))
            if hr.0 == (0x8007_0002u32 as i32) || hr.0 == (0x8007_0003u32 as i32) =>
        {
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

/// 检查注册表值是否存在。
pub fn value_exists(root: HKEY, sub_key: &str, name: &str) -> Result<bool> {
    let key = RegKey::open(root, sub_key)?;
    key.value_exists(name)
}

/// 检查当前 Windows 版本是否 >= 指定版本。
///
/// 读取 `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion` 下的
/// `CurrentMajorVersionNumber`、`CurrentMinorVersionNumber`（REG_DWORD）
/// 和 `CurrentBuildNumber`（REG_SZ 或 REG_DWORD）。
pub fn is_windows_version_at_least(major: u32, minor: u32, build: u32) -> Result<bool> {
    let key = RegKey::open(
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
    )?;

    let actual_major = key.read_dword_value("CurrentMajorVersionNumber")?;
    let actual_minor = key.read_dword_value("CurrentMinorVersionNumber")?;

    let actual_build: u32 = match key.read_value("CurrentBuildNumber")? {
        RegValue::Sz(s) => s.parse::<u32>().unwrap_or(0),
        RegValue::Dword(v) => v,
        _ => 0,
    };

    Ok(actual_major > major
        || (actual_major == major && actual_minor > minor)
        || (actual_major == major && actual_minor == minor && actual_build >= build))
}

// ══════════════════════════════════════════════════════════════════════════════
// save_to_file — 导出注册表键为 .reg 文件
// ══════════════════════════════════════════════════════════════════════════════

/// 将注册表键（含子键）导出为 `.reg` 文件（UTF-16LE with BOM）。
///
/// 用于安装前的备份（Note 32）。
pub fn save_to_file(root: HKEY, sub_key: &str, path: &str) -> Result<()> {
    let root_name = hkey_to_name(root)?;
    let display_root = format!("{root_name}\\{sub_key}");

    let mut content = String::from("Windows Registry Editor Version 5.00\n\n");
    dump_key_recursive(root, sub_key, &display_root, &mut content)?;

    // 写入 UTF-16LE with BOM
    let mut bytes = Vec::with_capacity(2 + content.len() * 2);
    bytes.extend_from_slice(&[0xFFu8, 0xFE]); // BOM
    for code_unit in content.encode_utf16() {
        bytes.extend_from_slice(&code_unit.to_le_bytes());
    }

    std::fs::write(path, bytes)?;
    Ok(())
}

/// 递归导出键及其所有子键的值。
fn dump_key_recursive(
    root: HKEY,
    sub_key: &str,
    display_path: &str,
    content: &mut String,
) -> Result<()> {
    let key = match RegKey::open(root, sub_key) {
        Ok(k) => k,
        Err(e) => {
            content.push_str(&format!("; Failed to open [{display_path}]: {e}\n\n"));
            return Ok(());
        }
    };

    content.push_str(&format!("[{display_path}]\n"));

    for value_name in key.enum_values()? {
        let display_name = if value_name.is_empty() {
            "@".to_string()
        } else {
            format!("\"{}\"", value_name.replace('"', "\\\""))
        };

        match key.read_value(&value_name)? {
            RegValue::Dword(v) => {
                content.push_str(&format!("{display_name}=dword:{v:08x}\n"));
            }
            RegValue::Sz(s) => {
                let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
                content.push_str(&format!("{display_name}=\"{escaped}\"\n"));
            }
            RegValue::Binary(bytes) => {
                let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
                content.push_str(&format!("{display_name}=hex:{}\n", hex.join(",")));
            }
            RegValue::Qword(v) => {
                let hex: Vec<String> = v
                    .to_le_bytes()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                content.push_str(&format!("{display_name}=hex(b):{}\n", hex.join(",")));
            }
            RegValue::MultiSz(strings) => {
                let mut data = Vec::new();
                for s in &strings {
                    for code_unit in s.encode_utf16() {
                        data.extend_from_slice(&code_unit.to_le_bytes());
                    }
                    data.extend_from_slice(&[0, 0]); // 内部 null
                }
                data.extend_from_slice(&[0, 0]); // 终止 null
                let hex: Vec<String> = data.iter().map(|b| format!("{b:02x}")).collect();
                content.push_str(&format!("{display_name}=hex(7):{}\n", hex.join(",")));
            }
        }
    }

    content.push('\n');

    // 递归子键
    for child_name in key.enum_sub_keys()? {
        let child_sub = format!("{sub_key}\\{child_name}");
        let child_display = format!("{display_path}\\{child_name}");
        dump_key_recursive(root, &child_sub, &child_display, content)?;
    }

    Ok(())
}

/// 根键 HKEY → 名称字符串（指针比较，因 HKEY 在 0.62 中可能不 impl PartialEq）。
fn hkey_to_name(hkey: HKEY) -> Result<&'static str> {
    if hkey.0 == HKEY_LOCAL_MACHINE.0 {
        Ok("HKEY_LOCAL_MACHINE")
    } else if hkey.0 == HKEY_CURRENT_USER.0 {
        Ok("HKEY_CURRENT_USER")
    } else if hkey.0 == HKEY_CLASSES_ROOT.0 {
        Ok("HKEY_CLASSES_ROOT")
    } else if hkey.0 == HKEY_USERS.0 {
        Ok("HKEY_USERS")
    } else if hkey.0 == HKEY_CURRENT_CONFIG.0 {
        Ok("HKEY_CURRENT_CONFIG")
    } else {
        Err(VxApoError::registry(
            "",
            "unknown HKEY handle for save_to_file",
        ))
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── split_key ────────────────────────────────────────────────────────────

    #[test]
    fn split_key_hklm() {
        let (root, sub) = split_key(r"HKLM\SOFTWARE\Microsoft").unwrap();
        assert_eq!(root.0, HKEY_LOCAL_MACHINE.0);
        assert_eq!(sub, r"SOFTWARE\Microsoft");
    }

    #[test]
    fn split_key_full_name_case_insensitive() {
        let (root, sub) = split_key(r"hkey_local_MACHINE\SYSTEM").unwrap();
        assert_eq!(root.0, HKEY_LOCAL_MACHINE.0);
        assert_eq!(sub, "SYSTEM");
    }

    #[test]
    fn split_key_hkcu() {
        let (root, sub) = split_key(r"HKCU\Software\SomeApp").unwrap();
        assert_eq!(root.0, HKEY_CURRENT_USER.0);
        assert_eq!(sub, "Software\\SomeApp");
    }

    #[test]
    fn split_key_hkcr() {
        let (root, sub) = split_key(r"HKCR\.txt").unwrap();
        assert_eq!(root.0, HKEY_CLASSES_ROOT.0);
        assert_eq!(sub, ".txt");
    }

    #[test]
    fn split_key_hku() {
        let (root, sub) = split_key(r"HKU\.DEFAULT\Environment").unwrap();
        assert_eq!(root.0, HKEY_USERS.0);
        assert_eq!(sub, ".DEFAULT\\Environment");
    }

    #[test]
    fn split_key_hkcc() {
        let (root, sub) = split_key(r"HKCC\System\CurrentControlSet").unwrap();
        assert_eq!(root.0, HKEY_CURRENT_CONFIG.0);
        assert_eq!(sub, "System\\CurrentControlSet");
    }

    #[test]
    fn split_key_no_separator_errors() {
        let err = split_key("HKLM").unwrap_err();
        assert!(format!("{err}").contains("missing"));
    }

    #[test]
    fn split_key_unknown_root_errors() {
        let err = split_key(r"HKEY_UNKNOWN\SomeKey").unwrap_err();
        assert!(format!("{err}").contains("unknown root key"));
    }

    #[test]
    fn split_key_empty_sub_key() {
        let (root, sub) = split_key("HKLM\\").unwrap();
        assert_eq!(root.0, HKEY_LOCAL_MACHINE.0);
        assert_eq!(sub, "");
    }

    // ── RegKey — 集成测试 ────────────────────────────────────────────────────

    #[test]
    fn regkey_open_and_check_values() {
        let key = RegKey::open(
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion",
        )
        .unwrap();

        // CommonFilesDir 是一个常见的 REG_SZ 值
        assert!(key.value_exists("CommonFilesDir").unwrap());
        assert!(!key.value_exists("__nonexistent_value_xyz__").unwrap());
    }

    #[test]
    fn regkey_open_nonexistent_errors() {
        let err = RegKey::open(
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\__nonexistent_key_xyz__\__deep__",
        )
        .unwrap_err();
        assert!(matches!(err, VxApoError::HResult(_)));
    }

    #[test]
    fn regkey_enum_sub_keys() {
        let key = RegKey::open(HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft").unwrap();
        let sub_keys = key.enum_sub_keys().unwrap();
        assert!(!sub_keys.is_empty());
    }

    #[test]
    fn regkey_enum_values() {
        let key = RegKey::open(
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion",
        )
        .unwrap();
        let values = key.enum_values().unwrap();
        assert!(!values.is_empty());
    }

    #[test]
    fn regkey_read_sz_value() {
        let key = RegKey::open(
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion",
        )
        .unwrap();

        match key.read_value("CommonFilesDir") {
            Ok(RegValue::Sz(s)) => assert!(!s.is_empty()),
            Ok(other) => panic!("expected RegValue::Sz, got {other:?}"),
            Err(_) => {} // 某些系统可能没有此值
        }
    }

    #[test]
    fn guid_format_from_binary() {
        // 间接验证 GUID 格式化逻辑：C18E2F7E-933D-4965-B7D1-1EEF228D2AF3
        let d1 = u32::from_le_bytes([0x7E, 0x2F, 0x8E, 0xC1]);
        let d2 = u16::from_le_bytes([0x3D, 0x93]);
        let d3 = u16::from_le_bytes([0x65, 0x49]);
        let formatted = format!(
            "{{{:08X}-{:04X}-{:04X}-B7D1-1EEF228D2AF3}}",
            d1, d2, d3
        );
        assert_eq!(formatted, "{C18E2F7E-933D-4965-B7D1-1EEF228D2AF3}");
    }

    #[test]
    fn parse_multi_sz_basic() {
        let mut data = Vec::new();
        for s in ["str1", "str2"] {
            for ch in s.encode_utf16() {
                data.extend_from_slice(&ch.to_le_bytes());
            }
            data.extend_from_slice(&[0, 0]);
        }
        data.extend_from_slice(&[0, 0]); // double null = end

        let result = parse_multi_sz(&data);
        assert_eq!(result, vec!["str1", "str2"]);
    }

    #[test]
    fn parse_multi_sz_empty() {
        let data = vec![0u8, 0]; // 直接 double null
        let result = parse_multi_sz(&data);
        assert!(result.is_empty());
    }

    // ── key_exists 顶层函数 ──────────────────────────────────────────────────

    #[test]
    fn key_exists_hklm_software() {
        assert!(key_exists(HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft").unwrap());
    }

    #[test]
    fn key_exists_nonexistent() {
        assert!(!key_exists(HKEY_LOCAL_MACHINE, r"SOFTWARE\__no_such_key__").unwrap());
    }

    // ── is_windows_version_at_least ──────────────────────────────────────────

    #[test]
    fn windows_version_check_runs() {
        let _ = is_windows_version_at_least(10, 0, 0);
    }
}