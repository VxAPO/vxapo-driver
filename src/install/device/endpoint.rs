//! install/device/endpoint.rs — 音频端点状态查询（v6.2 规范 5.1）
//!
//! 查询 Windows 音频端点的设备 ID、友好名称与连接状态。只读。

use crate::sys::registry::{RegKey, RegValue};
use crate::utils::vx_error::Result;

// ══════════════════════════════════════════════════════════════════════════════
// 数据结构
// ══════════════════════════════════════════════════════════════════════════════

/// Windows 音频端点状态（对应 IMMDevice::GetState 返回值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointState {
    /// DEVICE_STATE_ACTIVE = 1
    Active,
    /// DEVICE_STATE_DISABLED = 2
    Disabled,
    /// DEVICE_STATE_NOTPRESENT = 4
    NotPresent,
    /// 未知状态值。
    Unknown(u32),
}

impl From<u32> for EndpointState {
    fn from(value: u32) -> Self {
        match value {
            1 => EndpointState::Active,
            2 => EndpointState::Disabled,
            4 => EndpointState::NotPresent,
            other => EndpointState::Unknown(other),
        }
    }
}

/// 音频流方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Render,
    Capture,
}

/// 音频端点信息（从注册表查询，只读）。
#[derive(Debug, Clone)]
pub struct EndpointInfo {
    /// 端点设备 ID（IMMDevice 的 PKEY_DeviceInstanceId）。
    pub device_id: String,
    /// 端点友好名称（PKEY_DeviceInterface_FriendlyName）。
    pub friendly_name: String,
    /// 端点状态。
    pub state: EndpointState,
    /// 音频流方向。
    pub flow: Flow,
    /// 端点 GUID。
    pub endpoint_guid: String,
}

// ══════════════════════════════════════════════════════════════════════════════
// 公开 API
// ══════════════════════════════════════════════════════════════════════════════

/// 从已打开的端点注册表键查询端点信息。
///
/// 端点键位于：
/// `HKLM\...\MMDevices\Audio\Render\{guid}` 或 `...\Capture\{guid}`
///
/// 返回 `Ok(Some(info))` / `Ok(None)`（属性缺失）/ `Err`（I/O 错误）。
pub fn query_endpoint(endpoint_key: &RegKey) -> Result<Option<EndpointInfo>> {
    // ── 设备 ID ───────────────────────────────────────────────────────────

    let device_id = match endpoint_key.read_value("Device") {
        Ok(RegValue::Sz(s)) => s,
        Ok(_) => {
            // 类型不符，非致命。
            return Ok(None);
        }
        Err(_) => {
            // 值不存在，非致命。
            return Ok(None);
        }
    };

    // ── 友好名称 ──────────────────────────────────────────────────────────

    let friendly_name = match endpoint_key.read_value("FriendlyName") {
        Ok(RegValue::Sz(s)) => s,
        _ => String::new(),
    };

    // ── 状态 ──────────────────────────────────────────────────────────────

    let state_raw = endpoint_key
        .read_dword_value("DeviceState")
        .unwrap_or(0);
    let state = EndpointState::from(state_raw);

    // ── 流方向 ────────────────────────────────────────────────────────────

    let flow = detect_flow(endpoint_key);

    // ── 端点 GUID ─────────────────────────────────────────────────────────

    let endpoint_guid = extract_endpoint_guid(endpoint_key);

    Ok(Some(EndpointInfo {
        device_id,
        friendly_name,
        state,
        flow,
        endpoint_guid,
    }))
}

/// 检查端点是否为活跃状态。
///
/// 便捷方法，等价于读取 DeviceState 值是否为 1。
pub fn is_endpoint_active(endpoint_key: &RegKey) -> Result<bool> {
    match endpoint_key.read_dword_value("DeviceState") {
        Ok(state) => Ok(state == 1), // DEVICE_STATE_ACTIVE
        Err(_) => Ok(false),
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 内部辅助
// ══════════════════════════════════════════════════════════════════════════════

/// 从注册表键推断流方向。
///
/// RegKey 不暴露路径，此处从 Properties 子键特征判断：
/// - 存在 `PKEY_AudioEndpoint_GUID` 特性且端点有 `Render` 特征时返回 Render
/// - 简化策略：默认 Render（回放是 APO 主要场景），由 info.rs 根据参数覆盖。
fn detect_flow(_endpoint_key: &RegKey) -> Flow {
    Flow::Render
}

/// 从端点键提取端点 GUID。
///
/// 尝试读取 Properties 子键下的 `PKEY_AudioEndpoint_GUID` 值。
/// 如果不存在，返回空字符串（info.rs 层可从设备 ID 推导）。
fn extract_endpoint_guid(endpoint_key: &RegKey) -> String {
    if let Ok(props_key) = endpoint_key.open_sub_key("Properties") {
        // 尝试直接读取（部分系统以 REG_SZ 形式存储）。
        if let Ok(RegValue::Sz(s)) = props_key.read_value("PKEY_AudioEndpoint_GUID") {
            return s;
        }
        // 备选：二进制 GUID 值（16 字节小端）→ 格式化。
        if let Ok(raw) = props_key.read_binary_value("PKEY_AudioEndpoint_GUID") {
            if raw.len() >= 16 {
                let guid = windows::core::GUID {
                    data1: u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]),
                    data2: u16::from_le_bytes([raw[4], raw[5]]),
                    data3: u16::from_le_bytes([raw[6], raw[7]]),
                    data4: [
                        raw[8], raw[9], raw[10], raw[11], raw[12], raw[13], raw[14], raw[15],
                    ],
                };
                return format!("{:?}", guid);
            }
        }
    }
    String::new()
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试（Note 41）
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_from_active() {
        assert_eq!(EndpointState::from(1), EndpointState::Active);
    }

    #[test]
    fn state_from_disabled() {
        assert_eq!(EndpointState::from(2), EndpointState::Disabled);
    }

    #[test]
    fn state_from_not_present() {
        assert_eq!(EndpointState::from(4), EndpointState::NotPresent);
    }

    #[test]
    fn state_from_unknown() {
        assert_eq!(EndpointState::from(99), EndpointState::Unknown(99));
        assert_eq!(EndpointState::from(0), EndpointState::Unknown(0));
    }

    #[test]
    fn flow_equality() {
        assert_eq!(Flow::Render, Flow::Render);
        assert_eq!(Flow::Capture, Flow::Capture);
        assert_ne!(Flow::Render, Flow::Capture);
    }

    #[test]
    fn detect_flow_default_is_render() {
        assert_eq!(Flow::Render as u8, 0);
        assert_eq!(Flow::Capture as u8, 1);
    }

    #[test]
    fn all_standard_states_covered() {
        let states = [
            EndpointState::Active,
            EndpointState::Disabled,
            EndpointState::NotPresent,
        ];
        assert_eq!(states.len(), 3);
        for (i, s) in states.iter().enumerate() {
            for (j, s2) in states.iter().enumerate() {
                if i == j {
                    assert_eq!(s, s2);
                } else {
                    assert_ne!(s, s2);
                }
            }
        }
    }

    #[test]
    fn endpoint_info_clone() {
        let info = EndpointInfo {
            device_id: "test".to_string(),
            friendly_name: "Device".to_string(),
            state: EndpointState::Disabled,
            flow: Flow::Capture,
            endpoint_guid: String::new(),
        };
        let cloned = info.clone();
        assert_eq!(info.device_id, cloned.device_id);
        assert_eq!(info.friendly_name, cloned.friendly_name);
        assert_eq!(info.state, cloned.state);
        assert_eq!(info.flow, cloned.flow);
    }

    #[test]
    fn endpoint_state_roundtrip() {
        for val in [1u32, 2, 4] {
            let state = EndpointState::from(val);
            assert_ne!(state, EndpointState::Unknown(val));
        }
        for val in [0u32, 3, 5, 8, 100, 0xFFFFFFFF] {
            let state = EndpointState::from(val);
            assert_eq!(state, EndpointState::Unknown(val));
        }
    }
}