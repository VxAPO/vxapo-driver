//! sys/com/apo_interfaces.rs — APO 接口 re-export + IID 常量（v6.3 规范 3.2，修正版）
//!
//! 职责：re-export windows-rs 0.62.2 已提供的 4 个 APO 接口结构体
//! （IAudioMediaType、IAudioProcessingObject、IAudioProcessingObjectRT、
//! IAudioProcessingObjectConfiguration，由 define_interface! 宏生成）；
//! 导出全部 7 个接口 IID 常量。
//!
//! 引用来源：
//! - `windows::Win32::Media::Audio::Apo`（4 个 APO 接口结构体 + 3 个系统接口结构体）
//! - `windows::core::{Interface, IUnknown}`（取 ::IID、接口根基）
//!
//! 导出给：`object/apo.rs`、`object/child.rs`、`object/factory.rs`。
//!
//! 禁止：不包含任何实现逻辑。

use windows::core::Interface;

// ══════════════════════════════════════════════════════════════════════════════
// 4 个 APO 接口结构体 re-export（windows-rs 提供，非自定义 trait）
// ══════════════════════════════════════════════════════════════════════════════

/// 音频媒体类型接口——格式协商用（windows-rs 结构体）。
pub use windows::Win32::Media::Audio::Apo::IAudioMediaType;

/// 基础 APO 接口——注册、格式协商、延迟查询（windows-rs 结构体）。
pub use windows::Win32::Media::Audio::Apo::IAudioProcessingObject;

/// 实时处理接口——APOProcess 在多媒体实时线程上调用（windows-rs 结构体）。
pub use windows::Win32::Media::Audio::Apo::IAudioProcessingObjectRT;

/// 配置接口——锁定/解锁处理流程（windows-rs 结构体）。
pub use windows::Win32::Media::Audio::Apo::IAudioProcessingObjectConfiguration;

// ── 对应 _Impl traits（#[implement] 实现对象时使用）───────────────────────────
pub use windows::Win32::Media::Audio::Apo::IAudioProcessingObject_Impl;
pub use windows::Win32::Media::Audio::Apo::IAudioProcessingObjectRT_Impl;
pub use windows::Win32::Media::Audio::Apo::IAudioProcessingObjectConfiguration_Impl;

// IAudioSystemEffects（marker 接口，EAPO 对齐）：`_Impl` 空体（windows-rs 0.62.2
// Apo/mod.rs:1109 `pub trait IAudioSystemEffects_Impl: IUnknownImpl {}`）——
// 独立父槽位（SFX/MFX/EFX）时引擎 QI 此接口确认「系统效果」，缺失则拒载。
pub use windows::Win32::Media::Audio::Apo::IAudioSystemEffects;
pub use windows::Win32::Media::Audio::Apo::IAudioSystemEffects_Impl;

// ══════════════════════════════════════════════════════════════════════════════
// IID 导出常量（7 个）
// ══════════════════════════════════════════════════════════════════════════════

// ── 4 个 APO 接口 IID（通过 Interface trait 取 ::IID）────────────────────────

/// `IAudioProcessingObject` IID。
pub const IID_IAPO: windows::core::GUID = IAudioProcessingObject::IID;

/// `IAudioProcessingObjectRT` IID。
pub const IID_IAPO_RT: windows::core::GUID = IAudioProcessingObjectRT::IID;

/// `IAudioProcessingObjectConfiguration` IID。
pub const IID_IAPO_CONFIG: windows::core::GUID = IAudioProcessingObjectConfiguration::IID;

/// `IAudioMediaType` IID。
pub const IID_IAUDIO_MEDIA_TYPE: windows::core::GUID = IAudioMediaType::IID;

// ── 3 个系统接口 IID（从 windows-rs 直接引用）────────────────────────────────

// 这 3 个接口由 Windows 实现（非 APO 实现），APO 侧仅需 IID 用于 QueryInterface 查询。
// IAudioSystemEffects 已在上面 pub use（APO 侧需实现该 marker 接口），此处不再重复引入。
use windows::Win32::Media::Audio::Apo::{
    IAudioProcessingObjectNotifications, IAudioSystemEffects2,
};

/// `IAudioSystemEffects` IID。
pub const IID_IAUDIO_SYSTEM_EFFECTS: windows::core::GUID = IAudioSystemEffects::IID;

/// `IAudioSystemEffects2` IID。
pub const IID_IAUDIO_SYSTEM_EFFECTS2: windows::core::GUID = IAudioSystemEffects2::IID;

/// `IAudioProcessingObjectNotifications` IID。
pub const IID_IAUDIO_PROCESSING_OBJECT_NOTIFICATIONS: windows::core::GUID =
    IAudioProcessingObjectNotifications::IID;

// ══════════════════════════════════════════════════════════════════════════════
// AEC 接口预留（O4/v6.6，feature-gated）
// ══════════════════════════════════════════════════════════════════════════════
//
// Windows 11 AEC（声学回声消除）APO 未来若支持，在此预留接口 re-export 与 IID 常量。
// 当前**不实现**，仅声明与门控位置（`feature = "aec"` 非默认）。
// 实现时 object/apo.rs 三接口 + 3 个 AEC 接口 = 六接口承载。

#[cfg(feature = "aec")]
pub use windows::Win32::Media::Audio::Apo::{
    IApoAcousticEchoCancellation,
    IApoAuxiliaryInputConfiguration,
    IApoAuxiliaryInputRT,
};

/// `IApoAcousticEchoCancellation` IID（仅 `feature = "aec"` 时存在）。
#[cfg(feature = "aec")]
pub const IID_IAPO_ACOUSTIC_ECHO_CANCELLATION: windows::core::GUID =
    IApoAcousticEchoCancellation::IID;

/// `IApoAuxiliaryInputConfiguration` IID（仅 `feature = "aec"` 时存在）。
#[cfg(feature = "aec")]
pub const IID_IAPO_AUXILIARY_INPUT_CONFIGURATION: windows::core::GUID =
    IApoAuxiliaryInputConfiguration::IID;

/// `IApoAuxiliaryInputRT` IID（仅 `feature = "aec"` 时存在）。
#[cfg(feature = "aec")]
pub const IID_IAPO_AUXILIARY_INPUT_RT: windows::core::GUID = IApoAuxiliaryInputRT::IID;

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_guids_are_non_null() {
        let null_guid = windows::core::GUID::zeroed();
        assert_ne!(IID_IAPO, null_guid);
        assert_ne!(IID_IAPO_RT, null_guid);
        assert_ne!(IID_IAPO_CONFIG, null_guid);
        assert_ne!(IID_IAUDIO_MEDIA_TYPE, null_guid);
        assert_ne!(IID_IAUDIO_SYSTEM_EFFECTS, null_guid);
        assert_ne!(IID_IAUDIO_SYSTEM_EFFECTS2, null_guid);
        assert_ne!(IID_IAUDIO_PROCESSING_OBJECT_NOTIFICATIONS, null_guid);
    }

    #[test]
    fn all_guids_are_unique() {
        let guids = [
            IID_IAPO,
            IID_IAPO_RT,
            IID_IAPO_CONFIG,
            IID_IAUDIO_MEDIA_TYPE,
            IID_IAUDIO_SYSTEM_EFFECTS,
            IID_IAUDIO_SYSTEM_EFFECTS2,
            IID_IAUDIO_PROCESSING_OBJECT_NOTIFICATIONS,
        ];
        for i in 0..guids.len() {
            for j in (i + 1)..guids.len() {
                assert_ne!(guids[i], guids[j], "GUIDs at index {i} and {j} are equal");
            }
        }
    }

    #[test]
    fn interface_structs_re_exported() {
        // 验证 re-export 的类型确实是结构体（可构造性以外仅检查 Sized）
        fn _assert_sized<T: Sized>() {}
        _assert_sized::<IAudioMediaType>();
        _assert_sized::<IAudioProcessingObject>();
        _assert_sized::<IAudioProcessingObjectRT>();
        _assert_sized::<IAudioProcessingObjectConfiguration>();
    }
}