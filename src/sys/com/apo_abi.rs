//! sys/com/apo_abi.rs — APO COM 接口 ABI 定义（Phase 1）
//!
//! 定义 Windows Audio APO 所需的三个核心 COM 接口及配套 POD 类型。
//! 所有结构体采用 `#[repr(C)]` 布局，函数指针顺序与 Windows ABI 严格一致（Note 1）。
//!
//! 此模块仅提供类型与常量声明，不包含任何实现逻辑。

use windows::core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface, interface};

// ══════════════════════════════════════════════════════════════════════════════
// COM 接口定义
//
// 使用 windows-rs 的 #[interface] 宏自动生成 vtable（Note 1/3）。
// 宏会自动为 Trait 关联全局常量 `IID: GUID`。
// ══════════════════════════════════════════════════════════════════════════════

/// 音频媒体类型接口 — 格式协商用。
///
/// `GetAudioFormat()` 返回底层 `WAVEFORMATEX` 的只读指针。
#[interface("4e997f73-b71f-4798-873b-ed7dfcf15b4d")]
pub unsafe trait IAudioMediaType: IUnknown {
    /// 判断是否为压缩格式。结果写入 `*pf_compressed`（`BOOL` 用 `u32`）。
    fn IsCompressedFormat(&self, pf_compressed: *mut u32) -> HRESULT;
    /// 比较两个媒体类型，结果写入 `*pdw_flags`。
    fn IsEqual(&self, p_type: *mut IAudioMediaType, pdw_flags: *mut u32) -> HRESULT;
    /// 返回只读 `WAVEFORMATEX*`。生命周期由 COM 对象管理。
    fn GetAudioFormat(&self) -> *const std::ffi::c_void;
    /// 获取未压缩格式描述。仅当 `IsCompressedFormat` 返回 TRUE 时调用。
    fn GetUncompressedAudioFormat(&self, p_format: *mut UNCOMPRESSED_AUDIO_FORMAT) -> HRESULT;
}

/// 基础 APO 接口 — 注册、格式协商、延迟查询。
#[interface("fd7f2b29-24d0-4b5c-b177-592c39f9ca10")]
pub unsafe trait IAudioProcessingObject: IUnknown {
    /// 重置 APO 内部状态。
    fn Reset(&self) -> HRESULT;
    /// 获取 APO 引入的延迟（单位：`REFERENCE_TIME` = 100ns）。
    fn GetLatency(&self, p_latency: *mut REFERENCE_TIME) -> HRESULT;
    /// 获取 APO 注册属性。调用方负责 `CoTaskMemFree` 释放。
    fn GetRegistrationProperties(&self, pp_props: *mut *mut APO_REG_PROPERTIES) -> HRESULT;
    /// 初始化 APO。引擎实例化后调用，传入初始化数据。
    fn Initialize(&self, cb_data_size: u32, pby_data: *mut u8) -> HRESULT;
    /// 查询输入格式支持。返回 `S_FALSE` 时表示返回替代格式。
    fn IsInputFormatSupported(
        &self,
        p_opposite_format: *mut IAudioMediaType,
        p_requested: *mut IAudioMediaType,
        pp_supported: *mut *mut IAudioMediaType,
    ) -> HRESULT;
    /// 查询输出格式支持。返回 `S_FALSE` 时表示返回替代格式。
    fn IsOutputFormatSupported(
        &self,
        p_opposite_format: *mut IAudioMediaType,
        p_requested: *mut IAudioMediaType,
        pp_supported: *mut *mut IAudioMediaType,
    ) -> HRESULT;
    /// 获取输入通道数。
    fn GetInputChannelCount(&self, p_count: *mut u32) -> HRESULT;
}

/// 实时处理接口 — `APOProcess` 在多媒体实时线程上调用。
///
/// `APOProcess` 返回 `void`（无 HRESULT）——实时路径不允许错误传播（Note 12/57）。
#[interface("9e1d6a6d-ddbc-4e95-a4c7-ad64ba37846c")]
pub unsafe trait IAudioProcessingObjectRT: IUnknown {
    /// 处理一帧音频。输入输出均为 `APO_CONNECTION_PROPERTY` 指针数组。
    fn APOProcess(
        &self,
        num_input: u32,
        pp_inputs: *mut *mut APO_CONNECTION_PROPERTY,
        num_output: u32,
        pp_outputs: *mut *mut APO_CONNECTION_PROPERTY,
    );
    /// 给定输出帧数，计算需要的输入帧数。
    fn CalcInputFrames(&self, output_frames: u32) -> u32;
    /// 给定输入帧数，计算可产生的输出帧数。
    fn CalcOutputFrames(&self, input_frames: u32) -> u32;
}

/// 配置接口 — 锁定/解锁处理流程。
///
/// `LockForProcess` 在格式协商完成后调用，将连接描述符固化；
/// `UnlockForProcess` 释放锁定状态。
#[interface("0e5ed805-aba6-49c3-8f9a-2b8c889c4fa8")]
pub unsafe trait IAudioProcessingObjectConfiguration: IUnknown {
    /// 锁定处理流程，传入输入输出连接描述符。
    fn LockForProcess(
        &self,
        num_input: u32,
        pp_inputs: *mut *mut APO_CONNECTION_DESCRIPTOR,
        num_output: u32,
        pp_outputs: *mut *mut APO_CONNECTION_DESCRIPTOR,
    ) -> HRESULT;
    /// 解锁处理流程。
    fn UnlockForProcess(&self) -> HRESULT;
}

// ══════════════════════════════════════════════════════════════════════════════
// 接口 GUID 导出常量（直接绑定 Trait::IID）
// ══════════════════════════════════════════════════════════════════════════════

/// `IAudioProcessingObject` — 基础 APO 接口 IID
pub const IID_IAPO: GUID = IAudioProcessingObject::IID;

/// `IAudioProcessingObjectRT` — 实时处理接口 IID
pub const IID_IAPO_RT: GUID = IAudioProcessingObjectRT::IID;

/// `IAudioProcessingObjectConfiguration` — 配置接口 IID
pub const IID_IAPO_CONFIG: GUID = IAudioProcessingObjectConfiguration::IID;

/// `IAudioMediaType` — 音频媒体类型（格式协商）IID
pub const IID_IAUDIO_MEDIA_TYPE: GUID = IAudioMediaType::IID;

// ══════════════════════════════════════════════════════════════════════════════
// 类型别名
// ══════════════════════════════════════════════════════════════════════════════

/// 100 纳秒时间单位，`GetLatency` 的输出格式。
pub type REFERENCE_TIME = i64;

// ══════════════════════════════════════════════════════════════════════════════
// APO_FLAG — APO 注册标志位
// ══════════════════════════════════════════════════════════════════════════════

/// APO 注册标志位（bitfield）。
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct APO_FLAG(pub u32);

impl APO_FLAG {
    pub const NONE: Self = Self(0x0000_0000);
    /// 支持输入输出同缓冲区（Note 13b：对外声明，引擎内部另作判断）
    pub const INPLACE: Self = Self(0x0000_0001);
    /// 每帧采样数必须匹配
    pub const SAMPLESPERFRAME_MUST_MATCH: Self = Self(0x0000_0002);
    /// 采样率必须匹配
    pub const FRAMESPERSECOND_MUST_MATCH: Self = Self(0x0000_0004);
    /// 位深必须匹配
    pub const BITSPERSAMPLE_MUST_MATCH: Self = Self(0x0000_0008);
    /// 混音器 APO
    pub const MIXER: Self = Self(0x0000_0010);
    /// 默认标志（SAMPLESPERFRAME | FRAMESPERSECOND | BITSPERSAMPLE = 0xE）
    pub const DEFAULT: Self = Self(0x0000_000E);
}

impl std::ops::BitOr for APO_FLAG {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for APO_FLAG {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl std::ops::BitAnd for APO_FLAG {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl std::ops::BitAndAssign for APO_FLAG {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 枚举类型
// ══════════════════════════════════════════════════════════════════════════════

/// APO 音频流方向。
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AUDIO_FLOW_TYPE {
    /// 渲染（回放）路径 — 拉模式
    PULL = 0,
    /// 捕获路径 — 推模式
    PUSH = 1,
}

/// APO 连接缓冲区类型（`LockForProcess` 时描述缓冲区分配方式）。
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum APO_CONNECTION_BUFFER_TYPE {
    /// 由 APO 引擎分配的缓冲区
    ALLOCATED = 0,
    /// 外部提供的缓冲区
    EXTERNAL = 1,
    /// 依赖型缓冲区
    DEPENDANT = 2,
}

// ══════════════════════════════════════════════════════════════════════════════
// 缓冲区标志位常量（APO_CONNECTION_PROPERTY::buffer_flags，Note 11）
// ══════════════════════════════════════════════════════════════════════════════

/// 缓冲区无效（未初始化）。
pub const BUFFER_INVALID: u32 = 0x00;
/// 缓冲区包含有效音频数据。
pub const BUFFER_VALID: u32 = 0x01;
/// 缓冲区为静音。
pub const BUFFER_SILENT: u32 = 0x02;

// ══════════════════════════════════════════════════════════════════════════════
// 缓冲区签名常量（APO_CONNECTION_DESCRIPTOR / APO_CONNECTION_PROPERTY 验证用）
// ══════════════════════════════════════════════════════════════════════════════

/// `APO_CONNECTION_DESCRIPTOR` 签名（'ACDS'）。
pub const APO_CONNECTION_DESCRIPTOR_SIGNATURE: u32 = u32::from_le_bytes(*b"ACDS");
/// `APO_CONNECTION_PROPERTY` 签名（'ACPS'）。
pub const APO_CONNECTION_PROPERTY_SIGNATURE: u32 = u32::from_le_bytes(*b"ACPS");
/// `APO_CONNECTION_PROPERTY` V2 签名（'ACP2'）。
pub const APO_CONNECTION_PROPERTY_V2_SIGNATURE: u32 = u32::from_le_bytes(*b"ACP2");

// ══════════════════════════════════════════════════════════════════════════════
// 比较标志常量（IAudioMediaType::IsEqual 返回值解释）
// ══════════════════════════════════════════════════════════════════════════════

/// 格式类型 GUID 相等。
pub const AUDIOMEDIATYPE_EQUAL_FORMAT_TYPES: u32 = 0x0000_0002;
/// 格式数据相等。
pub const AUDIOMEDIATYPE_EQUAL_FORMAT_DATA: u32 = 0x0000_0004;
/// 用户数据相等。
pub const AUDIOMEDIATYPE_EQUAL_FORMAT_USER_DATA: u32 = 0x0000_0008;

// ══════════════════════════════════════════════════════════════════════════════
// APO 专用 HRESULT（facility 0x87D）
//
// 注意：HRESULT::message() 涉及系统调用和堆分配，
// 禁止在 APOProcess 实时路径中使用。实时路径中仅使用 is_ok() / is_err() 进行判断。
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
// POD 结构体
// ══════════════════════════════════════════════════════════════════════════════

/// APO 注册属性（`GetRegistrationProperties` 返回值）。
///
/// 对应 `com/reg_props.rs` 中的实际属性实例（Note 42）。
#[repr(C)]
#[derive(Clone)]
pub struct APO_REG_PROPERTIES {
    pub clsid: GUID,
    pub flags: APO_FLAG,
    /// APO 名称（UTF-16，最多 255 字符 + null）
    pub sz_name: [u16; 256],
    /// 版权信息（UTF-16，最多 255 字符 + null）
    pub sz_copyright: [u16; 256],
    pub major_version: u32,
    pub minor_version: u32,
    pub min_input_connections: u32,
    pub max_input_connections: u32,
    pub min_output_connections: u32,
    pub max_output_connections: u32,
    pub max_instances: u32,
    /// APO 支持的 COM 接口数量
    pub num_apo_interfaces: u32,
    /// APO 支持的 COM 接口 IID 列表
    pub iid_apo_interface_list: [GUID; 1],
}

/// APO 连接描述符（`LockForProcess` 参数）。
///
/// 描述一个输入或输出连接的缓冲区、帧数上限和格式。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct APO_CONNECTION_DESCRIPTOR {
    pub buffer_type: APO_CONNECTION_BUFFER_TYPE,
    /// 缓冲区指针（`UINT_PTR`，用 `usize` 匹配平台位宽）
    pub buffer: usize,
    pub max_frame_count: u32,
    /// `IAudioMediaType*`（COM 接口指针，用 `*mut c_void` 避免循环依赖）
    pub format: *mut std::ffi::c_void,
    pub signature: u32,
}

/// APO 连接属性（`APOProcess` 参数）。
///
/// `buffer_flags` 使用 `BUFFER_INVALID` / `BUFFER_VALID` / `BUFFER_SILENT`（Note 11）。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct APO_CONNECTION_PROPERTY {
    /// 缓冲区指针（`UINT_PTR`）
    pub p_buffer: usize,
    /// 有效帧数
    pub valid_frame_count: u32,
    /// 缓冲区标志（`APO_BUFFER_FLAGS`）
    pub buffer_flags: u32,
    /// 结构体签名（`APO_CONNECTION_PROPERTY_SIGNATURE` 或 `V2`）
    pub signature: u32,
}

/// 未压缩音频格式描述（`GetUncompressedAudioFormat` 输出结构）。
///
/// `fFramesPerSecond` 是 `f32`（`FLOAT`），不是 `f64`。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct UNCOMPRESSED_AUDIO_FORMAT {
    /// 格式类型 GUID（如 `KSDATAFORMAT_SUBTYPE_PCM`）
    pub guid_format_type: GUID,
    /// 每帧采样数
    pub dw_samples_per_frame: u32,
    /// 每个样本容器字节数
    pub dw_bytes_per_sample_container: u32,
    /// 每个样本有效位数
    pub dw_valid_bits_per_sample: u32,
    /// 采样率（`FLOAT` = `f32`）
    pub f_frames_per_second: f32,
    /// 声道掩码
    pub dw_channel_mask: u32,
}

// ══════════════════════════════════════════════════════════════════════════════
// 编译期断言（Note 1）
//
// 精确值断言 + offset_of 偏移校验，确保 ABI 布局与 Windows SDK 完全一致。
// 含指针的结构体通过 cfg(target_pointer_width) 区分 32/64 位预期值。
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

    // ── 枚举 repr 语义（确保 repr(i32) 生效，C 侧期望有符号 32 位值）──
    assert!(APO_CONNECTION_BUFFER_TYPE::ALLOCATED as i32 == 0);
    assert!(APO_CONNECTION_BUFFER_TYPE::EXTERNAL as i32 == 1);
    assert!(APO_CONNECTION_BUFFER_TYPE::DEPENDANT as i32 == 2);

    // ── UNCOMPRESSED_AUDIO_FORMAT（无指针，跨平台一致）────────────────────
    // GUID(16) + 4×u32(16) + f32(4) + u32(4) = 36
    assert!(
        std::mem::size_of::<UNCOMPRESSED_AUDIO_FORMAT>() == 36,
        "UNCOMPRESSED_AUDIO_FORMAT must be 36 bytes"
    );
    assert!(std::mem::offset_of!(UNCOMPRESSED_AUDIO_FORMAT, guid_format_type) == 0);
    assert!(std::mem::offset_of!(UNCOMPRESSED_AUDIO_FORMAT, dw_samples_per_frame) == 16);
    assert!(
        std::mem::offset_of!(UNCOMPRESSED_AUDIO_FORMAT, dw_bytes_per_sample_container) == 20
    );
    assert!(
        std::mem::offset_of!(UNCOMPRESSED_AUDIO_FORMAT, dw_valid_bits_per_sample) == 24
    );
    assert!(std::mem::offset_of!(UNCOMPRESSED_AUDIO_FORMAT, f_frames_per_second) == 28);
    assert!(std::mem::offset_of!(UNCOMPRESSED_AUDIO_FORMAT, dw_channel_mask) == 32);

    // ── APO_REG_PROPERTIES（无指针，跨平台一致）───────────────────────────
    // GUID(16) + FLAG(4) + 2×[u16;256](1024) + 8×u32(32) + IID[1](16) = 1092
    // GUID 对齐为 4（内部最大成员 u32），1092 % 4 == 0，无需尾部填充。
    assert!(
        std::mem::size_of::<APO_REG_PROPERTIES>() == 1092,
        "APO_REG_PROPERTIES must be 1092 bytes"
    );
    assert!(std::mem::offset_of!(APO_REG_PROPERTIES, clsid) == 0);
    assert!(std::mem::offset_of!(APO_REG_PROPERTIES, flags) == 16);
    assert!(std::mem::offset_of!(APO_REG_PROPERTIES, sz_name) == 20);
    assert!(std::mem::offset_of!(APO_REG_PROPERTIES, sz_copyright) == 532);
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
        // APO_CONNECTION_DESCRIPTOR (40 bytes on x64):
        //   buffer_type[0..4] + pad[4..8] + buffer[8..16] + max_frame_count[16..20]
        //   + pad[20..24] + format[24..32] + signature[32..36] + tail_pad[36..40]
        assert!(
            std::mem::size_of::<APO_CONNECTION_DESCRIPTOR>() == 40,
            "APO_CONNECTION_DESCRIPTOR must be 40 bytes on x64"
        );
        assert!(std::mem::offset_of!(APO_CONNECTION_DESCRIPTOR, buffer_type) == 0);
        assert!(std::mem::offset_of!(APO_CONNECTION_DESCRIPTOR, buffer) == 8);
        assert!(std::mem::offset_of!(APO_CONNECTION_DESCRIPTOR, max_frame_count) == 16);
        assert!(std::mem::offset_of!(APO_CONNECTION_DESCRIPTOR, format) == 24);
        assert!(std::mem::offset_of!(APO_CONNECTION_DESCRIPTOR, signature) == 32);

        // APO_CONNECTION_PROPERTY (24 bytes on x64):
        //   p_buffer[0..8] + valid_frame_count[8..12] + buffer_flags[12..16] + signature[16..20] + tail_pad[20..24]
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
        // APO_CONNECTION_DESCRIPTOR (20 bytes on x32):
        //   buffer_type[0..4] + buffer[4..8] + max_frame_count[8..12]
        //   + format[12..16] + signature[16..20]
        assert!(
            std::mem::size_of::<APO_CONNECTION_DESCRIPTOR>() == 20,
            "APO_CONNECTION_DESCRIPTOR must be 20 bytes on x32"
        );

        // APO_CONNECTION_PROPERTY (16 bytes on x32):
        //   p_buffer[0..4] + valid_frame_count[4..8] + buffer_flags[8..12] + signature[12..16]
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

    // ── GUID 绑定的直接一致性测试 ─────────────────────────────────────────────

    #[test]
    fn interface_guids_match_constants() {
        assert_eq!(IID_IAPO, IAudioProcessingObject::IID);
        assert_eq!(IID_IAPO_RT, IAudioProcessingObjectRT::IID);
        assert_eq!(IID_IAPO_CONFIG, IAudioProcessingObjectConfiguration::IID);
        assert_eq!(IID_IAUDIO_MEDIA_TYPE, IAudioMediaType::IID);
    }

    // ── APO_FLAG 位操作 ──────────────────────────────────────────────────────

    #[test]
    fn apo_flag_bitor() {
        let flags = APO_FLAG::INPLACE | APO_FLAG::FRAMESPERSECOND_MUST_MATCH;
        assert_eq!(flags.0, 0x0000_0001 | 0x0000_0004);
    }

    #[test]
    fn apo_flag_bitand() {
        let flags = APO_FLAG::INPLACE | APO_FLAG::BITSPERSAMPLE_MUST_MATCH;
        assert!(flags & APO_FLAG::INPLACE != APO_FLAG::NONE);
        assert!(flags & APO_FLAG::MIXER == APO_FLAG::NONE);
    }

    #[test]
    fn apo_flag_bitor_assign() {
        let mut flags = APO_FLAG::NONE;
        flags |= APO_FLAG::INPLACE;
        flags |= APO_FLAG::SAMPLESPERFRAME_MUST_MATCH;
        assert_eq!(flags.0, 0x0000_0003);
    }

    #[test]
    fn apo_flag_bitand_assign() {
        let mut flags = APO_FLAG::INPLACE | APO_FLAG::BITSPERSAMPLE_MUST_MATCH;
        flags &= APO_FLAG::INPLACE;
        assert_eq!(flags, APO_FLAG::INPLACE);
    }

    #[test]
    fn apo_flag_default_value() {
        // DEFAULT = SAMPLESPERFRAME(0x2) | FRAMESPERSECOND(0x4) | BITSPERSAMPLE(0x8) = 0xE
        assert_eq!(APO_FLAG::DEFAULT.0, 0x0000_000E);
    }

    // ── 枚举值验证 ──────────────────────────────────────────────────────────

    #[test]
    fn audio_flow_type_values() {
        assert_eq!(AUDIO_FLOW_TYPE::PULL as u32, 0);
        assert_eq!(AUDIO_FLOW_TYPE::PUSH as u32, 1);
    }

    #[test]
    fn apo_connection_buffer_type_values() {
        assert_eq!(APO_CONNECTION_BUFFER_TYPE::ALLOCATED as i32, 0);
        assert_eq!(APO_CONNECTION_BUFFER_TYPE::EXTERNAL as i32, 1);
        assert_eq!(APO_CONNECTION_BUFFER_TYPE::DEPENDANT as i32, 2);
    }

    // ── 缓冲区标志位 ────────────────────────────────────────────────────────

    #[test]
    fn buffer_flags_values() {
        assert_eq!(BUFFER_INVALID, 0x00);
        assert_eq!(BUFFER_VALID, 0x01);
        assert_eq!(BUFFER_SILENT, 0x02);
    }

    // ── 缓冲区签名常量 ──────────────────────────────────────────────────────

    #[test]
    fn signature_constants() {
        assert_eq!(APO_CONNECTION_DESCRIPTOR_SIGNATURE, u32::from_le_bytes(*b"ACDS"));
        assert_eq!(APO_CONNECTION_PROPERTY_SIGNATURE, u32::from_le_bytes(*b"ACPS"));
        assert_eq!(APO_CONNECTION_PROPERTY_V2_SIGNATURE, u32::from_le_bytes(*b"ACP2"));
    }

    // ── 比较标志常量 ────────────────────────────────────────────────────────

    #[test]
    fn comparison_flag_values() {
        assert_eq!(AUDIOMEDIATYPE_EQUAL_FORMAT_TYPES, 0x0000_0002);
        assert_eq!(AUDIOMEDIATYPE_EQUAL_FORMAT_DATA, 0x0000_0004);
        assert_eq!(AUDIOMEDIATYPE_EQUAL_FORMAT_USER_DATA, 0x0000_0008);
        // 完全相等时三个标志全部设置 = 0xE
        let all_equal = AUDIOMEDIATYPE_EQUAL_FORMAT_TYPES
            | AUDIOMEDIATYPE_EQUAL_FORMAT_DATA
            | AUDIOMEDIATYPE_EQUAL_FORMAT_USER_DATA;
        assert_eq!(all_equal, 0x0000_000E);
    }

    // ── 结构体布局验证（平台感知）───────────────────────────────────────────

    #[test]
    fn apo_connection_descriptor_size() {
        let expected = if cfg!(target_pointer_width = "64") { 40 } else { 20 };
        assert_eq!(std::mem::size_of::<APO_CONNECTION_DESCRIPTOR>(), expected);
    }

    #[test]
    fn apo_connection_property_size() {
        let expected = if cfg!(target_pointer_width = "64") { 24 } else { 16 };
        assert_eq!(std::mem::size_of::<APO_CONNECTION_PROPERTY>(), expected);
    }

    #[test]
    fn uncompressed_audio_format_size() {
        // GUID(16) + u32(4)×4 + f32(4) = 36 字节
        assert_eq!(std::mem::size_of::<UNCOMPRESSED_AUDIO_FORMAT>(), 36);
    }

    #[test]
    fn apo_reg_properties_size() {
        // GUID(16) + FLAG(4) + 2×[u16;256](1024) + 8×u32(32) + IID[1](16) = 1092
        assert_eq!(std::mem::size_of::<APO_REG_PROPERTIES>(), 1092);
    }

    // ── REFERENCE_TIME 别名 ─────────────────────────────────────────────────

    #[test]
    fn reference_time_is_i64() {
        let t: REFERENCE_TIME = 10_000; // 1ms
        assert_eq!(t, 10_000i64);
    }
}