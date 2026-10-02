//! install/audiodg.rs — DisableProtectedAudioDG 检查与修复（规范 5.6）
//!
//! 保护模式阻止第三方 APO 加载。通过注册表
//! `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Audio` 下的
//! `DisableProtectedAudioDG`（REG_DWORD）控制。

use crate::sys::registry::RegKey;
use crate::utils::vx_error::{Result, VxApoError};
use windows::core::PWSTR;
use windows::Win32::System::Registry::{HKEY, HKEY_LOCAL_MACHINE};
use windows::Win32::System::Services::SC_HANDLE;

/// 注册表路径：HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Audio。
const AUDIO_KEY_PATH: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Audio";

/// 值名：DisableProtectedAudioDG。
const VALUE_NAME: &str = "DisableProtectedAudioDG";

// ══════════════════════════════════════════════════════════════════════════════
// 公开 API
// ══════════════════════════════════════════════════════════════════════════════

/// 检查保护是否已禁用。
///
/// 返回 `true`：DisableProtectedAudioDG 值存在且 == 1（允许第三方 APO 加载）。
/// 返回 `false`：值不存在或 != 1（Windows 阻止第三方 APO 加载）。
pub(crate) fn is_disabled() -> Result<bool> {
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

/// 设置 DisableProtectedAudioDG = 1（禁用保护，允许第三方加载）。
///
/// 需要管理员权限（写入 HKLM）。
pub(crate) fn disable() -> Result<()> {
    let key = RegKey::create(HKEY_LOCAL_MACHINE, AUDIO_KEY_PATH)?;
    key.write_dword(VALUE_NAME, 1)?;
    Ok(())
}

/// 检查并确保允许加载，不允许时尝试修复。
///
/// 由 object/apo.rs LockForProcess 调用。
pub(crate) fn ensure_can_load() -> Result<()> {
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
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::System::Services::{
        CloseServiceHandle, ControlService, OpenSCManagerW, OpenServiceW, QueryServiceStatus,
        SC_HANDLE, SC_MANAGER_ALL_ACCESS, SERVICE_ALL_ACCESS, SERVICE_CONTROL_STOP,
        SERVICE_RUNNING, SERVICE_STATUS, SERVICE_STOPPED,
    };

    // SAFETY: 本函数在非 RT 控制线程调用（install/CLI），无实时约束。
    let scm = unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ALL_ACCESS) }
        .map_err(|e| VxApoError::internal(format!("OpenSCManagerW failed: {e}")))?;

    struct ScmGuard(SC_HANDLE);
    impl Drop for ScmGuard {
        fn drop(&mut self) {
            // SAFETY: 句柄由 OpenSCManagerW 成功返回并交由本 guard 独占持有，
            // Drop 每个实例只执行一次。
            let _ = unsafe { CloseServiceHandle(self.0) };
        }
    }
    let _scm_guard = ScmGuard(scm);

    let service_name = HSTRING::from("AudioSrv");
    // SAFETY: scm 由 OpenSCManagerW 成功返回且由 _scm_guard 持有至函数结束；
    // 服务名 HSTRING 在调用期间存活；失败经 map_err 转 Err，不会使用无效句柄。
    let svc = unsafe { OpenServiceW(scm, &service_name, SERVICE_ALL_ACCESS) }
        .map_err(|e| VxApoError::internal(format!("OpenServiceW(AudioSrv) failed: {e}")))?;

    struct SvcGuard(SC_HANDLE);
    impl Drop for SvcGuard {
        fn drop(&mut self) {
            // SAFETY: svc 由 OpenServiceW 成功返回并交由本 guard 独占持有，只关闭一次。
            let _ = unsafe { CloseServiceHandle(self.0) };
        }
    }
    let _svc_guard = SvcGuard(svc);

    // SAFETY: SERVICE_STATUS 是 POD，全零位模式合法（仅作初值，随后被 SCM 覆写）。
    let mut status: SERVICE_STATUS = unsafe { std::mem::zeroed() };
    // SAFETY: svc 有效（上方 OpenServiceW + guard 持有）；status 为本地可写 POD。
    unsafe { QueryServiceStatus(svc, &mut status) }
        .map_err(|e| VxApoError::internal(format!("QueryServiceStatus(AudioSrv) failed: {e}")))?;

    if status.dwCurrentState == SERVICE_RUNNING {
        // SAFETY: 同上——svc 有效，status 可写，SERVICE_CONTROL_STOP 为合法控制码。
        unsafe { ControlService(svc, SERVICE_CONTROL_STOP, &mut status) }.map_err(|e| {
            VxApoError::internal(format!("ControlService(AudioSrv STOP) failed: {e}"))
        })?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while status.dwCurrentState != SERVICE_STOPPED {
            if std::time::Instant::now() > deadline {
                return Err(VxApoError::internal("AudioSrv stop timed out (30s)"));
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            // SAFETY: 轮询中 svc 仍由 guard 持有；status 为本地可写 POD。
            unsafe { QueryServiceStatus(svc, &mut status) }.map_err(|e| {
                VxApoError::internal(format!("QueryServiceStatus(AudioSrv) failed: {e}"))
            })?;
        }
    }
    log::info!("AudioSrv stopped");
    Ok(())
}

/// 确保 AudioSrv 处于运行状态（幂等：已运行直接返回）。
///
/// 与 `restart_audio_service` 的区别：不强制 stop→start，只保证服务在跑。
/// 安装/卸载收尾在端点设备重启后调用，避免“pnputil 重启端点成功但服务仍停”
/// 导致音频服务未被重新启用。
pub fn ensure_audio_service_running() -> Result<()> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus, StartServiceW,
        SC_HANDLE, SC_MANAGER_ALL_ACCESS, SERVICE_ALL_ACCESS, SERVICE_RUNNING, SERVICE_STATUS,
    };

    // SAFETY: 非 RT 控制线程调用（install/CLI）。
    let scm = unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ALL_ACCESS) }
        .map_err(|e| VxApoError::internal(format!("OpenSCManagerW failed: {e}")))?;
    struct ScmGuard(SC_HANDLE);
    impl Drop for ScmGuard {
        fn drop(&mut self) {
            // SAFETY: 句柄由 OpenSCManagerW 成功返回并交由本 guard 独占持有。
            let _ = unsafe { CloseServiceHandle(self.0) };
        }
    }
    let _scm_guard = ScmGuard(scm);

    // SAFETY: scm 有效且由 _scm_guard 持有；服务名 HSTRING 临时对象存活至调用结束。
    let svc = unsafe { OpenServiceW(scm, &HSTRING::from("AudioSrv"), SERVICE_ALL_ACCESS) }
        .map_err(|e| VxApoError::internal(format!("OpenServiceW(AudioSrv) failed: {e}")))?;
    struct SvcGuard(SC_HANDLE);
    impl Drop for SvcGuard {
        fn drop(&mut self) {
            // SAFETY: svc 由 OpenServiceW 成功返回并交由本 guard 独占持有。
            let _ = unsafe { CloseServiceHandle(self.0) };
        }
    }
    let _svc_guard = SvcGuard(svc);

    // SAFETY: SERVICE_STATUS 为 POD，全零位模式合法（初值，随即被 SCM 覆写）。
    let mut status: SERVICE_STATUS = unsafe { std::mem::zeroed() };
    // SAFETY: svc 有效（上方 OpenServiceW）；status 为本地可写 POD。
    unsafe { QueryServiceStatus(svc, &mut status) }
        .map_err(|e| VxApoError::internal(format!("QueryServiceStatus(AudioSrv) failed: {e}")))?;
    if status.dwCurrentState == SERVICE_RUNNING {
        return Ok(());
    }

    // SAFETY: svc 有效；None 表示无需额外参数数组，StartServiceW 允许该形式。
    unsafe { StartServiceW(svc, None) }
        .map_err(|e| VxApoError::internal(format!("StartServiceW(AudioSrv) failed: {e}")))?;
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
        // SAFETY: entry 的 dwSize 已按 API 约定设为结构大小（首次调用前设置）；
        // snapshot 由本函数创建并独占，循环内复用同一个 entry。
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
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, SC_HANDLE, SC_MANAGER_ALL_ACCESS,
        SERVICE_ENUMERATE_DEPENDENTS, SERVICE_QUERY_STATUS, SERVICE_STOP,
    };

    // SAFETY: 非 RT 控制线程调用（install/CLI）。
    let scm = unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ALL_ACCESS) }
        .map_err(|e| VxApoError::internal(format!("OpenSCManagerW failed: {e}")))?;
    struct ScmGuard(SC_HANDLE);
    impl Drop for ScmGuard {
        fn drop(&mut self) {
            // SAFETY: 句柄由 OpenSCManagerW 成功返回并交由本 guard 独占持有。
            let _ = unsafe { CloseServiceHandle(self.0) };
        }
    }
    let _scm_guard = ScmGuard(scm);

    // SAFETY: scm 有效且由 _scm_guard 持有；服务名 HSTRING 临时对象存活至调用结束。
    let svc = unsafe {
        OpenServiceW(
            scm,
            &HSTRING::from("AudioSrv"),
            SERVICE_STOP | SERVICE_QUERY_STATUS | SERVICE_ENUMERATE_DEPENDENTS,
        )
    }
    .map_err(|e| VxApoError::internal(format!("OpenServiceW(AudioSrv) failed: {e}")))?;
    struct SvcGuard(SC_HANDLE);
    impl Drop for SvcGuard {
        fn drop(&mut self) {
            // SAFETY: svc 由 OpenServiceW 成功返回并交由本 guard 独占持有。
            let _ = unsafe { CloseServiceHandle(self.0) };
        }
    }
    let _svc_guard = SvcGuard(svc);

    for dep in active_dependents(svc)? {
        // SAFETY: scm 有效；dep 是 active_dependents 枚举出的服务名，其临时 HSTRING
        // 存活至本次调用结束；失败经 if let Ok 跳过该依赖。
        if let Ok(dep_svc) = unsafe {
            OpenServiceW(
                scm,
                &HSTRING::from(&dep),
                SERVICE_STOP | SERVICE_QUERY_STATUS,
            )
        } {
            let result = stop_service_and_wait(dep_svc, &dep, stop_timeout_secs);
            // SAFETY: dep_svc 由上方 OpenServiceW 成功返回（if let Ok 分支），此处释放一次。
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
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, SC_HANDLE, SC_MANAGER_ALL_ACCESS,
        SERVICE_ENUMERATE_DEPENDENTS, SERVICE_QUERY_STATUS, SERVICE_START,
    };

    // SAFETY: 非 RT 控制线程调用（install/CLI）。
    let scm = unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ALL_ACCESS) }
        .map_err(|e| VxApoError::internal(format!("OpenSCManagerW failed: {e}")))?;
    struct ScmGuard(SC_HANDLE);
    impl Drop for ScmGuard {
        fn drop(&mut self) {
            // SAFETY: 句柄由 OpenSCManagerW 成功返回并交由本 guard 独占持有。
            let _ = unsafe { CloseServiceHandle(self.0) };
        }
    }
    let _scm_guard = ScmGuard(scm);

    // SAFETY: scm 有效且由 _scm_guard 持有；服务名 HSTRING 临时对象存活至调用结束。
    let svc = unsafe {
        OpenServiceW(
            scm,
            &HSTRING::from("AudioSrv"),
            SERVICE_START | SERVICE_QUERY_STATUS | SERVICE_ENUMERATE_DEPENDENTS,
        )
    }
    .map_err(|e| VxApoError::internal(format!("OpenServiceW(AudioSrv) failed: {e}")))?;
    struct SvcGuard(SC_HANDLE);
    impl Drop for SvcGuard {
        fn drop(&mut self) {
            // SAFETY: svc 由 OpenServiceW 成功返回并交由本 guard 独占持有。
            let _ = unsafe { CloseServiceHandle(self.0) };
        }
    }
    let _svc_guard = SvcGuard(svc);

    start_service_and_wait(svc, "AudioSrv", start_timeout_secs)?;
    for dep in active_dependents(svc)? {
        // SAFETY: scm 有效；dep 为枚举出的依赖服务名（临时 HSTRING 存活至调用结束）。
        if let Ok(dep_svc) = unsafe {
            OpenServiceW(
                scm,
                &HSTRING::from(&dep),
                SERVICE_START | SERVICE_QUERY_STATUS,
            )
        } {
            let result = start_service_and_wait(dep_svc, &dep, start_timeout_secs);
            // SAFETY: dep_svc 由上方 OpenServiceW 成功返回，此处释放一次。
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
    // SAFETY: svc 有效；lpServices=None 且 cbBufSize=0 表示“只查询所需缓冲大小”，
    // 该调用不写入任何缓冲区，只回填 needed/returned。
    let _ =
        unsafe { EnumDependentServicesW(svc, SERVICE_ACTIVE, None, 0, &mut needed, &mut returned) };
    if needed == 0 {
        return Ok(Vec::new());
    }
    // 用**类型化数组**承载结果：ENUM_SERVICE_STATUSW 是 repr(C) POD，Vec<T> 的分配
    // 天然满足它的对齐要求（旧实现用 Vec<u8> 再转型，对齐只靠 malloc 的分配保证）。
    // 元素数按 (needed + 32) 向上取整，保留原有 32 字节余量。
    let elem_size = std::mem::size_of::<ENUM_SERVICE_STATUSW>();
    let elem_count = (needed as usize + 32).div_ceil(elem_size);
    let mut buf = vec![ENUM_SERVICE_STATUSW::default(); elem_count];
    // SAFETY: buf 是 ENUM_SERVICE_STATUSW 的连续数组（自然对齐、已初始化）；
    // cbBufSize 必须传**字节长度**（= 元素数 × 元素大小），API 只会写入该数组；
    // svc 有效；结果数量不超过 needed/returned。
    unsafe {
        EnumDependentServicesW(
            svc,
            SERVICE_ACTIVE,
            Some(buf.as_mut_ptr()),
            (buf.len() * elem_size) as u32,
            &mut needed,
            &mut returned,
        )
    }
    .map_err(|e| VxApoError::internal(format!("EnumDependentServicesW failed: {e}")))?;

    let mut names = Vec::with_capacity(returned as usize);
    // 安全索引：buf 已由 API 填充 returned 个条目（take 保证不读未写入的尾部）。
    for entry in buf.iter().take(returned as usize) {
        names.push(string_from_wide(entry.lpServiceName));
    }
    Ok(names)
}

/// 停止单个服务并轮询到 STOPPED（超时返回 Err）。
fn stop_service_and_wait(svc: SC_HANDLE, name: &str, timeout_secs: u32) -> Result<()> {
    use windows::Win32::System::Services::{
        ControlService, QueryServiceStatus, SERVICE_CONTROL_STOP, SERVICE_RUNNING, SERVICE_STATUS,
        SERVICE_STOPPED,
    };

    // SAFETY: SERVICE_STATUS 为 POD，全零位模式合法（初值，随即被 SCM 覆写）。
    let mut status: SERVICE_STATUS = unsafe { std::mem::zeroed() };
    // SAFETY: svc 由调用方保证有效（本函数只做服务控制）；status 为本地可写 POD。
    unsafe { QueryServiceStatus(svc, &mut status) }
        .map_err(|e| VxApoError::internal(format!("QueryServiceStatus({name}) failed: {e}")))?;
    if status.dwCurrentState != SERVICE_RUNNING {
        return Ok(());
    }
    // SAFETY: svc 有效；SERVICE_CONTROL_STOP 为合法控制码；status 可写。
    unsafe { ControlService(svc, SERVICE_CONTROL_STOP, &mut status) }
        .map_err(|e| VxApoError::internal(format!("ControlService({name} STOP) failed: {e}")))?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs as u64);
    while status.dwCurrentState != SERVICE_STOPPED {
        if std::time::Instant::now() > deadline {
            return Err(VxApoError::internal(format!(
                "{name} stop timed out ({timeout_secs}s)"
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        // SAFETY: 轮询期间 svc 仍有效；status 为本地可写 POD。
        unsafe { QueryServiceStatus(svc, &mut status) }
            .map_err(|e| VxApoError::internal(format!("QueryServiceStatus({name}) failed: {e}")))?;
    }
    Ok(())
}

/// 启动单个服务并轮询到 RUNNING（5s 无进展重试一次，EAPO 同款）。
fn start_service_and_wait(svc: SC_HANDLE, name: &str, timeout_secs: u32) -> Result<()> {
    use windows::Win32::System::Services::{
        QueryServiceStatus, StartServiceW, SERVICE_RUNNING, SERVICE_STATUS,
    };

    // SAFETY: SERVICE_STATUS 为 POD，全零位模式合法（初值，随即被 SCM 覆写）。
    let mut status: SERVICE_STATUS = unsafe { std::mem::zeroed() };
    // SAFETY: svc 由调用方保证有效；status 为本地可写 POD。
    unsafe { QueryServiceStatus(svc, &mut status) }
        .map_err(|e| VxApoError::internal(format!("QueryServiceStatus({name}) failed: {e}")))?;
    if status.dwCurrentState == SERVICE_RUNNING {
        return Ok(());
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs as u64);
    let mut next_retry = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        // StartServiceW 对“已启动/启动中”可能返回 ERROR_SERVICE_ALREADY_RUNNING——
        // 忽略错误，继续轮询状态。
        // SAFETY: svc 有效；None 表示无额外参数数组。
        let _ = unsafe { StartServiceW(svc, None) };
        // SAFETY: 同上轮询——svc 有效，status 为本地可写 POD。
        unsafe { QueryServiceStatus(svc, &mut status) }
            .map_err(|e| VxApoError::internal(format!("QueryServiceStatus({name}) failed: {e}")))?;
        if status.dwCurrentState == SERVICE_RUNNING {
            return Ok(());
        }
        let now = std::time::Instant::now();
        if now > deadline {
            return Err(VxApoError::internal(format!(
                "{name} start timed out ({timeout_secs}s)"
            )));
        }
        if now > next_retry {
            // SAFETY: svc 有效；重试语义同上（已启动/启动中会返回错误，忽略即可）。
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
    // SAFETY: p.0 是服务 API 返回的 NUL 结尾宽字符串（调用方已检查非空）；
    // 循环只逐字读取直到终止符，不越过终止符读取。
    unsafe {
        while *p.0.add(len) != 0 {
            len += 1;
        }
    }
    // SAFETY: len 为上面的扫描结果（不含终止符），p.0 至少有 len+1 个有效的 u16；
    // 切片只在本函数内使用，不逃逸。
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
pub(crate) fn restart_endpoint_device(device_guid: &str, is_capture: bool) -> Result<()> {
    let flow = if is_capture {
        "1.0.0.00000000"
    } else {
        "0.0.0.00000000"
    };
    let guid = device_guid.trim_matches(|c| c == '{' || c == '}');
    let instance_id = format!(r"SWD\MMDEVAPI\{{{flow}}}.{{{guid}}}");
    let out = std::process::Command::new("pnputil")
        .args(["/restart-device", &instance_id])
        .output()
        .map_err(|e| VxApoError::internal(format!("pnputil 启动失败：{e}")))?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr);
        return Err(VxApoError::internal(format!(
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
        let result = is_disabled_at(
            HKEY_CURRENT_USER,
            r"SOFTWARE\VxAPO_Test_Nonexistent_Audiodg",
        );
        assert!(!result.unwrap());
    }

    #[test]
    fn write_1_then_disabled() {
        let path = test_path("write_1");
        cleanup(&path);
        let key = RegKey::create(HKEY_CURRENT_USER, &path).unwrap();
        key.write_dword(VALUE_NAME, 1).unwrap();
        assert!(is_disabled_at(HKEY_CURRENT_USER, &path).unwrap());
        cleanup(&path);
    }

    #[test]
    fn write_0_means_not_disabled() {
        let path = test_path("write_0");
        cleanup(&path);
        let key = RegKey::create(HKEY_CURRENT_USER, &path).unwrap();
        key.write_dword(VALUE_NAME, 0).unwrap();
        assert!(!is_disabled_at(HKEY_CURRENT_USER, &path).unwrap());
        cleanup(&path);
    }

    #[test]
    fn delete_restores_default() {
        let path = test_path("delete_restore");
        cleanup(&path);
        let key = RegKey::create(HKEY_CURRENT_USER, &path).unwrap();
        key.write_dword(VALUE_NAME, 1).unwrap();
        assert!(is_disabled_at(HKEY_CURRENT_USER, &path).unwrap());

        // 模拟 delete_value（通过 key）
        key.delete_value(VALUE_NAME).unwrap();
        assert!(!is_disabled_at(HKEY_CURRENT_USER, &path).unwrap());
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

    /// 真机验证：`active_dependents` 的**填充 + 读取**路径成立
    /// （类型化缓冲 `Vec<ENUM_SERVICE_STATUSW>` + 以字节长度传 cbBufSize）。
    ///
    /// 说明：AudioSrv 的依赖只有 `AarSvc`（已停止），按 `SERVICE_ACTIVE` 过滤后本就为 0；
    /// 音频侧真正的依赖关系是反方向（Audiosrv ← AudioEndpointBuilder 的 DependOnService）。
    /// 因此这里改用本机实测有多个活跃依赖的服务（Dhcp 4 个 / Dnscache 5 个），
    /// 只要其中一个返回非空即证明多条目填充与读取路径正确。
    /// 依赖本机 SCM 访问权限，默认忽略；显式运行：`cargo test -- --ignored`。
    #[test]
    #[ignore = "需要本机 SCM 访问权限"]
    fn active_dependents_enumerates_active_dependents() {
        use windows::core::{HSTRING, PCWSTR};
        use windows::Win32::System::Services::{
            CloseServiceHandle, OpenSCManagerW, OpenServiceW, SC_MANAGER_CONNECT,
            SERVICE_ENUMERATE_DEPENDENTS, SERVICE_QUERY_STATUS,
        };

        // SAFETY: 打开本机 SCM（只申请 CONNECT）；失败直接 panic 视为环境不满足。
        let scm = unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) }
            .expect("OpenSCManagerW");

        let mut non_empty: Vec<(String, Vec<String>)> = Vec::new();
        for name in ["Dhcp", "Dnscache", "EventSystem", "AudioSrv"] {
            // SAFETY: scm 有效；服务名 HSTRING 临时对象存活至调用结束；
            // 枚举依赖需要 SERVICE_ENUMERATE_DEPENDENTS。
            let svc = unsafe {
                OpenServiceW(
                    scm,
                    &HSTRING::from(name),
                    SERVICE_ENUMERATE_DEPENDENTS | SERVICE_QUERY_STATUS,
                )
            }
            .unwrap_or_else(|e| panic!("OpenServiceW({name}) failed: {e}"));

            let names = active_dependents(svc).unwrap_or_else(|e| panic!("{name}: {e}"));
            // SAFETY: svc 由上面的 OpenServiceW 成功返回，此处释放一次。
            let _ = unsafe { CloseServiceHandle(svc) };
            if !names.is_empty() {
                non_empty.push((name.to_string(), names));
            }
        }
        // SAFETY: scm 由 OpenSCManagerW 成功返回，此处释放一次。
        let _ = unsafe { CloseServiceHandle(scm) };

        assert!(
            !non_empty.is_empty(),
            "本机应至少有一个服务返回活跃依赖（否则无法覆盖填充路径）"
        );
        println!("active_dependents 实测：{non_empty:?}");
    }
}
