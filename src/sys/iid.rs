//! sys/iid.rs — APO 相关接口 IID 常量
//!
//! 系统接口 IID 与 Windows 音频子系统常量。
//! 直接引用 `windows` crate 内部导出的接口 IID（`Interface::IID`），
//! 确保 100% 符合 Windows SDK 标准且零运行时开销。
//!
//! VxAPO 自身的 CLSID 定义在 `host/instance/reg_props.rs`，不在此模块中。
//!
//! 此模块为纯常量定义层，不包含任何运行时逻辑。

use windows::core::{Interface, GUID};
use windows::Win32::Media::Audio::Apo::{
    IAudioProcessingObjectNotifications, IAudioSystemEffects, IAudioSystemEffects2,
};

// ══════════════════════════════════════════════════════════════════════════════
// Windows 音频 APO 相关接口 IID
// ══════════════════════════════════════════════════════════════════════════════

/// `IAudioSystemEffects` — 系统效果 APO 初始化接口
/// 用于 `APOInitSystemEffects` 初始化参数中的查询（Note 7）。
pub const IID_IAUDIO_SYSTEM_EFFECTS: GUID = IAudioSystemEffects::IID;

/// `IAudioSystemEffects2` — 扩展系统效果接口（Win10+）
pub const IID_IAUDIO_SYSTEM_EFFECTS2: GUID = IAudioSystemEffects2::IID;

/// `IAudioProcessingObjectNotifications` — Windows 10/11 APO 系统效果与端点属性变更通知注册接口
pub const IID_IAUDIO_PROCESSING_OBJECT_NOTIFICATIONS: GUID =
    IAudioProcessingObjectNotifications::IID;

// ══════════════════════════════════════════════════════════════════════════════
// 默认处理模式 GUID（Note 26/47）
// ══════════════════════════════════════════════════════════════════════════════

/// 默认处理模式 GUID —— 写入 FxProperties 告诉 Windows 使用此 APO 处理音频。
///
/// `{C18E2F7E-933D-4965-B7D1-1EEF228D2AF3}`
///
/// ⚠️ 注意：此类音频处理模式 (Audio Processing Mode) GUID 在 SDK 中属于数据标识，
/// 并非 COM 接口，因此需要保留 `GUID::from_values` 显式构造。
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
            IID_IAUDIO_PROCESSING_OBJECT_NOTIFICATIONS,
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
        assert_ne!(IID_IAUDIO_PROCESSING_OBJECT_NOTIFICATIONS, null_guid);
        assert_ne!(KSDATAFORMAT_SUBTYPE_DEFAULT_PROCESSMODE, null_guid);
    }
}