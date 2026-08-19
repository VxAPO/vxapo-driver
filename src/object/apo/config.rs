//! object/apo/config.rs — per-device 配置路径与热重载
//!
//! 职责：从 APOInitSystemEffects 提取端点 GUID、解析 per-device config 路径、
//! 运行 watcher 驱动的热重载逻辑。不包含 COM 接口方法。

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use windows::core::Result;

use crate::config::parser::ConfigParser;
use crate::config::watcher::ConfigWatcher;
use crate::pipeline::chain::Chain;
use crate::pipeline::dsp::transition::{SmoothingProvider, default_smoothing_length};
use crate::sys::com::apo_types::{
    APOInitSystemEffects, PKEY_AudioEndpoint_GUID, PROPVARIANT, VT_CLSID, VT_LPWSTR,
};
use crate::sys::com::prelude::{GUID, guid_to_string};

use super::ApoObject_Impl;
use super::inner::{ApoObjectInner, build_dsp_context};

/// 配置文件默认路径（兜底：无设备 GUID / 配置根创建失败时回退单实例共用路径）。
pub(crate) const DEFAULT_CONFIG_PATH: &str = r"C:\ProgramData\VxAPO\config.toml";

/// per-device 配置根目录（方案 A， 确认）。
pub(crate) const CONFIG_ROOT: &str = r"C:\ProgramData\VxAPO";

/// 单实例共用子目录名（无设备 GUID 兜底，object 7.1.8）。
pub(crate) const DEFAULT_DEVICE_DIR: &str = "_default";

/// 从 APOInitSystemEffects 提取端点 GUID（object 7.1.8）。
///
/// EAPO 源码（EqualizerAPO.cpp:126）从 `pAPOEndpointProperties` 取端点属性存储；
/// windows-rs 0.62.2 的 APOInitSystemEffects 同时有 pAPOEndpointProperties 和
/// pAPOSystemEffectsProperties 两个字段——端点 GUID 在 Endpoint 那个里面。
/// 先用 pAPOEndpointProperties，缺失时回退 pAPOSystemEffectsProperties。
pub(crate) fn extract_endpoint_guid(init: &APOInitSystemEffects) -> Option<GUID> {
    let props = init
        .pAPOEndpointProperties
        .as_ref()
        .or_else(|| init.pAPOSystemEffectsProperties.as_ref())?;
    // Safety: PKEY_AudioEndpoint_GUID 为静态键；GetValue 返回的 PROPVARIANT 由
    // windows-rs 管理内存（含 puuid/pwszVal 指针有效期内读取）。
    // PROPVARIANT 是 union（Anonymous.Anonymous.Anonymous），读取/比较均在 unsafe 内。
    let pv: PROPVARIANT = unsafe { props.GetValue(&PKEY_AudioEndpoint_GUID) }.ok()?;
    unsafe {
        // PROPVARIANT_0_0: { vt: VARENUM, wReserved1-3, Anonymous: PROPVARIANT_0_0_0 }
        match pv.Anonymous.Anonymous.vt {
            // 部分系统/驱动返回 VT_CLSID（puuid 指向 GUID）。
            VT_CLSID => {
                let guid_ptr = pv.Anonymous.Anonymous.Anonymous.puuid;
                if guid_ptr.is_null() {
                    None
                } else {
                    Some(*guid_ptr)
                }
            }
            // Windows 11 实测 EAPO 同款：PKEY_AudioEndpoint_GUID 返回 VT_LPWSTR，
            // 字符串形如 {3b1c3cb8-af9e-47b9-b776-3dac8c7ca333}。
            VT_LPWSTR => {
                let str_ptr = pv.Anonymous.Anonymous.Anonymous.pwszVal;
                if str_ptr.is_null() {
                    None
                } else {
                    let s = str_ptr.to_string().ok()?;
                    let s = s.trim().trim_start_matches('{').trim_end_matches('}');
                    GUID::try_from(s).ok()
                }
            }
            _ => None,
        }
    }
}

/// 确定 per-device 配置路径（object 7.1.8，方案 A）：
/// `C:\ProgramData\VxAPO\{GUID}\config.toml`；无 GUID / 解析失败 → `_default` 兜底。
/// 目录自动创建；config.toml 缺失时写默认 passthrough（空配置 → 链为空即 passthrough）。
pub(crate) fn resolve_config_path(init: Option<&APOInitSystemEffects>) -> String {
    resolve_config_path_from(CONFIG_ROOT, init)
}

/// 纯拼接 + 目录/文件保障（可单元测试，不依赖真实路径）。
pub(crate) fn resolve_config_path_from(
    config_root: &str,
    init: Option<&APOInitSystemEffects>,
) -> String {
    let device_dir = match init.and_then(extract_endpoint_guid) {
        Some(guid) => {
            let s = guid_to_string(&guid);
            log::info!("endpoint GUID: {}", s);
            s
        }
        None => DEFAULT_DEVICE_DIR.to_owned(),
    };

    let dir = std::path::Path::new(config_root).join(&device_dir);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::warn!("create_dir_all({}) failed: {} — fallback to shared default config", dir.display(), e);
        return DEFAULT_CONFIG_PATH.to_owned();
    }
    let path = dir.join("config.toml");

    // config.toml 缺失 → 写默认 passthrough（空文件 = 无滤波器 = passthrough）。
    if !path.exists() {
        log::info!("config not found at {}, writing default passthrough", path.display());
        if let Err(e) = std::fs::write(&path, "# VxAPO default passthrough\n") {
            log::warn!("write default config failed: {}", e);
        }
    }
    path.display().to_string()
}

/// watcher 运行时状态（， 外部驱动模型）。
pub(crate) struct WatcherState {
    pub thread: Option<std::thread::JoinHandle<()>>,
    pub shutdown_event: Option<windows::Win32::Foundation::HANDLE>,
}

impl Default for WatcherState {
    fn default() -> Self {
        Self { thread: None, shutdown_event: None }
    }
}

/// 启动配置监控线程（object 7.1.9， 外部驱动模型）。
///
/// 流程：CreateEventW(shutdown_event) → ConfigWatcher::new(watch_dir, shutdown_event)
/// → spawn 线程循环 `wait_and_handle` → `hot_reload_impl`（DirectoryChanged → 重载）。
/// 失败降级（watcher 未启动，仅日志）——不阻塞锁定（配置热重载失效但音频链路正常）。
///
/// `#[implement]` 只暴露 `&self`（gen.rs：不向安全代码暴露所有权实例）——因此用
/// `Arc<Mutex<WatcherState>>` 内部可变性（方案 A）；spawn 线程 clone `config_path`/
/// `mutex` 的 Arc 移入（'static），线程内调 `hot_reload_impl`（无需持有 apo）。
pub(crate) fn start_watcher(apo: &ApoObject_Impl) -> Result<()> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{CreateEventW, SetEvent};

    // 幂等：已有 watcher 线程则不重复。
    let mut st = apo.watcher_state.lock().unwrap_or_else(|e| e.into_inner());
    if st.thread.is_some() {
        return Ok(());
    }

    // 1. 创建退出事件（manual-reset，初始 non-signaled）。
    // Safety: CreateEventW 无安全属性、无名字；返回句柄由 watcher_state 持有，stop_watcher 释放。
    let shutdown_event = unsafe { CreateEventW(None, true, false, None)? };

    // 2. 目录级监控器（不自启线程）。watch_dir = config_path 父目录。
    let config_path = apo
        .config_path
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let watch_dir = std::path::Path::new(&config_path)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::Path::new(&config_path).to_path_buf());
    let mut watcher = ConfigWatcher::new(watch_dir, shutdown_event);
    if watcher.notify_handle().is_invalid() {
        // 目录不存在（FindFirstChangeNotificationW 失败）→ 释放事件，降级。
        let _ = unsafe { SetEvent(shutdown_event) };
        let _ = unsafe { CloseHandle(shutdown_event) };
        log::warn!("start_watcher: watch dir unavailable — config hot-reload disabled");
        return Ok(());
    }

    // 3. spawn 线程：wait_and_handle → hot_reload_impl；shutdown 事件置位 → 退出。
    let cfg = apo.config_path.clone();
    let inner = apo.mutex.clone();
    let clsid = apo.clsid;
    // 仅诊断日志用（不 deref）：用 mutex Arc 的稳定堆地址，避免 &ApoObject 生命周期耦合。
    let obj_ptr = Arc::as_ptr(&apo.mutex) as usize;
    let handle = std::thread::spawn(move || loop {
        if !watcher.wait_and_handle() {
            break; // shutdown 或句柄失效。
        }
        // 目录级变更 → 热重载（spec 短路 + 128KB 闸门内部处理）。
        hot_reload_impl(&cfg, &inner, clsid, obj_ptr);
    });

    // 4. 记录 watcher 运行时状态（&self 可写：Arc<Mutex> 内部可变性）。
    //    watcher 已 move 进线程（循环消费）；线程退出时 watcher Drop 自动关闭
    //    notify_handle（FindCloseChangeNotification）——stop 只需 SetEvent + join。
    st.shutdown_event = Some(shutdown_event);
    st.thread = Some(handle);
    Ok(())
}

/// 停止配置监控线程（object 7.1.10， 外部驱动模型）。
///
/// 1. SetEvent(shutdown_event) → wait_and_handle 返回 false → 线程循环退出
/// 2. join(watcher_thread) → 确保线程已退出（无泄漏）
/// 3. watcher.shutdown() → FindCloseChangeNotification + CloseHandle
/// 幂等：watcher 为 None（未启动/启动失败）时直接返回。
pub(crate) fn stop_watcher(apo: &ApoObject_Impl) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::SetEvent;

    // 先取出事件与线程句柄，避免持 watcher_state 锁执行 join（审查 #8）。
    let (evt, handle) = {
        let mut st = apo.watcher_state.lock().unwrap_or_else(|e| e.into_inner());
        (st.shutdown_event.take(), st.thread.take())
    };

    // 1. 置位退出事件（唤醒等待中的 wait_and_handle）。
    if let Some(evt) = evt {
        // Safety: 事件句柄由 start_watcher 创建且有效。
        let _ = unsafe { SetEvent(evt) };
    }

    // 2. join 线程（确保已退出）。watcher 在线程内（move 消费），线程退出时
    //    watcher Drop 已关闭 notify_handle（FindCloseChangeNotification）。
    if let Some(handle) = handle {
        let _ = handle.join();
    }

    // 3. 释放事件句柄。
    if let Some(evt) = evt {
        // Safety: 事件句柄由 start_watcher 创建且有效（此处唯一持有者，关闭后不再使用）。
        let _ = unsafe { CloseHandle(evt) };
    }
}

/// 热重载实现（object 7.1.18， 六步）。
pub(crate) fn hot_reload_impl(
    config_path: &Arc<Mutex<String>>,
    inner: &Arc<Mutex<ApoObjectInner>>,
    clsid: GUID,
    obj_ptr: usize,
) {
    // 1. 阻塞式（短锁检查，不构建新链）。
    {
        let mut guard = inner.lock().unwrap_or_else(|e| e.into_inner());
        if guard.transition.is_some() || guard.reloading {
            // 过渡在途：不丢弃，记 pending，过渡完成后 APOProcess 会触发一次重载。
            // 若过渡对象已到终点但未被 APOProcess 清掉（实例可能未走 RT 路径），
            // 直接清掉陈旧 transition，让本次变更立即走正常解析。
            let finished = guard
                .transition
                .as_ref()
                .map_or(false, |p| p.counter() >= p.length());
            if finished {
                guard.transition = None;
            } else {
                guard.pending_reload = true;
                let msg = format!(
                    "RELOAD pending(transition) clsid={clsid:?} obj=0x{obj_ptr:x}"
                );
                drop(guard);
                diag_append(&msg); // 锁外写盘（不持 inner 锁做磁盘 I/O）
                return;
            }
        }
        // 决策继续解析 → 立即置位防覆盖（reloading 表示“正在解析中”），
        // 并发 watcher 事件在短锁检查看到 true 时只记 pending_reload。
        guard.reloading = true;
    }
    // RAII：任意提前返回路径（大小闸门/解析失败/spec 相同/二次过渡）都复位标志。
    let _reloading_clear = ReloadingClear { inner };

    // 2. 128KB 文件大小闸门（控制线程 IO 安全上限，主文件提前短路）。
    let config_path = config_path
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if std::fs::metadata(&config_path)
        .map(|m| m.len() > crate::config::parser::MAX_CONFIG_FILE_SIZE)
        .unwrap_or(false)
    {
        log::warn!("hot_reload: config exceeded 128KB — keeping old chain");
        diag_append(&format!("RELOAD size-gate clsid={clsid:?}"));
        return;
    }

    // 3. 锁外解析（不持有 mutex）。parse_file_with_spec 双返回。
    let current_ctx = {
        inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pipeline_context
            .clone()
    };
    let dsp_ctx = build_dsp_context(&current_ctx);
    let parser = ConfigParser::new();
    let (filters, new_spec) = match parser.parse_file_with_spec(&config_path, &dsp_ctx) {
        Ok(r) => r,
        Err(e) => {
            log::warn!("hot_reload: config parse failed — keeping old chain");
            diag_append(&format!("RELOAD parse-fail clsid={clsid:?} err={e}"));
            return;
        }
    };

    // 4. spec 指纹短路（短锁内比较，避免与交换的 TOCTOU）。
    {
        let guard = inner.lock().unwrap_or_else(|e| e.into_inner());
        let same = guard.active_spec.len() == new_spec.len()
            && guard.active_spec.iter().zip(&new_spec).all(|(a, b)| a == b);
        if same {
            log::debug!("hot_reload: config unchanged — skip");
            diag_append(&format!("RELOAD spec-same clsid={clsid:?}"));
            return;
        }
    }

    // 5. 锁内构建 + 交换。构建成功即更新 active_spec（与 current_chain 同步）。
    let spec_len = new_spec.len();
    let mut new_chain = Chain::new();
    for f in filters {
        if new_chain.add_filter(f).is_err() {
            log::warn!("hot_reload: add_filter failed — keeping old chain");
            return;
        }
    }
    // DSP 依赖 initialize 预计算系数/状态（GraphicEQ/PEQ/IIR/Delay/Convolution）。
    new_chain.initialize(dsp_ctx.sample_rate, &dsp_ctx.channel_names);

    let mut guard = inner.lock().unwrap_or_else(|e| e.into_inner());
    if guard.transition.is_some() {
        guard.pending_reload = true;
        return;
    }
    let old = std::mem::replace(&mut guard.current_chain, Box::new(new_chain));
    guard.outgoing_chain = Some(old);
    guard.pending_reload = false;
    guard.reloading = false;
    // 热重载后同步复用键（config_path + 采样率 + 通道），下次 Relock 直接复用热重载后的链。
    guard.last_lock_key = Some((
        config_path.clone(),
        dsp_ctx.sample_rate,
        dsp_ctx.channel_names.clone(),
    ));
    guard.active_spec = new_spec;
    let filter_count = guard.current_chain.filter_count();
    let length = default_smoothing_length(guard.pipeline_context.sample_rate);
    let mut sm = SmoothingProvider::new(length);
    sm.begin();
    guard.transition = Some(sm);
    drop(guard);
    // 锁外写盘。
    diag_append(&format!(
        "RELOAD applied clsid={clsid:?} filters={filter_count} spec={spec_len}"
    ));
}

/// RAII 清位：`reloading` 表示“正在解析中”，任意提前返回路径
/// （大小闸门/解析失败/spec 相同/add_filter 失败/二次过渡）都必须复位，
/// 防止后续重载被自己拦截。
struct ReloadingClear<'a> {
    inner: &'a Arc<Mutex<ApoObjectInner>>,
}

impl Drop for ReloadingClear<'_> {
    fn drop(&mut self) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.reloading = false;
    }
}

/// 运行期诊断日志（，仅控制线程调用，非 RT）：`C:\ProgramData\VxAPO\diag.log`。
/// 记录 Lock/热重载的关键事件，供设备切换/热重载失效问题定位；失败静默。
pub(crate) fn diag_append(line: &str) {
    use std::io::Write;
    /// 诊断日志单文件上限：达到后轮转为 `diag.1.log`，防止无界增长。
    const DIAG_MAX_BYTES: u64 = 1024 * 1024;
    const DIAG_PATH: &str = r"C:\ProgramData\VxAPO\diag.log";
    const DIAG_ROTATED_PATH: &str = r"C:\ProgramData\VxAPO\diag.1.log";
    // serialize log writes across all watcher/control threads to prevent
    // interleaved/corrupted lines during reload storms.
    static DIAG_LOCK: Mutex<()> = Mutex::new(());
    let _diag_guard = DIAG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // 1MB 轮转：先移除上一份轮转文件再改名（best-effort，失败继续追加）。
    if std::fs::metadata(DIAG_PATH)
        .map(|m| m.len() >= DIAG_MAX_BYTES)
        .unwrap_or(false)
    {
        let _ = std::fs::remove_file(DIAG_ROTATED_PATH);
        let _ = std::fs::rename(DIAG_PATH, DIAG_ROTATED_PATH);
    }
    let secs = std::time::SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(DIAG_PATH)
    {
        let _ = writeln!(f, "[{secs}] {line}");
    }
}
