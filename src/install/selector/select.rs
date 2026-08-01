//! install/selector/select.rs — 设备选择交互（v6.4 规范 5.5.1）
//!
//! 职责：枚举设备、列出名称、让用户选择，并调度 operation 执行安装/卸载。
//!
//! **禁止**：
//! - 不自行遍历注册表 MMDevices（不得依赖 `sys/registry` 直接操作）——设备列表一律来自 `device/info::enumerate_devices`
//! - 不包含安装/回滚业务逻辑（在 operation.rs）

use std::io::{self, Write};

use crate::install::device::info::{enumerate_devices, DeviceInfo};
use crate::utils::vx_error::{Result, VxApoError};

/// 列出所有设备（委托 `device/info::enumerate_devices`）。
pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    enumerate_devices()
}

/// 选择设备。
///
/// 枚举所有设备，列出编号，等待用户输入。
/// 输入无效或用户取消（EOF）时返回 `Ok(None)`。
pub fn select_device() -> Result<Option<DeviceInfo>> {
    let devices = list_devices()?;
    if devices.is_empty() {
        return Err(VxApoError::internal("未发现任何音频端点"));
    }
    print_device_list(&devices);
    let idx = prompt_user(&devices)?;
    Ok(devices.get(idx).cloned())
}

/// 运行安装流程：选择设备 → 调度 operation::install_endpoint。
pub fn run_install_flow() -> Result<()> {
    let device = select_device()?
        .ok_or_else(|| VxApoError::internal("未选择设备"))?;

    // 从 DeviceInfo 提取端点 GUID 与友好名称。
    let guid = device
        .endpoint
        .as_ref()
        .map(|e| e.endpoint_guid.clone())
        .ok_or_else(|| VxApoError::internal("设备缺少端点信息"))?;
    let name = device
        .endpoint
        .as_ref()
        .map(|e| e.friendly_name.clone())
        .unwrap_or_else(|| guid.clone());

    let config = crate::install::selector::operation::InstallConfig::default_config();
    crate::install::selector::operation::install_endpoint(&guid, &name, &name, &config)
}

/// 运行卸载流程：选择设备 → 调度 operation::uninstall_endpoint。
pub fn run_uninstall_flow() -> Result<()> {
    let device = select_device()?
        .ok_or_else(|| VxApoError::internal("未选择设备"))?;

    let guid = device
        .endpoint
        .as_ref()
        .map(|e| e.endpoint_guid.clone())
        .ok_or_else(|| VxApoError::internal("设备缺少端点信息"))?;

    crate::install::selector::operation::uninstall_endpoint(&guid)
}

/// 打印设备列表。
pub fn print_device_list(devices: &[DeviceInfo]) {
    for (i, d) in devices.iter().enumerate() {
        let name = d
            .endpoint
            .as_ref()
            .map(|e| e.friendly_name.as_str())
            .unwrap_or("<unknown>");
        let state = if d.is_installed() { "已安装" } else { "未安装" };
        let mode = match d.install_mode {
            crate::install::device::slots::InstallMode::LfxGfx => "LfxGfx",
            crate::install::device::slots::InstallMode::SfxMfx => "SfxMfx",
            crate::install::device::slots::InstallMode::SfxEfx => "SfxEfx",
        };
        println!("  [{}] {} ({}, {})", i, name, state, mode);
    }
}

/// 提示用户选择设备，返回索引。
pub fn prompt_user(devices: &[DeviceInfo]) -> Result<usize> {
    print!("请选择设备编号 [0-{}]: ", devices.len().saturating_sub(1));
    io::stdout()
        .flush()
        .map_err(|e| VxApoError::internal(&format!("flush failed: {}", e)))?;

    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .map_err(|e| VxApoError::internal(&format!("read failed: {}", e)))?;

    let idx: usize = line
        .trim()
        .parse()
        .map_err(|_| VxApoError::internal("无效的设备编号"))?;

    if idx >= devices.len() {
        return Err(VxApoError::internal("设备编号超出范围"));
    }
    Ok(idx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::device::endpoint::{EndpointInfo, EndpointState, Flow};
    use crate::install::device::slots::{InstallMode, SlotValue};

    fn make_device(name: &str, installed: bool) -> DeviceInfo {
        let mut slots = [SlotValue::NoKey; 5];
        if installed {
            slots[2] = SlotValue::Guid(crate::object::vx_reg_props::CLSID_VXAPO_PRE_MIX);
        }
        DeviceInfo {
            endpoint: Some(EndpointInfo {
                device_id: "test-id".to_string(),
                friendly_name: name.to_string(),
                state: EndpointState::Active,
                flow: Flow::Render,
                endpoint_guid: "{00000000-0000-0000-0000-000000000000}".to_string(),
            }),
            install_mode: InstallMode::SfxEfx,
            slots,
            format: None,
            installed_version: if installed { "2".to_string() } else { String::new() },
        }
    }

    #[test]
    fn print_device_list_renders() {
        let devices = vec![make_device("Speakers", true), make_device("Headphones", false)];
        print_device_list(&devices);
    }

    #[test]
    fn detect_endpoint_guid() {
        let d = make_device("Test", false);
        let guid = d
            .endpoint
            .as_ref()
            .map(|e| e.endpoint_guid.clone())
            .unwrap();
        assert_eq!(guid, "{00000000-0000-0000-0000-000000000000}");
    }
}