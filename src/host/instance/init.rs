//! host/instance/init.rs — APO 初始化（Note 7）
//!
//! 解析 `APOInitSystemEffects` 初始化参数，完成 APO 实例的设备绑定与配置加载。
//!
//! 解析顺序（Note 7）：
//! 1. 验证 `cbDataSize`
//! 2. 提取 APO CLSID 确定模式（PreMix / PostMix）
//! 3. 获取端点 GUID
//! 4. 加载设备配置
//! 5. 获取子 APO GUID
//! 6. 读取 `allowSilentBufferModification`
//! 7. `CoCreateInstance` 子 APO
//! 8. 命名管道测试通信
//!
//! 类型安全（Note 7）：
//! 优先使用 `windows` crate 提供的 `APOInitSystemEffects` 类型定义。
//! 若手写定义，必须加编译期大小断言：
//! `const _: () = assert!(std::mem::size_of::<APOInitSystemEffects>() > 0);`
//!
//! 依赖 `APOGUID_NOKEY` / `APOGUID_NOVALUE` 常量（`host/instance/object.rs`，Note 6）。
//!
//! Phase 4 为结构与解析框架占位，Phase 6 补全设备绑定的完整流程。

use windows::core::HRESULT;

use crate::sys::com::prelude;
use crate::host::instance::object::APOGUID_NOKEY;

// ══════════════════════════════════════════════════════════════════════════════
// APOInitSystemEffects 解析（Note 7）
//
// 验证 cbDataSize → 提取 APO CLSID 确定模式 → 获取端点 GUID →
// 加载设备配置 → 获取子 APO GUID → 读取 allowSilentBufferModification →
// CoCreateInstance 子 APO → 命名管道测试通信
// ══════════════════════════════════════════════════════════════════════════════

/// APO 初始化参数（从 `APOInitSystemEffects` 解析）。
///
/// Phase 4 占位——Phase 6 补全完整字段。
#[derive(Debug, Clone)]
pub struct ApoInitParams {
    /// APO 自身的 CLSID。
    pub apo_clsid: windows::core::GUID,
    /// 端点设备 GUID。
    pub endpoint_guid: windows::core::GUID,
    /// 子 APO GUID（可能为 `APOGUID_NOKEY` 表示无子 APO）。
    pub child_apo_guid: windows::core::GUID,
    /// 是否允许静音缓冲区修改。
    pub allow_silent_buffer_modification: bool,
    /// 设备注册表路径。
    pub device_reg_path: String,
}

impl ApoInitParams {
    /// 创建默认初始化参数（Phase 4 占位）。
    ///
    /// Phase 6 将从真正的 `APOInitSystemEffects` 结构体中解析。
    pub fn default_for_clsid(clsid: windows::core::GUID) -> Self {
        Self {
            apo_clsid: clsid,
            endpoint_guid: windows::core::GUID::zeroed(),
            child_apo_guid: APOGUID_NOKEY,
            allow_silent_buffer_modification: false,
            device_reg_path: String::new(),
        }
    }

    /// 是否有子 APO。
    pub fn has_child_apo(&self) -> bool {
        self.child_apo_guid != APOGUID_NOKEY
            && self.child_apo_guid != super::object::APOGUID_NOVALUE
            && self.child_apo_guid != super::object::GUID_NULL
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// APOInitSystemEffects 解析（Note 7，Phase 6 完整实现）
// ══════════════════════════════════════════════════════════════════════════════

use crate::host::instance::apo_child::ChildApo;

/// 从原始初始化数据解析 `ApoInitParams`（Note 7）。
///
/// `p_init_data` 指向 `APOInitSystemEffects` 结构体，
/// `cb_data_size` 为字节大小。
///
/// # 解析顺序（Note 7）
///
/// 1. 验证 `cb_data_size` ≥ 最小大小
/// 2. 提取端点 GUID（偏移 20）
/// 3. 子 APO GUID 和 `allowSilentBufferModification` 从设备注册表读取
///
/// # Safety
///
/// - `p_init_data` 必须指向有效内存，至少 `cb_data_size` 字节
/// - `cb_data_size` 必须 ≥ `MIN_INIT_SIZE`
pub unsafe fn parse_apo_init_data(
    apo_clsid: windows::core::GUID,
    p_init_data: *const u8,
    cb_data_size: u32,
) -> Result<ApoInitParams, HRESULT> {
    const MIN_INIT_SIZE: u32 = 36; // cbSize(4) + APO CLSID(16) + endpoint GUID(16)

    if p_init_data.is_null() || cb_data_size < MIN_INIT_SIZE {
        return Err(prelude::E_INVALIDARG);
    }

    // Step 1: 读取 cbDataSize
    let reported_size = std::ptr::read_unaligned(p_init_data as *const u32);
    if reported_size < MIN_INIT_SIZE {
        return Err(prelude::E_INVALIDARG);
    }

    // Step 2: APO CLSID（由调用方传入，确定 PreMix/PostMix 模式）
    // Step 3: 端点 GUID（偏移 20 = cbSize(4) + CLSID(16)）
    let endpoint_guid = std::ptr::read_unaligned(
        p_init_data.add(20) as *const windows::core::GUID,
    );

    // Step 4: 从设备注册表加载子 APO GUID 和配置
    let device_reg_path = format_device_reg_path(&endpoint_guid);
    let (child_apo_guid, allow_silent) = load_device_config(&device_reg_path);

    Ok(ApoInitParams {
        apo_clsid,
        endpoint_guid,
        child_apo_guid,
        allow_silent_buffer_modification: allow_silent,
        device_reg_path,
    })
}

/// 从端点 GUID 构造设备注册表路径。
fn format_device_reg_path(endpoint_guid: &windows::core::GUID) -> String {
    // Windows 音频设备注册表路径格式：
    // HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render\{GUID}
    let g = endpoint_guid;
    format!(
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\MMDevices\\Audio\\Render\\\
         {{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        g.data1, g.data2, g.data3,
        g.data4[0], g.data4[1],
        g.data4[2], g.data4[3], g.data4[4], g.data4[5], g.data4[6], g.data4[7],
    )
}

/// 从设备注册表加载子 APO GUID 和 allowSilentBufferModification。
///
/// 读取 `FXProperties` 子键下的：
/// - `{00000000-0000-0000-0000-000000000000}` → 无子 APO（APOGUID_NOKEY）
/// - 其他 GUID → 子 APO CLSID
/// - `allowSilentBufferModification` (DWORD)
fn load_device_config(reg_path: &str) -> (windows::core::GUID, bool) {
    use crate::host::instance::object::APOGUID_NOKEY;

    // Phase 6: 从注册表读取。
    // 实际实现使用 sys::registry::read 模块。
    // 当前返回默认值（无子 APO，不允许静音修改）。
    //
    // TODO Phase 7: 完整注册表读取
    // let child_guid = registry::read_guid(reg_path, "FXProperties", "...");
    // let allow_silent = registry::read_dword(reg_path, "FXProperties", "...") != 0;

    let _ = reg_path;
    (APOGUID_NOKEY, false)
}

/// 初始化子 APO。
///
/// 如果 `params.child_apo_guid` 为有效子 APO GUID，
/// 通过 `CoCreateInstance` 创建子 APO 实例。
///
/// # Safety
///
/// COM 必须已初始化。
pub unsafe fn initialize_child_apo(
    params: &ApoInitParams,
) -> Result<Option<ChildApo>, HRESULT> {
    if !params.has_child_apo() {
        return Ok(None);
    }

    let child = ChildApo::create(&params.child_apo_guid)?;
    Ok(Some(child))
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::instance::reg_props::{CLSID_VXAPO_PRE_MIX, CLSID_VXAPO_POST_MIX};

    #[test]
    fn default_params_no_child() {
        let params = ApoInitParams::default_for_clsid(CLSID_VXAPO_PRE_MIX);
        assert_eq!(params.apo_clsid, CLSID_VXAPO_PRE_MIX);
        assert!(!params.has_child_apo());
        assert!(!params.allow_silent_buffer_modification);
    }

    #[test]
    fn default_params_postmix() {
        let params = ApoInitParams::default_for_clsid(CLSID_VXAPO_POST_MIX);
        assert_eq!(params.apo_clsid, CLSID_VXAPO_POST_MIX);
    }

    #[test]
    fn has_child_apo_with_valid_guid() {
        let mut params = ApoInitParams::default_for_clsid(CLSID_VXAPO_PRE_MIX);
        params.child_apo_guid = CLSID_VXAPO_POST_MIX;
        assert!(params.has_child_apo());
    }

    #[test]
    fn has_child_apo_with_nokey() {
        let params = ApoInitParams::default_for_clsid(CLSID_VXAPO_PRE_MIX);
        assert!(!params.has_child_apo());
    }

    #[test]
    fn has_child_apo_with_novalue() {
        let mut params = ApoInitParams::default_for_clsid(CLSID_VXAPO_PRE_MIX);
        params.child_apo_guid = super::super::object::APOGUID_NOVALUE;
        assert!(!params.has_child_apo());
    }

    #[test]
    fn has_child_apo_with_null_guid() {
        let mut params = ApoInitParams::default_for_clsid(CLSID_VXAPO_PRE_MIX);
        params.child_apo_guid = super::super::object::GUID_NULL;
        assert!(!params.has_child_apo());
    }

    #[test]
    fn init_params_debug() {
        let params = ApoInitParams::default_for_clsid(CLSID_VXAPO_PRE_MIX);
        let debug = format!("{params:?}");
        assert!(debug.contains("ApoInitParams"));
    }

    #[test]
    fn init_params_clone() {
        let params = ApoInitParams::default_for_clsid(CLSID_VXAPO_PRE_MIX);
        let cloned = params.clone();
        assert_eq!(params.apo_clsid, cloned.apo_clsid);
    }

        // ── Phase 6 解析测试 ────────────────────────────────────────────────────

    #[test]
    fn parse_init_data_null_pointer() {
        let result = unsafe {
            parse_apo_init_data(CLSID_VXAPO_PRE_MIX, std::ptr::null(), 0)
        };
        assert_eq!(result.unwrap_err(), prelude::E_INVALIDARG);
    }

    #[test]
    fn parse_init_data_too_small() {
        let data = [0u8; 10];
        let result = unsafe {
            parse_apo_init_data(CLSID_VXAPO_PRE_MIX, data.as_ptr(), 10)
        };
        assert_eq!(result.unwrap_err(), prelude::E_INVALIDARG);
    }

    #[test]
    fn parse_init_data_valid() {
        // 构造最小有效 APOInitSystemEffects：
        // [0..4]  cbSize = 36
        // [4..20] APO CLSID (unused here, passed separately)
        // [20..36] endpoint GUID
        let mut data = [0u8; 36];
        // cbSize = 36
        data[0..4].copy_from_slice(&36u32.to_ne_bytes());
        // endpoint GUID: 随机有效 GUID
        let ep_guid = windows::core::GUID::from_values(
            0x12345678, 0x1234, 0x5678,
            [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08],
        );
        let ep_bytes = guid_to_bytes(&ep_guid);
        data[20..36].copy_from_slice(&ep_bytes);

        let result = unsafe {
            parse_apo_init_data(CLSID_VXAPO_PRE_MIX, data.as_ptr(), 36)
        };
        let params = result.unwrap();
        assert_eq!(params.apo_clsid, CLSID_VXAPO_PRE_MIX);
        assert_eq!(params.endpoint_guid, ep_guid);
        assert!(!params.has_child_apo()); // 默认无子 APO
    }

    #[test]
    fn initialize_child_apo_none() {
        let params = ApoInitParams::default_for_clsid(CLSID_VXAPO_PRE_MIX);
        let result = unsafe { initialize_child_apo(&params) }.unwrap();
        assert!(result.is_none());
    }

    /// GUID → [u8; 16] 辅助（测试用）。
    fn guid_to_bytes(g: &windows::core::GUID) -> [u8; 16] {
        let mut buf = [0u8; 16];
        buf[0..4].copy_from_slice(&g.data1.to_le_bytes());
        buf[4..6].copy_from_slice(&g.data2.to_le_bytes());
        buf[6..8].copy_from_slice(&g.data3.to_le_bytes());
        buf[8..16].copy_from_slice(&g.data4);
        buf
    }
}