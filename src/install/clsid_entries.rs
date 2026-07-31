//! host/installation/clsid_entries.rs — CLSID 注册逻辑（Note 4/5/29/30）
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
//! 写入操作委托 `sys/registry/write.rs`，由 `host/installation/exports.rs` 调用。

use crate::host::instance::reg_props::{CLSID_VXAPO_PRE_MIX, CLSID_VXAPO_POST_MIX};
use crate::utils::guid::{format_guid, format_guid_bytes};

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
            clsid_str: format_guid(&clsid),
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
    assert!(format_guid_bytes(&CLSID_VXAPO_PRE_MIX).len() == 38);
    assert!(format_guid_bytes(&CLSID_VXAPO_POST_MIX).len() == 38);
    // 第一个和最后一个字符
    assert!(format_guid_bytes(&CLSID_VXAPO_PRE_MIX)[0] == b'{');
    assert!(format_guid_bytes(&CLSID_VXAPO_PRE_MIX)[37] == b'}');
    // 位置 9, 14, 19, 24 是 '-'
    assert!(format_guid_bytes(&CLSID_VXAPO_PRE_MIX)[9] == b'-');
    assert!(format_guid_bytes(&CLSID_VXAPO_PRE_MIX)[14] == b'-');
    assert!(format_guid_bytes(&CLSID_VXAPO_PRE_MIX)[19] == b'-');
    assert!(format_guid_bytes(&CLSID_VXAPO_PRE_MIX)[24] == b'-');
};

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_guid_bytes_length() {
        let s = format_guid(&CLSID_VXAPO_PRE_MIX);
        assert_eq!(s.len(), 38);
        assert!(s.starts_with('{'));
        assert!(s.ends_with('}'));
    }

    #[test]
    fn format_guid_bytes_separators() {
        let s = format_guid(&CLSID_VXAPO_PRE_MIX);
        assert_eq!(s.as_bytes()[9], b'-');
        assert_eq!(s.as_bytes()[14], b'-');
        assert_eq!(s.as_bytes()[19], b'-');
        assert_eq!(s.as_bytes()[24], b'-');
    }

    #[test]
    fn format_guid_bytes_all_ascii_hex() {
        let s = format_guid(&CLSID_VXAPO_PRE_MIX);
        for (i, &c) in s.as_bytes().iter().enumerate() {
            if i == 0 || i == 37 || [9, 14, 19, 24].contains(&i) {
                continue; // '{', '}', '-'
            }
            assert!(
                (b'0'..=b'9').contains(&c) || (b'A'..=b'F').contains(&c),  // a-f → A-F
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