//! utils/guid.rs — GUID 纯解析工具（无 I/O、无业务逻辑）
//!
//! 职责：提供 16 字节小端原始数据 → GUID、标准字符串 `{XXXXXXXX-...}` → GUID
//! 的纯函数转换。GUID → 字符串格式化仍由 `sys/com/prelude::guid_to_string`
//! （StringFromGUID2 安全收窄）负责，本模块只做反向解析。

use windows::core::GUID;

/// 16 字节小端（data1/data2/data3）+ data4 原始 → GUID。
pub fn guid_from_bytes(bytes: &[u8]) -> Option<GUID> {
    if bytes.len() < 16 {
        return None;
    }
    Some(GUID {
        data1: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        data2: u16::from_le_bytes([bytes[4], bytes[5]]),
        data3: u16::from_le_bytes([bytes[6], bytes[7]]),
        data4: [
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ],
    })
}

/// GUID 是否为全零（Windows「无 APO」占位）。
pub fn is_zero_guid(g: &GUID) -> bool {
    *g == GUID::zeroed()
}

/// 解析 `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}` 格式 GUID 字符串。
pub fn parse_guid_string(s: &str) -> Option<GUID> {
    let s = s.trim();
    if !s.starts_with('{') || !s.ends_with('}') {
        return None;
    }
    let inner = &s[1..s.len() - 1];
    // xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx = 8-4-4-4-12
    let parts: Vec<&str> = inner.split('-').collect();
    if parts.len() != 5 {
        return None;
    }
    let data1 = u32::from_str_radix(parts[0], 16).ok()?;
    let data2 = u16::from_str_radix(parts[1], 16).ok()?;
    let data3 = u16::from_str_radix(parts[2], 16).ok()?;
    if parts[3].len() != 4 || parts[4].len() != 12 {
        return None;
    }
    // data4 共 16 个 hex 字符 = 8 字节：parts[3](4 字符) + parts[4](12 字符)。
    // 每 2 个 hex 字符 = 1 字节；旧实现按每字符 1 字节写 data4[i+4] 越界
    // （data4 仅 [u8; 8]）——EAPO REG_SZ 真实 GUID 解析触发后 panic，已修正。
    let hex4 = format!("{}{}", parts[3], parts[4]);
    let mut data4 = [0u8; 8];
    for i in 0..8 {
        let hi = hex4.as_bytes()[i * 2];
        let lo = hex4.as_bytes()[i * 2 + 1];
        data4[i] = (hex_val(hi)? << 4) | hex_val(lo)?;
    }
    Some(GUID { data1, data2, data3, data4 })
}

/// 单个 ASCII hex 字符 → 数值（0-15）。
fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guid_from_bytes_roundtrip() {
        let guid = GUID::from_values(
            0x1234_5678,
            0x9ABC,
            0xDEF0,
            [0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF],
        );
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&guid.data1.to_le_bytes());
        bytes.extend_from_slice(&guid.data2.to_le_bytes());
        bytes.extend_from_slice(&guid.data3.to_le_bytes());
        bytes.extend_from_slice(&guid.data4);
        assert_eq!(guid_from_bytes(&bytes), Some(guid));
    }

    #[test]
    fn guid_from_bytes_zeroed() {
        let parsed = guid_from_bytes(&[0u8; 16]).unwrap();
        assert_eq!(parsed, GUID::zeroed());
    }

    #[test]
    fn guid_from_bytes_max() {
        let bytes = [0xFFu8; 16];
        let parsed = guid_from_bytes(&bytes).unwrap();
        assert_eq!(parsed.data1, u32::MAX);
        assert_eq!(parsed.data2, u16::MAX);
        assert_eq!(parsed.data3, u16::MAX);
        assert_eq!(parsed.data4, [0xFF; 8]);
    }

    #[test]
    fn guid_from_bytes_too_short() {
        assert!(guid_from_bytes(&[0u8; 15]).is_none());
    }

    #[test]
    fn parse_guid_string_valid() {
        let s = "{12345678-9ABC-DEF0-0123-456789ABCDEF}";
        let guid = parse_guid_string(s).unwrap();
        assert_eq!(guid.data1, 0x1234_5678);
        assert_eq!(guid.data2, 0x9ABC);
        assert_eq!(guid.data3, 0xDEF0);
        assert_eq!(guid.data4, [0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF]);
    }

    #[test]
    fn parse_guid_string_rejects_malformed() {
        assert!(parse_guid_string("12345678-9ABC-DEF0-0123-456789ABCDEF").is_none());
        assert!(parse_guid_string("{12345678-9ABC-DEF0-0123-456789ABCDE}").is_none());
        assert!(parse_guid_string("{}").is_none());
    }

    #[test]
    fn parse_guid_string_rejects_invalid_charset() {
        // 各段长度合法但含非 hex 字符 → None（不 panic）。
        assert!(parse_guid_string("{GGGGGGGG-9ABC-DEF0-0123-456789ABCDEF}").is_none());
        assert!(parse_guid_string("{12345678-9ABC-DEF0-0123-456789ABCDEZ}").is_none());
        assert!(parse_guid_string("{12345678-9ABC-DEF0-0123-45_789ABCDEF}").is_none());
    }
}
