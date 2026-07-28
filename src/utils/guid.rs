// src/utils/guid.rs

use windows::core::GUID;

/// GUID → `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`
pub fn format_guid(g: &GUID) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        g.data1, g.data2, g.data3,
        g.data4[0], g.data4[1],
        g.data4[2], g.data4[3], g.data4[4], g.data4[5], g.data4[6], g.data4[7],
    )
}

/// 将 GUID 格式化为固定 38 字节的 ASCII 缓冲区（const 兼容）。
///
/// 输出格式：`{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}`
/// 字母为大写。
pub const fn format_guid_bytes(g: &GUID) -> [u8; 38] {
    let d1 = g.data1;
    let d2 = g.data2;
    let d3 = g.data3;
    let d4 = &g.data4;

    let mut buf = [0u8; 38];
    buf[0]  = b'{';
    buf[1]  = hex(((d1 >> 28) & 0x0F) as u8);
    buf[2]  = hex(((d1 >> 24) & 0x0F) as u8);
    buf[3]  = hex(((d1 >> 20) & 0x0F) as u8);
    buf[4]  = hex(((d1 >> 16) & 0x0F) as u8);
    buf[5]  = hex(((d1 >> 12) & 0x0F) as u8);
    buf[6]  = hex(((d1 >> 8) & 0x0F) as u8);
    buf[7]  = hex(((d1 >> 4) & 0x0F) as u8);
    buf[8]  = hex((d1 & 0x0F) as u8);
    buf[9]  = b'-';
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

/// GUID → 16 字节小端字节数组。
pub fn guid_to_bytes(g: GUID) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&g.data1.to_le_bytes());
    bytes[4..6].copy_from_slice(&g.data2.to_le_bytes());
    bytes[6..8].copy_from_slice(&g.data3.to_le_bytes());
    bytes[8..16].copy_from_slice(&g.data4);
    bytes
}

/// 16 字节 → GUID。
pub fn parse_guid_from_bytes(bytes: &[u8]) -> GUID {
    GUID {
        data1: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        data2: u16::from_le_bytes([bytes[4], bytes[5]]),
        data3: u16::from_le_bytes([bytes[6], bytes[7]]),
        data4: {
            let mut d = [0u8; 8];
            d.copy_from_slice(&bytes[8..16]);
            d
        },
    }
}

/// nibble → ASCII hex 字符（0-9 / A-F）
const fn hex(nibble: u8) -> u8 {
    match nibble {
        0..=9 => b'0' + nibble,
        10..=15 => b'A' + nibble - 10,
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Media::KernelStreaming::AUDIO_SIGNALPROCESSINGMODE_DEFAULT;

    #[test]
    fn roundtrip() {
        let g = AUDIO_SIGNALPROCESSINGMODE_DEFAULT;
        let bytes = guid_to_bytes(g);
        let g2 = parse_guid_from_bytes(&bytes);
        println!("{}", format_guid(&g));
        assert_eq!(format_guid(&g), format_guid(&g2));
    }
}