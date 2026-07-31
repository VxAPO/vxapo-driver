//! sys/com/apo_types.rs — APO POD 结构体 + 枚举 + SDK 类型重导出 + 常量（v6.2 规范 3.3）
//!
//! 职责：定义 `#[repr(C)]` POD 结构体、枚举、常量，以及从 `windows-rs` 重导出的 SDK 类型。
//!
//! 引用来源：
//! - `windows::core::{GUID, HRESULT}`
//! - `windows::Win32::Media::Audio::Apo::APO_FLAG`（及全部关联常量）
//! - `windows::Win32::Media::Audio::Apo::APO_BUFFER_FLAGS as WinAPO_BUFFER_FLAGS`
//!
//! 导出给：`sys/com/apo_interfaces.rs`、`pipeline/`、`install/`、`object/`、`config/`。

pub use windows::core::GUID;
pub use windows::core::HRESULT;

// ══════════════════════════════════════════════════════════════════════════════
// 3.3.1 从 windows-rs 重导出的类型
// ══════════════════════════════════════════════════════════════════════════════

/// `APO_FLAG`——直接使用 SDK 类型（`repr(transparent)` 包装 `i32`）。
pub use windows::Win32::Media::Audio::Apo::{
    APO_FLAG,
    APO_FLAG_NONE,
    APO_FLAG_INPLACE,
    APO_FLAG_SAMPLESPERFRAME_MUST_MATCH,
    APO_FLAG_FRAMESPERSECOND_MUST_MATCH,
    APO_FLAG_BITSPERSAMPLE_MUST_MATCH,
    APO_FLAG_MIXER,
    APO_FLAG_DEFAULT,
};

/// `WinAPO_BUFFER_FLAGS`——windows-rs 类型，供对外交互时使用。
pub use windows::Win32::Media::Audio::Apo::APO_BUFFER_FLAGS as WinAPO_BUFFER_FLAGS;

// ══════════════════════════════════════════════════════════════════════════════
// 3.3.2 自定义枚举
// ══════════════════════════════════════════════════════════════════════════════

/// `APO_BUFFER_FLAGS`——内部使用的 Rust 枚举。
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum APO_BUFFER_FLAGS {
    Invalid = 0,
    Valid = 1,
    Silent = 2,
}

/// 自定义枚举 → windows-rs 类型。
impl From<APO_BUFFER_FLAGS> for WinAPO_BUFFER_FLAGS {
    fn from(f: APO_BUFFER_FLAGS) -> Self {
        Self(f as i32)
    }
}

/// windows-rs 类型 → 自定义枚举。
impl TryFrom<WinAPO_BUFFER_FLAGS> for APO_BUFFER_FLAGS {
    type Error = ();

    fn try_from(f: WinAPO_BUFFER_FLAGS) -> Result<Self, Self::Error> {
        match f.0 {
            0 => Ok(Self::Invalid),
            1 => Ok(Self::Valid),
            2 => Ok(Self::Silent),
            _ => Err(()),
        }
    }
}

/// `AUDIO_FLOW_TYPE`——音频流方向。
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AUDIO_FLOW_TYPE {
    PULL = 0,
    PUSH = 1,
}

/// `APO_CONNECTION_BUFFER_TYPE`——连接缓冲区类型。
///
/// **注意**：使用 `#[repr(i32)]`（有符号），与 C 的 `int` 底层语义一致。
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum APO_CONNECTION_BUFFER_TYPE {
    ALLOCATED = 0,
    EXTERNAL = 1,
    DEPENDANT = 2,
}

// ══════════════════════════════════════════════════════════════════════════════
// 3.3.3 类型别名
// ══════════════════════════════════════════════════════════════════════════════

/// 100 纳秒时间单位，`GetLatency` 的输出格式。
pub type REFERENCE_TIME = i64;

// ══════════════════════════════════════════════════════════════════════════════
// 3.3.4 POD 结构体
// ══════════════════════════════════════════════════════════════════════════════

/// `APO_REG_PROPERTIES`（1092 字节，conformant array 模式）。
#[repr(C)]
#[derive(Clone)]
pub struct APO_REG_PROPERTIES {
    pub clsid: GUID,
    pub flags: APO_FLAG,
    pub sz_friendly_name: [u16; 256],
    pub sz_copyright_info: [u16; 256],
    pub major_version: u32,
    pub minor_version: u32,
    pub min_input_connections: u32,
    pub max_input_connections: u32,
    pub min_output_connections: u32,
    pub max_output_connections: u32,
    pub max_instances: u32,
    pub num_apo_interfaces: u32,
    pub iid_apo_interface_list: [GUID; 1],
}

/// `APO_CONNECTION_DESCRIPTOR`——连接描述符。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct APO_CONNECTION_DESCRIPTOR {
    pub buffer_type: APO_CONNECTION_BUFFER_TYPE,
    pub buffer: usize,
    pub max_frame_count: u32,
    pub format: *mut std::ffi::c_void,
    pub signature: u32,
}

/// `APO_CONNECTION_PROPERTY`——连接属性。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct APO_CONNECTION_PROPERTY {
    pub p_buffer: usize,
    pub valid_frame_count: u32,
    pub buffer_flags: APO_BUFFER_FLAGS,
    pub signature: u32,
}

/// `UNCOMPRESSED_AUDIO_FORMAT`（36 字节，无指针，跨平台一致）。
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

// ══════════════════════════════════════════════════════════════════════════════
// 3.3.5 签名常量
// ══════════════════════════════════════════════════════════════════════════════

pub const APO_CONNECTION_DESCRIPTOR_SIGNATURE: u32 = u32::from_le_bytes(*b"ACDS");
pub const APO_CONNECTION_PROPERTY_SIGNATURE: u32 = u32::from_le_bytes(*b"ACPS");
pub const APO_CONNECTION_PROPERTY_V2_SIGNATURE: u32 = u32::from_le_bytes(*b"ACP2");

// ══════════════════════════════════════════════════════════════════════════════
// 3.3.6 比较标志常量
// ══════════════════════════════════════════════════════════════════════════════

pub const AUDIOMEDIATYPE_EQUAL_FORMAT_TYPES: u32 = 0x0000_0002;
pub const AUDIOMEDIATYPE_EQUAL_FORMAT_DATA: u32 = 0x0000_0004;
pub const AUDIOMEDIATYPE_EQUAL_FORMAT_USER_DATA: u32 = 0x0000_0008;

// ══════════════════════════════════════════════════════════════════════════════
// 3.3.7 APO 专用 HRESULT 错误码
//
// 注意：HRESULT::message() 涉及系统调用和堆分配，
// 禁止在 APOProcess 实时路径中使用。实时路径中仅使用 is_ok() / is_err() 判断。
// ══════════════════════════════════════════════════════════════════════════════

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

// ══════════════════════════════════════════════════════════════════════════════
// 3.3.8 编译期断言
// ══════════════════════════════════════════════════════════════════════════════

const _: () = {
    // ── 基础类型大小 ──────────────────────────────────────────────────────
    assert!(std::mem::size_of::<APO_FLAG>() == 4, "APO_FLAG must be 4 bytes");
    assert!(
        std::mem::size_of::<AUDIO_FLOW_TYPE>() == 4,
        "AUDIO_FLOW_TYPE must be 4 bytes"
    );
    assert!(
        std::mem::size_of::<APO_CONNECTION_BUFFER_TYPE>() == 4,
        "APO_CONNECTION_BUFFER_TYPE must be 4 bytes"
    );

    // ── 枚举 repr 语义 ────────────────────────────────────────────────────
    assert!(APO_CONNECTION_BUFFER_TYPE::ALLOCATED as i32 == 0);
    assert!(APO_CONNECTION_BUFFER_TYPE::EXTERNAL as i32 == 1);
    assert!(APO_CONNECTION_BUFFER_TYPE::DEPENDANT as i32 == 2);

    // ── UNCOMPRESSED_AUDIO_FORMAT（无指针，跨平台一致）────────────────────
    assert!(
        std::mem::size_of::<UNCOMPRESSED_AUDIO_FORMAT>() == 36,
        "UNCOMPRESSED_AUDIO_FORMAT must be 36 bytes"
    );
    assert!(std::mem::offset_of!(UNCOMPRESSED_AUDIO_FORMAT, guid_format_type) == 0);
    assert!(std::mem::offset_of!(UNCOMPRESSED_AUDIO_FORMAT, dw_samples_per_frame) == 16);
    assert!(std::mem::offset_of!(UNCOMPRESSED_AUDIO_FORMAT, dw_bytes_per_sample_container) == 20);
    assert!(std::mem::offset_of!(UNCOMPRESSED_AUDIO_FORMAT, dw_valid_bits_per_sample) == 24);
    assert!(std::mem::offset_of!(UNCOMPRESSED_AUDIO_FORMAT, f_frames_per_second) == 28);
    assert!(std::mem::offset_of!(UNCOMPRESSED_AUDIO_FORMAT, dw_channel_mask) == 32);

    // ── APO_REG_PROPERTIES（无指针，跨平台一致）───────────────────────────
    // GUID(16) + FLAG(4) + 2×[u16;256](1024) + 8×u32(32) + IID[1](16) = 1092
    assert!(
        std::mem::size_of::<APO_REG_PROPERTIES>() == 1092,
        "APO_REG_PROPERTIES must be 1092 bytes"
    );
    assert!(std::mem::offset_of!(APO_REG_PROPERTIES, clsid) == 0);
    assert!(std::mem::offset_of!(APO_REG_PROPERTIES, flags) == 16);
    assert!(std::mem::offset_of!(APO_REG_PROPERTIES, sz_friendly_name) == 20);
    assert!(std::mem::offset_of!(APO_REG_PROPERTIES, sz_copyright_info) == 532);
    assert!(std::mem::offset_of!(APO_REG_PROPERTIES, major_version) == 1044);
    assert!(std::mem::offset_of!(APO_REG_PROPERTIES, minor_version) == 1048);
    assert!(std::mem::offset_of!(APO_REG_PROPERTIES, min_input_connections) == 1052);
    assert!(std::mem::offset_of!(APO_REG_PROPERTIES, max_input_connections) == 1056);
    assert!(std::mem::offset_of!(APO_REG_PROPERTIES, min_output_connections) == 1060);
    assert!(std::mem::offset_of!(APO_REG_PROPERTIES, max_output_connections) == 1064);
    assert!(std::mem::offset_of!(APO_REG_PROPERTIES, max_instances) == 1068);
    assert!(std::mem::offset_of!(APO_REG_PROPERTIES, num_apo_interfaces) == 1072);
    assert!(std::mem::offset_of!(APO_REG_PROPERTIES, iid_apo_interface_list) == 1076);

    // ── 含指针的结构体：平台感知 ──────────────────────────────────────────
    #[cfg(target_pointer_width = "64")]
    {
        // APO_CONNECTION_DESCRIPTOR (40 bytes on x64)
        assert!(
            std::mem::size_of::<APO_CONNECTION_DESCRIPTOR>() == 40,
            "APO_CONNECTION_DESCRIPTOR must be 40 bytes on x64"
        );
        assert!(std::mem::offset_of!(APO_CONNECTION_DESCRIPTOR, buffer_type) == 0);
        assert!(std::mem::offset_of!(APO_CONNECTION_DESCRIPTOR, buffer) == 8);
        assert!(std::mem::offset_of!(APO_CONNECTION_DESCRIPTOR, max_frame_count) == 16);
        assert!(std::mem::offset_of!(APO_CONNECTION_DESCRIPTOR, format) == 24);
        assert!(std::mem::offset_of!(APO_CONNECTION_DESCRIPTOR, signature) == 32);

        // APO_CONNECTION_PROPERTY (24 bytes on x64)
        assert!(
            std::mem::size_of::<APO_CONNECTION_PROPERTY>() == 24,
            "APO_CONNECTION_PROPERTY must be 24 bytes on x64"
        );
        assert!(std::mem::offset_of!(APO_CONNECTION_PROPERTY, p_buffer) == 0);
        assert!(std::mem::offset_of!(APO_CONNECTION_PROPERTY, valid_frame_count) == 8);
        assert!(std::mem::offset_of!(APO_CONNECTION_PROPERTY, buffer_flags) == 12);
        assert!(std::mem::offset_of!(APO_CONNECTION_PROPERTY, signature) == 16);
    }

    #[cfg(target_pointer_width = "32")]
    {
        // APO_CONNECTION_DESCRIPTOR (20 bytes on x32)
        assert!(
            std::mem::size_of::<APO_CONNECTION_DESCRIPTOR>() == 20,
            "APO_CONNECTION_DESCRIPTOR must be 20 bytes on x32"
        );

        // APO_CONNECTION_PROPERTY (16 bytes on x32)
        assert!(
            std::mem::size_of::<APO_CONNECTION_PROPERTY>() == 16,
            "APO_CONNECTION_PROPERTY must be 16 bytes on x32"
        );
        assert!(std::mem::offset_of!(APO_CONNECTION_PROPERTY, p_buffer) == 0);
        assert!(std::mem::offset_of!(APO_CONNECTION_PROPERTY, valid_frame_count) == 4);
        assert!(std::mem::offset_of!(APO_CONNECTION_PROPERTY, buffer_flags) == 8);
        assert!(std::mem::offset_of!(APO_CONNECTION_PROPERTY, signature) == 12);
    }
};

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_flags_conversion_roundtrip() {
        for f in [APO_BUFFER_FLAGS::Invalid, APO_BUFFER_FLAGS::Valid, APO_BUFFER_FLAGS::Silent] {
            let win: WinAPO_BUFFER_FLAGS = f.into();
            let back = APO_BUFFER_FLAGS::try_from(win).unwrap();
            assert_eq!(f, back);
        }
    }

    #[test]
    fn buffer_flags_invalid_rejected() {
        let win = WinAPO_BUFFER_FLAGS(99);
        assert!(APO_BUFFER_FLAGS::try_from(win).is_err());
    }

    #[test]
    fn enum_values() {
        assert_eq!(AUDIO_FLOW_TYPE::PULL as u32, 0);
        assert_eq!(AUDIO_FLOW_TYPE::PUSH as u32, 1);
        assert_eq!(APO_CONNECTION_BUFFER_TYPE::ALLOCATED as i32, 0);
        assert_eq!(APO_CONNECTION_BUFFER_TYPE::EXTERNAL as i32, 1);
        assert_eq!(APO_CONNECTION_BUFFER_TYPE::DEPENDANT as i32, 2);
        assert_eq!(APO_BUFFER_FLAGS::Invalid as u32, 0);
        assert_eq!(APO_BUFFER_FLAGS::Valid as u32, 1);
        assert_eq!(APO_BUFFER_FLAGS::Silent as u32, 2);
    }

    #[test]
    fn signature_constants() {
        assert_eq!(APO_CONNECTION_DESCRIPTOR_SIGNATURE, u32::from_le_bytes(*b"ACDS"));
        assert_eq!(APO_CONNECTION_PROPERTY_SIGNATURE, u32::from_le_bytes(*b"ACPS"));
        assert_eq!(APO_CONNECTION_PROPERTY_V2_SIGNATURE, u32::from_le_bytes(*b"ACP2"));
    }

    #[test]
    fn apoerr_values() {
        assert_eq!(APOERR_ALREADY_INITIALIZED.0, 0x887D_0001u32 as i32);
        assert_eq!(APOERR_FORMAT_NOT_SUPPORTED.0, 0x887D_0003u32 as i32);
        assert_eq!(APOERR_APO_LOCKED.0, 0x887D_000Au32 as i32);
    }

    #[test]
    fn reference_time_is_i64() {
        let t: REFERENCE_TIME = 10_000; // 1ms
        assert_eq!(t, 10_000i64);
    }
}