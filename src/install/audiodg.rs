//! install/audiodg.rs — DisableProtectedAudioDG 检查与修复（规范 5.6）
//!
//! 保护模式阻止第三方 APO 加载。通过注册表
//! `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Audio` 下的
//! `DisableProtectedAudioDG`（REG_DWORD）控制。

use crate::sys::registry::RegKey;
use crate::utils::vx_error::{Result, VxApoError};
use windows::Win32::System::Registry::{HKEY, HKEY_LOCAL_MACHINE};
use windows::Win32::System::Services::SC_HANDLE;
use windows::core::PWSTR;

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
    // 进程内缓存：设置页/多流会瞬间调用大量 LockForProcess；该值安装后
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
/// 写/删端点 `FxProperties` 值只需要句柄具备 `KEY_SET_VALUE`（`RegKey::open_for_write`
/// 即是），与 audiodg 是否持有点端无关——在活动音频流上删除槽位值同样成功；
/// ACCESS_DENIED 的成因是句柄权限不足（`SAM_ALL` 含未授予的 CreateSubKey 位、
/// 或只读句柄打开），不是音频栈加锁。
///
/// 停服真正有用的是**让变更生效**：引擎会缓存端点的 APO 链，只改注册表不会立刻
/// 重载（新起的流仍加载旧 APO），需要端点/服务重建后才生效。
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
/// **为什么必须**（实证根因）：EAPO 安装器装完调用
/// `DeviceSelector.exe /i` → `ServiceHelper::restartService(L"AudioSrv")`
/// （DeviceTestThread.cpp:74/254）——**重启音频服务触发引擎重枚举端点建图**，
/// 装完 DLL 立即进 audiodg（实测 47 模块含 EqualizerAPO.dll）。
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

/// 确保 AudioSrv 处于运行状态（幂等：已运行直接返回）。
///
/// 与 `restart_audio_service` 的区别：不强制 stop→start，只保证服务在跑。
/// 安装/卸载收尾在端点设备重启后调用，避免“pnputil 重启端点成功但服务仍停”
/// 导致音频服务未被重新启用。
pub fn ensure_audio_service_running() -> Result<()> {
    use windows::Win32::System::Services::{
        OpenSCManagerW, OpenServiceW, StartServiceW, QueryServiceStatus, CloseServiceHandle,
        SC_HANDLE, SERVICE_STATUS, SC_MANAGER_ALL_ACCESS, SERVICE_ALL_ACCESS, SERVICE_RUNNING,
    };
    use windows::core::{PCWSTR, HSTRING};

    // SAFETY: 非 RT 控制线程调用（install/CLI）。
    let scm = unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ALL_ACCESS) }
        .map_err(|e| VxApoError::internal(&format!("OpenSCManagerW failed: {e}")))?;
    struct ScmGuard(SC_HANDLE);
    impl Drop for ScmGuard {
        fn drop(&mut self) {
            let _ = unsafe { CloseServiceHandle(self.0) };
        }
    }
    let _scm_guard = ScmGuard(scm);

    let svc = unsafe { OpenServiceW(scm, &HSTRING::from("AudioSrv"), SERVICE_ALL_ACCESS) }
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
        return Ok(());
    }

    unsafe { StartServiceW(svc, None) }
        .map_err(|e| VxApoError::internal(&format!("StartServiceW(AudioSrv) failed: {e}")))?;
    log::info!("AudioSrv started (ensure running)");
    Ok(())
}

/// 等待 `audiodg.exe` 全部退出（**事件驱动**，不盲等固定时长）。
///
/// 用途：停服/`taskkill` 之后确认模块映像已释放——audiodg 不退出时
/// `vxapo_driver.dll` 仍被占用，紧随其后的重装/换 DLL 会覆盖失败。
/// **注意**：槽位值的写/删不需要这步（只需 `KEY_SET_VALUE` 句柄，活动流上删值
/// 同样成功）；这里只解决文件/模块占用。
///
/// 实现：Toolhelp 快照取 `audiodg.exe` 的 PID → `OpenProcess(SYNCHRONIZE)` →
/// `WaitForSingleObject`（内核事件等待，进程一退出立即返回），预算耗尽即收手。
/// 最多两轮扫描：覆盖"等待期间才收尾"和"停服瞬间又被拉起"的实例。
///
/// 返回 `true` = 已无 audiodg 进程；`false` = 超时仍有残留（调用方 best-effort 继续）。
pub fn wait_for_audiodg_exit(timeout_ms: u32) -> bool {
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
    for _ in 0..2 {
        let pids = audiodg_pids();
        if pids.is_empty() {
            return true;
        }
        for pid in pids {
            // SAFETY: pid 来自 Toolhelp 快照；仅申请 SYNCHRONIZE（等待退出信号）。
            let Ok(handle) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }) else {
                continue;
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            let wait_ms = remaining.as_millis().min(u32::MAX as u128) as u32;
            // SAFETY: handle 由 OpenProcess 返回且有效；超时上限受 deadline 约束。
            let waited = unsafe { WaitForSingleObject(handle, wait_ms) };
            // SAFETY: 句柄由本函数独占，等待结束后关闭。
            let _ = unsafe { CloseHandle(handle) };
            if waited != WAIT_OBJECT_0 {
                // 超时/异常：不再空转，按当前快照判定。
                return audiodg_pids().is_empty();
            }
        }
    }
    audiodg_pids().is_empty()
}

/// 枚举 `audiodg.exe` 的 PID（Toolhelp 进程快照，只读）。
fn audiodg_pids() -> Vec<u32> {
    use windows::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let mut pids = Vec::new();
    // SAFETY: 无参快照；失败返回无效句柄，直接返回空列表。
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return pids;
    };
    if snapshot == INVALID_HANDLE_VALUE {
        return pids;
    }

    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: entry 已按约定填写 dwSize；句柄有效；后续 Next 同理。
    let mut ok = unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok();
    while ok {
        let name = String::from_utf16_lossy(
            &entry.szExeFile[..entry
                .szExeFile
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(entry.szExeFile.len())],
        );
        if name.eq_ignore_ascii_case("audiodg.exe") {
            pids.push(entry.th32ProcessID);
        }
        ok = unsafe { Process32NextW(snapshot, &mut entry) }.is_ok();
    }
    // SAFETY: 快照句柄由本函数独占。
    let _ = unsafe { CloseHandle(snapshot) };
    pids
}

/// 停止 AudioSrv 及其活动依赖服务（EAPO ServiceHelper::restartService 对齐）。
///
/// 顺序：先枚举 AudioSrv 的活动依赖服务（如 AudioEndpointBuilder）逐个停止并
/// 轮询到 STOPPED，再停止 AudioSrv 并轮询——SCM 不允许在依赖服务运行时停止
/// 父服务（ERROR_DEPENDENT_SERVICES_RUNNING），必须先停依赖。
pub fn stop_audio_service_with_dependents(stop_timeout_secs: u32) -> Result<()> {
    use windows::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, SC_HANDLE, SC_MANAGER_ALL_ACCESS,
        SERVICE_ENUMERATE_DEPENDENTS, SERVICE_QUERY_STATUS, SERVICE_STOP,
    };
    use windows::core::{HSTRING, PCWSTR};

    // SAFETY: 非 RT 控制线程调用（install/CLI）。
    let scm = unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ALL_ACCESS) }
        .map_err(|e| VxApoError::internal(&format!("OpenSCManagerW failed: {e}")))?;
    struct ScmGuard(SC_HANDLE);
    impl Drop for ScmGuard {
        fn drop(&mut self) {
            let _ = unsafe { CloseServiceHandle(self.0) };
        }
    }
    let _scm_guard = ScmGuard(scm);

    let svc = unsafe {
        OpenServiceW(
            scm,
            &HSTRING::from("AudioSrv"),
            SERVICE_STOP | SERVICE_QUERY_STATUS | SERVICE_ENUMERATE_DEPENDENTS,
        )
    }
    .map_err(|e| VxApoError::internal(&format!("OpenServiceW(AudioSrv) failed: {e}")))?;
    struct SvcGuard(SC_HANDLE);
    impl Drop for SvcGuard {
        fn drop(&mut self) {
            let _ = unsafe { CloseServiceHandle(self.0) };
        }
    }
    let _svc_guard = SvcGuard(svc);

    for dep in active_dependents(svc)? {
        if let Ok(dep_svc) = unsafe {
            OpenServiceW(
                scm,
                &HSTRING::from(&dep),
                SERVICE_STOP | SERVICE_QUERY_STATUS,
            )
        } {
            let result = stop_service_and_wait(dep_svc, &dep, stop_timeout_secs);
            let _ = unsafe { CloseServiceHandle(dep_svc) };
            result?;
        }
    }
    stop_service_and_wait(svc, "AudioSrv", stop_timeout_secs)
}

/// 启动 AudioSrv 及其活动依赖服务，并轮询到 RUNNING。
///
/// 顺序与 `stop_audio_service_with_dependents` 相反：先启动 AudioSrv 并轮询到
/// RUNNING，再枚举依赖服务逐个启动（启动失败按 5s 间隔重试一次，EAPO 同款）。
pub fn start_audio_service_with_dependents(start_timeout_secs: u32) -> Result<()> {
    use windows::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, SC_HANDLE, SC_MANAGER_ALL_ACCESS,
        SERVICE_ENUMERATE_DEPENDENTS, SERVICE_QUERY_STATUS, SERVICE_START,
    };
    use windows::core::{HSTRING, PCWSTR};

    // SAFETY: 非 RT 控制线程调用（install/CLI）。
    let scm = unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ALL_ACCESS) }
        .map_err(|e| VxApoError::internal(&format!("OpenSCManagerW failed: {e}")))?;
    struct ScmGuard(SC_HANDLE);
    impl Drop for ScmGuard {
        fn drop(&mut self) {
            let _ = unsafe { CloseServiceHandle(self.0) };
        }
    }
    let _scm_guard = ScmGuard(scm);

    let svc = unsafe {
        OpenServiceW(
            scm,
            &HSTRING::from("AudioSrv"),
            SERVICE_START | SERVICE_QUERY_STATUS | SERVICE_ENUMERATE_DEPENDENTS,
        )
    }
    .map_err(|e| VxApoError::internal(&format!("OpenServiceW(AudioSrv) failed: {e}")))?;
    struct SvcGuard(SC_HANDLE);
    impl Drop for SvcGuard {
        fn drop(&mut self) {
            let _ = unsafe { CloseServiceHandle(self.0) };
        }
    }
    let _svc_guard = SvcGuard(svc);

    start_service_and_wait(svc, "AudioSrv", start_timeout_secs)?;
    for dep in active_dependents(svc)? {
        if let Ok(dep_svc) = unsafe {
            OpenServiceW(scm, &HSTRING::from(&dep), SERVICE_START | SERVICE_QUERY_STATUS)
        } {
            let result = start_service_and_wait(dep_svc, &dep, start_timeout_secs);
            let _ = unsafe { CloseServiceHandle(dep_svc) };
            result?;
        }
    }
    Ok(())
}

/// 依赖服务感知的整服重启（`--verify` 与 uninstall 收尾复用）。
pub fn restart_audio_service_wait(stop_timeout_secs: u32, start_timeout_secs: u32) -> Result<()> {
    stop_audio_service_with_dependents(stop_timeout_secs)?;
    start_audio_service_with_dependents(start_timeout_secs)
}

/// 枚举指定服务的活动依赖服务（短名列表）。
fn active_dependents(svc: SC_HANDLE) -> Result<Vec<String>> {
    use windows::Win32::System::Services::{
        EnumDependentServicesW, ENUM_SERVICE_STATUSW, SERVICE_ACTIVE,
    };

    let mut needed = 0u32;
    let mut returned = 0u32;
    let _ = unsafe {
        EnumDependentServicesW(svc, SERVICE_ACTIVE, None, 0, &mut needed, &mut returned)
    };
    if needed == 0 {
        return Ok(Vec::new());
    }
    let mut buf = vec![0u8; needed as usize + 32];
    unsafe {
        EnumDependentServicesW(
            svc,
            SERVICE_ACTIVE,
            Some(buf.as_mut_ptr() as *mut ENUM_SERVICE_STATUSW),
            buf.len() as u32,
            &mut needed,
            &mut returned,
        )
    }
    .map_err(|e| VxApoError::internal(&format!("EnumDependentServicesW failed: {e}")))?;

    let mut names = Vec::with_capacity(returned as usize);
    for i in 0..returned {
        let entry = unsafe { &*(buf.as_ptr() as *const ENUM_SERVICE_STATUSW).add(i as usize) };
        names.push(string_from_wide(entry.lpServiceName));
    }
    Ok(names)
}

/// 停止单个服务并轮询到 STOPPED（超时返回 Err）。
fn stop_service_and_wait(svc: SC_HANDLE, name: &str, timeout_secs: u32) -> Result<()> {
    use windows::Win32::System::Services::{
        ControlService, QueryServiceStatus, SERVICE_CONTROL_STOP, SERVICE_RUNNING,
        SERVICE_STOPPED, SERVICE_STATUS,
    };

    let mut status: SERVICE_STATUS = unsafe { std::mem::zeroed() };
    unsafe { QueryServiceStatus(svc, &mut status) }
        .map_err(|e| VxApoError::internal(&format!("QueryServiceStatus({name}) failed: {e}")))?;
    if status.dwCurrentState != SERVICE_RUNNING {
        return Ok(());
    }
    unsafe { ControlService(svc, SERVICE_CONTROL_STOP, &mut status) }
        .map_err(|e| VxApoError::internal(&format!("ControlService({name} STOP) failed: {e}")))?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs as u64);
    while status.dwCurrentState != SERVICE_STOPPED {
        if std::time::Instant::now() > deadline {
            return Err(VxApoError::internal(&format!(
                "{name} stop timed out ({timeout_secs}s)"
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        unsafe { QueryServiceStatus(svc, &mut status) }
            .map_err(|e| VxApoError::internal(&format!("QueryServiceStatus({name}) failed: {e}")))?;
    }
    Ok(())
}

/// 启动单个服务并轮询到 RUNNING（5s 无进展重试一次，EAPO 同款）。
fn start_service_and_wait(svc: SC_HANDLE, name: &str, timeout_secs: u32) -> Result<()> {
    use windows::Win32::System::Services::{
        QueryServiceStatus, StartServiceW, SERVICE_RUNNING, SERVICE_STATUS,
    };

    let mut status: SERVICE_STATUS = unsafe { std::mem::zeroed() };
    unsafe { QueryServiceStatus(svc, &mut status) }
        .map_err(|e| VxApoError::internal(&format!("QueryServiceStatus({name}) failed: {e}")))?;
    if status.dwCurrentState == SERVICE_RUNNING {
        return Ok(());
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs as u64);
    let mut next_retry = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        // StartServiceW 对“已启动/启动中”可能返回 ERROR_SERVICE_ALREADY_RUNNING——
        // 忽略错误，继续轮询状态。
        let _ = unsafe { StartServiceW(svc, None) };
        unsafe { QueryServiceStatus(svc, &mut status) }
            .map_err(|e| VxApoError::internal(&format!("QueryServiceStatus({name}) failed: {e}")))?;
        if status.dwCurrentState == SERVICE_RUNNING {
            return Ok(());
        }
        let now = std::time::Instant::now();
        if now > deadline {
            return Err(VxApoError::internal(&format!(
                "{name} start timed out ({timeout_secs}s)"
            )));
        }
        if now > next_retry {
            let _ = unsafe { StartServiceW(svc, None) };
            next_retry = now + std::time::Duration::from_secs(5);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// 宽字符指针转 String（ENUM_SERVICE_STATUSW.lpServiceName 为 PWSTR）。
fn string_from_wide(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    unsafe {
        while *p.0.add(len) != 0 {
            len += 1;
        }
    }
    let slice = unsafe { std::slice::from_raw_parts(p.0, len) };
    String::from_utf16_lossy(slice)
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

    /// `wait_for_audiodg_exit(0)` 必须立即返回（预算耗尽即收手，不阻塞）。
    #[test]
    fn wait_for_audiodg_exit_is_bounded() {
        let start = std::time::Instant::now();
        let _ = wait_for_audiodg_exit(0);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "超时预算为 0 时不应阻塞"
        );
    }

    /// Toolhelp 枚举不 panic，且不会返回异常多的实例。
    #[test]
    fn audiodg_pids_enumeration_is_sane() {
        let pids = audiodg_pids();
        assert!(pids.len() < 64, "audiodg 实例数异常：{}", pids.len());
    }
}
