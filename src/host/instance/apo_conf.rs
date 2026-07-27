//! host/instance/apo_conf.rs — LockForProcess / UnlockForProcess 辅助逻辑（Note 9）
//!
//! `IAudioProcessingObjectConfiguration` 的接口实现在 `apo_interface.rs` 中，
//! 本模块提供通道数确定规则与通道掩码确定规则等辅助逻辑。
//!
//! 通道数确定规则（Note 9）：
//! - 有子 APO 时使用输出通道数
//! - 无子 APO 时使用输入通道数
//! - 采集设备使用输入掩码，回放使用输出掩码，优先非零
//!
//! 此模块不包含 COM 接口实现，仅提供纯逻辑辅助函数。

use windows::core::HRESULT;

use crate::sys::com::base;
use crate::sys::com::apo_abi::{
    APO_CONNECTION_DESCRIPTOR, APO_CONNECTION_BUFFER_TYPE,
    APO_CONNECTION_DESCRIPTOR_SIGNATURE,
};
use crate::host::instance::apo_interface::ApoObject;

// ══════════════════════════════════════════════════════════════════════════════
// LockForProcess 参数
// ══════════════════════════════════════════════════════════════════════════════

/// 连接格式描述（LockForProcess 参数的简化表示）。
///
/// 对应 `APO_CONNECTION_DESCRIPTOR` 中的格式信息。
#[derive(Debug, Clone)]
pub struct ConnectionFormat {
    /// 通道数。
    pub channel_count: u32,
    /// 采样率（Hz）。
    pub sample_rate: u32,
    /// 每样本位数。
    pub bits_per_sample: u32,
    /// 通道掩码（Windows dwChannelMask）。
    pub channel_mask: u32,
}

/// LockForProcess 的输入输出描述。
#[derive(Debug, Clone)]
pub struct LockConfig {
    /// 输入连接格式列表。
    pub inputs: Vec<ConnectionFormat>,
    /// 输出连接格式列表。
    pub outputs: Vec<ConnectionFormat>,
}

// ══════════════════════════════════════════════════════════════════════════════
// LockForProcess 辅助
// ══════════════════════════════════════════════════════════════════════════════

/// 执行 LockForProcess 逻辑（Note 9）。
///
/// 返回 `S_OK` 成功，`E_FAIL` 失败。
///
/// # 通道数确定规则（Note 9）
///
/// - 有子 APO：用输出通道数
/// - 无子 APO：用输入通道数
/// - 采集设备：用输入掩码
/// - 回放设备：用输出掩码
/// - 优先非零
///
/// # 已锁定时的行为
///
/// 重复调用 `LockForProcess` 返回 `E_FAIL`。
/// 必须先 `UnlockForProcess`。
pub fn lock_for_process(obj: &mut ApoObject, config: &LockConfig) -> HRESULT {
    let mut state = match obj.state.lock() {
        Ok(s) => s,
        Err(_) => return base::E_FAIL,
    };

    // 已锁定 → 错误
    if state.is_locked {
        return base::E_FAIL;
    }

    // 至少需要一个输入和一个输出
    if config.inputs.is_empty() || config.outputs.is_empty() {
        return base::E_FAIL;
    }

    let input = &config.inputs[0];
    let output = &config.outputs[0];

    // 采样率必须匹配
    if input.sample_rate != output.sample_rate {
        return base::E_FAIL;
    }

    // 位深必须匹配
    if input.bits_per_sample != output.bits_per_sample {
        return base::E_FAIL;
    }

    // 通道数确定规则（Note 9）
    let (input_channels, output_channels) = determine_channel_counts(&state, input, output);

    // 通道掩码确定规则（Note 9）
    let channel_mask = determine_channel_mask(&state, input, output);

    // 锁定
    state.lock_for_process(
        input.sample_rate,
        input_channels,
        output_channels,
        channel_mask,
        input.bits_per_sample,
    );

    // ── 子 APO LockForProcess 委托 ──────────────────────────
    if let Some(ref child) = obj.child_apo {
        // 从 LockConfig 构造子 APO 所需的 APO_CONNECTION_DESCRIPTOR。
        // format 和 buffer 由父 APO 管理，子 APO 使用引擎提供的缓冲区。
        // 这里构造最小描述符，子 APO 可能返回错误——不阻塞父 APO 锁定。
        let mut input_desc = APO_CONNECTION_DESCRIPTOR {
            buffer_type: APO_CONNECTION_BUFFER_TYPE::ALLOCATED,
            buffer: 0,
            max_frame_count: 1024,
            format: std::ptr::null_mut(),
            signature: APO_CONNECTION_DESCRIPTOR_SIGNATURE,
        };
        let mut output_desc = APO_CONNECTION_DESCRIPTOR {
            buffer_type: APO_CONNECTION_BUFFER_TYPE::ALLOCATED,
            buffer: 0,
            max_frame_count: 1024,
            format: std::ptr::null_mut(),
            signature: APO_CONNECTION_DESCRIPTOR_SIGNATURE,
        };

        let mut pp_inputs: *mut APO_CONNECTION_DESCRIPTOR = &mut input_desc;
        let mut pp_outputs: *mut APO_CONNECTION_DESCRIPTOR = &mut output_desc;

        let child_hr = unsafe {
            child.lock_for_process(1, &mut pp_inputs, 1, &mut pp_outputs)
        };
        if child_hr.is_err() {
            // 子 APO 拒绝锁定——不阻塞父 APO（Note 57 降级模式）
            // TODO Phase 9P: 记录到 ring_logger
        }
    }

    base::S_OK
}

/// 解锁处理流程。
pub fn unlock_for_process(obj: &mut ApoObject) -> HRESULT {
    // Phase 6: 子 APO UnlockForProcess
    if let Some(ref child) = obj.child_apo {
        let _ = child.unlock_for_process();
    }

    let mut state = match obj.state.lock() {
        Ok(s) => s,
        Err(_) => return base::E_FAIL,
    };

    if !state.is_locked {
        return base::S_FALSE; // 未锁定，无需解锁
    }

    state.unlock_for_process();
    base::S_OK
}

// ══════════════════════════════════════════════════════════════════════════════
// 通道数确定规则（Note 9）
// ══════════════════════════════════════════════════════════════════════════════

fn determine_channel_counts(
    state: &crate::host::instance::object::ApoObjectState,
    input: &ConnectionFormat,
    output: &ConnectionFormat,
) -> (u32, u32) {
    if state.child_apo_guid.is_some() {
        (input.channel_count, output.channel_count)
    } else {
        let channels = if input.channel_count > 0 {
            input.channel_count
        } else {
            output.channel_count
        };
        (channels, channels)
    }
}

/// 确定通道掩码（Note 9）。
fn determine_channel_mask(
    state: &crate::host::instance::object::ApoObjectState,
    input: &ConnectionFormat,
    output: &ConnectionFormat,
) -> u32 {
    if state.is_pre_mix() || state.is_post_mix() {
        // 回放设备：优先输出掩码
        if output.channel_mask != 0 {
            output.channel_mask
        } else {
            input.channel_mask
        }
    } else {
        // 采集设备：优先输入掩码
        if input.channel_mask != 0 {
            input.channel_mask
        } else {
            output.channel_mask
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::instance::reg_props::{CLSID_VXAPO_PRE_MIX, CLSID_VXAPO_POST_MIX};
    use crate::host::instance::ref_count as inst_count;

    fn stereo_config() -> LockConfig {
        LockConfig {
            inputs: vec![ConnectionFormat {
                channel_count: 2,
                sample_rate: 48000,
                bits_per_sample: 32,
                channel_mask: 0x3,
            }],
            outputs: vec![ConnectionFormat {
                channel_count: 2,
                sample_rate: 48000,
                bits_per_sample: 32,
                channel_mask: 0x3,
            }],
        }
    }

    fn surround_config() -> LockConfig {
        LockConfig {
            inputs: vec![ConnectionFormat {
                channel_count: 6,
                sample_rate: 48000,
                bits_per_sample: 32,
                channel_mask: 0x3F,
            }],
            outputs: vec![ConnectionFormat {
                channel_count: 6,
                sample_rate: 48000,
                bits_per_sample: 32,
                channel_mask: 0x3F,
            }],
        }
    }

    // ── LockForProcess 基础 ─────────────────────────────────────────────────

    #[test]
    fn lock_stereo_success() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        let config = stereo_config();
        let hr = lock_for_process(&mut obj, &config);
        assert_eq!(hr, base::S_OK);

        let state = obj.state.lock().unwrap();
        assert!(state.is_locked);
        assert_eq!(state.sample_rate, 48000);
        assert_eq!(state.input_channel_count, 2);
        assert_eq!(state.output_channel_count, 2);
        assert_eq!(state.channel_mask, 0x3);
        assert_eq!(state.bits_per_sample, 32);
        drop(state);
        drop(obj);
    }

    #[test]
    fn lock_surround_success() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        let config = surround_config();
        let hr = lock_for_process(&mut obj, &config);
        assert_eq!(hr, base::S_OK);

        let state = obj.state.lock().unwrap();
        assert_eq!(state.input_channel_count, 6);
        assert_eq!(state.channel_mask, 0x3F);
        drop(state);
        drop(obj);
    }

    #[test]
    fn lock_fails_when_already_locked() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        let config = stereo_config();

        lock_for_process(&mut obj, &config);
        let hr = lock_for_process(&mut obj, &config);
        assert_eq!(hr, base::E_FAIL);
        drop(obj);
    }

    #[test]
    fn lock_fails_empty_inputs() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        let config = LockConfig {
            inputs: vec![],
            outputs: vec![ConnectionFormat {
                channel_count: 2, sample_rate: 48000,
                bits_per_sample: 32, channel_mask: 0x3,
            }],
        };
        let hr = lock_for_process(&mut obj, &config);
        assert_eq!(hr, base::E_FAIL);
        drop(obj);
    }

    #[test]
    fn lock_fails_empty_outputs() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        let config = LockConfig {
            inputs: vec![ConnectionFormat {
                channel_count: 2, sample_rate: 48000,
                bits_per_sample: 32, channel_mask: 0x3,
            }],
            outputs: vec![],
        };
        let hr = lock_for_process(&mut obj, &config);
        assert_eq!(hr, base::E_FAIL);
        drop(obj);
    }

    #[test]
    fn lock_fails_rate_mismatch() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        let config = LockConfig {
            inputs: vec![ConnectionFormat {
                channel_count: 2, sample_rate: 44100,
                bits_per_sample: 32, channel_mask: 0x3,
            }],
            outputs: vec![ConnectionFormat {
                channel_count: 2, sample_rate: 48000,
                bits_per_sample: 32, channel_mask: 0x3,
            }],
        };
        let hr = lock_for_process(&mut obj, &config);
        assert_eq!(hr, base::E_FAIL);
        drop(obj);
    }

    #[test]
    fn lock_fails_bits_mismatch() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        let config = LockConfig {
            inputs: vec![ConnectionFormat {
                channel_count: 2, sample_rate: 48000,
                bits_per_sample: 16, channel_mask: 0x3,
            }],
            outputs: vec![ConnectionFormat {
                channel_count: 2, sample_rate: 48000,
                bits_per_sample: 32, channel_mask: 0x3,
            }],
        };
        let hr = lock_for_process(&mut obj, &config);
        assert_eq!(hr, base::E_FAIL);
        drop(obj);
    }

    // ── UnlockForProcess ────────────────────────────────────────────────────

    #[test]
    fn unlock_after_lock() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        let config = stereo_config();

        lock_for_process(&mut obj, &config);
        {
            let state = obj.state.lock().unwrap();
            assert!(state.is_locked);
        }

        let hr = unlock_for_process(&mut obj);
        assert_eq!(hr, base::S_OK);
        {
            let state = obj.state.lock().unwrap();
            assert!(!state.is_locked);
            assert_eq!(state.sample_rate, 0);
            assert_eq!(state.input_channel_count, 0);
        }
        drop(obj);
    }

    #[test]
    fn unlock_when_not_locked() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        let hr = unlock_for_process(&mut obj);
        assert_eq!(hr, base::S_FALSE);
        drop(obj);
    }

    #[test]
    fn lock_unlock_lock_cycle() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);

        let config1 = stereo_config();
        assert_eq!(lock_for_process(&mut obj, &config1), base::S_OK);
        assert_eq!(obj.state.lock().unwrap().sample_rate, 48000);
        assert_eq!(obj.state.lock().unwrap().input_channel_count, 2);

        assert_eq!(unlock_for_process(&mut obj), base::S_OK);
        assert!(!obj.state.lock().unwrap().is_locked);

        let config2 = surround_config();
        assert_eq!(lock_for_process(&mut obj, &config2), base::S_OK);
        assert_eq!(obj.state.lock().unwrap().input_channel_count, 6);

        drop(obj);
    }

    // ── 通道数确定规则（Note 9） ────────────────────────────────────────────

    #[test]
    fn channel_count_no_child_uses_input() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);

        let config = LockConfig {
            inputs: vec![ConnectionFormat {
                channel_count: 4, sample_rate: 48000,
                bits_per_sample: 32, channel_mask: 0x33,
            }],
            outputs: vec![ConnectionFormat {
                channel_count: 2, sample_rate: 48000,
                bits_per_sample: 32, channel_mask: 0x3,
            }],
        };
        lock_for_process(&mut obj, &config);

        let state = obj.state.lock().unwrap();
        // 无子 APO：输入输出通道数一致，用输入
        assert_eq!(state.input_channel_count, 4);
        assert_eq!(state.output_channel_count, 4);
        drop(state);
        drop(obj);
    }

    #[test]
    fn channel_count_with_child_uses_output() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        // 模拟有子 APO
        obj.state.lock().unwrap().child_apo_guid = Some(CLSID_VXAPO_POST_MIX);

        let config = LockConfig {
            inputs: vec![ConnectionFormat {
                channel_count: 2, sample_rate: 48000,
                bits_per_sample: 32, channel_mask: 0x3,
            }],
            outputs: vec![ConnectionFormat {
                channel_count: 6, sample_rate: 48000,
                bits_per_sample: 32, channel_mask: 0x3F,
            }],
        };
        lock_for_process(&mut obj, &config);

        let state = obj.state.lock().unwrap();
        assert_eq!(state.input_channel_count, 2);
        assert_eq!(state.output_channel_count, 6);
        drop(state);
        drop(obj);
    }

    // ── 通道掩码确定规则（Note 9） ──────────────────────────────────────────

    #[test]
    fn channel_mask_premix_uses_output() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);

        let config = LockConfig {
            inputs: vec![ConnectionFormat {
                channel_count: 2, sample_rate: 48000,
                bits_per_sample: 32, channel_mask: 0x3,
            }],
            outputs: vec![ConnectionFormat {
                channel_count: 2, sample_rate: 48000,
                bits_per_sample: 32, channel_mask: 0x3F,
            }],
        };
        lock_for_process(&mut obj, &config);

        assert_eq!(obj.state.lock().unwrap().channel_mask, 0x3F);
        drop(obj);
    }

    #[test]
    fn channel_mask_postmix_uses_output() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_POST_MIX);

        let config = LockConfig {
            inputs: vec![ConnectionFormat {
                channel_count: 2, sample_rate: 48000,
                bits_per_sample: 32, channel_mask: 0x3,
            }],
            outputs: vec![ConnectionFormat {
                channel_count: 2, sample_rate: 48000,
                bits_per_sample: 32, channel_mask: 0x3F,
            }],
        };
        lock_for_process(&mut obj, &config);

        assert_eq!(obj.state.lock().unwrap().channel_mask, 0x3F);
        drop(obj);
    }

    #[test]
    fn channel_mask_output_zero_falls_back_to_input() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);

        let config = LockConfig {
            inputs: vec![ConnectionFormat {
                channel_count: 2, sample_rate: 48000,
                bits_per_sample: 32, channel_mask: 0x3,
            }],
            outputs: vec![ConnectionFormat {
                channel_count: 2, sample_rate: 48000,
                bits_per_sample: 32, channel_mask: 0,
            }],
        };
        lock_for_process(&mut obj, &config);

        assert_eq!(obj.state.lock().unwrap().channel_mask, 0x3);
        drop(obj);
    }

    // ── Debug ───────────────────────────────────────────────────────────────

    #[test]
    fn connection_format_debug() {
        let fmt = ConnectionFormat {
            channel_count: 2, sample_rate: 48000,
            bits_per_sample: 32, channel_mask: 0x3,
        };
        let debug = format!("{fmt:?}");
        assert!(debug.contains("ConnectionFormat"));
        assert!(debug.contains("48000"));
    }

    #[test]
    fn lock_config_debug() {
        let config = stereo_config();
        let debug = format!("{config:?}");
        assert!(debug.contains("LockConfig"));
    }

    // ── 端到端 ──────────────────────────────────────────────────────────────

    #[test]
    fn full_lock_process_unlock_cycle() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);

        let config = stereo_config();
        assert_eq!(lock_for_process(&mut obj, &config), base::S_OK);

        assert!(obj.state.lock().unwrap().is_locked);
        assert_eq!(obj.state.lock().unwrap().input_channel_count, 2);

        assert_eq!(unlock_for_process(&mut obj), base::S_OK);

        assert!(!obj.state.lock().unwrap().is_locked);
        assert_eq!(obj.state.lock().unwrap().input_channel_count, 0);

        drop(obj);
    }
}