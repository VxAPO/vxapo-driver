//! install/device/stale/matching.rs — 旧记录到活跃端点的分层配对
//!
//! 端点历史 / 旧端点键实例 ID / 记录落盘身份 / 硬件 ID 四层匹配，
//! 歧义或多候选一律判未命中（只提供清理，不猜）。
//! 共享常量与公开类型见父模块 `install/device/stale.rs`。

use super::*;
use super::detect::*;

/// 活跃端点 + 稳定身份（身份读取失败时为空值，不影响其它端点）。
pub(super) struct ActiveEndpoint {
    pub(super) guid: String,
    pub(super) name: String,
    pub(super) identity: EndpointIdentity,
}

pub(super) fn active_endpoints() -> Result<Vec<ActiveEndpoint>> {
    let devices = enumerate_devices()?;
    Ok(devices
        .into_iter()
        .filter_map(|d| {
            let endpoint = d.endpoint?;
            let identity = find_endpoint_path(&endpoint.endpoint_guid)
                .ok()
                .map(|path| read_endpoint_identity(&path))
                .unwrap_or_default();
            Some(ActiveEndpoint {
                guid: endpoint.endpoint_guid,
                name: endpoint.friendly_name,
                identity,
            })
        })
        .collect())
}

/// 配对结果。
#[derive(Debug)]
pub(super) enum MatchOutcome {
    /// 命中唯一活跃端点 + 命中来源。
    Matched(usize, &'static str),
    /// 命中多个候选：不猜，保持 unmatched。
    Ambiguous,
    /// 未命中。
    None,
}

/// 活跃端点索引：端点历史 / 实例 ID / 硬件 ID 三个倒排表。
pub(super) struct MatchIndex {
    /// 活跃端点归一化产品名（与下标一一对应，供硬件 ID 兜底做产品名比较）。
    products: Vec<String>,
    /// 历史端点 GUID（小写）→ 活跃端点下标（可能多候选）。
    by_history: HashMap<String, Vec<usize>>,
    /// 活跃端点 GUID（小写）→ 下标。
    active_by_guid: HashMap<String, usize>,
    /// 归一化设备实例 ID → 活跃端点下标。
    by_instance: HashMap<String, Vec<usize>>,
    /// 归一化硬件 ID → 活跃端点下标。
    by_hardware: HashMap<String, Vec<usize>>,
}

impl MatchIndex {
    pub(super) fn new(active: &[ActiveEndpoint]) -> Self {
        let mut index = MatchIndex {
            products: Vec::with_capacity(active.len()),
            by_history: HashMap::new(),
            active_by_guid: HashMap::new(),
            by_instance: HashMap::new(),
            by_hardware: HashMap::new(),
        };
        for (i, ep) in active.iter().enumerate() {
            index.products.push(ep.identity.product_key());
            index
                .active_by_guid
                .insert(ep.guid.to_ascii_lowercase(), i);
            for guid in &ep.identity.endpoint_history {
                push_unique(index.by_history.entry(guid.clone()).or_default(), i);
            }
            if !ep.identity.instance_id.is_empty() {
                push_unique(
                    index
                        .by_instance
                        .entry(ep.identity.instance_id.clone())
                        .or_default(),
                    i,
                );
            }
            for hw in &ep.identity.hardware_ids {
                push_unique(index.by_hardware.entry(hw.clone()).or_default(), i);
            }
        }
        index
    }

    /// 分层匹配置记录 → 活跃端点。
    pub(super) fn resolve(&self, record: &StaleRecord, stored: &EndpointIdentity) -> MatchOutcome {
        // ① 端点历史：老 GUID 出现在某活跃端点的历史里（GUID 刷新后最可靠的现成线索）。
        if let Some(candidates) = self.by_history.get(&record.guid.to_ascii_lowercase()) {
            match unique_index(candidates.as_slice()) {
                Some(i) => return MatchOutcome::Matched(i, "endpoint_history"),
                None => return MatchOutcome::Ambiguous,
            }
        }
        // ①b 反方向：记录已落盘的 EndpointHistory 里出现活跃端点 GUID。
        let reverse: Vec<usize> = stored
            .endpoint_history
            .iter()
            .filter_map(|guid| self.active_by_guid.get(guid).copied())
            .collect();
        match unique_index(&reverse) {
            Some(i) => return MatchOutcome::Matched(i, "endpoint_history"),
            None if !reverse.is_empty() => return MatchOutcome::Ambiguous,
            None => {}
        }
        // ② 老端点键仍可读时的实例 ID（旧路径保留）。
        if !record.device_instance_id.is_empty() {
            if let Some(candidates) = self.by_instance.get(&record.device_instance_id) {
                match unique_index(candidates.as_slice()) {
                    Some(i) => return MatchOutcome::Matched(i, "device_instance_id"),
                    None => return MatchOutcome::Ambiguous,
                }
            }
        }
        // ③ 记录键里落盘的实例 ID。
        if !stored.instance_id.is_empty() {
            if let Some(candidates) = self.by_instance.get(&stored.instance_id) {
                match unique_index(candidates.as_slice()) {
                    Some(i) => return MatchOutcome::Matched(i, "stored_identity"),
                    None => return MatchOutcome::Ambiguous,
                }
            }
        }
        // ④ 硬件 ID 相交 + 产品名一致，且唯一候选（USB 换口导致实例 ID 变化时兜底）。
        let mut candidates: Vec<usize> = Vec::new();
        for hw in &stored.hardware_ids {
            if let Some(found) = self.by_hardware.get(hw) {
                for i in found {
                    push_unique(&mut candidates, *i);
                }
            }
        }
        if !stored.product_name.is_empty() {
            // 产品名一致优先：过滤后仍为空则退回硬件 ID 候选（部分设备产品名缺失/不同）。
            let product_key = stored.product_key();
            let filtered: Vec<usize> = candidates
                .iter()
                .copied()
                .filter(|i| self.products.get(*i).map(|p| *p == product_key).unwrap_or(false))
                .collect();
            if !filtered.is_empty() {
                candidates = filtered;
            }
        }
        match unique_index(&candidates) {
            Some(i) => MatchOutcome::Matched(i, "hardware_id"),
            None if candidates.len() > 1 => MatchOutcome::Ambiguous,
            None => MatchOutcome::None,
        }
    }
}

/// 下标去重追加。
pub(super) fn push_unique(list: &mut Vec<usize>, value: usize) {
    if !list.contains(&value) {
        list.push(value);
    }
}

/// 候选唯一下标；`None` 表示 0 个或多个。
pub(super) fn unique_index(candidates: &[usize]) -> Option<usize> {
    match candidates {
        [only] => Some(*only),
        _ => None,
    }
}

/// 从旧 GUID 的 MMDevices 端点键读设备实例 ID（端点未启用/未插入也可读）。
pub(super) fn endpoint_device_instance_id(guid: &str) -> Option<String> {
    for root in [
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render",
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture",
    ] {
        let path = format!("{root}\\{guid}");
        if let Ok(key) = RegKey::open(HKEY_LOCAL_MACHINE, &path) {
            if let Ok(Some(info)) = query_endpoint(&key) {
                if !info.device_id.is_empty() {
                    return Some(normalize_device_id(&info.device_id));
                }
            }
        }
    }
    None
}
