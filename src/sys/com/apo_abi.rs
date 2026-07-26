//! sys/com/apo_abi.rs — APO COM 接口 ABI 定义（Phase 1）
//!
//! 定义 Windows Audio APO 所需的三个核心 COM 接口及配套 POD 类型。
//! 所有结构体采用 `#[repr(C)]` 布局，函数指针顺序与 Windows ABI 严格一致（Note 1）。
//!
//! 此模块仅提供类型与常量声明，不包含任何实现逻辑。

use windows::core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, interface};

// ══════════════════════════════════════════════════════════════════════════════
// 接口 GUIDs（来自 audioenginebaseapo.h）
// ══════════════════════════════════════════════════════════════════════════════

/// `IAudioProcessingObject` — 基础 APO 接口
pub const IID_IAPO: GUID = GUID::from_values(
    0xFD7F2B29,
    0x24D0,
    0x4B5C,
    [0xB1, 0x77, 0x59, 0x2C, 0x39, 0xF9, 0xCA, 0x10],
);

/// `IAudioProcessingObjectRT` — 实时处理接口
pub const IID_IAPO_RT: GUID = GUID::from_values(
    0x9E1D6A6D,
    0xDDBC,
    0x4E95,
    [0xA4, 0xC7, 0xAD, 0x64, 0xBA, 0x37, 0x84, 0x6C],
);

/// `IAudioProcessingObjectConfiguration` — 配置接口
pub const IID_IAPO_CONFIG: GUID = GUID::from_values(
    0x0E5D4480,
    0x149E,
    0x4842,
    [0xB6, 0xE0, 0x74, 0xB9, 0x0A, 0x48, 0x59, 0xDE],
);

/// `IAudioMediaType` — 音频媒体类型（格式协商）
pub const IID_IAUDIO_MEDIA_TYPE: GUID = GUID::from_values(
    0x4E9966C0,
    0xE244,
    0x4908,
    [0xA5, 0x87, 0x3D, 0x28, 0x8C, 0x22, 0x3F, 0x24],
);

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
    /// 默认标志
    pub const DEFAULT: Self = Self(0x0000_0020);
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

// ══════════════════════════════════════════════════════════════════════════════
// 枚举类型
// ══════════════════════════════════════════════════════════════════════════════

/// APO 音频流方向。
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AUDIO_FLOW_TYPE {
    /// 渲染（回放）路径 — 应用 → 扬声器
    RENDER = 0,
    /// 捕获路径 — 麦克风 → 应用
    CAPTURE = 1,
}

/// APO 连接缓冲区类型（`LockForProcess` 时描述缓冲区状态）。
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum APO_BUFFER_TYPE {
    /// 缓冲区无效
    INVALID = 0,
    /// 常量缓冲区
    CONSTANT = 1,
    /// 静音缓冲区
    SILENT = 2,
    /// 零内存缓冲区
    ZERO_MEMORY = 3,
}

// ══════════════════════════════════════════════════════════════════════════════
// 缓冲区标志位常量（APO_CONNECTION_PROPERTY::flags，Note 11）
// ══════════════════════════════════════════════════════════════════════════════

/// 缓冲区无效（未初始化）。
pub const BUFFER_INVALID: u32 = 0x00;
/// 缓冲区包含有效音频数据。
pub const BUFFER_VALID: u32 = 0x01;
/// 缓冲区为静音。
pub const BUFFER_SILENT: u32 = 0x02;

// ══════════════════════════════════════════════════════════════════════════════
// POD 结构体
// ══════════════════════════════════════════════════════════════════════════════

/// APO 注册属性（`GetRegistrationProperties` 返回值）。
///
/// 对应 `com/reg_props.rs` 中的实际属性实例（Note 42）。
#[repr(C)]
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
    pub audio_flow_type: AUDIO_FLOW_TYPE,
}

/// APO 连接描述符（`LockForProcess` 参数）。
///
/// 描述一个输入或输出连接的缓冲区、帧数上限和格式。
#[repr(C)]
pub struct APO_CONNECTION_DESCRIPTOR {
    pub buffer_type: APO_BUFFER_TYPE,
    /// 缓冲区指针（`UINT_PTR`，用 `usize` 匹配平台位宽）
    pub buffer: usize,
    pub max_frame_count: u32,
    /// `IAudioMediaType*`（COM 接口指针，用 `*mut c_void` 避免循环依赖）
    pub format: *mut std::ffi::c_void,
    pub signature: u32,
}

/// APO 连接属性（`APOProcess` 参数）。
///
/// `flags` 使用 `BUFFER_INVALID` / `BUFFER_VALID` / `BUFFER_SILENT`（Note 11）。
#[repr(C)]
pub struct APO_CONNECTION_PROPERTY {
    /// 缓冲区指针（`UINT_PTR`）
    pub buffer: usize,
    /// 缓冲区大小（字节）
    pub size: u32,
    /// `BUFFER_*` 标志
    pub flags: u32,
}

// ══════════════════════════════════════════════════════════════════════════════
// COM 接口定义
//
// 使用 windows-rs 的 #[interface] 宏自动生成 vtable（Note 1/3）。
// GUID 字符串必须与上方 const 定义一致。
// ══════════════════════════════════════════════════════════════════════════════

/// 音频媒体类型接口 — 格式协商用。
///
/// `GetAudioFormat()` 返回底层 `WAVEFORMATEX` 的只读指针。
#[interface("4e9966c0-e244-4908-a587-3d288c223f24")]
pub unsafe trait IAudioMediaType: IUnknown {
    /// 返回只读 `WAVEFORMATEX*`。生命周期由 COM 对象管理。
    fn GetAudioFormat(&self) -> *const std::ffi::c_void;
    /// 比较两个媒体类型，结果写入 `*pdw_flags`。
    fn IsEqual(&self, p_type: *mut IAudioMediaType, pdw_flags: *mut u32) -> HRESULT;
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
    /// 查询输入格式支持。返回 `S_FALSE` 时表示返回替代格式。
    fn IsInputFormatSupported(
        &self,
        p_output_format: *mut IAudioMediaType,
        p_requested: *mut IAudioMediaType,
        pp_supported: *mut *mut IAudioMediaType,
    ) -> HRESULT;
    /// 查询输出格式支持。返回 `S_FALSE` 时表示返回替代格式。
    fn IsOutputFormatSupported(
        &self,
        p_input_format: *mut IAudioMediaType,
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
        u32_num_input: u32,
        pp_inputs: *mut *mut APO_CONNECTION_PROPERTY,
        u32_num_output: u32,
        pp_outputs: *mut *mut APO_CONNECTION_PROPERTY,
    );
    /// 给定输出帧数，计算需要的输入帧数。
    fn CalcInputFrames(&self, u32_output_frames: u32) -> u32;
    /// 给定输入帧数，计算可产生的输出帧数。
    fn CalcOutputFrames(&self, u32_input_frames: u32) -> u32;
}

/// 配置接口 — 锁定/解锁处理流程。
///
/// `LockForProcess` 在格式协商完成后调用，将连接描述符固化；
/// `UnlockForProcess` 释放锁定状态。
#[interface("0e5d4480-149e-4842-b6e0-74b90a4859de")]
pub unsafe trait IAudioProcessingObjectConfiguration: IUnknown {
    /// 锁定处理流程，传入输入输出连接描述符。
    fn LockForProcess(
        &self,
        u32_num_input: u32,
        pp_inputs: *mut *mut APO_CONNECTION_DESCRIPTOR,
        u32_num_output: u32,
        pp_outputs: *mut *mut APO_CONNECTION_DESCRIPTOR,
    ) -> HRESULT;
    /// 解锁处理流程。
    fn UnlockForProcess(&self) -> HRESULT;
}

// ══════════════════════════════════════════════════════════════════════════════
// 编译期断言（Note 1）
// ══════════════════════════════════════════════════════════════════════════════

const _: () = {
    // POD 类型大小验证
    assert!(std::mem::size_of::<APO_FLAG>() == 4, "APO_FLAG must be 4 bytes");
    assert!(
        std::mem::size_of::<AUDIO_FLOW_TYPE>() == 4,
        "AUDIO_FLOW_TYPE must be 4 bytes"
    );
    assert!(
        std::mem::size_of::<APO_BUFFER_TYPE>() == 4,
        "APO_BUFFER_TYPE must be 4 bytes"
    );
    // APO_REG_PROPERTIES 至少包含 GUID(16) + flags(4) + padding(?) + 2×[u16;256](1024) + 7×u32(28) + flow(4)
    assert!(
        std::mem::size_of::<APO_REG_PROPERTIES>() > 1024,
        "APO_REG_PROPERTIES too small"
    );
    // APO_CONNECTION_DESCRIPTOR 至少包含 enum(4) + ptr(8) + u32(4) + ptr(8) + u32(4) + padding
    assert!(
        std::mem::size_of::<APO_CONNECTION_DESCRIPTOR>() >= 24,
        "APO_CONNECTION_DESCRIPTOR too small"
    );
    // APO_CONNECTION_PROPERTY 至少包含 ptr(8) + u32(4) + u32(4) = 16
    assert!(
        std::mem::size_of::<APO_CONNECTION_PROPERTY>() >= 16,
        "APO_CONNECTION_PROPERTY too small"
    );
};

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── GUID 格式验证 ────────────────────────────────────────────────────────

    #[test]
    fn guid_iapo_is_correct() {
        // FD7F2B29-24D0-4B5C-B177-592C39F9CA10
        assert_eq!(IID_IAPO.data1, 0xFD7F2B29);
        assert_eq!(IID_IAPO.data2, 0x24D0);
        assert_eq!(IID_IAPO.data3, 0x4B5C);
        assert_eq!(IID_IAPO.data4, [0xB1, 0x77, 0x59, 0x2C, 0x39, 0xF9, 0xCA, 0x10]);
    }

    #[test]
    fn guid_iapo_rt_is_correct() {
        // 9E1D6A6D-DDBC-4E95-A4C7-AD64BA37846C
        assert_eq!(IID_IAPO_RT.data1, 0x9E1D6A6D);
        assert_eq!(IID_IAPO_RT.data2, 0xDDBC);
        assert_eq!(IID_IAPO_RT.data3, 0x4E95);
        assert_eq!(IID_IAPO_RT.data4, [0xA4, 0xC7, 0xAD, 0x64, 0xBA, 0x37, 0x84, 0x6C]);
    }

    #[test]
    fn guid_iapo_config_is_correct() {
        // 0E5D4480-149E-4842-B6E0-74B90A4859DE
        assert_eq!(IID_IAPO_CONFIG.data1, 0x0E5D4480);
        assert_eq!(IID_IAPO_CONFIG.data2, 0x149E);
        assert_eq!(IID_IAPO_CONFIG.data3, 0x4842);
        assert_eq!(
            IID_IAPO_CONFIG.data4,
            [0xB6, 0xE0, 0x74, 0xB9, 0x0A, 0x48, 0x59, 0xDE]
        );
    }

    #[test]
    fn guid_iaudio_media_type_is_correct() {
        // 4E9966C0-E244-4908-A587-3D288C223F24
        assert_eq!(IID_IAUDIO_MEDIA_TYPE.data1, 0x4E9966C0);
        assert_eq!(IID_IAUDIO_MEDIA_TYPE.data2, 0xE244);
        assert_eq!(IID_IAUDIO_MEDIA_TYPE.data3, 0x4908);
        assert_eq!(
            IID_IAUDIO_MEDIA_TYPE.data4,
            [0xA5, 0x87, 0x3D, 0x28, 0x8C, 0x22, 0x3F, 0x24]
        );
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

    // ── 枚举值验证 ──────────────────────────────────────────────────────────

    #[test]
    fn audio_flow_type_values() {
        assert_eq!(AUDIO_FLOW_TYPE::RENDER as u32, 0);
        assert_eq!(AUDIO_FLOW_TYPE::CAPTURE as u32, 1);
    }

    #[test]
    fn apo_buffer_type_values() {
        assert_eq!(APO_BUFFER_TYPE::INVALID as u32, 0);
        assert_eq!(APO_BUFFER_TYPE::CONSTANT as u32, 1);
        assert_eq!(APO_BUFFER_TYPE::SILENT as u32, 2);
        assert_eq!(APO_BUFFER_TYPE::ZERO_MEMORY as u32, 3);
    }

    // ── 缓冲区标志位 ────────────────────────────────────────────────────────

    #[test]
    fn buffer_flags_values() {
        assert_eq!(BUFFER_INVALID, 0x00);
        assert_eq!(BUFFER_VALID, 0x01);
        assert_eq!(BUFFER_SILENT, 0x02);
    }

    // ── 结构体布局验证 ──────────────────────────────────────────────────────
    #[test]
    fn apo_connection_property_sizes() {
        assert_eq!(std::mem::size_of::<APO_CONNECTION_PROPERTY>(), 16);
    }

    // ── REFERENCE_TIME 别名 ─────────────────────────────────────────────────

    #[test]
    fn reference_time_is_i64() {
        let t: REFERENCE_TIME = 10_000; // 1ms
        assert_eq!(t, 10_000i64);
    }
}