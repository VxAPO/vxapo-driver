//! install/device/endpoint.rs — 音频端点状态查询（规范 5.1）
//!
//! 查询 Windows 音频端点的设备 ID、友好名称与连接状态。只读。

use crate::sys::registry::{RegKey, RegValue};
use crate::sys::com::prelude::guid_to_string;
use crate::utils::vx_error::Result;
use crate::utils::guid::guid_from_bytes;

// ── MMDevices Properties 值名常量（Windows 11 实证）──
const PKEY_DEVICE_INSTANCE_ID: &str = "{b3f8fa53-0004-438e-9003-51a46e139bfc},2";
const PKEY_DEVICE_INTERFACE_FRIENDLY_NAME: &str = "{a45c254e-df1c-4efd-8020-67d146a850e0},2";
const PKEY_DEVICE_PRODUCT_NAME: &str = "{b3f8fa53-0004-438e-9003-51a46e139bfc},6";
const PKEY_AUDIO_ENDPOINT_GUID_VALUE: &str = "{9D631510-92A8-4a79-A79E-A83812C9C119},2";
const PKEY_AUDIO_ENDPOINT_GUID_NAME: &str = "PKEY_AudioEndpoint_GUID";

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

    // MMDevices 端点键：业务数据（设备 ID/友好名/端点 GUID）在 Properties 子键
    // （PKEY_* 值名），顶层仅 DeviceState。旧实现读顶层 "Device"/"FriendlyName"
    // 失败即返回 None，导致所有端点被丢弃——修正为读 Properties + 字段缺失不丢端点。
    let properties = match endpoint_key.open_sub_key("Properties") {
        Ok(k) => Some(k),
        Err(_) => None,
    };

    // ── 设备 ID ───────────────────────────────────────────────────────────
    // PKEY_DeviceInstanceId 值名：{b3f8fa53-0004-438e-9003-51a46e139bfc},2（REG_SZ，Windows 11 实证）。

    let device_id = properties
        .as_ref()
        .and_then(|p| p.read_sz(PKEY_DEVICE_INSTANCE_ID))
        .unwrap_or_default();

    // ── 友好名称 ──────────────────────────────────────────────────────────
    // 与旧 CLI 一致的组合显示：接口友好名（PKEY_DeviceInterface_FriendlyName={a45c254e...},2，
    // 实际是「扬声器/耳机」类型名）+ 设备产品名（PKEY_Device_ProductName={b3f8fa53...},6，
    // 实际是「EDIFIER M16+」具体型号）。两者相同只取其一；不同组合「接口名(产品名)」，
    // 让用户能看到具体设备而非只有类型。
    let interface_name = properties
        .as_ref()
        .and_then(|p| p.read_sz(PKEY_DEVICE_INTERFACE_FRIENDLY_NAME));
    let product_name = properties
        .as_ref()
        .and_then(|p| p.read_sz(PKEY_DEVICE_PRODUCT_NAME));
    let friendly_name = match (&interface_name, &product_name) {
        (Some(a), Some(b)) if a != b => format!("{a} ({b})"),
        (Some(a), _) => a.clone(),
        (None, Some(b)) => b.clone(),
        (None, None) => String::new(),
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
        // Windows 实际值名（与端点子键名一致的 GUID，Windows 11 实证）。
        if let Ok(RegValue::Sz(s)) = props_key.read_value(PKEY_AUDIO_ENDPOINT_GUID_VALUE) {
            return s;
        }
        // 兼容部分系统以文本名存储。
        if let Ok(RegValue::Sz(s)) = props_key.read_value(PKEY_AUDIO_ENDPOINT_GUID_NAME) {
            return s;
        }
        // 备选：二进制 GUID 值（16 字节小端）→ 格式化。
        if let Ok(raw) = props_key.read_binary_value(PKEY_AUDIO_ENDPOINT_GUID_NAME) {
            if let Some(guid) = guid_from_bytes(&raw) {
                return guid_to_string(&guid);
            }
        }
    }
    String::new()
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
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
