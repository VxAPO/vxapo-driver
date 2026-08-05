//! object/apo/config.rs — per-device 配置路径与热重载
//!
//! 职责：从 APOInitSystemEffects 提取端点 GUID、解析 per-device config 路径、
//! 运行 watcher 驱动的热重载逻辑。不包含 COM 接口方法。

use std::sync::{Arc, Mutex};

use crate::config::commands::register_all_commands;
use crate::config::parser::ConfigParser;
use crate::pipeline::chain::Chain;
use crate::pipeline::dsp::factory::FilterRegistry;
use crate::pipeline::dsp::transition::{SmoothingProvider, default_smoothing_length};
use crate::sys::com::apo_types::{
    APOInitSystemEffects, PKEY_AudioEndpoint_GUID, PROPVARIANT, VT_CLSID, VT_LPWSTR,
};
use crate::sys::com::prelude::{GUID, guid_to_string};

use super::inner::{ApoObjectInner, build_dsp_context};

/// 配置文件默认路径（兜底：无设备 GUID / 配置根创建失败时回退单实例共用路径）。
pub(crate) const DEFAULT_CONFIG_PATH: &str = r"C:\ProgramData\VxAPO\config.txt";

/// per-device 配置根目录（方案 A，2026-08-04 用户确认）。
pub(crate) const CONFIG_ROOT: &str = r"C:\ProgramData\VxAPO";

/// 单实例共用子目录名（无设备 GUID 兜底，object 7.1.8）。
pub(crate) const DEFAULT_DEVICE_DIR: &str = "_default";

/// 从 APOInitSystemEffects 提取端点 GUID（object 7.1.8，v7.2）。
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
/// `C:\ProgramData\VxAPO\{GUID}\config.txt`；无 GUID / 解析失败 → `_default` 兜底。
/// 目录自动创建；config.txt 缺失时写默认 passthrough（空配置 → 链为空即 passthrough）。
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
    let path = dir.join("config.txt");

    // config.txt 缺失 → 写默认 passthrough（空文件 = 无滤波器 = passthrough）。
    if !path.exists() {
        log::info!("config not found at {}, writing default passthrough", path.display());
        if let Err(e) = std::fs::write(&path, "# VxAPO default passthrough\n") {
            log::warn!("write default config failed: {}", e);
        }
    }
    path.display().to_string()
}

/// watcher 运行时状态（v7.10，P0-4 外部驱动模型）。
pub(crate) struct WatcherState {
    pub thread: Option<std::thread::JoinHandle<()>>,
    pub shutdown_event: Option<windows::Win32::Foundation::HANDLE>,
}

impl Default for WatcherState {
    fn default() -> Self {
        Self { thread: None, shutdown_event: None }
    }
}

/// 热重载实现（object 7.1.18，v7.9 六步）。
pub(crate) fn hot_reload_impl(
    config_path: &Arc<Mutex<String>>,
    inner: &Arc<Mutex<ApoObjectInner>>,
    clsid: GUID,
    obj_ptr: usize,
) {
    // 1. R2 阻塞式（短锁检查，不构建新链）。
    {
        let mut guard = inner.lock().unwrap();
        if guard.transition.is_some() || guard.reloading {
            #[cfg(debug_assertions)]
            {
                let transition = guard.transition.is_some();
                let reloading = guard.reloading;
                let _ = std::fs::write(
                    r"C:\ProgramData\VxAPO\hot_reload_probe.txt",
                    format!(
                        "hot_reload pending/stale: clsid={clsid:?} obj=0x{obj_ptr:x} transition={transition} reloading={reloading} thread={:?}\n",
                        std::thread::current().id(),
                    ),
                );
            }
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
                return;
            }
        }
    }

    // 2. 128KB 文件大小闸门（控制线程 IO 安全上限，主文件提前短路）。
    let config_path = config_path.lock().unwrap().clone();
    if std::fs::metadata(&config_path)
        .map(|m| m.len() > crate::config::parser::MAX_CONFIG_FILE_SIZE)
        .unwrap_or(false)
    {
        log::warn!("hot_reload: config exceeded 128KB — keeping old chain");
        return;
    }

    // 3. 锁外解析（不持有 mutex）。parse_file_with_spec 双返回。
    let current_ctx = { inner.lock().unwrap().pipeline_context.clone() };
    let dsp_ctx = build_dsp_context(&current_ctx, 32);
    let mut registry = FilterRegistry::new();
    register_all_commands(&mut registry);
    let parser = ConfigParser::new(registry);
    let (filters, new_spec) = match parser.parse_file_with_spec(&config_path, &dsp_ctx) {
        Ok(r) => r,
        Err(_) => {
            log::warn!("hot_reload: config parse failed — keeping old chain");
            #[cfg(debug_assertions)]
            {
                let _ = std::fs::write(
                    r"C:\ProgramData\VxAPO\hot_reload_probe.txt",
                    "hot_reload parse failed\n",
                );
            }
            return;
        }
    };

    // 4. spec 指纹短路（短锁内比较，避免与交换的 TOCTOU）。
    {
        let guard = inner.lock().unwrap();
        let same = guard.active_spec.len() == new_spec.len()
            && guard.active_spec.iter().zip(&new_spec).all(|(a, b)| a == b);
        if same {
            log::debug!("hot_reload: config unchanged — skip");
            #[cfg(debug_assertions)]
            {
                let _ = std::fs::write(
                    r"C:\ProgramData\VxAPO\hot_reload_probe.txt",
                    "hot_reload unchanged (spec same)\n",
                );
            }
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

    let mut guard = inner.lock().unwrap();
    if guard.transition.is_some() {
        guard.pending_reload = true;
        return;
    }
    let old = std::mem::replace(&mut guard.current_chain, Box::new(new_chain));
    guard.outgoing_chain = Some(old);
    guard.pending_reload = false;
    guard.reloading = false;
    guard.active_spec = new_spec;

    #[cfg(debug_assertions)]
    {
        let _ = std::fs::write(
            r"C:\ProgramData\VxAPO\hot_reload_probe.txt",
            format!(
                "hot_reload applied clsid={clsid:?} obj=0x{obj_ptr:x} spec_len={spec_len} path={config_path} thread={:?}\n",
                std::thread::current().id(),
            ),
        );
    }

    let length = default_smoothing_length(guard.pipeline_context.sample_rate);
    let mut sm = SmoothingProvider::new(length);
    sm.begin();
    guard.transition = Some(sm);
}
