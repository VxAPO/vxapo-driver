//! sys/registry.rs — 注册表模块（v6.3 规范 3.4，按 windows-rs 0.62.2 真实 API）

use windows::core::{HSTRING, PCWSTR, Result};
use windows::Win32::Foundation::WIN32_ERROR;
use windows::Win32::System::Registry::{
    HKEY, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegOpenKeyExW,
    RegQueryValueExW, RegSetValueExW, HKEY_CLASSES_ROOT, HKEY_CURRENT_CONFIG, HKEY_CURRENT_USER,
    HKEY_LOCAL_MACHINE, HKEY_USERS, REG_BINARY, REG_DWORD, REG_MULTI_SZ, REG_OPEN_CREATE_OPTIONS,
    REG_QWORD, REG_SAM_FLAGS, REG_SZ, REG_VALUE_TYPE,
};

const SAM_READ: REG_SAM_FLAGS = REG_SAM_FLAGS(0x0002_0019); // KEY_READ = STANDARD_RIGHTS_READ | KEY_QUERY_VALUE | KEY_ENUMERATE_SUB_KEYS | KEY_NOTIFY
const SAM_ALL: REG_SAM_FLAGS = REG_SAM_FLAGS(0x000F_003F); // KEY_ALL_ACCESS

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

    /// 检查值是否存在。
    pub fn value_exists(&self, name: &str) -> Result<bool> {
        let name = HSTRING::from(name);
        let err = unsafe { RegQueryValueExW(self.handle, &name, None, None, None, None) };
        Ok(err.0 == 0)
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