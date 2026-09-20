//! install/device/identity.rs — 端点稳定身份（跨端点 GUID 刷新）
//!
//! 背景：Windows 重新枚举音频端点后端点 GUID 会变化，老端点键
//! （`...\MMDevices\Audio\{Render|Capture}\{oldGuid}`）可能被**整体删除**，
//! 此时无法再从句柄读出老端点的设备实例 ID——`stale.rs` 的配对因此
//! 退化为「unmatched」，App 只剩清理出口（2026-09-16 实测）。本模块提供
//! 不依赖老端点键的身份来源：
//!
//! 1. **活跃端点 Properties**（只读）：实例 ID / 硬件 ID / 产品名 / 接口名；
//! 2. **端点历史属性** `{4b416b7d-8501-40c1-acfd-97aa9bdc17c8},1`：Windows
//!    自己维护的「同一端点的历史端点 ID」列表（REG_MULTI_SZ，元素形如
//!    `{0.0.0.00000000}.{guid}`，采集端点前缀为 `{0.0.1.00000000}`）。
//!    实测该属性保留了 GUID 刷新前的老 GUID，是当前唯一现成的新老联动关系；
//! 3. **VxAPO 记录键**（`HKLM\SOFTWARE\VxAPO\Child APOs\{guid}`）：安装/迁移
//!    时把身份落盘成**值**（不是子键——`stale.rs` 的 `copy_values` 只搬值，
//!    写成子键会在迁移时静默丢失）。
//!
//! 边界：只读端点注册表 + 读写 VxAPO 记录键值；不做安装/卸载决策。

use std::collections::HashMap;

use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;

use crate::sys::registry::{RegKey, RegValue};
use crate::utils::guid::parse_guid_string;
use crate::utils::vx_error::Result;

// ══════════════════════════════════════════════════════════════════════════════
// 端点 Properties 值名（Windows 11 实证）
// ══════════════════════════════════════════════════════════════════════════════

/// 端点 Properties 子键名。
pub const PROPERTIES_KEY: &str = "Properties";

/// PKEY_DeviceInstanceId（REG_SZ），形如 `{1}.USB\VID_2D99&PID_A037&MI_00\6&...&0&0000`。
pub const PKEY_DEVICE_INSTANCE_ID: &str = "{b3f8fa53-0004-438e-9003-51a46e139bfc},2";

/// PKEY_Device_ProductName（REG_SZ），形如 `EDIFIER M16+`。
pub const PKEY_DEVICE_PRODUCT_NAME: &str = "{b3f8fa53-0004-438e-9003-51a46e139bfc},6";

/// PKEY_DeviceInterface_FriendlyName（REG_SZ），形如 `扬声器`。
pub const PKEY_DEVICE_INTERFACE_FRIENDLY_NAME: &str =
    "{a45c254e-df1c-4efd-8020-67d146a850e0},2";

/// 设备节点硬件 ID 列表（REG_MULTI_SZ），形如
/// `{USB\VID_2D99&PID_A037&REV_0100&MI_00, USB\VID_2D99&PID_A037&MI_00}`。
pub const PKEY_DEVICE_HARDWARE_IDS: &str = "{9dad2fed-3c19-4cde-b3c9-1bd56be25698},0";

/// 端点历史 ID 列表（REG_MULTI_SZ），元素形如 `{0.0.0.00000000}.{guid}`。
pub const PKEY_ENDPOINT_HISTORY: &str = "{4b416b7d-8501-40c1-acfd-97aa9bdc17c8},1";

// ══════════════════════════════════════════════════════════════════════════════
// VxAPO 记录键值名（写入 `Child APOs\{guid}`）
// ══════════════════════════════════════════════════════════════════════════════

/// 归一化设备实例 ID（REG_SZ）。
pub const VALUE_DEVICE_INSTANCE_ID: &str = "DeviceInstanceId";
/// 设备节点硬件 ID 列表（REG_MULTI_SZ，归一化）。
pub const VALUE_DEVICE_HARDWARE_IDS: &str = "DeviceHardwareIds";
/// 产品名（REG_SZ），形如 `EDIFIER M16+`。
pub const VALUE_DEVICE_PRODUCT_NAME: &str = "DeviceProductName";
/// 该端点已知的端点 GUID 列表（REG_MULTI_SZ，小写带花括号，含当前 GUID）。
pub const VALUE_ENDPOINT_HISTORY: &str = "EndpointHistory";

// ══════════════════════════════════════════════════════════════════════════════
// 数据结构
// ══════════════════════════════════════════════════════════════════════════════

/// 一个音频端点的稳定身份（可全部为空——属性缺失时降级）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EndpointIdentity {
    /// 归一化设备实例 ID（大写；`{1}.` 前缀已去）。
    pub instance_id: String,
    /// 归一化硬件 ID 列表（大写）。
    pub hardware_ids: Vec<String>,
    /// 产品名（保留原大小写）。
    pub product_name: String,
    /// 接口友好名（保留原大小写），如「扬声器」。
    pub interface_name: String,
    /// 历史端点 GUID 列表（小写带花括号）。
    pub endpoint_history: Vec<String>,
}

impl EndpointIdentity {
    /// 是否完全没有身份信息（三个来源都缺）。
    pub fn is_empty(&self) -> bool {
        self.instance_id.is_empty()
            && self.hardware_ids.is_empty()
            && self.product_name.is_empty()
            && self.interface_name.is_empty()
            && self.endpoint_history.is_empty()
    }

    /// 归一化产品名（用于不区分大小写的比较）。
    pub fn product_key(&self) -> String {
        self.product_name.trim().to_lowercase()
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 归一化（纯函数）
// ══════════════════════════════════════════════════════════════════════════════

/// 归一化设备实例 ID / 硬件 ID：去 `\\?\`、`{1}.`、`{2}.` 前缀，
/// `#` → `\`，转大写，去首尾 `\`。
///
/// 前缀可**叠加**（实证 `{2}.\\?\usb#vid_...#6&...#{...}` 形态：设备接口路径
/// 转实例 ID 时同时带 `{2}.` 与 `\\?\`），故循环剥离直到稳定；旧实现只处理
/// 开头单个 `\\?\`，会残留 `?\USB\...`（本模块单测覆盖）。
///
/// 与旧 `stale.rs::normalize_device_id` 语义一致（该实现已上移到此处共用）。
pub fn normalize_device_id(value: &str) -> String {
    let mut s = value.trim().replace('#', "\\").to_ascii_uppercase();
    loop {
        if let Some(rest) = s.strip_prefix("\\\\?\\") {
            s = rest.to_string();
            continue;
        }
        if let Some(rest) = s.strip_prefix("{1}.") {
            s = rest.to_string();
            continue;
        }
        if let Some(rest) = s.strip_prefix("{2}.") {
            s = rest.to_string();
            continue;
        }
        break;
    }
    s.trim_matches('\\').to_string()
}

/// 归一化端点标识为 `{guid}` 形式（小写带花括号）。
///
/// 接受三种形态：
/// - `{0.0.0.00000000}.{guid}` / `{0.0.1.00000000}.{guid}`（端点历史属性元素）；
/// - `\\?\SWD#MMDEVAPI#{0.0.0.00000000}.{guid}#{...}`（设备接口路径，取最后一段 GUID）；
/// - `{guid}`（直接给 GUID）。
///
/// 非法 / 非 GUID 输入返回 `None`。
pub fn normalize_endpoint_guid(value: &str) -> Option<String> {
    let s = value.trim();
    let start = s.rfind('{')?;
    let rest = &s[start..];
    let end = rest.find('}')?;
    let candidate = &rest[..=end];
    parse_guid_string(candidate)?;
    Some(candidate.to_ascii_lowercase())
}

/// 端点历史列表 → 归一化 GUID 列表（丢弃无法解析的项，去重保序）。
pub fn normalize_endpoint_history(values: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in values {
        if let Some(guid) = normalize_endpoint_guid(v) {
            if !out.contains(&guid) {
                out.push(guid);
            }
        }
    }
    out
}

/// 合并端点历史列表（去重后排序，便于稳定比较与落盘）。
pub fn merge_endpoint_history(lists: &[Vec<String>]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for list in lists {
        for guid in list {
            if let Some(g) = normalize_endpoint_guid(guid) {
                if !out.contains(&g) {
                    out.push(g);
                }
            }
        }
    }
    out.sort();
    out
}

// ══════════════════════════════════════════════════════════════════════════════
// 读取：活跃端点身份
// ══════════════════════════════════════════════════════════════════════════════

/// 读取端点身份（失败/缺失一律降级为空值，不返回错误）。
///
/// `endpoint_path` 为端点注册表键路径（`...\MMDevices\Audio\{Render|Capture}\{guid}`）。
pub fn read_endpoint_identity(endpoint_path: &str) -> EndpointIdentity {
    let mut identity = EndpointIdentity::default();
    let Ok(endpoint_key) = RegKey::open(HKEY_LOCAL_MACHINE, endpoint_path) else {
        return identity;
    };
    let Ok(props) = endpoint_key.open_sub_key(PROPERTIES_KEY) else {
        return identity;
    };

    if let Some(v) = props.read_sz(PKEY_DEVICE_INSTANCE_ID) {
        identity.instance_id = normalize_device_id(&v);
    }
    if let Some(v) = props.read_sz(PKEY_DEVICE_PRODUCT_NAME) {
        identity.product_name = v.trim().to_string();
    }
    if let Some(v) = props.read_sz(PKEY_DEVICE_INTERFACE_FRIENDLY_NAME) {
        identity.interface_name = v.trim().to_string();
    }
    if let Ok(values) = props.read_multi_value(PKEY_DEVICE_HARDWARE_IDS) {
        identity.hardware_ids = values
            .iter()
            .map(|v| normalize_device_id(v))
            .filter(|v| !v.is_empty())
            .collect();
    }
    if let Ok(values) = props.read_multi_value(PKEY_ENDPOINT_HISTORY) {
        identity.endpoint_history = normalize_endpoint_history(&values);
    }
    identity
}

// ══════════════════════════════════════════════════════════════════════════════
// 读写：VxAPO 记录键身份值
// ══════════════════════════════════════════════════════════════════════════════

/// 从记录键值表还原身份（`stale.rs` 已有的 `read_info_values` 结果）。
pub fn identity_from_values(values: &HashMap<String, RegValue>) -> EndpointIdentity {
    let mut identity = EndpointIdentity::default();
    if let Some(v) = read_string(values.get(VALUE_DEVICE_INSTANCE_ID)) {
        identity.instance_id = normalize_device_id(&v);
    }
    if let Some(v) = read_string(values.get(VALUE_DEVICE_PRODUCT_NAME)) {
        identity.product_name = v.trim().to_string();
    }
    identity.hardware_ids = read_string_list(values.get(VALUE_DEVICE_HARDWARE_IDS))
        .iter()
        .map(|v| normalize_device_id(v))
        .filter(|v| !v.is_empty())
        .collect();
    identity.endpoint_history =
        normalize_endpoint_history(&read_string_list(values.get(VALUE_ENDPOINT_HISTORY)));
    identity
}

/// 写身份值到记录键。
///
/// `history` 为**已合并**的端点 GUID 列表（调用方负责并集：旧值 ∪ 活跃历史 ∪
/// 当前 GUID）——只写非空项，避免用空值覆盖已有数据。
pub fn write_identity_values(
    info: &RegKey,
    identity: &EndpointIdentity,
    history: &[String],
) -> Result<()> {
    if !identity.instance_id.is_empty() {
        info.write_sz(VALUE_DEVICE_INSTANCE_ID, &identity.instance_id)?;
    }
    if !identity.hardware_ids.is_empty() {
        info.write_multi_value(VALUE_DEVICE_HARDWARE_IDS, &identity.hardware_ids)?;
    }
    if !identity.product_name.is_empty() {
        info.write_sz(VALUE_DEVICE_PRODUCT_NAME, &identity.product_name)?;
    }
    let merged = merge_endpoint_history(&[history.to_vec()]);
    if !merged.is_empty() {
        info.write_multi_value(VALUE_ENDPOINT_HISTORY, &merged)?;
    }
    Ok(())
}

/// 读取记录键已落盘的身份（键不存在时返回默认值）。
pub fn read_stored_identity(device_guid: &str, child_apo_root: &str) -> EndpointIdentity {
    let key_path = format!("{child_apo_root}\\{device_guid}");
    let Ok(key) = RegKey::open(HKEY_LOCAL_MACHINE, &key_path) else {
        return EndpointIdentity::default();
    };
    identity_from_key(&key)
}

/// 从已打开的记录键读取已落盘身份（缺值 → 默认空身份）。
pub fn identity_from_key(key: &RegKey) -> EndpointIdentity {
    let mut identity = EndpointIdentity::default();
    if let Some(v) = key.read_sz(VALUE_DEVICE_INSTANCE_ID) {
        identity.instance_id = normalize_device_id(&v);
    }
    if let Some(v) = key.read_sz(VALUE_DEVICE_PRODUCT_NAME) {
        identity.product_name = v.trim().to_string();
    }
    if let Ok(values) = key.read_multi_value(VALUE_DEVICE_HARDWARE_IDS) {
        identity.hardware_ids = values
            .iter()
            .map(|v| normalize_device_id(v))
            .filter(|v| !v.is_empty())
            .collect();
    }
    if let Ok(values) = key.read_multi_value(VALUE_ENDPOINT_HISTORY) {
        identity.endpoint_history = normalize_endpoint_history(&values);
    }
    identity
}

/// `RegValue` → 单字符串（`REG_SZ` / `REG_MULTI_SZ` 首项）。
fn read_string(value: Option<&RegValue>) -> Option<String> {
    match value? {
        RegValue::Sz(v) => Some(v.clone()),
        RegValue::MultiSz(v) => v.first().cloned(),
        _ => None,
    }
}

/// `RegValue` → 字符串列表（`REG_MULTI_SZ` 原样；`REG_SZ` 视为单元素列表）。
fn read_string_list(value: Option<&RegValue>) -> Vec<String> {
    match value {
        Some(RegValue::MultiSz(v)) => v.clone(),
        Some(RegValue::Sz(v)) => vec![v.clone()],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_device_id_strips_prefixes_and_separators() {
        assert_eq!(
            normalize_device_id(r"{1}.USB\VID_2D99&PID_A037&MI_00\6&20be7186&2&0000"),
            r"USB\VID_2D99&PID_A037&MI_00\6&20BE7186&2&0000"
        );
        assert_eq!(
            normalize_device_id(r"\\?\usb#vid_3302&pid_39c3&mi_01#6&2cc8475c&0&0001"),
            r"USB\VID_3302&PID_39C3&MI_01\6&2CC8475C&0&0001"
        );
        assert_eq!(
            normalize_device_id(r"{2}.\\?\usb#vid_3302&pid_39c3&mi_01#6&2cc8475c&0&0001"),
            r"USB\VID_3302&PID_39C3&MI_01\6&2CC8475C&0&0001"
        );
        assert_eq!(normalize_device_id("  USB\\Class_01\\  "), r"USB\CLASS_01");
    }

    #[test]
    fn normalize_endpoint_guid_accepts_history_entries() {
        let render = "{0.0.0.00000000}.{07BE3684-52E4-41E7-AEFC-FF8C42BCC1DD}";
        let capture = "{0.0.1.00000000}.{DDF937E0-2F72-47F7-B30C-6F25D776023D}";
        assert_eq!(
            normalize_endpoint_guid(render).as_deref(),
            Some("{07be3684-52e4-41e7-aefc-ff8c42bcc1dd}")
        );
        assert_eq!(
            normalize_endpoint_guid(capture).as_deref(),
            Some("{ddf937e0-2f72-47f7-b30c-6f25d776023d}")
        );
        assert_eq!(
            normalize_endpoint_guid("{07BE3684-52E4-41E7-AEFC-FF8C42BCC1DD}").as_deref(),
            Some("{07be3684-52e4-41e7-aefc-ff8c42bcc1dd}")
        );
    }

    #[test]
    fn normalize_endpoint_guid_accepts_interface_path() {
        let path = r"\\?\SWD#MMDEVAPI#{0.0.0.00000000}.{0B633063-AF03-4F85-853B-CEF12885D4C8}#{e6327cad-dcec-4949-ae8a-991e976a79d2}";
        assert_eq!(
            normalize_endpoint_guid(path).as_deref(),
            Some("{e6327cad-dcec-4949-ae8a-991e976a79d2}")
        );
    }

    #[test]
    fn normalize_endpoint_guid_rejects_non_guid() {
        assert!(normalize_endpoint_guid("{0.0.0.00000000}").is_none());
        assert!(normalize_endpoint_guid("扬声器").is_none());
        assert!(normalize_endpoint_guid("").is_none());
    }

    #[test]
    fn normalize_endpoint_history_dedups_and_skips_garbage() {
        let raw = vec![
            "{0.0.0.00000000}.{07BE3684-52E4-41E7-AEFC-FF8C42BCC1DD}".to_string(),
            "{0.0.0.00000000}.{07be3684-52e4-41e7-aefc-ff8c42bcc1dd}".to_string(),
            "garbage".to_string(),
        ];
        assert_eq!(
            normalize_endpoint_history(&raw),
            vec!["{07be3684-52e4-41e7-aefc-ff8c42bcc1dd}".to_string()]
        );
    }

    #[test]
    fn merge_endpoint_history_is_sorted_and_unique() {
        let merged = merge_endpoint_history(&[
            vec!["{0.0.0.00000000}.{BBBBBBBB-0000-0000-0000-000000000000}".to_string()],
            vec![
                "{AAAAAAAA-0000-0000-0000-000000000000}".to_string(),
                "{0.0.1.00000000}.{bbbbbbbb-0000-0000-0000-000000000000}".to_string(),
            ],
        ]);
        assert_eq!(
            merged,
            vec![
                "{aaaaaaaa-0000-0000-0000-000000000000}".to_string(),
                "{bbbbbbbb-0000-0000-0000-000000000000}".to_string(),
            ]
        );
    }

    #[test]
    fn identity_from_values_reads_all_fields() {
        let mut values: HashMap<String, RegValue> = HashMap::new();
        values.insert(
            VALUE_DEVICE_INSTANCE_ID.to_string(),
            RegValue::Sz(r"{1}.USB\VID_2D99&PID_A037&MI_00\6&20be7186&2&0000".to_string()),
        );
        values.insert(
            VALUE_DEVICE_HARDWARE_IDS.to_string(),
            RegValue::MultiSz(vec![r"USB\VID_2D99&PID_A037&MI_00".to_string()]),
        );
        values.insert(
            VALUE_DEVICE_PRODUCT_NAME.to_string(),
            RegValue::Sz("EDIFIER M16+".to_string()),
        );
        values.insert(
            VALUE_ENDPOINT_HISTORY.to_string(),
            RegValue::MultiSz(vec![
                "{0.0.0.00000000}.{52237A1A-647C-4196-855F-E693A6504EE9}".to_string()
            ]),
        );
        let identity = identity_from_values(&values);
        assert_eq!(
            identity.instance_id,
            r"USB\VID_2D99&PID_A037&MI_00\6&20BE7186&2&0000"
        );
        assert_eq!(identity.hardware_ids, vec![r"USB\VID_2D99&PID_A037&MI_00".to_string()]);
        assert_eq!(identity.product_key(), "edifier m16+");
        assert_eq!(
            identity.endpoint_history,
            vec!["{52237a1a-647c-4196-855f-e693a6504ee9}".to_string()]
        );
    }

    #[test]
    fn identity_from_values_empty_map_is_empty() {
        assert!(identity_from_values(&HashMap::new()).is_empty());
    }
}
