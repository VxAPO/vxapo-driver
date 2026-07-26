//! com/clsid_reg.rs — CLSID 注册逻辑（Note 4/5/29/30）
//!
//! 定义 COM 类在 Windows 注册表中的注册路径与键值。
//! `DllRegisterServer` 负责写入这些条目，`DllUnregisterServer` 负责删除。
//!
//! 注册表结构：
//! ```text
//! HKCR\CLSID\{GUID}\InprocServer32
//!     (Default) = "C:\...\vxapo.dll"
//!     ThreadingModel = "Both"
//! ```
//!
//! 此模块仅提供路径与常量定义，不执行实际注册表写入操作。

use crate::host::instance::reg_props::{CLSID_VXAPO_PRE_MIX, CLSID_VXAPO_POST_MIX};

// ══════════════════════════════════════════════════════════════════════════════
// GUID → 字符串格式化
// ══════════════════════════════════════════════════════════════════════════════

/// 将 GUID 格式化为注册表路径中使用的 `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}` 形式。
const fn hex(b: u8) -> u8 {
    if b < 10 { b + b'0' } else { b - 10 + b'a' }
}

pub const fn guid_to_string(guid: &windows::core::GUID) -> [u8; 38] {
    let d1 = guid.data1;
    let d2 = guid.data2;
    let d3 = guid.data3;
    let d4 = &guid.data4;

    let mut buf = [0u8; 38];
    buf[0] = b'{';
    buf[1] = hex(((d1 >> 28) & 0x0F) as u8);
    buf[2] = hex(((d1 >> 24) & 0x0F) as u8);
    buf[3] = hex(((d1 >> 20) & 0x0F) as u8);
    buf[4] = hex(((d1 >> 16) & 0x0F) as u8);
    buf[5] = hex(((d1 >> 12) & 0x0F) as u8);
    buf[6] = hex(((d1 >> 8) & 0x0F) as u8);
    buf[7] = hex(((d1 >> 4) & 0x0F) as u8);
    buf[8] = hex((d1 & 0x0F) as u8);
    buf[9] = b'-';
    buf[10] = hex(((d2 >> 12) & 0x0F) as u8);
    buf[11] = hex(((d2 >> 8) & 0x0F) as u8);
    buf[12] = hex(((d2 >> 4) & 0x0F) as u8);
    buf[13] = hex((d2 & 0x0F) as u8);
    buf[14] = b'-';
    buf[15] = hex(((d3 >> 12) & 0x0F) as u8);
    buf[16] = hex(((d3 >> 8) & 0x0F) as u8);
    buf[17] = hex(((d3 >> 4) & 0x0F) as u8);
    buf[18] = hex((d3 & 0x0F) as u8);
    buf[19] = b'-';
    buf[20] = hex((d4[0] >> 4) & 0x0F);
    buf[21] = hex(d4[0] & 0x0F);
    buf[22] = hex((d4[1] >> 4) & 0x0F);
    buf[23] = hex(d4[1] & 0x0F);
    buf[24] = b'-';
    buf[25] = hex((d4[2] >> 4) & 0x0F);
    buf[26] = hex(d4[2] & 0x0F);
    buf[27] = hex((d4[3] >> 4) & 0x0F);
    buf[28] = hex(d4[3] & 0x0F);
    buf[29] = hex((d4[4] >> 4) & 0x0F);
    buf[30] = hex(d4[4] & 0x0F);
    buf[31] = hex((d4[5] >> 4) & 0x0F);
    buf[32] = hex(d4[5] & 0x0F);
    buf[33] = hex((d4[6] >> 4) & 0x0F);
    buf[34] = hex(d4[6] & 0x0F);
    buf[35] = hex((d4[7] >> 4) & 0x0F);
    buf[36] = hex(d4[7] & 0x0F);
    buf[37] = b'}';
    buf
}

/// 将 `guid_to_string` 的结果转为 `&str`。
///
/// # Safety
///
/// `guid_to_string` 仅产生 ASCII 字符，因此结果始终是合法 UTF-8。
pub fn guid_str(guid: &windows::core::GUID) -> String {
    let bytes = guid_to_string(guid);
    // SAFETY: guid_to_string 仅输出 ASCII hex digits、'{'、'}'、'-'
    unsafe { String::from_utf8_unchecked(bytes.to_vec()) }
}

// ══════════════════════════════════════════════════════════════════════════════
// 注册表路径常量
// ══════════════════════════════════════════════════════════════════════════════

/// HKCR\CLSID 根路径前缀。
const CLSID_ROOT: &str = r"CLSID";

/// InprocServer32 子键名。
const INPROC_SERVER: &str = "InprocServer32";

/// ThreadingModel 值名。
const THREADING_MODEL_VALUE: &str = "ThreadingModel";

/// ThreadingModel 值（Note 5：必须为 "Both"）。
const THREADING_MODEL_BOTH: &str = "Both";

// ══════════════════════════════════════════════════════════════════════════════
// ClsidEntry — 待注册的 CLSID 条目
// ══════════════════════════════════════════════════════════════════════════════

/// 单个 CLSID 的注册信息。
#[derive(Debug)]
pub struct ClsidEntry {
    /// CLSID GUID
    pub clsid: windows::core::GUID,
    /// 格式化后的 CLSID 字符串 `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}`
    pub clsid_str: String,
}

impl ClsidEntry {
    /// 创建新的 CLSID 注册条目。
    pub fn new(clsid: windows::core::GUID) -> Self {
        Self {
            clsid,
            clsid_str: guid_str(&clsid),
        }
    }

    /// 返回 HKCR\CLSID\{GUID} 路径。
    pub fn clsid_key_path(&self) -> String {
        format!("{CLSID_ROOT}\\{}", self.clsid_str)
    }

    /// 返回 HKCR\CLSID\{GUID}\InprocServer32 路径。
    pub fn inproc_server_path(&self) -> String {
        format!("{CLSID_ROOT}\\{}\\{INPROC_SERVER}", self.clsid_str)
    }

    /// 返回注册所需的值列表：(路径, 值名, 值内容)。
    ///
    /// `dll_path` 为 DLL 的绝对路径（由 `DllRegisterServer` 传入）。
    pub fn registration_entries(&self, dll_path: &str) -> Vec<(&str, String, String)> {
        vec![
            ("Default", String::new(), dll_path.to_owned()),
            ("ThreadingModel", THREADING_MODEL_VALUE.to_owned(), THREADING_MODEL_BOTH.to_owned()),
        ]
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 所有 CLSID 条目
// ══════════════════════════════════════════════════════════════════════════════

/// 返回所有需要注册的 CLSID 条目。
///
/// 注册顺序：PostMix → PreMix（Note 29：先 PostMix，失败回滚 PostMix，再 PreMix）。
/// 注销顺序相反：PreMix → PostMix（Note 30）。
pub fn all_entries() -> Vec<ClsidEntry> {
    vec![
        ClsidEntry::new(CLSID_VXAPO_PRE_MIX),
        ClsidEntry::new(CLSID_VXAPO_POST_MIX),
    ]
}

/// 返回注册顺序的条目（PostMix 先）。
pub fn registration_order() -> Vec<ClsidEntry> {
    vec![
        ClsidEntry::new(CLSID_VXAPO_POST_MIX),
        ClsidEntry::new(CLSID_VXAPO_PRE_MIX),
    ]
}

/// 返回注销顺序的条目（PreMix 先）。
pub fn unregistration_order() -> Vec<ClsidEntry> {
    vec![
        ClsidEntry::new(CLSID_VXAPO_PRE_MIX),
        ClsidEntry::new(CLSID_VXAPO_POST_MIX),
    ]
}

// ══════════════════════════════════════════════════════════════════════════════
// 编译期断言
// ══════════════════════════════════════════════════════════════════════════════

const _: () = {
    // GUID 字符串长度固定 38 字节
    assert!(guid_to_string(&CLSID_VXAPO_PRE_MIX).len() == 38);
    assert!(guid_to_string(&CLSID_VXAPO_POST_MIX).len() == 38);
    // 第一个和最后一个字符
    assert!(guid_to_string(&CLSID_VXAPO_PRE_MIX)[0] == b'{');
    assert!(guid_to_string(&CLSID_VXAPO_PRE_MIX)[37] == b'}');
    // 位置 9, 14, 19, 24 是 '-'
    assert!(guid_to_string(&CLSID_VXAPO_PRE_MIX)[9] == b'-');
    assert!(guid_to_string(&CLSID_VXAPO_PRE_MIX)[14] == b'-');
    assert!(guid_to_string(&CLSID_VXAPO_PRE_MIX)[19] == b'-');
    assert!(guid_to_string(&CLSID_VXAPO_PRE_MIX)[24] == b'-');
};

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use windows::core::GUID;

    #[test]
    fn guid_to_string_format() {
        // C18E2F7E-933D-4965-B7D1-1EEF228D2AF3
        let guid = GUID::from_values(
            0xC18E2F7E,
            0x933D,
            0x4965,
            [0xB7, 0xD1, 0x1E, 0xEF, 0x22, 0x8D, 0x2A, 0xF3],
        );
        let s = guid_str(&guid);
        assert_eq!(s, "{c18e2f7e-933d-4965-b7d1-1eef228d2af3}");
    }

    #[test]
    fn guid_to_string_length() {
        let s = guid_str(&CLSID_VXAPO_PRE_MIX);
        assert_eq!(s.len(), 38);
        assert!(s.starts_with('{'));
        assert!(s.ends_with('}'));
    }

    #[test]
    fn guid_to_string_separators() {
        let s = guid_str(&CLSID_VXAPO_PRE_MIX);
        assert_eq!(s.as_bytes()[9], b'-');
        assert_eq!(s.as_bytes()[14], b'-');
        assert_eq!(s.as_bytes()[19], b'-');
        assert_eq!(s.as_bytes()[24], b'-');
    }

    #[test]
    fn guid_to_string_all_ascii_hex() {
        let s = guid_str(&CLSID_VXAPO_PRE_MIX);
        for (i, &c) in s.as_bytes().iter().enumerate() {
            if i == 0 || i == 37 || [9, 14, 19, 24].contains(&i) {
                continue; // '{', '}', '-'
            }
            assert!(
                (b'0'..=b'9').contains(&c) || (b'a'..=b'f').contains(&c),
                "non-hex char at position {i}: {c}"
            );
        }
    }

    #[test]
    fn pre_mix_entry_paths() {
        let entry = ClsidEntry::new(CLSID_VXAPO_PRE_MIX);
        let clsid_str = entry.clsid_str.clone();
        assert!(entry.clsid_key_path().starts_with("CLSID\\"));
        assert!(entry.clsid_key_path().ends_with(&clsid_str));
        assert!(entry.inproc_server_path().contains("InprocServer32"));
    }

    #[test]
    fn post_mix_entry_paths() {
        let entry = ClsidEntry::new(CLSID_VXAPO_POST_MIX);
        assert!(entry.inproc_server_path().contains("InprocServer32"));
        assert!(entry.inproc_server_path().contains("CLSID"));
    }

    #[test]
    fn registration_entries_contain_dll_path() {
        let entry = ClsidEntry::new(CLSID_VXAPO_PRE_MIX);
        let entries = entry.registration_entries(r"C:\Windows\System32\vxapo.dll");
        assert!(!entries.is_empty());
        // 第一个条目是 Default 值 = DLL 路径
        assert_eq!(entries[0].2, r"C:\Windows\System32\vxapo.dll");
    }

    #[test]
    fn registration_entries_contain_threading_model() {
        let entry = ClsidEntry::new(CLSID_VXAPO_PRE_MIX);
        let entries = entry.registration_entries("test.dll");
        let tm = entries.iter().find(|(name, _, _)| *name == "ThreadingModel");
        assert!(tm.is_some());
        assert_eq!(tm.unwrap().2, "Both");
    }

    #[test]
    fn registration_order_is_postmix_first() {
        let order = registration_order();
        assert_eq!(order.len(), 2);
        assert_eq!(order[0].clsid, CLSID_VXAPO_POST_MIX);
        assert_eq!(order[1].clsid, CLSID_VXAPO_PRE_MIX);
    }

    #[test]
    fn unregistration_order_is_premix_first() {
        let order = unregistration_order();
        assert_eq!(order.len(), 2);
        assert_eq!(order[0].clsid, CLSID_VXAPO_PRE_MIX);
        assert_eq!(order[1].clsid, CLSID_VXAPO_POST_MIX);
    }

    #[test]
    fn all_entries_count() {
        assert_eq!(all_entries().len(), 2);
    }

    #[test]
    fn clsid_strings_are_distinct() {
        let entries = all_entries();
        assert_ne!(entries[0].clsid_str, entries[1].clsid_str);
    }

    #[test]
    fn inproc_server_path_ends_with_subkey() {
        let entry = ClsidEntry::new(CLSID_VXAPO_PRE_MIX);
        assert!(entry.inproc_server_path().ends_with("InprocServer32"));
    }

    #[test]
    fn clsid_key_path_is_root_plus_guid() {
        let entry = ClsidEntry::new(CLSID_VXAPO_PRE_MIX);
        let path = entry.clsid_key_path();
        let expected = format!("CLSID\\{}", entry.clsid_str);
        assert_eq!(path, expected);
    }
}