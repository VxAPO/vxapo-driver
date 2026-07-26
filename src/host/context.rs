//! host/context.rs — 宿主层上下文（Phase 4/6）

/// 宿主层上下文。
///
/// Phase 4: 基础字段。
/// Phase 6: 设备信息字段。
#[derive(Debug, Clone)]
pub struct HostContext {
    // 基础字段（Phase 4）
    pub config_path: String,
    pub is_capture: bool,
    pub is_pre_mix: bool,

    // 设备信息（Phase 6，从 APOInitSystemEffects 解析）
    pub device_endpoint_id: Option<windows::core::GUID>,
    pub device_name: Option<String>,
    pub connection_name: Option<String>,
}

impl HostContext {
    /// 从初始化参数填充设备信息。
    pub fn update_from_init(&mut self, params: &super::instance::init::ApoInitParams) {
        self.device_endpoint_id = Some(params.endpoint_guid);
    }
}