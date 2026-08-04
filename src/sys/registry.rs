//! sys/registry.rs — 注册表模块（v6.3 规范 3.4，按 windows-rs 0.62.2 真实 API）

use windows::core::{HSTRING, PCWSTR, Result};
use windows::Win32::Foundation::WIN32_ERROR;
use windows::Win32::System::Registry::{
    HKEY, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegEnumKeyExW,
    RegEnumValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY_CLASSES_ROOT,
    HKEY_CURRENT_CONFIG, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, HKEY_USERS, REG_BINARY, REG_DWORD,
    REG_MULTI_SZ, REG_OPEN_CREATE_OPTIONS, REG_QWORD, REG_SAM_FLAGS, REG_SZ, REG_VALUE_TYPE,
};

const SAM_READ: REG_SAM_FLAGS = REG_SAM_FLAGS(0x0002_0019); // KEY_READ = STANDARD_RIGHTS_READ | KEY_QUERY_VALUE | KEY_ENUMERATE_SUB_KEYS | KEY_NOTIFY
const SAM_ALL: REG_SAM_FLAGS = REG_SAM_FLAGS(0x000F_003F); // KEY_ALL_ACCESS
// KEY_SET_VALUE | KEY_QUERY_VALUE（写值/删值，不含 KEY_CREATE_SUB_KEY）。
// MMDevices 端点 FxProperties 键 ACL 只给 Administrators SetValue,ReadKey——
// 请求 KEY_ALL_ACCESS（含 CreateSubKey 位）会超权限被 RegCreateKeyExW 拒绝（0x80070005）。
const SAM_SET_VALUE: REG_SAM_FLAGS = REG_SAM_FLAGS(0x0000_0002 | 0x0000_0001);

fn win32_ok(err: WIN32_ERROR) -> Result<()> {
    if err.0 == 0 {
        Ok(())
    } else {
        Err(windows::core::Error::from_hresult(windows::core::HRESULT(
            (0x8007_0000u32 | (err.0 & 0xFFFF)) as i32,
        )))
    }
}

/// 注册表值类型化表示。
#[derive(Debug, Clone, PartialEq)]
pub enum RegValue {
    Sz(String),
    Dword(u32),
    Qword(u64),
    Binary(Vec<u8>),
    MultiSz(Vec<String>),
}

/// 注册表键句柄（RAII）。
#[derive(Debug)]
pub struct RegKey {
    handle: HKEY,
}

unsafe impl Send for RegKey {}
unsafe impl Sync for RegKey {}

impl Drop for RegKey {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.handle);
        }
    }
}

impl RegKey {
    /// 以 KEY_READ 打开子键。
    pub fn open(root: HKEY, sub_key: &str) -> Result<Self> {
        let sub_key = HSTRING::from(sub_key);
        let mut handle = HKEY::default();
        let err = unsafe { RegOpenKeyExW(root, &sub_key, None, SAM_READ, &mut handle) };
        win32_ok(err)?;
        Ok(Self { handle })
    }

    /// 创建或打开子键（KEY_ALL_ACCESS）。
    ///
    /// **MMDevices 注意**：Windows 对 `MMDevices\...\Properties` 键的 ACL 仅授予
    /// Administrators `SetValue,ReadKey`（无 CreateSubKey）——此场景请求 KEY_ALL_ACCESS
    /// 会因含 KEY_CREATE_SUB_KEY 位而拒绝访问（0x80070005）。已存在的键请用
    /// [`Self::open_for_write`]（仅 KEY_SET_VALUE|KEY_QUERY_VALUE），新建键才用 create。
    pub fn create(root: HKEY, sub_key: &str) -> Result<Self> {
        let sub_key = HSTRING::from(sub_key);
        let mut handle = HKEY::default();
        let opts = REG_OPEN_CREATE_OPTIONS(0); // REG_OPTION_NON_VOLATILE
        let err = unsafe {
            RegCreateKeyExW(
                root,
                &sub_key,
                None,
                PCWSTR::null(),
                opts,
                SAM_ALL,
                None,
                &mut handle,
                None,
            )
        };
        win32_ok(err)?;
        Ok(Self { handle })
    }

    /// 以 KEY_SET_VALUE|KEY_QUERY_VALUE 打开已存在子键（写值/删值用）。
    ///
    /// MMDevices 端点键 ACL 只给 Administrators SetValue/ReadKey——请求含 CreateSubKey
    /// 的 KEY_ALL_ACCESS 会被拒。只请求写值所需的最小权限即可成功。
    pub fn open_for_write(root: HKEY, sub_key: &str) -> Result<Self> {
        let sub_key = HSTRING::from(sub_key);
        let mut handle = HKEY::default();
        let err = unsafe { RegOpenKeyExW(root, &sub_key, None, SAM_SET_VALUE, &mut handle) };
        win32_ok(err)?;
        Ok(Self { handle })
    }

    /// 打开子键（委托 open）。
    pub fn open_sub_key(&self, sub_key: &str) -> Result<Self> {
        Self::open(self.handle, sub_key)
    }

    /// 返回底层句柄。
    pub fn handle(&self) -> HKEY {
        self.handle
    }

    /// 自动识别类型读取。
    pub fn read_value(&self, name: &str) -> Result<RegValue> {
        let name = HSTRING::from(name);
        let mut value_type = REG_VALUE_TYPE(0);
        let mut size = 0u32;
        // 第一次查询获取类型和大小
        let err = unsafe {
            RegQueryValueExW(
                self.handle,
                &name,
                None,
                Some(&mut value_type),
                None,
                Some(&mut size),
            )
        };
        win32_ok(err)?;

        let mut buf: Vec<u8> = vec![0u8; size as usize];
        let err = unsafe {
            RegQueryValueExW(
                self.handle,
                &name,
                None,
                Some(&mut value_type),
                Some(buf.as_mut_ptr()),
                Some(&mut size),
            )
        };
        win32_ok(err)?;
        buf.truncate(size as usize);

        let value = match value_type {
            t if t == REG_SZ || t == REG_VALUE_TYPE(0x0002 /* REG_EXPAND_SZ */) => {
                RegValue::Sz(utf16_bytes_to_string(&buf))
            }
            t if t == REG_DWORD => {
                if buf.len() >= 4 {
                    RegValue::Dword(u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]))
                } else {
                    RegValue::Dword(0)
                }
            }
            t if t == REG_QWORD => {
                if buf.len() >= 8 {
                    RegValue::Qword(u64::from_le_bytes([
                        buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
                    ]))
                } else {
                    RegValue::Qword(0)
                }
            }
            t if t == REG_BINARY => RegValue::Binary(buf),
            t if t == REG_MULTI_SZ => RegValue::MultiSz(parse_multi_sz(&buf)),
            _ => RegValue::Binary(buf),
        };
        Ok(value)
    }

    /// 读取 REG_SZ。
    pub fn read_sz_value(&self, name: &str) -> Result<String> {
        match self.read_value(name)? {
            RegValue::Sz(s) => Ok(s),
            _ => Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                0x8000_000Du32 as i32,
            ))),
        }
    }

    /// read_sz_value 的 Option 包装。
    pub fn read_sz(&self, name: &str) -> Option<String> {
        self.read_sz_value(name).ok()
    }

    /// 读取 REG_DWORD。
    pub fn read_dword_value(&self, name: &str) -> Result<u32> {
        match self.read_value(name)? {
            RegValue::Dword(d) => Ok(d),
            _ => Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                0x8000_000Du32 as i32,
            ))),
        }
    }

    /// 读取 REG_BINARY。
    pub fn read_binary_value(&self, name: &str) -> Result<Vec<u8>> {
        match self.read_value(name)? {
            RegValue::Binary(b) => Ok(b),
            _ => Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                0x8000_000Du32 as i32,
            ))),
        }
    }

    /// 读取 REG_MULTI_SZ。
    pub fn read_multi_value(&self, name: &str) -> Result<Vec<String>> {
        match self.read_value(name)? {
            RegValue::MultiSz(v) => Ok(v),
            _ => Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                0x8000_000Du32 as i32,
            ))),
        }
    }

    /// 检查值是否存在。
    pub fn value_exists(&self, name: &str) -> Result<bool> {
        let name = HSTRING::from(name);
        let err = unsafe { RegQueryValueExW(self.handle, &name, None, None, None, None) };
        Ok(err.0 == 0)
    }

    /// 检查当前键下指定子键是否存在。
    pub fn key_exists_child(&self, sub_key: &str) -> Result<bool> {
        Ok(Self::open(self.handle, sub_key).is_ok())
    }

    /// 枚举所有子键名称。
    ///
    /// **设备枚举的底层能力源**（供 `install/device/info::enumerate_devices` 遍历 MMDevices 子键）。
    pub fn enum_sub_keys(&self) -> Result<Vec<String>> {
        use windows::core::PWSTR;

        let mut names = Vec::new();
        let mut index = 0u32;
        loop {
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            let err = unsafe {
                RegEnumKeyExW(
                    self.handle,
                    index,
                    Some(PWSTR(buf.as_mut_ptr())),
                    &mut len,
                    None,
                    None,
                    None,
                    None,
                )
            };
            if err.0 == 0 {
                names.push(String::from_utf16_lossy(&buf[..len as usize]));
                index += 1;
            } else if is_not_found(err) || err.0 == 259 /* ERROR_NO_MORE_ITEMS */ {
                break;
            } else {
                win32_ok(err)?;
            }
        }
        Ok(names)
    }

    /// 枚举所有值名称（含默认值 `""`）。
    pub fn enum_values(&self) -> Result<Vec<String>> {
        use windows::core::PWSTR;

        let mut names = Vec::new();
        let mut index = 0u32;
        loop {
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            let err = unsafe {
                RegEnumValueW(
                    self.handle,
                    index,
                    Some(PWSTR(buf.as_mut_ptr())),
                    &mut len,
                    None,
                    None,
                    None,
                    None,
                )
            };
            if err.0 == 0 {
                names.push(String::from_utf16_lossy(&buf[..len as usize]));
                index += 1;
            } else if is_not_found(err) || err.0 == 259 /* ERROR_NO_MORE_ITEMS */ {
                break;
            } else {
                win32_ok(err)?;
            }
        }
        Ok(names)
    }

    /// 读取 GUID，支持 REG_BINARY（16 字节 LE）和 REG_SZ。
    pub fn get_guid_string(&self, name: &str) -> Result<String> {
        let value = self.read_value(name)?;
        match value {
            RegValue::Binary(b) if b.len() >= 16 => {
                let guid = windows::core::GUID {
                    data1: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                    data2: u16::from_le_bytes([b[4], b[5]]),
                    data3: u16::from_le_bytes([b[6], b[7]]),
                    data4: [
                        b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15],
                    ],
                };
                Ok(crate::sys::com::prelude::guid_to_string(&guid))
            }
            RegValue::Sz(s) => Ok(s),
            _ => Err(windows::core::Error::from_hresult(
                windows::core::HRESULT(0x8000_000Du32 as i32),
            )),
        }
    }

    /// 写入 REG_SZ。
    pub fn write_sz(&self, name: &str, value: &str) -> Result<()> {
        let name = HSTRING::from(name);
        let mut bytes: Vec<u8> = value
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        bytes.extend_from_slice(&[0, 0]); // null terminator
        let err = unsafe {
            RegSetValueExW(self.handle, &name, None, REG_SZ, Some(&bytes))
        };
        win32_ok(err)
    }

    /// 写入 REG_DWORD。
    pub fn write_dword(&self, name: &str, value: u32) -> Result<()> {
        let name = HSTRING::from(name);
        let data = value.to_le_bytes();
        let err = unsafe {
            RegSetValueExW(
                self.handle,
                &name,
                None,
                REG_DWORD,
                Some(&data),
            )
        };
        win32_ok(err)
    }

    /// 写入 REG_BINARY。
    pub fn write_binary(&self, name: &str, data: &[u8]) -> Result<()> {
        let name = HSTRING::from(name);
        let err = unsafe {
            RegSetValueExW(
                self.handle,
                &name,
                None,
                REG_BINARY,
                Some(data),
            )
        };
        win32_ok(err)
    }

    /// 写入 REG_MULTI_SZ（多字符串，双 null 结束）。
    ///
    /// APO 处理模式注册（EAPO DeviceAPOInfo.cpp 74-77/603-638）：
    /// `{d3993a3f-...},{PID}` 槽位的 ProcessingModes 值是 REG_MULTI_SZ，
    /// 值 = 处理模式 GUID（如 AUDIO_SIGNALPROCESSINGMODE_DEFAULT）——Windows
    /// 音频引擎按此值判定「该槽位 APO 参与哪个处理模式」，缺了它父槽位 APO 不被加载。
    pub fn write_multi_value(&self, name: &str, values: &[String]) -> Result<()> {
        let name = HSTRING::from(name);
        // REG_MULTI_SZ 布局：各字符串 UTF-16LE + \0，最后双 \0 结束。
        let mut bytes: Vec<u8> = Vec::new();
        for v in values {
            for u in v.encode_utf16() {
                bytes.extend_from_slice(&u.to_le_bytes());
            }
            bytes.extend_from_slice(&[0, 0]); // 每项 null 终止
        }
        bytes.extend_from_slice(&[0, 0]); // 列表结束（双 null）
        let err = unsafe {
            RegSetValueExW(
                self.handle,
                &name,
                None,
                REG_MULTI_SZ,
                Some(&bytes),
            )
        };
        win32_ok(err)
    }

    /// 删除值（幂等）。
    pub fn delete_value(&self, name: &str) -> Result<()> {
        let name = HSTRING::from(name);
        let err = unsafe { RegDeleteValueW(self.handle, &name) };
        if err.0 == 0 || is_not_found(err) {
            Ok(())
        } else {
            win32_ok(err)
        }
    }

    /// 递归删除子键（幂等）。
    pub fn delete_sub_key(&self, name: &str) -> Result<()> {
        let name = HSTRING::from(name);
        let err = unsafe { RegDeleteTreeW(self.handle, &name) };
        if err.0 == 0 || is_not_found(err) {
            Ok(())
        } else {
            win32_ok(err)
        }
    }
}

/// UTF-16LE 字节流 → String（遇 null 停止）。
fn utf16_bytes_to_string(buf: &[u8]) -> String {
    let mut units = Vec::with_capacity(buf.len() / 2);
    for chunk in buf.chunks_exact(2) {
        let u = u16::from_le_bytes([chunk[0], chunk[1]]);
        if u == 0 {
            break;
        }
        units.push(u);
    }
    String::from_utf16_lossy(&units)
}

/// UTF-16LE 字节流 → Vec<String>（双 null 结束）。
fn parse_multi_sz(buf: &[u8]) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = Vec::new();
    for chunk in buf.chunks_exact(2) {
        let u = u16::from_le_bytes([chunk[0], chunk[1]]);
        if u == 0 {
            if current.is_empty() {
                break;
            }
            result.push(String::from_utf16_lossy(&current));
            current.clear();
        } else {
            current.push(u);
        }
    }
    result
}

/// 判断错误码是否为 FILE_NOT_FOUND(2)/PATH_NOT_FOUND(3)。
fn is_not_found(err: WIN32_ERROR) -> bool {
    err.0 == 2 || err.0 == 3
}

/// 拆分路径为根键 + 子键。
pub fn split_key(path: &str) -> Result<(HKEY, &str)> {
    let (root_str, rest) = match path.split_once('\\') {
        Some((r, rest)) => (r.to_uppercase(), rest),
        None => (path.to_uppercase(), ""),
    };
    let root = match root_str.as_str() {
        "HKLM" | "HKEY_LOCAL_MACHINE" => HKEY_LOCAL_MACHINE,
        "HKCU" | "HKEY_CURRENT_USER" => HKEY_CURRENT_USER,
        "HKCR" | "HKEY_CLASSES_ROOT" => HKEY_CLASSES_ROOT,
        "HKU" | "HKEY_USERS" => HKEY_USERS,
        "HKCC" | "HKEY_CURRENT_CONFIG" => HKEY_CURRENT_CONFIG,
        _ => {
            return Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                0x8007_001Bu32 as i32,
            )))
        }
    };
    Ok((root, rest))
}

/// 检查注册表键是否存在。
pub fn key_exists(root: HKEY, sub_key: &str) -> Result<bool> {
    Ok(RegKey::open(root, sub_key).is_ok())
}

/// 检查注册表值是否存在。
pub fn value_exists(root: HKEY, sub_key: &str, name: &str) -> Result<bool> {
    let key = match RegKey::open(root, sub_key) {
        Ok(k) => k,
        Err(_) => return Ok(false),
    };
    key.value_exists(name)
}

/// 递归删除子树（幂等，不需要已打开的句柄）。
pub fn delete_tree(root: HKEY, sub_key: &str) -> Result<()> {
    let sub_key = HSTRING::from(sub_key);
    let err = unsafe { RegDeleteTreeW(root, &sub_key) };
    if err.0 == 0 || is_not_found(err) {
        Ok(())
    } else {
        win32_ok(err)
    }
}

/// 读取 `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion` 检查 Windows 版本。
pub fn is_windows_version_at_least(major: u32, minor: u32, build: u32) -> Result<bool> {
    let key = RegKey::open(
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
    )?;
    let cur_major = key
        .read_dword_value("CurrentMajorVersionNumber")
        .unwrap_or(0);
    let cur_minor = key
        .read_dword_value("CurrentMinorVersionNumber")
        .unwrap_or(0);
    let cur_build = key
        .read_sz_value("CurrentBuildNumber")
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);
    let cur = (cur_major, cur_minor, cur_build);
    let target = (major, minor, build);
    Ok(cur >= target)
}

/// 递归导出注册表键为 `.reg` 文件（UTF-16LE with BOM），用于安装前备份。
pub fn save_to_file(root: HKEY, sub_key: &str, path: &str) -> Result<()> {
    let key = RegKey::open(root, sub_key)?;
    let mut content = String::new();
    content.push_str("Windows Registry Editor Version 5.00\r\n\r\n");
    dump_key_recursive(&key, "", sub_key, &mut content)?;
    let mut bytes = vec![0xFF, 0xFE]; // UTF-16LE BOM
    let mut utf16: Vec<u8> = content
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    bytes.append(&mut utf16);
    std::fs::write(path, bytes).map_err(|e| {
        let code = e.raw_os_error().unwrap_or(5) as u32 & 0xFFFF;
        windows::core::Error::from_hresult(windows::core::HRESULT(
            (0x8007_0000u32 | code) as i32,
        ))
    })?;
    Ok(())
}

/// 递归导出键及子键（`save_to_file` 内部）。
fn dump_key_recursive(
    key: &RegKey,
    display_path: &str,
    sub_key: &str,
    content: &mut String,
) -> Result<()> {
    let full_display = if display_path.is_empty() {
        sub_key.to_string()
    } else {
        format!("{}\\{}", display_path, sub_key)
    };

    content.push_str(&format!("[HKEY_LOCAL_MACHINE\\{}]\r\n", full_display));

    // 枚举本键所有值。
    let value_names = key.enum_values()?;
    for name in &value_names {
        match key.read_value(name) {
            Ok(value) => {
                let display_name = if name.is_empty() { "@" } else { name };
                match value {
                    RegValue::Sz(s) => content.push_str(&format!(
                        "\"{}\"=\"{}\"\r\n",
                        display_name,
                        s.replace('\\', "\\\\").replace('"', "\\\"")
                    )),
                    RegValue::Dword(d) => content.push_str(&format!(
                        "\"{}\"=dword:{:08x}\r\n",
                        display_name, d
                    )),
                    RegValue::Qword(q) => content.push_str(&format!(
                        "\"{}\"=hex(b):{},{}\r\n",
                        display_name,
                        (q & 0xFF) as u8,
                        ((q >> 8) & 0xFF) as u8
                    )),
                    RegValue::Binary(b) => {
                        let hex: Vec<String> = b.iter().map(|x| format!("{:02x}", x)).collect();
                        content.push_str(&format!(
                            "\"{}\"=hex:{}\r\n",
                            display_name,
                            hex.join(",")
                        ));
                    }
                    RegValue::MultiSz(v) => {
                        let items: Vec<String> = v
                            .iter()
                            .map(|s| s.replace('\\', "\\\\").replace('"', "\\\""))
                            .collect();
                        content.push_str(&format!(
                            "\"{}\"=hex(7):{}\\0\r\n",
                            display_name,
                            items.join(",00,")
                        ));
                    }
                }
            }
            Err(_) => continue,
        }
    }
    content.push('\n');

    // 递归子键。
    let sub_keys = key.enum_sub_keys()?;
    for child in &sub_keys {
        let child_key = key.open_sub_key(child)?;
        dump_key_recursive(&child_key, &full_display, child, content)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Registry::HKEY_CURRENT_USER;

    const TEST_ROOT: HKEY = HKEY_CURRENT_USER;
    const TEST_KEY: &str = r"SOFTWARE\VxAPO_Test_Registry";

    #[test]
    fn create_write_read_roundtrip() {
        let _ = RegKey::open(TEST_ROOT, TEST_KEY)
            .and_then(|k| k.delete_sub_key(TEST_KEY));
        let key = RegKey::create(TEST_ROOT, TEST_KEY).unwrap();

        key.write_sz("TestSz", "hello").unwrap();
        key.write_dword("TestDword", 42).unwrap();
        key.write_binary("TestBin", &[1, 2, 3]).unwrap();

        assert_eq!(key.read_sz("TestSz").unwrap(), "hello");
        assert_eq!(key.read_dword_value("TestDword").unwrap(), 42);
        assert_eq!(key.read_binary_value("TestBin").unwrap(), vec![1, 2, 3]);
        assert!(key.value_exists("TestSz").unwrap());

        key.delete_value("TestSz").unwrap();
        assert!(!key.value_exists("TestSz").unwrap());

        drop(key);
        let _ = RegKey::open(TEST_ROOT, TEST_KEY)
            .and_then(|k| k.delete_sub_key(TEST_KEY));
    }

    #[test]
    fn split_key_variants() {
        assert_eq!(split_key(r"HKLM\SOFTWARE").unwrap().0, HKEY_LOCAL_MACHINE);
        assert_eq!(split_key(r"HKCU\SOFTWARE").unwrap().0, HKEY_CURRENT_USER);
        assert!(split_key(r"BADROOT\X").is_err());
    }
}