//! host/instance/apo_conf.rs — IAudioProcessingObjectConfiguration 实现
//!
//! 实现 `IAudioProcessingObjectConfiguration` 接口：
//! - `LockForProcess`：格式协商完成后锁定处理流程，确定通道数、采样率、位深与通道掩码（Note 9）
//! - `UnlockForProcess`：释放锁定状态，允许重新协商格式
//!
//! Windows 约束：`APOProcess` 仅在 `LockForProcess` 成功后方可调用。
//!
//! 通道数确定规则（Note 9）：
//! - 有子 APO 时使用输出通道数
//! - 无子 APO 时使用输入通道数
//! - 采集设备使用输入掩码，回放使用输出掩码，优先非零
//!
//! 依赖 `APOGUID_NOKEY` / `APOGUID_NOVALUE` 常量（`host/instance/object.rs`，Note 6）。

use crate::sys::com::base;
use crate::host::instance::apo_interface::ApoObject;

// ══════════════════════════════════════════════════════════════════════════════
// LockForProcess 参数
// ══════════════════════════════════════════════════════════════════════════════

/// 连接格式描述（LockForProcess 参数的简化表示）。
///
/// 对应 `APO_CONNECTION_DESCRIPTOR` 中的格式信息。
/// Phase 4 初期只提取关键字段，Phase 6 补全 `IAudioMediaType` 解析。
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
// LockForProcess 实现（Note 9）
// ══════════════════════════════════════════════════════════════════════════════

/// 执行 LockForProcess 逻辑。
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
pub fn lock_for_process(obj: &mut ApoObject, config: &LockConfig) -> windows::core::HRESULT {
    // 已锁定 → 错误
    if obj.state.is_locked {
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
    let (input_channels, output_channels) = determine_channel_counts(obj, input, output);

    // 通道掩码确定规则（Note 9）
    let channel_mask = determine_channel_mask(obj, input, output);

    // 锁定
    obj.state.lock_for_process(
        input.sample_rate,
        input_channels,
        output_channels,
        channel_mask,
        input.bits_per_sample,
    );

    // Phase 6: 子 APO LockForProcess 委托
    // 完整实现需要构造 APO_CONNECTION_DESCRIPTOR，Phase 8 补全。
    // 当前：子 APO 在 init 阶段创建但不调用 LockForProcess。
    if obj.child_apo.is_some() {
        // TODO Phase 8: 子 APO LockForProcess
        // let child = obj.child_apo.as_ref().unwrap();
        // unsafe { child.lock_for_process(...); }
    }

    base::S_OK
}

/// 解锁处理流程。
pub fn unlock_for_process(obj: &mut ApoObject) -> windows::core::HRESULT {
    // Phase 6: 子 APO UnlockForProcess
    if let Some(ref child) = obj.child_apo {
        let _ = child.unlock_for_process();
    }

    if !obj.state.is_locked {
        return base::S_FALSE; // 未锁定，无需解锁
    }

    obj.state.unlock_for_process();
    base::S_OK
}

// ══════════════════════════════════════════════════════════════════════════════
// 通道数确定规则（Note 9）
// ══════════════════════════════════════════════════════════════════════════════

/// 确定输入和输出通道数。
///
/// Note 9 规则：
/// - 有子 APO 时用输出通道数作为输出
/// - 无子 APO 时用输入通道数
fn determine_channel_counts(
    obj: &ApoObject,
    input: &ConnectionFormat,
    output: &ConnectionFormat,
) -> (u32, u32) {
    if obj.state.child_apo_guid.is_some() {
        // 有子 APO：输出通道 = 子 APO 的输出（这里用 connection 描述的输出）
        (input.channel_count, output.channel_count)
    } else {
        // 无子 APO：输入输出通道一致
        let channels = if input.channel_count > 0 {
            input.channel_count
        } else {
            output.channel_count
        };
        (channels, channels)
    }
}

/// 确定通道掩码。
///
/// Note 9 规则：
/// - 采集设备用输入掩码
/// - 回放设备用输出掩码
/// - 优先非零
fn determine_channel_mask(
    obj: &ApoObject,
    input: &ConnectionFormat,
    output: &ConnectionFormat,
) -> u32 {
    if obj.state.is_pre_mix() || obj.state.is_post_mix() {
        // 回放设备：优先输出掩码
        if output.channel_mask != 0 {
            output.channel_mask
        } else {
            input.channel_mask
        }
    } else {
        // 采集设备或其他：优先输入掩码
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
        assert!(obj.state.is_locked);
        assert_eq!(obj.state.sample_rate, 48000);
        assert_eq!(obj.state.input_channel_count, 2);
        assert_eq!(obj.state.output_channel_count, 2);
        assert_eq!(obj.state.channel_mask, 0x3);
        assert_eq!(obj.state.bits_per_sample, 32);
        drop(obj);
    }

    #[test]
    fn lock_surround_success() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        let config = surround_config();
        let hr = lock_for_process(&mut obj, &config);
        assert_eq!(hr, base::S_OK);
        assert_eq!(obj.state.input_channel_count, 6);
        assert_eq!(obj.state.channel_mask, 0x3F);
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
        assert!(obj.state.is_locked);

        let hr = unlock_for_process(&mut obj);
        assert_eq!(hr, base::S_OK);
        assert!(!obj.state.is_locked);
        assert_eq!(obj.state.sample_rate, 0);
        assert_eq!(obj.state.input_channel_count, 0);
        drop(obj);
    }

    #[test]
    fn unlock_when_not_locked() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        let hr = unlock_for_process(&mut obj);
        assert_eq!(hr, base::S_FALSE); // 未锁定
        drop(obj);
    }

    #[test]
    fn lock_unlock_lock_cycle() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);

        // 第一次锁定
        let config1 = stereo_config();
        assert_eq!(lock_for_process(&mut obj, &config1), base::S_OK);
        assert_eq!(obj.state.sample_rate, 48000);
        assert_eq!(obj.state.input_channel_count, 2);

        // 解锁
        assert_eq!(unlock_for_process(&mut obj), base::S_OK);
        assert!(!obj.state.is_locked);

        // 第二次锁定（不同配置）
        let config2 = surround_config();
        assert_eq!(lock_for_process(&mut obj, &config2), base::S_OK);
        assert_eq!(obj.state.input_channel_count, 6);

        drop(obj);
    }

    // ── 通道数确定规则（Note 9） ────────────────────────────────────────────

    #[test]
    fn channel_count_no_child_uses_input() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        // 无子 APO
        assert!(obj.state.child_apo_guid.is_none());

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

        // 无子 APO：输入输出通道数一致，用输入
        assert_eq!(obj.state.input_channel_count, 4);
        assert_eq!(obj.state.output_channel_count, 4);
        drop(obj);
    }

    #[test]
    fn channel_count_with_child_uses_output() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        // 模拟有子 APO
        obj.state.child_apo_guid = Some(CLSID_VXAPO_POST_MIX);

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

        assert_eq!(obj.state.input_channel_count, 2);
        assert_eq!(obj.state.output_channel_count, 6);
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

        // 回放设备：优先输出掩码
        assert_eq!(obj.state.channel_mask, 0x3F);
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

        assert_eq!(obj.state.channel_mask, 0x3F);
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

        // 输出掩码为 0 → 回退到输入掩码
        assert_eq!(obj.state.channel_mask, 0x3);
        drop(obj);
    }

    // ── ConnectionFormat / LockConfig Debug ─────────────────────────────────

    #[test]
    fn connection_format_debug() {
        let fmt = ConnectionFormat {
            channel_count: 2,
            sample_rate: 48000,
            bits_per_sample: 32,
            channel_mask: 0x3,
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

    // ── 端到端：Lock → Process → Unlock ─────────────────────────────────────

    #[test]
    fn full_lock_process_unlock_cycle() {
        inst_count::reset_for_test();
        let mut obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);

        // Lock
        let config = stereo_config();
        assert_eq!(lock_for_process(&mut obj, &config), base::S_OK);
        assert!(obj.state.is_locked);
        assert_eq!(obj.get_input_channel_count(), 2);

        // Process（模拟）
        assert!(obj.state.is_locked);

        // Unlock
        assert_eq!(unlock_for_process(&mut obj), base::S_OK);
        assert!(!obj.state.is_locked);
        assert_eq!(obj.get_input_channel_count(), 0);

        drop(obj);
    }
}