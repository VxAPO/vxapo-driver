//! host/installation/notify.rs — 音频服务变更通知
//!
//! APO 注册或注销完成后，通知 Windows 音频服务刷新设备列表。
//!
//! Phase 5 初期为无操作占位，Phase 8 补全实现。
//!
//! `notify_audio_service_changed()` 在 `DllRegisterServer` 与
//! `DllUnregisterServer` 完成后调用（Note 29/30）。
//!
//! 实现方案（Phase 8 选一）：
//! 1. 写入 `FXProperties` 子键的某个值触发变更通知
//! 2. 通过 `PolicyConfigClient` COM 接口触发
//! 3. 调用 `IMMDeviceEnumerator::RegisterEndpointNotificationCallback` 的反向通知
//!
//! 此模块不涉及注册表操作，仅触发音频服务刷新。

/// 通知音频服务设备属性已变更。
///
/// Phase 8 实现：写入 FXProperties 后调用，
/// 触发 Windows Audio Service 重新查询 APO 注册属性。
///
/// 当前为空实现，不影响 Phase 6/7 功能。
pub fn notify_audio_service_changed() {
    // TODO Phase 8: 实现音频服务变更通知
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_does_not_panic() {
        notify_audio_service_changed();
    }
}