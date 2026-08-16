//! install/audiodg.rs — DisableProtectedAudioDG 检查与修复（v6.3 规范 5.6）
//!
//! 保护模式阻止第三方 APO 加载。通过注册表
//! `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Audio` 下的
//! `DisableProtectedAudioDG`（REG_DWORD）控制。

use crate::sys::registry::RegKey;
use crate::utils::vx_error::{Result, VxApoError};
use windows::Win32::System::Registry::{HKEY, HKEY_LOCAL_MACHINE};

/// 注册表路径：HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Audio。
const AUDIO_KEY_PATH: &str =
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\Audio";

/// 值名：DisableProtectedAudioDG。
const VALUE_NAME: &str = "DisableProtectedAudioDG";

// ══════════════════════════════════════════════════════════════════════════════
// 公开 API
// ══════════════════════════════════════════════════════════════════════════════

/// 检查保护是否已禁用。
///
/// 返回 `true`：DisableProtectedAudioDG 值存在且 == 1（允许第三方 APO 加载）。
/// 返回 `false`：值不存在或 != 1（Windows 阻止第三方 APO 加载）。
pub fn is_disabled() -> Result<bool> {
    let key = match RegKey::open(HKEY_LOCAL_MACHINE, AUDIO_KEY_PATH) {
        Ok(k) => k,
        Err(_) => {
            // 键不存在 → 未禁用。
            return Ok(false);
        }
    };

    match key.read_dword_value(VALUE_NAME) {
        Ok(v) => Ok(v == 1),
        Err(_) => Ok(false),
    }
}

/// 检查是否允许第三方 APO 加载。
///
/// `is_disabled()` 的语义别名——返回 `true` 表示可以加载。
pub fn is_third_party_allowed() -> Result<bool> {
    is_disabled()
}

/// 设置 DisableProtectedAudioDG = 1（禁用保护，允许第三方加载）。
///
/// 需要管理员权限（写入 HKLM）。
pub fn disable() -> Result<()> {
    let key = RegKey::create(HKEY_LOCAL_MACHINE, AUDIO_KEY_PATH)?;
    key.write_dword(VALUE_NAME, 1)?;
    Ok(())
}

/// 删除 DisableProtectedAudioDG 值（恢复 Windows 默认保护行为）。
///
/// 值不存在不算错误。
pub fn restore() -> Result<()> {
    let key = match RegKey::open(HKEY_LOCAL_MACHINE, AUDIO_KEY_PATH) {
        Ok(k) => k,
        Err(_) => {
            // 键不存在 → 无需恢复。
            return Ok(());
        }
    };
    key.delete_value(VALUE_NAME)?;
    Ok(())
}

/// 检查并确保允许加载，不允许时尝试修复。
///
/// 由 object/apo.rs LockForProcess 调用。
pub fn ensure_can_load() -> Result<()> {
    // v9.6 进程内缓存：设置页/多流会瞬间调用大量 LockForProcess；该值安装后
    // 已是 1（uninstall 会重启音频服务/进程），首查成功后无需反复读注册表。
    use std::sync::atomic::{AtomicBool, Ordering};
    static DISABLED: AtomicBool = AtomicBool::new(false);
    if DISABLED.load(Ordering::Relaxed) {
        return Ok(());
    }
    if is_disabled()? {
        DISABLED.store(true, Ordering::Relaxed);
        return Ok(());
    }
    disable()?;
    DISABLED.store(true, Ordering::Relaxed);
    Ok(())
}

/// 停止 Windows 音频服务（只停不启，uninstall 前置用）。
///
/// audiodg 持有点端锁 MMDevices 槽位句柄时，删除槽位值会失败——必须先停服务。
/// 与 `restart_audio_service` 共用停服逻辑，但**不开起**（uninstall 删槽位后由
/// CLI 层调 `restart_audio_service` 恢复）。
pub fn stop_audio_service() -> Result<()> {
    use windows::Win32::System::Services::{
        OpenSCManagerW, OpenServiceW, ControlService, QueryServiceStatus,
        CloseServiceHandle, SC_HANDLE, SERVICE_STATUS,
        SC_MANAGER_ALL_ACCESS, SERVICE_ALL_ACCESS, SERVICE_CONTROL_STOP,
        SERVICE_STOPPED, SERVICE_RUNNING,
    };
    use windows::core::{PCWSTR, HSTRING};

    // SAFETY: 本函数在非 RT 控制线程调用（install/CLI），无实时约束。
    let scm = unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ALL_ACCESS) }
        .map_err(|e| VxApoError::internal(&format!("OpenSCManagerW failed: {e}")))?;

    struct ScmGuard(SC_HANDLE);
    impl Drop for ScmGuard {
        fn drop(&mut self) {
            let _ = unsafe { CloseServiceHandle(self.0) };
        }
    }
    let _scm_guard = ScmGuard(scm);

    let service_name = HSTRING::from("AudioSrv");
    let svc = unsafe { OpenServiceW(scm, &service_name, SERVICE_ALL_ACCESS) }
        .map_err(|e| VxApoError::internal(&format!("OpenServiceW(AudioSrv) failed: {e}")))?;

    struct SvcGuard(SC_HANDLE);
    impl Drop for SvcGuard {
        fn drop(&mut self) {
            let _ = unsafe { CloseServiceHandle(self.0) };
        }
    }
    let _svc_guard = SvcGuard(svc);

    let mut status: SERVICE_STATUS = unsafe { std::mem::zeroed() };
    unsafe { QueryServiceStatus(svc, &mut status) }
        .map_err(|e| VxApoError::internal(&format!("QueryServiceStatus(AudioSrv) failed: {e}")))?;

    if status.dwCurrentState == SERVICE_RUNNING {
        unsafe { ControlService(svc, SERVICE_CONTROL_STOP, &mut status) }
            .map_err(|e| VxApoError::internal(&format!("ControlService(AudioSrv STOP) failed: {e}")))?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while status.dwCurrentState != SERVICE_STOPPED {
            if std::time::Instant::now() > deadline {
                return Err(VxApoError::internal("AudioSrv stop timed out (30s)"));
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            unsafe { QueryServiceStatus(svc, &mut status) }
                .map_err(|e| VxApoError::internal(&format!("QueryServiceStatus(AudioSrv) failed: {e}")))?;
        }
    }
    log::info!("AudioSrv stopped");
    Ok(())
}

/// 重启 Windows 音频服务（AudioSrv）——EAPO 安装收尾对齐（Setup.nsi / DeviceSelector /i）。
///
/// **为什么必须**（2026-08-05 实证根因）：EAPO 安装器装完调用
/// `DeviceSelector.exe /i` → `ServiceHelper::restartService(L"AudioSrv")`
/// （DeviceTestThread.cpp:74/254）——**重启音频服务触发引擎重枚举端点建图**，
/// 装完 DLL 立即进 audiodg（用户实测 47 模块含 EqualizerAPO.dll）。
/// VxAPO 之前安装只写注册表不重启服务 → audiodg 保持旧图 → 新装 DLL 不被加载。
///
/// 实现（对齐 EAPO ServiceHelper.cpp restartService）：
/// 1. OpenSCManagerW(SC_MANAGER_ALL_ACCESS)
/// 2. 停 AudioSrv（ControlService SERVICE_CONTROL_STOP）+ 轮询等 SERVICE_STOPPED
///    （30 秒超时，EAPO 同款）
/// 3. StartServiceW 启动 AudioSrv
///
/// 失败不阻塞安装（best-effort，仅日志——注册表已写入，服务下次重启自然生效）。
pub fn restart_audio_service() -> Result<()> {
    use windows::Win32::System::Services::{
        OpenSCManagerW, OpenServiceW, ControlService, StartServiceW, QueryServiceStatus,
        CloseServiceHandle, SC_HANDLE, SERVICE_STATUS,
        SC_MANAGER_ALL_ACCESS, SERVICE_ALL_ACCESS, SERVICE_CONTROL_STOP,
        SERVICE_STOPPED, SERVICE_RUNNING,
    };
    use windows::core::{PCWSTR, HSTRING};

    // SAFETY: 本函数在非 RT 控制线程调用（install/CLI），无实时约束。
    let scm = unsafe {
        OpenSCManagerW(
            PCWSTR::null(),
            PCWSTR::null(),
            SC_MANAGER_ALL_ACCESS,
        )
    }
    .map_err(|e| VxApoError::internal(&format!("OpenSCManagerW failed: {e}")))?;

    // RAII：SC 句柄必须关闭（即使中途失败）。
    struct ScmGuard(SC_HANDLE);
    impl Drop for ScmGuard {
        fn drop(&mut self) {
            // SAFETY: 句柄由 OpenSCManagerW 创建且仍有效。
            let _ = unsafe { CloseServiceHandle(self.0) };
        }
    }
    let _scm_guard = ScmGuard(scm);

    let service_name = HSTRING::from("AudioSrv");
    // SAFETY: scm 有效；serviceName 为静态 "AudioSrv"。
    let svc = unsafe { OpenServiceW(scm, &service_name, SERVICE_ALL_ACCESS) }
        .map_err(|e| VxApoError::internal(&format!("OpenServiceW(AudioSrv) failed: {e}")))?;

    struct SvcGuard(SC_HANDLE);
    impl Drop for SvcGuard {
        fn drop(&mut self) {
            // SAFETY: 句柄由 OpenServiceW 创建且仍有效。
            let _ = unsafe { CloseServiceHandle(self.0) };
        }
    }
    let _svc_guard = SvcGuard(svc);

    // 当前状态。
    let mut status: SERVICE_STATUS = unsafe { std::mem::zeroed() };
    // SAFETY: status 可变缓冲区由 SCM 填充。
    unsafe { QueryServiceStatus(svc, &mut status) }
        .map_err(|e| VxApoError::internal(&format!("QueryServiceStatus(AudioSrv) failed: {e}")))?;

    // 已在运行才停（EAPO：state==SERVICE_RUNNING 才 stop；否则直接启动）。
    if status.dwCurrentState == SERVICE_RUNNING {
        // SAFETY: 停服务。
        unsafe { ControlService(svc, SERVICE_CONTROL_STOP, &mut status) }
            .map_err(|e| VxApoError::internal(&format!("ControlService(AudioSrv STOP) failed: {e}")))?;

        // 轮询等 STOPPED（30 秒超时，EAPO 同款）。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while status.dwCurrentState != SERVICE_STOPPED {
            if std::time::Instant::now() > deadline {
                return Err(VxApoError::internal("AudioSrv stop timed out (30s)"));
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            // SAFETY: status 可变缓冲区由 SCM 填充。
            unsafe { QueryServiceStatus(svc, &mut status) }
                .map_err(|e| VxApoError::internal(&format!("QueryServiceStatus(AudioSrv) failed: {e}")))?;
        }
    }

    // 启动 AudioSrv。
    // SAFETY: 启动服务。
    unsafe { StartServiceW(svc, None) }
        .map_err(|e| VxApoError::internal(&format!("StartServiceW(AudioSrv) failed: {e}")))?;

    log::info!("AudioSrv restarted (EAPO install 对齐)");
    Ok(())
}

/// 定向重启指定音频端点设备，让 Windows 重新载入该端点。
///
/// 端点设备实例 ID 格式：
/// - 播放端点：`SWD\MMDEVAPI\{0.0.0.00000000}.{endpoint-guid}`
/// - 采集端点：`SWD\MMDEVAPI\{1.0.0.00000000}.{endpoint-guid}`
///
/// 与整服重启相比只影响目标端点，且 Windows 会保留其默认身份，
/// 避免应用在重启后优先路由到其他设备。
pub fn restart_endpoint_device(device_guid: &str, is_capture: bool) -> Result<()> {
    let flow = if is_capture { "1.0.0.00000000" } else { "0.0.0.00000000" };
    let guid = device_guid.trim_matches(|c| c == '{' || c == '}');
    let instance_id = format!(r"SWD\MMDEVAPI\{{{flow}}}.{{{guid}}}");
    let out = std::process::Command::new("pnputil")
        .args(["/restart-device", &instance_id])
        .output()
        .map_err(|e| VxApoError::internal(&format!("pnputil 启动失败：{e}")))?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr);
        return Err(VxApoError::internal(&format!(
            "pnputil /restart-device 失败：{}",
            msg.trim()
        )));
    }
    log::info!("endpoint device restarted: {instance_id}");
    Ok(())
}

// ══════════════════════════════════════════════════════════════════════════════
// 内部辅助（测试用）
// ══════════════════════════════════════════════════════════════════════════════

/// 使用显式路径查询（测试可注入 HKCU 路径验证逻辑）。
#[allow(dead_code)]
fn is_disabled_at(root: HKEY, path: &str) -> Result<bool> {
    let key = match RegKey::open(root, path) {
        Ok(k) => k,
        Err(_) => return Ok(false),
    };
    match key.read_dword_value(VALUE_NAME) {
        Ok(v) => Ok(v == 1),
        Err(_) => Ok(false),
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Registry::HKEY_CURRENT_USER;

    const TEST_PREFIX: &str = r"SOFTWARE\VxAPO_Test_Audiodg";

    /// 每个测试使用独立子键，避免并行测试互相干扰。
    fn test_path(name: &str) -> String {
        format!("{}\\{}", TEST_PREFIX, name)
    }

    fn cleanup(path: &str) {
        if let Ok(key) = RegKey::open(HKEY_CURRENT_USER, path) {
            let _ = key.delete_value(VALUE_NAME);
        }
    }

    #[test]
    fn missing_key_means_not_disabled() {
        let result = is_disabled_at(HKEY_CURRENT_USER, r"SOFTWARE\VxAPO_Test_Nonexistent_Audiodg");
        assert_eq!(result.unwrap(), false);
    }

    #[test]
    fn write_1_then_disabled() {
        let path = test_path("write_1");
        cleanup(&path);
        let key = RegKey::create(HKEY_CURRENT_USER, &path).unwrap();
        key.write_dword(VALUE_NAME, 1).unwrap();
        assert_eq!(is_disabled_at(HKEY_CURRENT_USER, &path).unwrap(), true);
        cleanup(&path);
    }

    #[test]
    fn write_0_means_not_disabled() {
        let path = test_path("write_0");
        cleanup(&path);
        let key = RegKey::create(HKEY_CURRENT_USER, &path).unwrap();
        key.write_dword(VALUE_NAME, 0).unwrap();
        assert_eq!(is_disabled_at(HKEY_CURRENT_USER, &path).unwrap(), false);
        cleanup(&path);
    }

    #[test]
    fn delete_restores_default() {
        let path = test_path("delete_restore");
        cleanup(&path);
        let key = RegKey::create(HKEY_CURRENT_USER, &path).unwrap();
        key.write_dword(VALUE_NAME, 1).unwrap();
        assert_eq!(is_disabled_at(HKEY_CURRENT_USER, &path).unwrap(), true);

        // 模拟 delete_value（通过 key）
        key.delete_value(VALUE_NAME).unwrap();
        assert_eq!(is_disabled_at(HKEY_CURRENT_USER, &path).unwrap(), false);
        cleanup(&path);
    }
}
