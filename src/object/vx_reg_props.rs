//! object/vx_reg_props.rs — APO 注册属性定义
//!
//! VxAPO 自身的 CLSID 定义与 APO 注册属性。
//! 系统级接口 IID 请见 `sys/com/apo_interfaces.rs`。
//!
//! 定义两个 APO 注册属性对象（PreMix / PostMix），各包含 CLSID、名称、版权信息
//! 及 APO 标志位（`FRAMESPERSECOND_MUST_MATCH | BITSPERSAMPLE_MUST_MATCH | INPLACE`）。
//! 两个对象共享名称与标志，仅 CLSID 不同。
//!
//! `DllRegisterServer` 调用 `RegisterAPO` 时传入这些属性，
//! `IAudioProcessingObject::GetRegistrationProperties` 返回这些属性。
//!
//! 此模块仅提供常量与结构体定义，不包含注册逻辑。

use crate::sys::com::prelude::GUID;

use crate::sys::com::apo_interfaces::IID_IAPO;
use crate::sys::com::apo_types::{APO_FLAG, APO_FLAG_BITSPERSAMPLE_MUST_MATCH, APO_FLAG_FRAMESPERSECOND_MUST_MATCH, APO_FLAG_INPLACE, APO_REG_PROPERTIES};

// ══════════════════════════════════════════════════════════════════════════════
// CLSID 常量
// ══════════════════════════════════════════════════════════════════════════════
// 由 PowerShell `[guid]::NewGuid()` 生成，**定死不改动**。
// 备份见 .clinerules/07-VxAPO_GUID.md。
// PRE_MIX = 41C34613-D391-459D-A039-72B2B15A1A1D
// POST_MIX = B4A97313-ABC0-45ED-9C33-428B20D39428

pub const CLSID_VXAPO_PRE_MIX: GUID = GUID::from_values(
    0x41C34613,
    0xD391,
    0x459D,
    [0xA0, 0x39, 0x72, 0xB2, 0xB1, 0x5A, 0x1A, 0x1D],
);

pub const CLSID_VXAPO_POST_MIX: GUID = GUID::from_values(
    0xB4A97313,
    0xABC0,
    0x45ED,
    [0x9C, 0x33, 0x42, 0x8B, 0x20, 0xD3, 0x94, 0x28],
);

// ══════════════════════════════════════════════════════════════════════════════
// APO 名称和版权
// ══════════════════════════════════════════════════════════════════════════════

const APO_NAME: &str = "VxAPO";
const APO_COPYRIGHT: &str = "VxAPO Project";

// ══════════════════════════════════════════════════════════════════════════════
// APO 标志位
// ══════════════════════════════════════════════════════════════════════════════

const APO_FLAGS: APO_FLAG = APO_FLAG(
    APO_FLAG_FRAMESPERSECOND_MUST_MATCH.0
    | APO_FLAG_BITSPERSAMPLE_MUST_MATCH.0
    | APO_FLAG_INPLACE.0,
);

// ══════════════════════════════════════════════════════════════════════════════
// UTF-16 编码辅助
// ══════════════════════════════════════════════════════════════════════════════

const fn str_to_u16_256(s: &str) -> [u16; 256] {
    let mut buf = [0u16; 256];
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut dst = 0;
    while i < bytes.len() && dst < 255 {
        buf[dst] = bytes[i] as u16;
        i += 1;
        dst += 1;
    }
    buf
}

// ══════════════════════════════════════════════════════════════════════════════
// 注册属性实例
// ══════════════════════════════════════════════════════════════════════════════

/// Pre/PostMix 共用注册属性构造（仅 CLSID 不同）。
const fn make_reg_props(clsid: GUID) -> APO_REG_PROPERTIES {
    APO_REG_PROPERTIES {
        clsid,
        Flags: APO_FLAGS,
        szFriendlyName: str_to_u16_256(APO_NAME),
        szCopyrightInfo: str_to_u16_256(APO_COPYRIGHT),
        u32MajorVersion: 1,
        u32MinorVersion: 0,
        u32MinInputConnections: 1,
        u32MaxInputConnections: 1,
        u32MinOutputConnections: 1,
        u32MaxOutputConnections: 1,
        u32MaxInstances: 1,
        u32NumAPOInterfaces: 1,
        iidAPOInterfaceList: [IID_IAPO],
    }
}

pub static REG_PROPS_PRE_MIX: APO_REG_PROPERTIES = make_reg_props(CLSID_VXAPO_PRE_MIX);

pub static REG_PROPS_POST_MIX: APO_REG_PROPERTIES = make_reg_props(CLSID_VXAPO_POST_MIX);

// ══════════════════════════════════════════════════════════════════════════════
// 编译期断言
// ══════════════════════════════════════════════════════════════════════════════

const _: () = {
    assert!(APO_FLAGS.0 == 0x0000_000D);
    // EAPO 对照：CRegAPOProperties<1> = u32NumAPOInterfaces=1（audiodg 校验
    // 接口数与 iidAPOInterfaceList 长度必须一致——曾写 3 导致独立父槽位被拒）。
    assert!(REG_PROPS_PRE_MIX.u32NumAPOInterfaces == 1);
    assert!(REG_PROPS_PRE_MIX.u32NumAPOInterfaces as usize == REG_PROPS_PRE_MIX.iidAPOInterfaceList.len());
    assert!(REG_PROPS_POST_MIX.u32NumAPOInterfaces as usize == REG_PROPS_POST_MIX.iidAPOInterfaceList.len());
    assert!(REG_PROPS_PRE_MIX.szFriendlyName[0] != 0);
    assert!(REG_PROPS_PRE_MIX.Flags.0 == REG_PROPS_POST_MIX.Flags.0);
    assert!(
        REG_PROPS_PRE_MIX.u32MaxInputConnections
            == REG_PROPS_POST_MIX.u32MaxInputConnections
    );
};

// ══════════════════════════════════════════════════════════════════════════════
// 查询辅助
// ══════════════════════════════════════════════════════════════════════════════

pub fn props_for_clsid(clsid: &GUID) -> Option<&'static APO_REG_PROPERTIES> {
    if *clsid == CLSID_VXAPO_PRE_MIX {
        Some(&REG_PROPS_PRE_MIX)
    } else if *clsid == CLSID_VXAPO_POST_MIX {
        Some(&REG_PROPS_POST_MIX)
    } else {
        None
    }
}

pub fn is_vxapo_clsid(clsid: &GUID) -> bool {
    *clsid == CLSID_VXAPO_PRE_MIX || *clsid == CLSID_VXAPO_POST_MIX
}

pub fn supported_clsids() -> &'static [GUID] {
    &[CLSID_VXAPO_PRE_MIX, CLSID_VXAPO_POST_MIX]
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clsids_are_distinct() {
        assert_ne!(CLSID_VXAPO_PRE_MIX, CLSID_VXAPO_POST_MIX);
    }

    #[test]
    fn clsids_are_non_null() {
        let null = GUID::zeroed();
        assert_ne!(CLSID_VXAPO_PRE_MIX, null);
        assert_ne!(CLSID_VXAPO_POST_MIX, null);
    }

    #[test]
    fn apo_name_encoded_correctly() {
        assert_eq!(REG_PROPS_PRE_MIX.szFriendlyName[0], 'V' as u16);
        assert_eq!(REG_PROPS_PRE_MIX.szFriendlyName[1], 'x' as u16);
        assert_eq!(REG_PROPS_PRE_MIX.szFriendlyName[2], 'A' as u16);
        assert_eq!(REG_PROPS_PRE_MIX.szFriendlyName[3], 'P' as u16);
        assert_eq!(REG_PROPS_PRE_MIX.szFriendlyName[4], 'O' as u16);
        assert_eq!(REG_PROPS_PRE_MIX.szFriendlyName[5], 0);
    }

    #[test]
    fn apo_copyright_encoded_correctly() {
        assert_eq!(REG_PROPS_PRE_MIX.szCopyrightInfo[0], 'V' as u16);
        assert_eq!(REG_PROPS_PRE_MIX.szCopyrightInfo[1], 'x' as u16);
        assert_eq!(REG_PROPS_PRE_MIX.szCopyrightInfo[13], 0);
    }

    #[test]
    fn flags_value() {
        let expected = APO_FLAG_INPLACE.0
            | APO_FLAG_FRAMESPERSECOND_MUST_MATCH.0
            | APO_FLAG_BITSPERSAMPLE_MUST_MATCH.0;
        assert_eq!(REG_PROPS_PRE_MIX.Flags.0, expected);
        assert_eq!(REG_PROPS_POST_MIX.Flags.0, expected);
    }

    #[test]
    fn props_share_everything_except_clsid() {
        assert_eq!(REG_PROPS_PRE_MIX.Flags, REG_PROPS_POST_MIX.Flags);
        assert_eq!(REG_PROPS_PRE_MIX.u32MajorVersion, REG_PROPS_POST_MIX.u32MajorVersion);
        assert_eq!(REG_PROPS_PRE_MIX.u32MinorVersion, REG_PROPS_POST_MIX.u32MinorVersion);
        assert_eq!(
            REG_PROPS_PRE_MIX.u32MinInputConnections,
            REG_PROPS_POST_MIX.u32MinInputConnections
        );
        assert_eq!(
            REG_PROPS_PRE_MIX.u32MaxInputConnections,
            REG_PROPS_POST_MIX.u32MaxInputConnections
        );
        assert_eq!(
            REG_PROPS_PRE_MIX.u32MinOutputConnections,
            REG_PROPS_POST_MIX.u32MinOutputConnections
        );
        assert_eq!(
            REG_PROPS_PRE_MIX.u32MaxOutputConnections,
            REG_PROPS_POST_MIX.u32MaxOutputConnections
        );
        assert_eq!(REG_PROPS_PRE_MIX.u32MaxInstances, REG_PROPS_POST_MIX.u32MaxInstances);
        assert_eq!(
            REG_PROPS_PRE_MIX.u32NumAPOInterfaces,
            REG_PROPS_POST_MIX.u32NumAPOInterfaces
        );
        assert_eq!(REG_PROPS_PRE_MIX.szFriendlyName, REG_PROPS_POST_MIX.szFriendlyName);
        assert_eq!(REG_PROPS_PRE_MIX.szCopyrightInfo, REG_PROPS_POST_MIX.szCopyrightInfo);
        assert_ne!(REG_PROPS_PRE_MIX.clsid, REG_PROPS_POST_MIX.clsid);
    }

    #[test]
    fn props_for_clsid_known() {
        assert!(props_for_clsid(&CLSID_VXAPO_PRE_MIX).is_some());
        assert!(props_for_clsid(&CLSID_VXAPO_POST_MIX).is_some());
    }

    #[test]
    fn props_for_clsid_unknown() {
        assert!(props_for_clsid(&GUID::zeroed()).is_none());
    }

    #[test]
    fn props_for_clsid_returns_correct_entry() {
        let pre = props_for_clsid(&CLSID_VXAPO_PRE_MIX).unwrap();
        assert_eq!(pre.clsid, CLSID_VXAPO_PRE_MIX);
        let post = props_for_clsid(&CLSID_VXAPO_POST_MIX).unwrap();
        assert_eq!(post.clsid, CLSID_VXAPO_POST_MIX);
    }

    #[test]
    fn is_vxapo_clsid_positive() {
        assert!(is_vxapo_clsid(&CLSID_VXAPO_PRE_MIX));
        assert!(is_vxapo_clsid(&CLSID_VXAPO_POST_MIX));
    }

    #[test]
    fn is_vxapo_clsid_negative() {
        assert!(!is_vxapo_clsid(&GUID::zeroed()));
    }

    #[test]
    fn supported_clsids_count() {
        assert_eq!(supported_clsids().len(), 2);
    }

    #[test]
    fn u32_num_apointerfaces_matches_interface_list() {
        assert_eq!(REG_PROPS_PRE_MIX.u32NumAPOInterfaces, 1);
        assert_eq!(
            REG_PROPS_PRE_MIX.u32NumAPOInterfaces as usize,
            REG_PROPS_PRE_MIX.iidAPOInterfaceList.len()
        );
    }

    #[test]
    fn version_numbers() {
        assert_eq!(REG_PROPS_PRE_MIX.u32MajorVersion, 1);
        assert_eq!(REG_PROPS_PRE_MIX.u32MinorVersion, 0);
    }
}

/// 单个 CLSID 的注册信息（规范 7.5）。
#[derive(Debug)]
pub struct ClsidEntry {
    pub clsid: GUID,
    pub clsid_str: String,
    /// CLSID 父键(Default) 友好名（EAPO 对齐：EAPO 注册树父键有
    /// (Default)="EqualizerAPO Pre-Mix Class"，VxAPO 之前缺失——补上供引擎辨识）。
    pub friendly_name: String,
}

impl ClsidEntry {
    pub fn new(clsid: GUID) -> Self {
        Self {
            clsid,
            clsid_str: crate::sys::com::prelude::guid_to_string(&clsid),
            friendly_name: if clsid == CLSID_VXAPO_PRE_MIX {
                "VxAPO Pre-Mix Class".to_owned()
            } else {
                "VxAPO Post-Mix Class".to_owned()
            },
        }
    }
    pub fn clsid_key_path(&self) -> String { format!("CLSID\\{}", self.clsid_str) }
    pub fn inproc_server_path(&self) -> String { format!("CLSID\\{}\\InprocServer32", self.clsid_str) }
    /// AudioEngine APO 注册键：引擎读槽位 CLSID 后查此键取 APO 属性，缺失则静默拒载。
    pub fn audio_engine_path(&self) -> String {
        format!("AudioEngine\\AudioProcessingObjects\\{}", self.clsid_str)
    }
    pub fn registration_entries(&self, dll_path: &str) -> Vec<(&str, String, String)> {
        vec![
            ("Default", String::new(), dll_path.to_owned()),
            ("ThreadingModel", "ThreadingModel".to_owned(), "Both".to_owned()),
        ]
    }
}

/// 注册顺序：PostMix → PreMix。
pub fn registration_order() -> Vec<ClsidEntry> {
    vec![ClsidEntry::new(CLSID_VXAPO_POST_MIX), ClsidEntry::new(CLSID_VXAPO_PRE_MIX)]
}

/// 注销顺序：PreMix → PostMix。
pub fn unregistration_order() -> Vec<ClsidEntry> {
    vec![ClsidEntry::new(CLSID_VXAPO_PRE_MIX), ClsidEntry::new(CLSID_VXAPO_POST_MIX)]
}
