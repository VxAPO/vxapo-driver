//! com/reg_props.rs — APO 注册属性定义（Note 42）
//!
//! VxAPO 自身的 CLSID 定义与 APO 注册属性。
//! 系统级接口 IID 请见 `com/iid.rs`。
//!
//! 定义两个 APO 注册属性对象（PreMix / PostMix），各包含 CLSID、名称、版权信息
//! 及 APO 标志位（`FRAMESPERSECOND_MUST_MATCH | BITSPERSAMPLE_MUST_MATCH | INPLACE`）。
//! 两个对象共享名称与标志，仅 CLSID 不同。
//!
//! `DllRegisterServer` 调用 `RegisterAPO` 时传入这些属性，
//! `IAudioProcessingObject::GetRegistrationProperties` 返回这些属性。
//!
//! 此模块仅提供常量与结构体定义，不包含注册逻辑。

use windows::core::GUID;

use crate::sys::com::apo_abi::{AUDIO_FLOW_TYPE, APO_FLAG, APO_REG_PROPERTIES};

// ══════════════════════════════════════════════════════════════════════════════
// CLSID 常量
// ══════════════════════════════════════════════════════════════════════════════

/// VxAPO PreMix APO 的 CLSID。
///
/// 在 `DllGetClassObject` 中用于路由 `IClassFactory` 创建请求（Note 4）。
/// 写入设备 FxProperties 的 PreMix APO GUID 槽位。
pub const CLSID_VXAPO_PRE_MIX: GUID = GUID::from_values(
    0xA1B2C3D4,
    0x1234,
    0x5678,
    [0x9A, 0xBC, 0xDE, 0xF0, 0x12, 0x34, 0x56, 0x78],
);

/// VxAPO PostMix APO 的 CLSID。
///
/// 用途与 PreMix 相同，写入 PostMix APO GUID 槽位。
pub const CLSID_VXAPO_POST_MIX: GUID = GUID::from_values(
    0xD4C3B2A1,
    0x4321,
    0x8765,
    [0x9A, 0xBC, 0xDE, 0xF0, 0x12, 0x34, 0x56, 0x79],
);

// ══════════════════════════════════════════════════════════════════════════════
// APO 名称和版权（UTF-16，最多 255 字符 + null）
// ══════════════════════════════════════════════════════════════════════════════

/// APO 显示名称（PreMix / PostMix 共享）。
const APO_NAME: &str = "VxAPO";

/// 版权信息。
const APO_COPYRIGHT: &str = "VxAPO Project";

// ══════════════════════════════════════════════════════════════════════════════
// APO 标志位（Note 42）
//
// FRAMESPERSECOND_MUST_MATCH | BITSPERSAMPLE_MUST_MATCH | INPLACE
// ══════════════════════════════════════════════════════════════════════════════

const APO_FLAGS: APO_FLAG = APO_FLAG(APO_FLAG::FRAMESPERSECOND_MUST_MATCH.0
    | APO_FLAG::BITSPERSAMPLE_MUST_MATCH.0
    | APO_FLAG::INPLACE.0);

// ══════════════════════════════════════════════════════════════════════════════
// UTF-16 编码辅助
// ══════════════════════════════════════════════════════════════════════════════

/// 将 &str 编码为 UTF-16 并写入 [u16; 256]，末尾补 null。
const fn str_to_u16_256(s: &str) -> [u16; 256] {
    let mut buf = [0u16; 256];
    // const fn 中无法使用 encode_utf16 迭代器，手动逐字节处理
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut dst = 0;
    // 简单 ASCII 快速路径（APO 名称和版权通常为 ASCII）
    while i < bytes.len() && dst < 255 {
        buf[dst] = bytes[i] as u16;
        i += 1;
        dst += 1;
    }
    // buf 已经是零初始化，最后一个位置自然是 null 终止符
    buf
}

// ══════════════════════════════════════════════════════════════════════════════
// 注册属性实例
// ══════════════════════════════════════════════════════════════════════════════

/// PreMix APO 注册属性。
pub static REG_PROPS_PRE_MIX: APO_REG_PROPERTIES = APO_REG_PROPERTIES {
    clsid: CLSID_VXAPO_PRE_MIX,
    flags: APO_FLAGS,
    sz_name: str_to_u16_256(APO_NAME),
    sz_copyright: str_to_u16_256(APO_COPYRIGHT),
    major_version: 1,
    minor_version: 0,
    min_input_connections: 1,
    max_input_connections: 1,
    min_output_connections: 1,
    max_output_connections: 1,
    max_instances: 1,
    audio_flow_type: AUDIO_FLOW_TYPE::RENDER,
};

/// PostMix APO 注册属性。
pub static REG_PROPS_POST_MIX: APO_REG_PROPERTIES = APO_REG_PROPERTIES {
    clsid: CLSID_VXAPO_POST_MIX,
    ..REG_PROPS_PRE_MIX
};

// ══════════════════════════════════════════════════════════════════════════════
// 编译期断言
// ══════════════════════════════════════════════════════════════════════════════

const _: () = {
    // flags 值验证：INPLACE(1) | FRAMESPERSECOND_MUST_MATCH(4) | BITSPERSAMPLE_MUST_MATCH(8) = 0x0D
    assert!(APO_FLAGS.0 == 0x0000_000D);
    // 名称不为空（第一个字符非 null）
    assert!(REG_PROPS_PRE_MIX.sz_name[0] != 0);
    // PreMix / PostMix 只有 CLSID 不同
    assert!(REG_PROPS_PRE_MIX.flags.0 == REG_PROPS_POST_MIX.flags.0);
    assert!(REG_PROPS_PRE_MIX.max_input_connections == REG_PROPS_POST_MIX.max_input_connections);
};

// ══════════════════════════════════════════════════════════════════════════════
// 查询辅助
// ══════════════════════════════════════════════════════════════════════════════

/// 根据 CLSID 返回对应的注册属性指针。
///
/// `DllGetClassObject` 和 `GetRegistrationProperties` 使用。
/// 未知 CLSID 返回 `None`。
pub fn props_for_clsid(clsid: &GUID) -> Option<&'static APO_REG_PROPERTIES> {
    if *clsid == CLSID_VXAPO_PRE_MIX {
        Some(&REG_PROPS_PRE_MIX)
    } else if *clsid == CLSID_VXAPO_POST_MIX {
        Some(&REG_PROPS_POST_MIX)
    } else {
        None
    }
}

/// 检查给定 CLSID 是否是 VxAPO 的两个 APO 之一。
pub fn is_vxapo_clsid(clsid: &GUID) -> bool {
    *clsid == CLSID_VXAPO_PRE_MIX || *clsid == CLSID_VXAPO_POST_MIX
}

/// 返回所有支持的 CLSID（用于 `DllGetClassObject` 的路由判断，Note 4）。
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
        // "VxAPO" = V(0x56) x(0x78) A(0x41) P(0x50) O(0x4F)
        assert_eq!(REG_PROPS_PRE_MIX.sz_name[0], 'V' as u16);
        assert_eq!(REG_PROPS_PRE_MIX.sz_name[1], 'x' as u16);
        assert_eq!(REG_PROPS_PRE_MIX.sz_name[2], 'A' as u16);
        assert_eq!(REG_PROPS_PRE_MIX.sz_name[3], 'P' as u16);
        assert_eq!(REG_PROPS_PRE_MIX.sz_name[4], 'O' as u16);
        // null terminator
        assert_eq!(REG_PROPS_PRE_MIX.sz_name[5], 0);
    }

    #[test]
    fn apo_copyright_encoded_correctly() {
        assert_eq!(REG_PROPS_PRE_MIX.sz_copyright[0], 'V' as u16);
        assert_eq!(REG_PROPS_PRE_MIX.sz_copyright[1], 'x' as u16);
        // "VxAPO Project" = 13 chars
        assert_eq!(REG_PROPS_PRE_MIX.sz_copyright[13], 0);
    }

    #[test]
    fn flags_value() {
        let expected = APO_FLAG::INPLACE.0
            | APO_FLAG::FRAMESPERSECOND_MUST_MATCH.0
            | APO_FLAG::BITSPERSAMPLE_MUST_MATCH.0;
        assert_eq!(REG_PROPS_PRE_MIX.flags.0, expected);
        assert_eq!(REG_PROPS_POST_MIX.flags.0, expected);
    }

    #[test]
    fn props_share_everything_except_clsid() {
        assert_eq!(REG_PROPS_PRE_MIX.flags, REG_PROPS_POST_MIX.flags);
        assert_eq!(
            REG_PROPS_PRE_MIX.major_version,
            REG_PROPS_POST_MIX.major_version
        );
        assert_eq!(
            REG_PROPS_PRE_MIX.minor_version,
            REG_PROPS_POST_MIX.minor_version
        );
        assert_eq!(
            REG_PROPS_PRE_MIX.min_input_connections,
            REG_PROPS_POST_MIX.min_input_connections
        );
        assert_eq!(
            REG_PROPS_PRE_MIX.max_input_connections,
            REG_PROPS_POST_MIX.max_input_connections
        );
        assert_eq!(
            REG_PROPS_PRE_MIX.min_output_connections,
            REG_PROPS_POST_MIX.min_output_connections
        );
        assert_eq!(
            REG_PROPS_PRE_MIX.max_output_connections,
            REG_PROPS_POST_MIX.max_output_connections
        );
        assert_eq!(
            REG_PROPS_PRE_MIX.max_instances,
            REG_PROPS_POST_MIX.max_instances
        );
        assert_eq!(
            REG_PROPS_PRE_MIX.audio_flow_type,
            REG_PROPS_POST_MIX.audio_flow_type
        );
        assert_eq!(REG_PROPS_PRE_MIX.sz_name, REG_PROPS_POST_MIX.sz_name);
        assert_eq!(
            REG_PROPS_PRE_MIX.sz_copyright,
            REG_PROPS_POST_MIX.sz_copyright
        );
        // 只有 CLSID 不同
        assert_ne!(REG_PROPS_PRE_MIX.clsid, REG_PROPS_POST_MIX.clsid);
    }

    #[test]
    fn props_for_clsid_known() {
        assert!(props_for_clsid(&CLSID_VXAPO_PRE_MIX).is_some());
        assert!(props_for_clsid(&CLSID_VXAPO_POST_MIX).is_some());
    }

    #[test]
    fn props_for_clsid_unknown() {
        let unknown = GUID::zeroed();
        assert!(props_for_clsid(&unknown).is_none());
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
    fn max_instances_is_1() {
        assert_eq!(REG_PROPS_PRE_MIX.max_instances, 1);
    }

    #[test]
    fn audio_flow_is_render() {
        assert_eq!(REG_PROPS_PRE_MIX.audio_flow_type, AUDIO_FLOW_TYPE::RENDER);
    }

    #[test]
    fn version_numbers() {
        assert_eq!(REG_PROPS_PRE_MIX.major_version, 1);
        assert_eq!(REG_PROPS_PRE_MIX.minor_version, 0);
    }
}