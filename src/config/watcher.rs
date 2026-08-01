//! config/watcher.rs — 配置文件变更监控（v6.3 规范 6.2）

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::config::error::ConfigError;

// ══════════════════════════════════════════════════════════════════════════════
// 事件类型
// ══════════════════════════════════════════════════════════════════════════════

/// 监控到的变更事件类型。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WatchEvent {
    /// 配置文件内容变化。
    ConfigFileChanged(PathBuf),
    /// 配置文件被删除。
    ConfigFileDeleted(PathBuf),
    /// 注册表配置变化。
    RegistryChanged,
}

// ══════════════════════════════════════════════════════════════════════════════
// 去重器
// ══════════════════════════════════════════════════════════════════════════════

/// 事件去重器（Note 50：500ms 窗口内相同事件只触发一次）。
#[derive(Debug)]
pub struct Deduplicator {
    /// 事件 → 上次触发时间。
    last_emitted: HashMap<WatchEvent, Instant>,
    /// 去重窗口。
    window: Duration,
}

impl Deduplicator {
    /// 创建去重器。
    pub fn new(window: Duration) -> Self {
        Self {
            last_emitted: HashMap::new(),
            window,
        }
    }

    /// 尝试去重：窗口内相同事件只允许通过一次。
    /// 返回 `true` 表示通过（应触发），`false` 表示在窗口内已触发过。
    pub fn should_emit(&mut self, event: WatchEvent) -> bool {
        let now = Instant::now();
        if let Some(last) = self.last_emitted.get(&event) {
            if now.duration_since(*last) < self.window {
                return false;
            }
        }
        self.last_emitted.insert(event, now);
        true
    }

    /// 清空记录。
    pub fn clear(&mut self) {
        self.last_emitted.clear();
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// ConfigWatcher
// ══════════════════════════════════════════════════════════════════════════════

/// 配置文件状态快照（用于差异检测）。
#[derive(Debug, Clone)]
struct FileSnapshot {
    /// 最后修改时间。
    modified: std::time::SystemTime,
}

/// 配置目录变更监控器。
///
/// 轮询模式：每 poll_interval_ms 毫秒扫描一次目录。
/// 支持文件修改时间比对 + 注册表变更哈希比对。
#[derive(Debug)]
pub struct ConfigWatcher {
    /// 监控目录。
    watch_dir: PathBuf,
    /// 轮询间隔。
    poll_interval: Duration,
    /// 去重器。
    dedup: Deduplicator,
    /// 上次轮询时间。
    last_poll: Instant,
    /// 文件快照。
    file_snapshots: HashMap<PathBuf, FileSnapshot>,
    /// 上次注册表哈希。
    last_registry_hash: Option<u64>,
}

impl ConfigWatcher {
    /// 创建监控器。
    ///
    /// - `watch_dir`：监控目录
    /// - `poll_interval_ms`：轮询间隔（默认 2000ms）
    /// - `dedup_window_ms`：去重窗口（默认 500ms）
    pub fn new(watch_dir: PathBuf, poll_interval_ms: u64, dedup_window_ms: u64) -> Self {
        Self {
            watch_dir,
            poll_interval: Duration::from_millis(poll_interval_ms),
            dedup: Deduplicator::new(Duration::from_millis(dedup_window_ms)),
            last_poll: Instant::now() - Duration::from_millis(poll_interval_ms),
            file_snapshots: HashMap::new(),
            last_registry_hash: None,
        }
    }

    /// 执行一次轮询扫描，返回去重后的变更事件列表。
    pub fn poll(&mut self) -> Vec<WatchEvent> {
        self.last_poll = Instant::now();
        let mut events = Vec::new();

        // 收集目录下所有 .txt 配置文件（简化：所有文件都视为配置）。
        let mut found: Vec<PathBuf> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.watch_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    found.push(path);
                }
            }
        }

        // 检测新文件 / 修改的文件。
        for path in &found {
            match std::fs::metadata(path) {
                Ok(meta) => {
                    let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                    let changed = match self.file_snapshots.get(path) {
                        Some(snap) => snap.modified != modified,
                        None => true, // 新文件
                    };
                    if changed {
                        self.file_snapshots.insert(
                            path.clone(),
                            FileSnapshot { modified },
                        );
                        if self.dedup.should_emit(WatchEvent::ConfigFileChanged(path.clone())) {
                            events.push(WatchEvent::ConfigFileChanged(path.clone()));
                        }
                    }
                }
                Err(_) => {
                    // 文件读取失败：可能被删除。
                }
            }
        }

        // 检测删除的文件。
        let deleted: Vec<PathBuf> = self
            .file_snapshots
            .keys()
            .filter(|p| !p.exists())
            .cloned()
            .collect();
        for path in deleted {
            self.file_snapshots.remove(&path);
            if self.dedup.should_emit(WatchEvent::ConfigFileDeleted(path.clone())) {
                events.push(WatchEvent::ConfigFileDeleted(path));
            }
        }

        events
    }

    /// 检查注册表变更（传入当前哈希，与上次比对）。
    pub fn poll_registry(&mut self, current_hash: u64) -> Option<WatchEvent> {
        if let Some(last) = self.last_registry_hash {
            if last != current_hash {
                self.last_registry_hash = Some(current_hash);
                if self.dedup.should_emit(WatchEvent::RegistryChanged) {
                    return Some(WatchEvent::RegistryChanged);
                }
            }
        } else {
            // 首次调用：仅记录，不触发。
            self.last_registry_hash = Some(current_hash);
        }
        None
    }

    /// 是否到达轮询时间。
    pub fn should_poll(&self) -> bool {
        self.last_poll.elapsed() >= self.poll_interval
    }

    /// 监控目录。
    pub fn watch_dir(&self) -> &Path {
        &self.watch_dir
    }

    /// 手动执行一次错误检查（辅助方法，供调用方使用）。
    #[allow(dead_code)]
    fn _check_dir_exists(&self) -> Result<(), ConfigError> {
        if !self.watch_dir.is_dir() {
            return Err(ConfigError::IoError {
                path: self.watch_dir.display().to_string(),
                message: "directory does not exist".to_owned(),
            });
        }
        Ok(())
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_same_event_blocked_within_window() {
        let mut dedup = Deduplicator::new(Duration::from_millis(100));
        assert!(dedup.should_emit(WatchEvent::RegistryChanged));
        assert!(!dedup.should_emit(WatchEvent::RegistryChanged));
    }

    #[test]
    fn dedup_different_events_pass() {
        let mut dedup = Deduplicator::new(Duration::from_millis(100));
        assert!(dedup.should_emit(WatchEvent::RegistryChanged));
        assert!(dedup.should_emit(WatchEvent::ConfigFileChanged(PathBuf::from("a.txt"))));
    }

    #[test]
    fn dedup_clear_resets() {
        let mut dedup = Deduplicator::new(Duration::from_millis(100));
        assert!(dedup.should_emit(WatchEvent::RegistryChanged));
        dedup.clear();
        assert!(dedup.should_emit(WatchEvent::RegistryChanged));
    }

    #[test]
    fn watcher_tracks_registry_hash() {
        let mut watcher = ConfigWatcher::new(
            PathBuf::from("."),
            2000,
            500,
        );
        // 首次调用只记录
        assert!(watcher.poll_registry(42).is_none());
        // 相同哈希不触发
        assert!(watcher.poll_registry(42).is_none());
        // 变化触发一次
        assert!(watcher.poll_registry(43).is_some());
        // 再相同不触发
        assert!(watcher.poll_registry(43).is_none());
    }

    #[test]
    fn watcher_should_poll_initial_true() {
        let watcher = ConfigWatcher::new(PathBuf::from("."), 0, 0);
        assert!(watcher.should_poll());
    }
}