//! instance/object.rs — APO 对象常量与基础结构（Note 6）
//!
//! 定义 APO COM 对象的核心状态结构，以及 APOGUID 特殊值常量：
//! - `APOGUID_NOKEY`：FxProperties 键不存在
//! - `APOGUID_NOVALUE`：值为空或已被其他 APO 占据
//!
//! 依赖关系：
//! - `instance/init.rs`：解析 `APOInitSystemEffects` 时使用（Note 7）
//! - `instance/audio_proc_obj_conf.rs`：`LockForProcess` 判定时使用（Note 9）
//!
//! 此模块仅提供常量与结构体定义，不包含 COM 接口实现。

use windows::core::GUID;

// ══════════════════════════════════════════════════════════════════════════════
// APOGUID 特殊值常量（Note 6）
//
// 设备的 FxProperties 注册表中，APO GUID 槽位有三种状态：
//   1. NOKEY   — 键本身不存在（设备从未安装过 APO）
//   2. NOVALUE — 值为空或被其他 APO 占据
//   3. 具体 GUID — 已安装的 APO 的 CLSID
//
// init.rs 和 audio_proc_obj_conf.rs 依赖这些常量判断设备状态。
// ══════════════════════════════════════════════════════════════════════════════

/// FxProperties 键不存在。
///
/// 含义：设备注册表路径下根本没有 FxProperties 子键。
/// `init.rs` 在此状态下回退到默认初始化逻辑。
pub const APOGUID_NOKEY: GUID = GUID::from_values(
    0x00000000,
    0x0000,
    0x0000,
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01],
);

/// FxProperties 值为空或被其他 APO 占据。
///
/// 含义：FxProperties 键存在，但目标 GUID 槽位值为空字符串、
/// 或者已被系统 APO / 第三方 APO 写入了非 VxAPO 的 GUID。
/// `audio_proc_obj_conf.rs` 在此状态下跳过子 APO 创建。
pub const APOGUID_NOVALUE: GUID = GUID::from_values(
    0x00000000,
    0x0000,
    0x0000,
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02],
);

/// 空 GUID（全零），用于初始化和比较。
pub const GUID_NULL: GUID = GUID::from_values(
    0x00000000,
    0x0000,
    0x0000,
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
);

// ══════════════════════════════════════════════════════════════════════════════
// 判定辅助
// ══════════════════════════════════════════════════════════════════════════════

/// 判断给定 GUID 是否为特殊值（NOKEY / NOVALUE）。
///
/// `init.rs` 在解析 APOInitSystemEffects 后调用，判断设备状态。
pub fn is_special_guid(guid: &GUID) -> bool {
    *guid == APOGUID_NOKEY || *guid == APOGUID_NOVALUE
}

/// 判断给定 GUID 是否为有效的 APO CLSID（非特殊值、非空）。
pub fn is_valid_apo_guid(guid: &GUID) -> bool {
    !is_special_guid(guid) && *guid != GUID_NULL
}

/// 判断两个 GUID 是否代表同一个 APO。
///
/// 特殊值之间不相等（NOKEY ≠ NOVALUE），各自只与自身相等。
pub fn guid_matches(a: &GUID, b: &GUID) -> bool {
    *a == *b
}

// ══════════════════════════════════════════════════════════════════════════════
// APO 对象状态
//
// Phase 2 只定义结构，Phase 4 的 #[implement] 宏为它生成 COM 接口。
// 此处定义所有 APO 实例共享的核心状态字段。
// ══════════════════════════════════════════════════════════════════════════════

/// APO 对象的核心状态。
///
/// Phase 4 中由 `#[implement]` 宏包装，自动生成：
/// - `IAudioProcessingObject`（Reset / GetLatency / IsInputFormatSupported 等）
/// - `IAudioProcessingObjectRT`（APOProcess / CalcInputFrames / CalcOutputFrames）
/// - `IAudioProcessingObjectConfiguration`（LockForProcess / UnlockForProcess）
/// - `INonDelegatingUnknown`（聚合支持，Note 3）
///
/// 引用计数由 `#[implement]` 宏自动管理，`inst_count` 模块的
/// `increment` / `decrement` 在 CreateInstance 和 Drop 中调用。
#[derive(Debug)]
pub struct ApoObjectState {
    /// 此实例的 CLSID（PreMix 或 PostMix）。
    pub clsid: GUID,

    /// 子 APO 的 COM 接口指针（Phase 6 填充）。
    /// 为 None 表示无子 APO（降级模式，Note 57）。
    pub child_apo_guid: Option<GUID>,

    /// 是否已通过 LockForProcess 锁定。
    ///
    /// 锁定前不得调用 APOProcess（Windows 约束）。
    pub is_locked: bool,

    /// 采样率（LockForProcess 时确定）。
    pub sample_rate: u32,

    /// 输入通道数。
    pub input_channel_count: u32,

    /// 输出通道数。
    pub output_channel_count: u32,

    /// 通道掩码。
    pub channel_mask: u32,

    /// 每样本位数。
    pub bits_per_sample: u32,

    /// 是否允许静音缓冲区快速路径（Note 11）。
    pub allow_silent_buffer_modification: bool,
}

impl ApoObjectState {
    /// 创建新的 APO 对象状态。
    ///
    /// 由 `ClassFactory::CreateInstance` 调用。
    pub fn new(clsid: GUID) -> Self {
        Self {
            clsid,
            child_apo_guid: None,
            is_locked: false,
            sample_rate: 0,
            input_channel_count: 0,
            output_channel_count: 0,
            channel_mask: 0,
            bits_per_sample: 0,
            allow_silent_buffer_modification: false,
        }
    }

    /// 判断此实例是 PreMix APO 还是 PostMix APO。
    pub fn is_pre_mix(&self) -> bool {
        self.clsid == super::super::com::reg_props::CLSID_VXAPO_PRE_MIX
    }

    /// 判断此实例是 PostMix APO。
    pub fn is_post_mix(&self) -> bool {
        self.clsid == super::super::com::reg_props::CLSID_VXAPO_POST_MIX
    }

    /// 锁定处理流程（LockForProcess 成功后调用）。
    pub fn lock_for_process(
        &mut self,
        sample_rate: u32,
        input_channels: u32,
        output_channels: u32,
        channel_mask: u32,
        bits_per_sample: u32,
    ) {
        self.sample_rate = sample_rate;
        self.input_channel_count = input_channels;
        self.output_channel_count = output_channels;
        self.channel_mask = channel_mask;
        self.bits_per_sample = bits_per_sample;
        self.is_locked = true;
    }

    /// 解锁处理流程（UnlockForProcess 调用）。
    pub fn unlock_for_process(&mut self) {
        self.is_locked = false;
        self.sample_rate = 0;
        self.input_channel_count = 0;
        self.output_channel_count = 0;
        self.channel_mask = 0;
        self.bits_per_sample = 0;
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 编译期断言
// ══════════════════════════════════════════════════════════════════════════════

const _: () = {
    // 三个特殊 GUID 互不相同
    // const fn 中不能用 assert_ne，用 assert + !=
    assert!(APOGUID_NOKEY.data1 != APOGUID_NOVALUE.data1 || APOGUID_NOKEY.data4[7] != APOGUID_NOVALUE.data4[7]);
    assert!(GUID_NULL.data1 != APOGUID_NOKEY.data1 || GUID_NULL.data4[7] != APOGUID_NOKEY.data4[7]);
    assert!(GUID_NULL.data1 != APOGUID_NOVALUE.data1 || GUID_NULL.data4[7] != APOGUID_NOVALUE.data4[7]);
};

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── 特殊 GUID 互不相同 ──────────────────────────────────────────────────

    #[test]
    fn special_guids_distinct() {
        assert_ne!(APOGUID_NOKEY, APOGUID_NOVALUE);
        assert_ne!(APOGUID_NOKEY, GUID_NULL);
        assert_ne!(APOGUID_NOVALUE, GUID_NULL);
    }

    // ── is_special_guid ─────────────────────────────────────────────────────

    #[test]
    fn is_special_guid_nokey() {
        assert!(is_special_guid(&APOGUID_NOKEY));
    }

    #[test]
    fn is_special_guid_novalue() {
        assert!(is_special_guid(&APOGUID_NOVALUE));
    }

    #[test]
    fn is_special_guid_null() {
        assert!(!is_special_guid(&GUID_NULL));
    }

    #[test]
    fn is_special_guid_real() {
        assert!(!is_special_guid(&super::super::super::com::reg_props::CLSID_VXAPO_PRE_MIX));
    }

    // ── is_valid_apo_guid ───────────────────────────────────────────────────

    #[test]
    fn is_valid_apo_guid_real() {
        assert!(is_valid_apo_guid(&super::super::super::com::reg_props::CLSID_VXAPO_PRE_MIX));
    }

    #[test]
    fn is_valid_apo_guid_nokey() {
        assert!(!is_valid_apo_guid(&APOGUID_NOKEY));
    }

    #[test]
    fn is_valid_apo_guid_novalue() {
        assert!(!is_valid_apo_guid(&APOGUID_NOVALUE));
    }

    #[test]
    fn is_valid_apo_guid_null() {
        assert!(!is_valid_apo_guid(&GUID_NULL));
    }

    // ── guid_matches ────────────────────────────────────────────────────────

    #[test]
    fn guid_matches_same() {
        assert!(guid_matches(&APOGUID_NOKEY, &APOGUID_NOKEY));
    }

    #[test]
    fn guid_matches_different() {
        assert!(!guid_matches(&APOGUID_NOKEY, &APOGUID_NOVALUE));
    }

    // ── ApoObjectState ──────────────────────────────────────────────────────

    #[test]
    fn apo_object_state_new() {
        let state = ApoObjectState::new(super::super::super::com::reg_props::CLSID_VXAPO_PRE_MIX);
        assert!(state.is_pre_mix());
        assert!(!state.is_post_mix());
        assert!(!state.is_locked);
        assert_eq!(state.sample_rate, 0);
        assert_eq!(state.input_channel_count, 0);
        assert_eq!(state.output_channel_count, 0);
        assert!(state.child_apo_guid.is_none());
        assert!(!state.allow_silent_buffer_modification);
    }

    #[test]
    fn apo_object_state_post_mix() {
        let state = ApoObjectState::new(super::super::super::com::reg_props::CLSID_VXAPO_POST_MIX);
        assert!(!state.is_pre_mix());
        assert!(state.is_post_mix());
    }

    #[test]
    fn apo_object_state_lock_unlock() {
        let mut state = ApoObjectState::new(super::super::super::com::reg_props::CLSID_VXAPO_PRE_MIX);

        state.lock_for_process(48000, 2, 2, 0x3, 32);
        assert!(state.is_locked);
        assert_eq!(state.sample_rate, 48000);
        assert_eq!(state.input_channel_count, 2);
        assert_eq!(state.output_channel_count, 2);
        assert_eq!(state.channel_mask, 0x3);
        assert_eq!(state.bits_per_sample, 32);

        state.unlock_for_process();
        assert!(!state.is_locked);
        assert_eq!(state.sample_rate, 0);
        assert_eq!(state.input_channel_count, 0);
    }

    #[test]
    fn apo_object_state_format_debug() {
        let state = ApoObjectState::new(super::super::super::com::reg_props::CLSID_VXAPO_PRE_MIX);
        let debug = format!("{state:?}");
        assert!(debug.contains("clsid"));
        assert!(debug.contains("is_locked"));
    }
}