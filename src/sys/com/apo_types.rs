//! sys/com/apo_types.rs — APO 类型（v6.2 规范 3.3，修正版）
//!
//! 修正：windows-rs 0.62.2 已提供 APO_REG_PROPERTIES / APO_CONNECTION_DESCRIPTOR /
//! APO_CONNECTION_PROPERTY / APO_FLAG / APO_BUFFER_FLAGS，此处全部 re-export。
//! 仅保留 windows-rs 缺失的自定义项：UNCOMPRESSED_AUDIO_FORMAT、AUDIO_FLOW_TYPE、
//! REFERENCE_TIME、签名常量、比较标志、APOERR 错误码、编译期断言。

pub use windows::core::GUID;
pub use windows::core::HRESULT;

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
};

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

// ── APO 专用 HRESULT 错误码 ────────────────────────────────────────────────
pub const APOERR_ALREADY_INITIALIZED:          HRESULT = HRESULT(0x887D_0001u32 as i32);
pub const APOERR_NOT_INITIALIZED:              HRESULT = HRESULT(0x887D_0002u32 as i32);
pub const APOERR_FORMAT_NOT_SUPPORTED:         HRESULT = HRESULT(0x887D_0003u32 as i32);
pub const APOERR_INVALID_APO_CLSID:            HRESULT = HRESULT(0x887D_0004u32 as i32);
pub const APOERR_BUFFERS_OVERLAP:              HRESULT = HRESULT(0x887D_0005u32 as i32);
pub const APOERR_ALREADY_UNLOCKED:             HRESULT = HRESULT(0x887D_0006u32 as i32);
pub const APOERR_NUM_CONNECTIONS_INVALID:      HRESULT = HRESULT(0x887D_0007u32 as i32);
pub const APOERR_INVALID_OUTPUT_MAXFRAMECOUNT: HRESULT = HRESULT(0x887D_0008u32 as i32);
pub const APOERR_INVALID_CONNECTION_FORMAT:    HRESULT = HRESULT(0x887D_0009u32 as i32);
pub const APOERR_APO_LOCKED:                   HRESULT = HRESULT(0x887D_000Au32 as i32);
pub const APOERR_INVALID_COEFFCOUNT:           HRESULT = HRESULT(0x887D_000Bu32 as i32);
pub const APOERR_INVALID_COEFFICIENT:          HRESULT = HRESULT(0x887D_000Cu32 as i32);
pub const APOERR_INVALID_CURVE_PARAM:          HRESULT = HRESULT(0x887D_000Du32 as i32);
pub const APOERR_INVALID_INPUTID:              HRESULT = HRESULT(0x887D_000Eu32 as i32);

// ── 编译期断言（验证 re-export 的 SDK 类型布局） ───────────────────────────
const _: () = {
    assert!(std::mem::size_of::<APO_FLAG>() == 4, "APO_FLAG must be 4 bytes");
    assert!(std::mem::size_of::<AUDIO_FLOW_TYPE>() == 4, "AUDIO_FLOW_TYPE must be 4 bytes");
    assert!(std::mem::size_of::<UNCOMPRESSED_AUDIO_FORMAT>() == 36, "UNCOMPRESSED_AUDIO_FORMAT must be 36 bytes");
    assert!(std::mem::size_of::<APO_REG_PROPERTIES>() == 1092, "APO_REG_PROPERTIES must be 1092 bytes");
};