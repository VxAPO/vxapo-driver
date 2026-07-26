//! com/iid.rs — APO 相关接口 IID 常量
//!
//! 系统接口 IID 与 Windows 音频子系统常量。
//! `com/apo_abi.rs` 提供 ABI 层的接口标识 GUID，
//! 本模块补充 APO 注册与 Windows 音频子系统中使用的系统级 GUID
//! （如 `IID_IAPO`、`KSDATAFORMAT_SUBTYPE_DEFAULT_PROCESSMODE` 等）。
//!
//! 所有 GUID 使用 `GUID::from_values` 显式构造，避免字符串解析引入运行时开销。
//!
//! VxAPO 自身的 CLSID 定义在 `com/reg_props.rs`，不在此模块中。
//!
//! 此模块为纯常量定义层，不包含任何运行时逻辑。

use windows::core::GUID;

// ══════════════════════════════════════════════════════════════════════════════
// Windows 音频 APO 相关接口
// ══════════════════════════════════════════════════════════════════════════════

/// `IAudioSystemEffects` — 系统效果 APO 初始化接口
/// 用于 `APOInitSystemEffects` 初始化参数中的查询（Note 7）。
pub const IID_IAUDIO_SYSTEM_EFFECTS: GUID = GUID::from_values(
    0x5FA00F27,
    0xADD6,
    0x499A,
    [0x8A, 0x9D, 0x60, 0xB4, 0x2C, 0x1B, 0x3D, 0xA5],
);

/// `IAudioSystemEffects2` — 扩展系统效果接口（Win10+）
pub const IID_IAUDIO_SYSTEM_EFFECTS2: GUID = GUID::from_values(
    0xBAFE99D2,
    0x7436,
    0x44CE,
    [0x9E, 0x0E, 0x47, 0x3A, 0x1A, 0xCE, 0x01, 0xB1],
);

// ══════════════════════════════════════════════════════════════════════════════
// APO 初始化参数相关 GUID
// ══════════════════════════════════════════════════════════════════════════════

/// APO 初始化系统效果的通知注册对象
pub const IID_APO_NOTIFICATION_HANDLER: GUID = GUID::from_values(
    0x5780DFFA,
    0x5C1B,
    0x4C18,
    [0xA4, 0x3A, 0x8A, 0x3A, 0x7D, 0xF3, 0xF1, 0xB2],
);

/// 主 APO 对象的聚合 IID
///
/// `non_delegating.rs` 中 NonDelegatingQueryInterface 使用此值标识外部对象。
/// 对应 `IAudioProcessingObject` 接口的 IID（Note 3）。
pub const IID_IAPO: GUID = GUID::from_values(
    0xFD7F2B29,
    0x24D0,
    0x4B5C,
    [0xB1, 0x77, 0x59, 0x2C, 0x39, 0xF9, 0xCA, 0x10],
);

// ══════════════════════════════════════════════════════════════════════════════
// 默认处理模式 GUID（Note 26/47）
// ══════════════════════════════════════════════════════════════════════════════

/// 默认处理模式 GUID —— 写入 FxProperties 告诉 Windows 使用此 APO 处理音频。
///
/// `{C18E2F7E-933D-4965-B7D1-1EEF228D2AF3}`
///
/// 三种安装模式切换时都写入此值。
pub const KSDATAFORMAT_SUBTYPE_DEFAULT_PROCESSMODE: GUID = GUID::from_values(
    0xC18E2F7E,
    0x933D,
    0x4965,
    [0xB7, 0xD1, 0x1E, 0xEF, 0x22, 0x8D, 0x2A, 0xF3],
);

// ══════════════════════════════════════════════════════════════════════════════
// 编译期断言（Note 1）
// ══════════════════════════════════════════════════════════════════════════════

const _: () = {
    // GUID 恒为 16 字节
    assert!(std::mem::size_of::<GUID>() == 16);
};

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guid_sizes() {
        assert_eq!(std::mem::size_of::<GUID>(), 16);
    }

    #[test]
    fn default_processmode_guid_values() {
        // C18E2F7E-933D-4965-B7D1-1EEF228D2AF3
        assert_eq!(KSDATAFORMAT_SUBTYPE_DEFAULT_PROCESSMODE.data1, 0xC18E2F7E);
        assert_eq!(KSDATAFORMAT_SUBTYPE_DEFAULT_PROCESSMODE.data2, 0x933D);
        assert_eq!(KSDATAFORMAT_SUBTYPE_DEFAULT_PROCESSMODE.data3, 0x4965);
        assert_eq!(
            KSDATAFORMAT_SUBTYPE_DEFAULT_PROCESSMODE.data4,
            [0xB7, 0xD1, 0x1E, 0xEF, 0x22, 0x8D, 0x2A, 0xF3]
        );
    }

    #[test]
    fn all_guids_are_unique() {
        let guids = [
            IID_IAUDIO_SYSTEM_EFFECTS,
            IID_IAUDIO_SYSTEM_EFFECTS2,
            IID_APO_NOTIFICATION_HANDLER,
            KSDATAFORMAT_SUBTYPE_DEFAULT_PROCESSMODE,
        ];
        for i in 0..guids.len() {
            for j in (i + 1)..guids.len() {
                assert_ne!(guids[i], guids[j], "GUIDs at index {i} and {j} are equal");
            }
        }
    }

    #[test]
    fn guids_are_non_null() {
        let null_guid = GUID::zeroed();
        assert_ne!(IID_IAUDIO_SYSTEM_EFFECTS, null_guid);
        assert_ne!(IID_IAUDIO_SYSTEM_EFFECTS2, null_guid);
        assert_ne!(IID_APO_NOTIFICATION_HANDLER, null_guid);
        assert_ne!(KSDATAFORMAT_SUBTYPE_DEFAULT_PROCESSMODE, null_guid);
    }
}