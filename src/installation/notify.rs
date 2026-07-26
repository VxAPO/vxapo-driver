//! installation/notify.rs — 音频服务变更通知
//!
//! APO 注册或注销完成后，通知 Windows 音频服务刷新设备列表。
//!
//! Phase 5 初期为无操作占位，Phase 6 补全完整的 `IMMNotificationClient` 实现。
//!
//! `notify_audio_service_changed()` 在 `DllRegisterServer` 与
//! `DllUnregisterServer` 完成后调用（Note 29/30）。
//!
//! 此模块不涉及注册表操作，仅触发音频服务刷新。

pub fn notify_audio_service_changed() {
    // TODO Phase 6: 通过 IMMNotificationClient 通知音频服务
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