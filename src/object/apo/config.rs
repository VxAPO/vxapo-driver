//! object/apo/config.rs — per-device 配置路径解析与运行期诊断输出
//!
//! 职责：从 APOInitSystemEffects 提取端点 GUID、解析 per-device config 路径、
//! 输出运行期诊断日志（diag.log）。不包含 COM 接口方法；热重载编排与 RT 转储
//! 分别在 reload.rs / rtdump.rs，实现经本模块 re-export 保持调用路径不变。

use std::sync::Mutex;
use std::time::SystemTime;

use crate::sys::com::apo_types::{
    APOInitSystemEffects, PKEY_AudioEndpoint_GUID, PROPVARIANT, VT_CLSID, VT_LPWSTR,
};
use crate::sys::com::prelude::{GUID, guid_to_string};

/// 配置文件默认路径（兜底：无设备 GUID / 配置根创建失败时回退单实例共用路径）。
pub(crate) const DEFAULT_CONFIG_PATH: &str = r"C:\ProgramData\VxAPO\config.toml";

/// per-device 配置根目录（每设备一个 `{GUID}` 子目录）。
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

/// 确定 per-device 配置路径（object 7.1.8）：
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

/// per-device 配置文件的绝对路径（`{CONFIG_ROOT}\{guid}\config.toml`）。
///
/// 供 cli 与 driver 共用同一路径布局；纯拼接，不创建目录、不写文件。
pub fn device_config_path(guid: &str) -> String {
    std::path::Path::new(CONFIG_ROOT)
        .join(guid)
        .join("config.toml")
        .display()
        .to_string()
}

/// 运行期诊断日志（仅控制线程调用，非 RT）：`C:\ProgramData\VxAPO\diag.log`。
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
// ── 子模块 re-export（实现见 object/apo/reload.rs 与 rtdump.rs）────────────

pub(crate) use super::reload::{WatcherState, hot_reload_impl, start_watcher, stop_watcher};
pub(crate) use super::rtdump::{rt_dump_flush, rt_dump_open};
