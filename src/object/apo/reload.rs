//! object/apo/reload.rs — 配置热重载与监控线程编排

//! 配置指纹变化时重建 DSP 链（hot_reload_impl），watcher 负责目录监控与去抖
//! （start_watcher / stop_watcher）；路径解析与诊断输出仍在 config.rs。

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
use super::config::{DEFAULT_CONFIG_PATH, CONFIG_ROOT, diag_append, resolve_config_path};
use super::rtdump::{rt_dump_flush, rt_dump_open};

/// watcher 运行时状态（外部驱动模型）。
pub(crate) struct WatcherState {
    pub thread: Option<std::thread::JoinHandle<()>>,
    pub shutdown_event: Option<windows::Win32::Foundation::HANDLE>,
}

impl Default for WatcherState {
    fn default() -> Self {
        Self { thread: None, shutdown_event: None }
    }
}

/// 启动配置监控线程（object 7.1.9，外部驱动模型）。
///
/// 流程：CreateEventW(shutdown_event) → ConfigWatcher::new(watch_dir, shutdown_event)
/// → spawn 线程循环 `wait_and_handle` → `hot_reload_impl`（DirectoryChanged → 重载）。
/// 失败降级（watcher 未启动，仅日志）——不阻塞锁定（配置热重载失效但音频链路正常）。
///
/// `#[implement]` 只暴露 `&self`（gen.rs：不向安全代码暴露所有权实例）——因此用
/// `Arc<Mutex<WatcherState>>` 内部可变性；spawn 线程 clone `config_path`/
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
    // 热重载后同步复用键（config_path + 文件指纹 + 采样率 + 通道），
    // 下次 Relock 直接复用热重载后的链。
    let (cfg_mtime, cfg_size) = crate::object::apo::lock_key::config_stamp(&config_path);
    guard.last_lock_key = Some((
        config_path.clone(),
        cfg_mtime,
        cfg_size,
        dsp_ctx.sample_rate,
        dsp_ctx.channel_names.clone(),
    ));
    guard.active_spec = new_spec;
    let filter_count = guard.current_chain.filter_count();
    // 临时 RT 转储（诊断）：热重载应用含滤波器的新链时也开启采集——
    // 用户“播放中加 PEQ”的电流现场发生在 hot reload 路径，Lock 时未必命中。
    if filter_count > 0 && guard.rt_dump.is_none() {
        guard.rt_dump = rt_dump_open(guard.pipeline_context.sample_rate);
    }
    let length = default_smoothing_length(guard.pipeline_context.sample_rate);
    let mut sm = SmoothingProvider::new(length);
    sm.begin();
    guard.transition = Some(sm);
    drop(guard);
    // 锁外写盘。
    diag_append(&format!(
        "RELOAD applied clsid={clsid:?} path={config_path} filters={filter_count} spec={spec_len}"
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

