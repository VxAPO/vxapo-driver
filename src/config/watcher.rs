//! config/watcher.rs — 配置文件变更监控（规范 6.2，事件驱动）
//!
//! **职责**：监控配置目录变更（Win32 事件驱动，对齐 EAPO `notificationThread`）。
//! 不再使用轮询模式（旧 2000ms 轮询延迟高、浪费 CPU）。
//!
//! **目录级语义**：`FindFirstChangeNotificationW` 是目录级通知——只告知
//! "监控目录下有变更"，**不提供具体文件名**（文件名信息只有 `ReadDirectoryChangesW`
//! 扩展才有）。因此统一 `DirectoryChanged(watch_dir)`，不做逐文件过滤；
//! object 层 `hot_reload`（7.1.18）对任何目录变更：128KB 闸门 → 重新解析 →
//! spec 指纹比对（与 active_spec 逐项比较）——内容未变幂等跳过。
//!
//! **线程模型**：本结构体**不自启线程**——`wait_and_handle` 由调用方
//! （object/apo.rs `start_watcher`）在自建线程内循环调用；`shutdown_event`
//! 由 APO 实例持有，`UnlockForProcess` 时 `SetEvent` 触发退出 + join。
//! （new 注释的"启动 watcher 线程"为模板残留—— API 明确 2 参、
//!   线程由 object 层驱动，见 `object 7.1.9`。）

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{
    FindCloseChangeNotification, FindFirstChangeNotificationW, FindNextChangeNotification,
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
};
use windows::Win32::System::Threading::{SetEvent, WaitForMultipleObjects};

/// 去重窗口：编辑器「写临时文件 + rename」的多次通知在此窗口内合并。
const DEDUP_WINDOW_MS: u32 = 10;

// ══════════════════════════════════════════════════════════════════════════════
// 事件类型
// ══════════════════════════════════════════════════════════════════════════════

/// 监控到的变更事件类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    /// 监控目录内发生变更（**目录级通知**——`FindFirstChangeNotificationW`
    /// 不提供具体文件名（澄清），无法逐文件过滤）。
    /// 触发方（object/apo.rs hot_reload）重新解析 config.toml，经 spec 指纹
    /// 比对决定是否真正切换（内容未变 → 幂等跳过，无听感副作用）。
    DirectoryChanged(PathBuf), // watch_dir
    /// 注册表配置变化（保留：poll_registry 哈希兜底，低频）。
    RegistryChanged,
}

// ══════════════════════════════════════════════════════════════════════════════
// ConfigWatcher
// ══════════════════════════════════════════════════════════════════════════════

/// 配置目录变更监控器（事件驱动）。
///
/// 核心：监控 **目录**（非文件——文件被删除重建时句柄失效，目录天然健壮）。
/// `wait_and_handle` 阻塞直到目录变更或 shutdown，由调用方线程循环驱动；
/// 10ms 去重窗口合并编辑器多次通知；shutdown 时置位事件使循环退出。
pub struct ConfigWatcher {
    /// 监控目录。
    watch_dir: PathBuf,
    /// 退出事件（APO 实例持有；UnlockForProcess 时 SetEvent + join）。
    shutdown_event: HANDLE,
    /// 目录变更通知句柄（FindFirstChangeNotificationW）。
    notify_handle: HANDLE,
    /// 上次注册表哈希（poll_registry 用）。
    last_registry_hash: Option<u64>,
}

impl ConfigWatcher {
    /// 创建监控器并建立目录变更通知句柄。
    ///
/// - `watch_dir`：**监控目录**（如 `Documents\VxAPO\{GUID}`），非 config.toml 文件本身
    /// - `shutdown_event`：外部持有的退出事件（APO 实例持有；UnlockForProcess 时
    /// `SetEvent` 后 join 线程—— 生命周期随锁定周期）
    ///
    /// 若 `FindFirstChangeNotificationW` 失败（目录不存在等），`notify_handle`
    /// 为无效句柄，`wait_and_handle` 立即返回 `false`（等效不监控）。
    /// 调用方（apo.rs `start_watcher`）可预检目录存在以降级日志。
    pub fn new(watch_dir: PathBuf, shutdown_event: HANDLE) -> Self {
        // Safety: watch_dir 为所有权 PathBuf 转 HSTRING 借用（存活至调用返回）；
        // bWatchSubtree=true（含子目录）、FILE_NAME|LAST_WRITE 过滤。
        let notify_handle = unsafe {
            use windows::core::HSTRING;
            let dir_w = HSTRING::from(watch_dir.as_os_str());
            FindFirstChangeNotificationW(
                &dir_w,
                true,
                FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE,
            )
        }
        .unwrap_or(HANDLE(std::ptr::null_mut()));

        Self {
            watch_dir,
            shutdown_event,
            notify_handle,
            last_registry_hash: None,
        }
    }

    /// 通知句柄（供调用方检查是否有效）。
    pub fn notify_handle(&self) -> HANDLE {
        self.notify_handle
    }

    /// 监控目录。
    pub fn watch_dir(&self) -> &Path {
        &self.watch_dir
    }

    /// 等待并处理一个事件（阻塞直到文件变更或 shutdown）。
    ///
    /// - 返回 `true`：目录发生变更（已重置通知句柄，可继续下一次等待）。
    ///   调用方应触发 `hot_reload`（object 层：128KB 闸门 + spec 指纹短路）。
    /// - 返回 `false`：shutdown 事件已置位（循环应退出）；或句柄无效/等待失败。
    ///
    /// 内部流程：
    ///   1. `WaitForMultipleObjects([shutdown_event, notify_handle], false, 无限)` 异步阻塞
    ///   2. shutdown → 返回 false
    ///   3. 目录变更 → `WaitForMultipleObjects([notify_handle], false, 10ms)` 去重
    /// （合并编辑器「写临时文件 + rename」的多次通知， 对齐 EAPO）→
    ///      `FindNextChangeNotification` 重置通知句柄 → 返回 true
    pub fn wait_and_handle(&mut self) -> bool {
        if self.notify_handle.is_invalid() {
            // 句柄无效（new 时 FindFirst 失败）→ 等效不监控。
            return false;
        }

        let handles = [self.shutdown_event, self.notify_handle];
        // Safety: 两个 HANDLE 均有效（shutdown_event 由调用方保证，notify 已检）；
        // bWaitAll=false 任一触发即返回。
        let wait = unsafe { WaitForMultipleObjects(&handles, false, u32::MAX) };

        // WAIT_OBJECT_0 = shutdown_event 触发 → 退出。
        if wait.0 == WAIT_OBJECT_0.0 {
            return false;
        }
        // WAIT_OBJECT_0 + 1 = notify_handle 触发 → 目录变更。
        if wait.0 == WAIT_OBJECT_0.0 + 1 {
            // 去重窗口 10ms：再次等待 notify（超时或再次触发均视为同一次变更合并）。
            // Safety: 同上句柄有效性；10ms 短超时。
            let _ = unsafe { WaitForMultipleObjects(&[self.notify_handle], false, DEDUP_WINDOW_MS) };
            // 重置通知，为下一次等待准备。
            // Safety: notify_handle 有效。
            let ok = unsafe { FindNextChangeNotification(self.notify_handle) };
            // 重置失败 → 句柄保持 signaled → wait 会立即返回 → hot_reload
            // 无限自旋（audiodg CPU 持续高位、声音设置页卡顿）。失败时先尝试
            // **重建**监控句柄；重建也失败才退出（等效不监控），绝不自旋。
            if ok.is_err() {
                if !self.recreate_notify() {
                    return false;
                }
            }
            return true;
        }

        // WAIT_TIMEOUT/WAIT_FAILED：不应发生（首等待无限），保守退出。
        false
    }

    /// 检查注册表变更（哈希比对，低频；与目录监控并行）。
    pub fn poll_registry(&mut self, current_hash: u64) -> Option<WatchEvent> {
        if let Some(last) = self.last_registry_hash {
            if last != current_hash {
                self.last_registry_hash = Some(current_hash);
                return Some(WatchEvent::RegistryChanged);
            }
        } else {
            // 首次调用：仅记录，不触发。
            self.last_registry_hash = Some(current_hash);
        }
        None
    }

    /// 停止监控：置位 shutdown 事件 + 关闭通知句柄。
    ///
    /// 调用方（apo.rs UnlockForProcess）随后 join 自己的 watcher 线程。
    /// 重复调用幂等（句柄关闭后置位无副作用）。
    pub fn shutdown(&mut self) {
        // Safety: shutdown_event 由 APO 实例持有且有效；SetEvent 置位唤醒等待线程。
        let _ = unsafe { SetEvent(self.shutdown_event) };
        if !self.notify_handle.is_invalid() {
            // Safety: 关闭 FindFirstChangeNotificationW 返回的通知句柄。
            let _ = unsafe { FindCloseChangeNotification(self.notify_handle) };
            // 标记失效（幂等：重复 shutdown 不再重复关闭）。
            self.notify_handle = HANDLE(std::ptr::null_mut());
        }
        // 不关闭 shutdown_event——由 APO 实例在 Drop/Unlock 统一管理，
        // 避免 ConfigWatcher 与 ApoObject 生命周期竞态（双关闭）。
    }

    /// 重建目录变更通知句柄（`FindNextChangeNotification` 失败后的自愈路径）。
    fn recreate_notify(&mut self) -> bool {
        let _ = unsafe { FindCloseChangeNotification(self.notify_handle) };
        self.notify_handle = HANDLE(std::ptr::null_mut());
        // Safety: watch_dir 为所有权 PathBuf 转 HSTRING 借用（存活至调用返回）。
        let new_handle = unsafe {
            use windows::core::HSTRING;
            let dir_w = HSTRING::from(self.watch_dir.as_os_str());
            FindFirstChangeNotificationW(
                &dir_w,
                true,
                FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE,
            )
        }
        .unwrap_or(HANDLE(std::ptr::null_mut()));
        if new_handle.is_invalid() {
            return false;
        }
        self.notify_handle = new_handle;
        true
    }
}

// Safety: ConfigWatcher 持有的 HANDLE（FindFirstChangeNotificationW 返回的目录通知句柄）
// 是句柄值（非拥有指针），跨线程传递合法——wait_and_handle 在任一线程调用均有效
// （Windows API 句柄线程安全）。shutdown_event 由 APO 实例创建、跨线程只读/置位。
unsafe impl Send for ConfigWatcher {}

impl Drop for ConfigWatcher {
    fn drop(&mut self) {
        // 兜底：若调用方未显式 shutdown（异常路径），关闭通知句柄。
        if !self.notify_handle.is_invalid() {
            // Safety: 同上。
            let _ = unsafe { FindCloseChangeNotification(self.notify_handle) };
            self.notify_handle = HANDLE(std::ptr::null_mut());
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watcher_tracks_registry_hash() {
        // poll_registry 无需真实句柄（new 失败不影响哈希追踪）。
        let dir = std::env::temp_dir().join("vxapo_watcher_test_nonexist");
        let shutdown = HANDLE(std::ptr::null_mut());
        let mut watcher = ConfigWatcher::new(dir, shutdown);
        // 首次调用只记录
        assert!(watcher.poll_registry(42).is_none());
        // 相同哈希不触发
        assert!(watcher.poll_registry(42).is_none());
        // 变化触发一次
        assert_eq!(watcher.poll_registry(43), Some(WatchEvent::RegistryChanged));
        // 再相同不触发
        assert!(watcher.poll_registry(43).is_none());
    }

    #[test]
    fn watcher_nonexistent_dir_returns_invalid_handle() {
        // 监控不存在的目录 → new 失败 → notify_handle 无效 → wait 立即返回 false。
        let dir = std::env::temp_dir().join("vxapo_nonexistent_dir_for_test");
        let _ = std::fs::remove_dir_all(&dir);
        let shutdown = HANDLE(std::ptr::null_mut());
        let mut watcher = ConfigWatcher::new(dir, shutdown);
        assert!(watcher.notify_handle().is_invalid());
        assert!(!watcher.wait_and_handle());
        // shutdown 幂等（句柄已无效）。
        watcher.shutdown();
    }

    #[test]
    fn watcher_rejects_null_shutdown_event() {
        // shutdown_event 为空句柄：wait 会返回 WAIT_FAILED → 返回 false（退出）。
        let dir = std::env::temp_dir().join("vxapo_watcher_test2");
        std::fs::create_dir_all(&dir).unwrap();
        let shutdown = HANDLE(std::ptr::null_mut());
        let mut watcher = ConfigWatcher::new(dir.clone(), shutdown);
        // 空 shutdown + 可能有效的 notify → WaitForMultipleObjects 返回 WAIT_FAILED → false。
        // 不阻塞（句柄参数含 null 时 Win32 立即返回失败）。
        let _ = watcher.wait_and_handle();
        watcher.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }
}
