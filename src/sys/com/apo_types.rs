//! sys/com/apo_types.rs — APO 类型（规范 3.3，修正版）
//!
//! 修正：windows-rs 0.62.2 已提供 APO_REG_PROPERTIES / APO_CONNECTION_DESCRIPTOR /
//! APO_CONNECTION_PROPERTY / APO_FLAG / APO_BUFFER_FLAGS，此处全部 re-export。
//! 仅保留 windows-rs 缺失的自定义项：UNCOMPRESSED_AUDIO_FORMAT、AUDIO_FLOW_TYPE、
//! REFERENCE_TIME、签名常量、比较标志、APOERR 错误码、编译期断言。

pub use windows::core::GUID;
pub use crate::sys::com::prelude::HRESULT;

// ── windows-rs 已提供的类型（直接 re-export） ─────────────────────────────
pub use windows::Win32::Media::Audio::Apo::{
    APO_FLAG,
    APO_FLAG_NONE,
    APO_FLAG_INPLACE,
    APO_FLAG_SAMPLESPERFRAME_MUST_MATCH,
    APO_FLAG_FRAMESPERSECOND_MUST_MATCH,
    APO_FLAG_BITSPERSAMPLE_MUST_MATCH,
    APO_FLAG_MIXER,
    APO_FLAG_DEFAULT,
    APO_BUFFER_FLAGS,
    APO_REG_PROPERTIES,
    APO_CONNECTION_DESCRIPTOR,
    APO_CONNECTION_PROPERTY,
    // APOInitSystemEffects：Initialize 初始化数据（， per-device 配置路径）。
    // 实测字段：{ APOInit: APOInitBaseStruct, pAPOEndpointProperties,
    //   pAPOSystemEffectsProperties: ManuallyDrop<Option<IPropertyStore>>,
    //   pReserved, pDeviceCollection }——端点 GUID 经 pAPOSystemEffectsProperties
    //   ->IPropertyStore::GetValue(PKEY_AudioEndpoint_GUID) 提取（windows-rs 0.62.2 实测）。
    APOInitSystemEffects,
};

// APOInit 基础结构（Initialize 参数校验 cb_size 用）
pub use windows::Win32::Media::Audio::Apo::APOInitBaseStruct;

// PKEY_AudioEndpoint_GUID（端点 GUID 属性键，Windows SDK 已提供）
pub use windows::Win32::Media::Audio::PKEY_AudioEndpoint_GUID;
pub use windows::Win32::Media::Audio::{WAVEFORMATEX, WAVEFORMATEXTENSIBLE};

// IPropertyStore（端点属性查询，IPropertyStore::GetValue 提取端点 GUID）
pub use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

// PROPVARIANT（IPropertyStore::GetValue 返回，含 GUID 类型 VT_CLSID）
pub use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
pub use windows::Win32::System::Variant::VARENUM;
pub use windows::Win32::System::Variant::{VT_CLSID, VT_LPWSTR, VT_BSTR};

/// windows-rs 的 APO_BUFFER_FLAGS 别名（对外交互用）。
pub use windows::Win32::Media::Audio::Apo::APO_BUFFER_FLAGS as WinAPO_BUFFER_FLAGS;

// APO_BUFFER_FLAGS 关联常量（windows-rs 命名），供内部语义引用
pub use windows::Win32::Media::Audio::Apo::{
    BUFFER_INVALID,
    BUFFER_VALID,
    BUFFER_SILENT,
};

// ── 自定义枚举（windows-rs 无） ────────────────────────────────────────────
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AUDIO_FLOW_TYPE {
    PULL = 0,
    PUSH = 1,
}

// ── 类型别名 ────────────────────────────────────────────────────────────────
pub type REFERENCE_TIME = i64;

// ── UNCOMPRESSED_AUDIO_FORMAT（windows-rs 无，自定义） ──────────────────────
#[repr(C)]
#[derive(Clone, Copy)]
pub struct UNCOMPRESSED_AUDIO_FORMAT {
    pub guid_format_type: GUID,
    pub dw_samples_per_frame: u32,
    pub dw_bytes_per_sample_container: u32,
    pub dw_valid_bits_per_sample: u32,
    pub f_frames_per_second: f32,
    pub dw_channel_mask: u32,
}

// ── 签名常量 ────────────────────────────────────────────────────────────────
pub const APO_CONNECTION_DESCRIPTOR_SIGNATURE: u32 = u32::from_le_bytes(*b"ACDS");
pub const APO_CONNECTION_PROPERTY_SIGNATURE: u32 = u32::from_le_bytes(*b"ACPS");
pub const APO_CONNECTION_PROPERTY_V2_SIGNATURE: u32 = u32::from_le_bytes(*b"ACP2");

// ── 比较标志常量 ────────────────────────────────────────────────────────────
pub const AUDIOMEDIATYPE_EQUAL_FORMAT_TYPES: u32 = 0x0000_0002;
pub const AUDIOMEDIATYPE_EQUAL_FORMAT_DATA: u32 = 0x0000_0004;
pub const AUDIOMEDIATYPE_EQUAL_FORMAT_USER_DATA: u32 = 0x0000_0008;

// ── APO 专用 HRESULT 错误码（定义集中在 prelude.rs，此处仅重导出） ────────
pub use crate::sys::com::prelude::{
    APOERR_ALREADY_INITIALIZED,
    APOERR_ALREADY_UNLOCKED,
    APOERR_APO_LOCKED,
    APOERR_BUFFERS_OVERLAP,
    APOERR_FORMAT_NOT_SUPPORTED,
    APOERR_INVALID_APO_CLSID,
    APOERR_INVALID_COEFFCOUNT,
    APOERR_INVALID_COEFFICIENT,
    APOERR_INVALID_CONNECTION_FORMAT,
    APOERR_INVALID_CURVE_PARAM,
    APOERR_INVALID_INPUTID,
    APOERR_INVALID_OUTPUT_MAXFRAMECOUNT,
    APOERR_NOT_INITIALIZED,
    APOERR_NUM_CONNECTIONS_INVALID,
};

// ── 编译期断言（验证 re-export 的 SDK 类型布局） ───────────────────────────
const _: () = {
    assert!(std::mem::size_of::<APO_FLAG>() == 4, "APO_FLAG must be 4 bytes");
    assert!(std::mem::size_of::<AUDIO_FLOW_TYPE>() == 4, "AUDIO_FLOW_TYPE must be 4 bytes");
    assert!(std::mem::size_of::<UNCOMPRESSED_AUDIO_FORMAT>() == 36, "UNCOMPRESSED_AUDIO_FORMAT must be 36 bytes");
    assert!(std::mem::size_of::<APO_REG_PROPERTIES>() == 1092, "APO_REG_PROPERTIES must be 1092 bytes");
};
