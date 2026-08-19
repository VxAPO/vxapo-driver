//! object/apo/lock_key.rs — Lock 复用键的文件指纹（配置变更检测）。

/// 配置文件指纹：`(mtime_nanos, size)`。文件不存在返回 `(0, 0)`。
///
/// 复用键携带指纹：配置文件一变（新增/修改 PEQ 等）指纹即变 → 复用键失效，
/// Lock 重新解析并重建链，避免"新流复用旧配置链"导致音频错乱。
pub(crate) fn config_stamp(path: &str) -> (u64, u64) {
    use std::time::UNIX_EPOCH;
    std::fs::metadata(path)
        .map(|m| {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            (mtime, m.len())
        })
        .unwrap_or((0, 0))
}
