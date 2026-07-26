/// 宿主层上下文。
/// Phase 4: 仅定义结构体和基础字段。
/// Phase 6: 补充子 APO 相关字段。
pub struct HostContext {
    // 基础字段（Phase 4 填充）
    pub config_path: String,
    pub is_capture: bool,
    pub is_pre_mix: bool,

    // Phase 6 补充（init.rs 解析 APOInitSystemEffects 后填充）
    pub device_guid: Option<String>,
    pub device_name: Option<String>,
    pub connection_name: Option<String>,
}