//! instance/init.rs — APO 初始化（Note 7）
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
//! 依赖 `APOGUID_NOKEY` / `APOGUID_NOVALUE` 常量（`instance/object.rs`，Note 6）。
//!
//! Phase 4 为结构与解析框架占位，Phase 6 补全设备绑定的完整流程。

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
}