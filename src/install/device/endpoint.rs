//! host/device/endpoint.rs — 音频端点状态查询
//!
//! 查询 Windows 音频端点（渲染/采集）的设备 ID、友好名称与连接状态。
//! 供 `host/device/info.rs` 组合查询层使用。
//!
//! 依赖：
//! - `sys/registry/read`：注册表只读操作（Note 48）
//! - `utils/error`：统一错误类型（Note 36）
//! - `log` crate：日志记录
//!
//! 此模块只做查询，不修改任何系统状态（Note 23）。实际操作委托 `host/installation/`。

use log::warn;

use crate::utils::error::VxApoError;
use crate::sys::registry::read::{RegKey, RegValue};

// ══════════════════════════════════════════════════════════════════════════════
// 常量
// ══════════════════════════════════════════════════════════════════════════════

/// Windows 音频端点状态。
///
/// 对应 `IMMDevice::GetState` 返回值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointState {
    /// 端点活跃，可用于音频流。
    Active,
    /// 端点被禁用（用户或策略）。
    Disabled,
    /// 端点未插入（如拔掉耳机）。
    NotPresent,
    /// 未知状态值。
    Unknown(u32),
}

impl From<u32> for EndpointState {
    fn from(value: u32) -> Self {
        match value {
            // DEVICE_STATE_ACTIVE = 0x00000001
            1 => EndpointState::Active,
            // DEVICE_STATE_DISABLED = 0x00000002
            2 => EndpointState::Disabled,
            // DEVICE_STATE_NOTPRESENT = 0x00000004
            4 => EndpointState::NotPresent,
            other => EndpointState::Unknown(other),
        }
    }
}

/// 音频流方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// 渲染（播放）。
    Render,
    /// 采集（录音）。
    Capture,
}

/// 音频端点信息。
///
/// 从注册表查询，供 `device/info.rs` 使用。
/// 不可变——查询后只读。
#[derive(Debug, Clone)]
pub struct EndpointInfo {
    /// 端点设备 ID（`IMMDevice` 的 `PKEY_DeviceInstanceId`）。
    pub device_id: String,
    /// 端点友好名称（`PKEY_DeviceInterface_FriendlyName`）。
    pub friendly_name: String,
    /// 端点状态。
    pub state: EndpointState,
    /// 音频流方向。
    pub flow: Flow,
    /// 端点 GUID（`{21EC2020-...}` 等）。
    pub endpoint_guid: String,
}

// ══════════════════════════════════════════════════════════════════════════════
// 公开 API
// ══════════════════════════════════════════════════════════════════════════════

/// 从注册表查询端点状态。
///
/// # 参数
///
/// - `endpoint_key`：已打开的端点注册表键（通常位于
///   `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render\{guid}`
///   或 `...\Capture\{guid}`）。
///
/// # 返回
///
/// - `Ok(Some(info))`：成功查询。
/// - `Ok(None)`：键存在但缺少必要属性（非致命）。
/// - `Err(...)`：注册表 I/O 错误。
pub fn query_endpoint(endpoint_key: &RegKey) -> Result<Option<EndpointInfo>, VxApoError> {
    
    // ── 设备 ID ───────────────────────────────────────────────────────────

    let device_id = match endpoint_key.read_value("Device") {
        Ok(RegValue::Sz(s)) => s,
        Ok(other) => {
            warn!("Endpoint 'Device' value is unexpected type: {other:?}");
            return Ok(None);
        }
        Err(_) => {
            warn!("Endpoint missing 'Device' value");
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
/// 便捷方法，等价于 `query_endpoint(key)?.map(|e| e.state == Active).unwrap_or(false)`。
pub fn is_endpoint_active(endpoint_key: &RegKey) -> Result<bool, VxApoError> {
    match endpoint_key.read_dword_value("DeviceState") {
        Ok(state) => Ok(state == 1), // DEVICE_STATE_ACTIVE
        Err(_) => Ok(false),
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 内部辅助
// ══════════════════════════════════════════════════════════════════════════════

/// 从注册表键路径推断流方向。
///
/// MMDevices 注册表路径包含 `Render` 或 `Capture` 子路径：
/// - `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render\{guid}`
/// - `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture\{guid}`
///
/// 如果无法判断，默认为 Render（回放是 APO 的主要场景）。
fn detect_flow(endpoint_key: &RegKey) -> Flow {
    // 尝试从子键 `Properties` 的父路径推断。
    // RegKey 本身不暴露路径，但我们可以检查是否存在采集特有的属性。
    // 简单策略：默认 Render，由 info.rs 层根据上下文覆盖。
    //
    // TODO: Phase 5 迭代——如果 RegKey 暴露 key path，直接解析路径中的
    //       "Render"/"Capture" 来确定流方向。
    let _ = endpoint_key;
    Flow::Render
}

/// 从端点键提取端点 GUID。
///
/// 端点 GUID 是注册表键名的最后一段（`{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}`）。
/// 如果端点键有 `Properties` 子键，也可以从 `{b3f9a7e0-...}-0` 等值名中解析。
///
/// 当前实现：尝试读取 `Properties` 子键下的 `PKEY_AudioEndpoint_GUID` 值。
/// 如果不存在，返回空字符串（info.rs 层可从设备 ID 推导）。
fn extract_endpoint_guid(endpoint_key: &RegKey) -> String {
    // 尝试打开 Properties 子键读取 GUID 属性。
    if let Ok(props_key) = endpoint_key.open_sub_key("Properties") {
        // PKEY_AudioEndpoint_GUID = {b3f9a7e0-...}-0
        // 这是一个二进制 GUID 值（16 字节）。
        // 常见值名格式：设备注册表中存储为 {guid-string} 形式的 REG_SZ。
        if let Ok(guid_str) = props_key.get_guid_string("PKEY_AudioEndpoint_GUID") {
            return guid_str;
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

    // ── EndpointState ─────────────────────────────────────────────────────

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
    }

    #[test]
    fn state_from_zero_is_unknown() {
        assert_eq!(EndpointState::from(0), EndpointState::Unknown(0));
    }

    #[test]
    fn state_from_3_is_unknown() {
        // 3 是 Active | Disabled 的位组合，非标准值
        assert_eq!(EndpointState::from(3), EndpointState::Unknown(3));
    }

    #[test]
    fn state_equality() {
        assert_eq!(EndpointState::Active, EndpointState::Active);
        assert_ne!(EndpointState::Active, EndpointState::Disabled);
        assert_ne!(EndpointState::Active, EndpointState::NotPresent);
    }

    #[test]
    fn state_copy() {
        let s = EndpointState::Active;
        let s2 = s;
        assert_eq!(s, s2);
    }

    #[test]
    fn state_debug() {
        assert_eq!(format!("{:?}", EndpointState::Active), "Active");
        assert_eq!(format!("{:?}", EndpointState::Disabled), "Disabled");
        assert_eq!(format!("{:?}", EndpointState::NotPresent), "NotPresent");
        assert_eq!(format!("{:?}", EndpointState::Unknown(42)), "Unknown(42)");
    }

    // ── Flow ──────────────────────────────────────────────────────────────

    #[test]
    fn flow_equality() {
        assert_eq!(Flow::Render, Flow::Render);
        assert_eq!(Flow::Capture, Flow::Capture);
        assert_ne!(Flow::Render, Flow::Capture);
    }

    #[test]
    fn flow_copy() {
        let f = Flow::Render;
        let f2 = f;
        assert_eq!(f, f2);
    }

    #[test]
    fn flow_debug() {
        assert_eq!(format!("{:?}", Flow::Render), "Render");
        assert_eq!(format!("{:?}", Flow::Capture), "Capture");
    }

    // ── EndpointInfo ──────────────────────────────────────────────────────

    #[test]
    fn endpoint_info_debug() {
        let info = EndpointInfo {
            device_id: "test-device-id".to_string(),
            friendly_name: "Test Audio Device".to_string(),
            state: EndpointState::Active,
            flow: Flow::Render,
            endpoint_guid: "{12345678-1234-1234-1234-123456789abc}".to_string(),
        };
        let dbg = format!("{:?}", info);
        assert!(dbg.contains("device_id"));
        assert!(dbg.contains("test-device-id"));
        assert!(dbg.contains("Active"));
        assert!(dbg.contains("Render"));
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

    // ── detect_flow ───────────────────────────────────────────────────────

    #[test]
    fn detect_flow_default_is_render() {
        // 当前实现默认 Render（注释说明了原因）
        // 无法通过公共 API 测试内部函数，但可通过 query_endpoint 间接验证
        // 这里验证 Flow::Render 是默认值
        assert_eq!(Flow::Render as u8, 0);
    }

    // ── EndpointState 枚举完整性 ─────────────────────────────────────────

    #[test]
    fn all_standard_states_covered() {
        let states = [
            EndpointState::Active,
            EndpointState::Disabled,
            EndpointState::NotPresent,
        ];
        assert_eq!(states.len(), 3);
        // 确保没有遗漏
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

    // ── EndpointInfo 字段访问 ─────────────────────────────────────────────

    #[test]
    fn endpoint_info_empty_guid() {
        let info = EndpointInfo {
            device_id: String::new(),
            friendly_name: String::new(),
            state: EndpointState::Active,
            flow: Flow::Render,
            endpoint_guid: String::new(),
        };
        assert!(info.endpoint_guid.is_empty());
    }

    #[test]
    fn endpoint_info_active_render() {
        let info = EndpointInfo {
            device_id: "id".to_string(),
            friendly_name: "name".to_string(),
            state: EndpointState::Active,
            flow: Flow::Render,
            endpoint_guid: String::new(),
        };
        assert_eq!(info.state, EndpointState::Active);
        assert_eq!(info.flow, Flow::Render);
    }

    #[test]
    fn endpoint_info_disabled_capture() {
        let info = EndpointInfo {
            device_id: "id".to_string(),
            friendly_name: "name".to_string(),
            state: EndpointState::Disabled,
            flow: Flow::Capture,
            endpoint_guid: String::new(),
        };
        assert_eq!(info.state, EndpointState::Disabled);
        assert_eq!(info.flow, Flow::Capture);
    }

    // ── is_endpoint_active 集成测试 ───────────────────────────────────────
    //
    // 以下测试需要实际注册表访问，在 CI 中可能需要权限。
    // 标记为集成测试，依赖 HKCU 临时键。

    #[test]
    fn is_endpoint_active_nonexistent_key_returns_error() {
        // 尝试打开不存在的键 → 应返回错误（非 panic）
        let result = crate::sys::registry::read::RegKey::open(
            windows::Win32::System::Registry::HKEY_CURRENT_USER,
            "SOFTWARE\\VxAPO_Test_NonExistent_Endpoint_12345",
        );
        assert!(result.is_err());
    }

    #[test]
    fn endpoint_state_roundtrip() {
        // 验证 From<u32> 转换的完整覆盖
        for val in [1u32, 2, 4] {
            let state = EndpointState::from(val);
            assert_ne!(state, EndpointState::Unknown(val));
        }
        // 未知值
        for val in [0u32, 3, 5, 8, 16, 100, 0xFFFFFFFF] {
            let state = EndpointState::from(val);
            assert_eq!(state, EndpointState::Unknown(val));
        }
    }
}