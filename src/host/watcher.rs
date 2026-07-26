//! host/watcher.rs — 配置目录 + 注册表键变更监控（Note 50）
//!
//! 监控两个位置的变更：
//! 1. 配置目录文件变更（config.txt 修改/新增/删除）
//! 2. 注册表 FXProperties 子键变更
//!
//! 通知去重（Note 50）：
//! - 文件系统 watcher 可能在短时间内触发多次事件（编辑器保存 = write + rename）
//! - 使用 500ms 去重窗口：窗口内多次事件只触发一次回调
//!
//! 当前实现使用轮询（polling）方式：
//! - 每 2 秒检查一次文件修改时间和注册表变更
//! - Phase 8+ 可切换到 ReadDirectoryChangesW（Windows 原生文件监控）
//!
//! 此模块运行在配置线程，不约束 Note 12（实时安全）。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Instant;

// ══════════════════════════════════════════════════════════════════════════════
// 变更事件
// ══════════════════════════════════════════════════════════════════════════════

/// 监控到的变更事件类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    /// 配置文件变更（修改/新增）。
    ConfigFileChanged(PathBuf),
    /// 配置文件删除。
    ConfigFileDeleted(PathBuf),
    /// 注册表 FXProperties 变更。
    RegistryChanged,
}

// ══════════════════════════════════════════════════════════════════════════════
// 去重器（Note 50）
// ══════════════════════════════════════════════════════════════════════════════

/// 事件去重器。
///
/// Note 50：500ms 窗口内相同事件只触发一次。
pub struct Deduplicator {
    /// 最近一次事件的时间。
    last_event_time: Option<Instant>,
    /// 最近一次事件的哈希（用于判断是否相同）。
    last_event_hash: u64,
    /// 去重窗口。
    window_ms: u64,
}

impl Deduplicator {
    pub fn new(window_ms: u64) -> Self {
        Self {
            last_event_time: None,
            last_event_hash: 0,
            window_ms,
        }
    }

    /// 检查事件是否应被传递（true = 传递，false = 去重跳过）。
    pub fn should_emit(&mut self, event: &WatchEvent) -> bool {
        let hash = self.hash_event(event);
        let now = Instant::now();

        if let Some(last) = self.last_event_time {
            let elapsed_ms = now.duration_since(last).as_millis() as u64;
            if elapsed_ms < self.window_ms && hash == self.last_event_hash {
                return false; // 窗口内相同事件，跳过
            }
        }

        self.last_event_time = Some(now);
        self.last_event_hash = hash;
        true
    }

    fn hash_event(&self, event: &WatchEvent) -> u64 {
        let mut hasher = DefaultHasher::new();
        std::mem::discriminant(event).hash(&mut hasher);
        match event {
            WatchEvent::ConfigFileChanged(p) | WatchEvent::ConfigFileDeleted(p) => {
                p.hash(&mut hasher);
            }
            WatchEvent::RegistryChanged => {}
        }
        hasher.finish()
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 文件修改时间快照
// ══════════════════════════════════════════════════════════════════════════════

/// 单个文件的修改时间记录。
#[derive(Debug, Clone)]
struct FileSnapshot {
    path: PathBuf,
    modified: Option<std::time::SystemTime>,
    exists: bool,
}

// ══════════════════════════════════════════════════════════════════════════════
// 配置目录监控器
// ══════════════════════════════════════════════════════════════════════════════

/// 配置目录变更监控器。
///
/// 轮询模式：每 `poll_interval_ms` 毫秒扫描一次目录。
pub struct ConfigWatcher {
    /// 监控的目录。
    watch_dir: PathBuf,
    /// 监控的文件扩展名。
    extensions: Vec<String>,
    /// 上次扫描的文件快照。
    snapshots: Vec<FileSnapshot>,
    /// 去重器。
    dedup: Deduplicator,
    /// 轮询间隔（毫秒）。
    poll_interval_ms: u64,
    /// 上次轮询时间。
    last_poll: Option<Instant>,
    /// 注册表监控：上次的哈希值。
    registry_hash: u64,
}

impl ConfigWatcher {
    /// 创建新的配置目录监控器。
    ///
    /// - `watch_dir`：监控目录路径
    /// - `poll_interval_ms`：轮询间隔（毫秒），默认 2000
    /// - `dedup_window_ms`：去重窗口（毫秒），默认 500
    pub fn new(
        watch_dir: PathBuf,
        poll_interval_ms: u64,
        dedup_window_ms: u64,
    ) -> Self {
        let mut watcher = Self {
            watch_dir,
            extensions: vec!["txt".into(), "conf".into()],
            snapshots: Vec::new(),
            dedup: Deduplicator::new(dedup_window_ms),
            poll_interval_ms,
            last_poll: None,
            registry_hash: 0,
        };

        // 初始化快照
        watcher.refresh_snapshots();
        watcher
    }

    /// 检查是否到达轮询时间。
    pub fn should_poll(&self) -> bool {
        match self.last_poll {
            Some(last) => {
                let elapsed = Instant::now().duration_since(last).as_millis() as u64;
                elapsed >= self.poll_interval_ms
            }
            None => true,
        }
    }

    /// 执行一次轮询扫描，返回变更事件列表（已去重）。
    pub fn poll(&mut self) -> Vec<WatchEvent> {
        if !self.should_poll() {
            return Vec::new();
        }

        self.last_poll = Some(Instant::now());
        let mut events = Vec::new();

        // 扫描目录
        let current_files = self.scan_directory();

        // 检测新增和修改
        for file in &current_files {
            let modified = get_modified_time(&file);
            let old = self.snapshots.iter().find(|s| s.path == *file);

            match old {
                None => {
                    // 新增文件
                    let event = WatchEvent::ConfigFileChanged(file.clone());
                    if self.dedup.should_emit(&event) {
                        events.push(event);
                    }
                }
                Some(old_snap) => {
                    // 检查修改时间是否变化
                    if old_snap.modified != modified {
                        let event = WatchEvent::ConfigFileChanged(file.clone());
                        if self.dedup.should_emit(&event) {
                            events.push(event);
                        }
                    }
                }
            }
        }

        // 检测删除
        for old_snap in &self.snapshots {
            if old_snap.exists && !current_files.contains(&old_snap.path) {
                let event = WatchEvent::ConfigFileDeleted(old_snap.path.clone());
                if self.dedup.should_emit(&event) {
                    events.push(event);
                }
            }
        }

        // 更新快照
        self.snapshots = current_files
            .iter()
            .map(|p| FileSnapshot {
                path: p.clone(),
                modified: get_modified_time(p),
                exists: true,
            })
            .collect();

        events
    }

    /// 检查注册表变更。
    ///
    /// 返回 `Some(RegistryChanged)` 如果检测到变更。
    pub fn poll_registry(&mut self, current_hash: u64) -> Option<WatchEvent> {
        if current_hash != self.registry_hash {
            self.registry_hash = current_hash;
            let event = WatchEvent::RegistryChanged;
            if self.dedup.should_emit(&event) {
                return Some(event);
            }
        }
        None
    }

    /// 扫描目录中的配置文件。
    fn scan_directory(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();

        let dir = match std::fs::read_dir(&self.watch_dir) {
            Ok(d) => d,
            Err(_) => return files,
        };

        for entry in dir.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension() {
                    let ext_str = ext.to_string_lossy().to_lowercase();
                    if self.extensions.iter().any(|e| e == &ext_str) {
                        files.push(path);
                    }
                }
            }
        }

        files.sort();
        files
    }

    /// 刷新初始快照（不触发事件）。
    fn refresh_snapshots(&mut self) {
        let files = self.scan_directory();
        self.snapshots = files
            .iter()
            .map(|p| FileSnapshot {
                path: p.clone(),
                modified: get_modified_time(p),
                exists: true,
            })
            .collect();
    }
}

/// 获取文件修改时间。
fn get_modified_time(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir() -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "vxapo_test_{}_{}",
            std::process::id(),
            id
        ));
        let _ = fs::remove_dir_all(&dir); // 清理上次残留
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    // ── Deduplicator ────────────────────────────────────────────────────────

    #[test]
    fn dedup_first_event_passes() {
        let mut dedup = Deduplicator::new(500);
        let event = WatchEvent::ConfigFileChanged(PathBuf::from("test.txt"));
        assert!(dedup.should_emit(&event));
    }

    #[test]
    fn dedup_same_event_within_window() {
        let mut dedup = Deduplicator::new(500);
        let event = WatchEvent::ConfigFileChanged(PathBuf::from("test.txt"));
        dedup.should_emit(&event); // 首次
        assert!(!dedup.should_emit(&event)); // 窗口内相同事件 → 跳过
    }

    #[test]
    fn dedup_different_events_within_window() {
        let mut dedup = Deduplicator::new(500);
        let event1 = WatchEvent::ConfigFileChanged(PathBuf::from("a.txt"));
        let event2 = WatchEvent::ConfigFileChanged(PathBuf::from("b.txt"));
        dedup.should_emit(&event1);
        assert!(dedup.should_emit(&event2)); // 不同文件 → 不去重
    }

    #[test]
    fn dedup_registry_vs_file() {
        let mut dedup = Deduplicator::new(500);
        let event1 = WatchEvent::ConfigFileChanged(PathBuf::from("test.txt"));
        let event2 = WatchEvent::RegistryChanged;
        dedup.should_emit(&event1);
        assert!(dedup.should_emit(&event2)); // 不同类型 → 不去重
    }

    // ── ConfigWatcher ───────────────────────────────────────────────────────

    #[test]
    fn watcher_empty_dir() {
        let dir = temp_dir();
        let mut watcher = ConfigWatcher::new(dir.clone(), 0, 500);
        let events = watcher.poll();
        assert!(events.is_empty());
        cleanup(&dir);
    }

    #[test]
    fn watcher_detects_new_file() {
        let dir = temp_dir();
        let mut watcher = ConfigWatcher::new(dir.clone(), 0, 0);

        // 首次轮询建立基线
        watcher.poll();

        // 新增文件
        fs::write(dir.join("config.txt"), "Stage: PreMix").unwrap();

        // 等一下确保修改时间不同
        std::thread::sleep(std::time::Duration::from_millis(200));

        let events = watcher.poll();
        assert_eq!(events.len(), 1);
        match &events[0] {
            WatchEvent::ConfigFileChanged(path) => {
                assert!(path.ends_with("config.txt"));
            }
            _ => panic!("expected ConfigFileChanged"),
        }
        cleanup(&dir);
    }

    #[test]
    fn watcher_detects_file_change() {
        let dir = temp_dir();
        fs::write(dir.join("config.txt"), "v1").unwrap();

        let mut watcher = ConfigWatcher::new(dir.clone(), 0, 0);
        watcher.poll(); // 基线

        // 修改文件
        std::thread::sleep(std::time::Duration::from_millis(200));
        fs::write(dir.join("config.txt"), "v2").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));

        let events = watcher.poll();
        assert_eq!(events.len(), 1);
        cleanup(&dir);
    }

    #[test]
    fn watcher_detects_file_delete() {
        let dir = temp_dir();
        let config = dir.join("config.txt");
        fs::write(&config, "data").unwrap();

        let mut watcher = ConfigWatcher::new(dir.clone(), 0, 0);
        watcher.poll(); // 基线

        fs::remove_file(&config).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));

        let events = watcher.poll();
        assert_eq!(events.len(), 1);
        match &events[0] {
            WatchEvent::ConfigFileDeleted(path) => {
                assert!(path.ends_with("config.txt"));
            }
            _ => panic!("expected ConfigFileDeleted"),
        }
        cleanup(&dir);
    }

    #[test]
    fn watcher_ignores_non_txt_files() {
        let dir = temp_dir();
        let mut watcher = ConfigWatcher::new(dir.clone(), 0, 0);
        watcher.poll();

        fs::write(dir.join("notes.md"), "not a config").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));

        let events = watcher.poll();
        assert!(events.is_empty());
        cleanup(&dir);
    }

    #[test]
    fn watcher_no_change_no_event() {
        let dir = temp_dir();
        fs::write(dir.join("config.txt"), "data").unwrap();

        let mut watcher = ConfigWatcher::new(dir.clone(), 0, 0);
        watcher.poll();

        // 没有修改
        let events = watcher.poll();
        assert!(events.is_empty());
        cleanup(&dir);
    }

    #[test]
    fn watcher_registry_change() {
        let dir = temp_dir();
        let mut watcher = ConfigWatcher::new(dir.clone(), 0, 0);

        let event = watcher.poll_registry(12345);
        assert!(event.is_some());
        assert_eq!(event.unwrap(), WatchEvent::RegistryChanged);

        // 相同哈希 → 不触发
        let event = watcher.poll_registry(12345);
        assert!(event.is_none());

        // 哈希变化 → 触发
        let event = watcher.poll_registry(67890);
        assert!(event.is_some());
        cleanup(&dir);
    }
}