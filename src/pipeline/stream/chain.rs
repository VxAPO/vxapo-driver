//! engine/chain.rs — 双缓冲区架构与过滤器链管理（Note 15/45）
//!
//! `Chain` 维护以下状态：
//!
//! 1. 双缓冲区：`allSamples`（主缓冲区）+ `allSamples2`（辅助缓冲区），
//!    总通道数 = 实际通道 + 辅助通道
//! 2. 过滤器链：`Vec<FilterInfo>`，按配置文件顺序排列
//! 3. 通道名称：三组列表（all / current / last），用于通道映射与缓存（Note 45）
//! 4. 通道映射缓存：`currentChannelNames` 与上次相同时复用映射结果，避免重复查找（Note 45）
//!
//! 原地处理与非原地处理（Note 13b）：
//! - `inPlace = true` 的 FilterInfo 直接操作 `allSamples` 主缓冲区
//! - `inPlace = false` 的 FilterInfo 使用 `allSamples2` 交换后操作
//!
//! `APO_FLAG_INPLACE`（对外）告诉 Windows 该 APO 支持输入输出同缓冲区；
//! 引擎内部的 `inPlace`（对内）决定过滤器使用哪个缓冲区。
//! 两层"原地"含义不同，不可混淆。
//!
//! 此模块运行在实时音频线程中，禁止堆分配、互斥锁与 panic（Note 12）。

use crate::dsp::filter::Filter;

// ══════════════════════════════════════════════════════════════════════════════
// FilterInfo — 过滤器链条目（Note 15）
// ══════════════════════════════════════════════════════════════════════════════

/// 过滤器链中的单个条目。
///
/// 包含过滤器 trait object 指针和它在缓冲区中的输入输出通道映射。
#[derive(Debug)]
pub struct FilterInfo {
    /// 过滤器实例。
    pub filter: Box<dyn Filter>,
    /// 输入通道在 `allSamples` / `allSamples2` 中的索引。
    pub input_channels: Vec<usize>,
    /// 输出通道在 `allSamples` / `allSamples2` 中的索引。
    pub output_channels: Vec<usize>,
    /// 是否原地处理（Note 13b）。
    ///
    /// `true`：直接操作 `allSamples` 主缓冲区。
    /// `false`：使用 `allSamples2` 辅助缓冲区后交换指针。
    pub in_place: bool,
}

impl FilterInfo {
    /// 创建新的过滤器链条目。
    pub fn new(
        filter: Box<dyn Filter>,
        input_channels: Vec<usize>,
        output_channels: Vec<usize>,
        in_place: bool,
    ) -> Self {
        Self {
            filter,
            input_channels,
            output_channels,
            in_place,
        }
    }

    /// 输入通道数。
    pub fn input_channel_count(&self) -> usize {
        self.input_channels.len()
    }

    /// 输出通道数。
    pub fn output_channel_count(&self) -> usize {
        self.output_channels.len()
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// Chain — 过滤器链 + 双缓冲区（Note 15）
// ══════════════════════════════════════════════════════════════════════════════

/// 过滤器链管理器。
///
/// 持有过滤器链、双缓冲区和通道名称状态。
/// `pipeline.rs` 的 `process` 方法通过 `Chain` 执行所有过滤器。
pub struct Chain {
    /// 过滤器链。
    filters: Vec<FilterInfo>,

    /// 主缓冲区 `allSamples[通道][采样]`。
    ///
    /// 总通道数 = 实际音频通道 + 辅助通道（由 `Copy` 命令创建，Note 52）。
    all_samples: Vec<Vec<f32>>,

    /// 辅助缓冲区 `allSamples2[通道][采样]`。
    ///
    /// 非原地过滤器使用此缓冲区，处理完成后与 `allSamples` 交换指针。
    all_samples2: Vec<Vec<f32>>,

    /// 实际音频通道数（不含辅助通道）。
    real_channel_count: usize,

    /// 总通道数（含辅助通道）。
    all_channel_count: usize,

    /// 最大帧数（缓冲区预分配大小）。
    max_frame_count: usize,

    /// 当前通道名称列表（含辅助通道名）。
    all_channel_names: Vec<String>,

    /// 当前活动通道名称子集（`Channel:` 命令选择后的结果）。
    current_channel_names: Vec<String>,

    /// 上一次的活动通道名称列表（用于映射缓存，Note 45）。
    last_channel_names: Vec<String>,

    /// 上一次的通道映射结果缓存（Note 45）。
    ///
    /// `(通道名 → 在 allSamples 中的索引)` 映射。
    /// 当 `currentChannelNames == lastChannelNames` 时复用。
    cached_channel_map: Option<Vec<usize>>,
}

impl Chain {
    /// 创建新的过滤器链。
    ///
    /// - `real_channel_count`：实际音频通道数
    /// - `max_frame_count`：最大帧数（缓冲区预分配大小）
    /// - `channel_names`：初始通道名称列表
    pub fn new(
        real_channel_count: usize,
        max_frame_count: usize,
        channel_names: Vec<String>,
    ) -> Self {
        let all_channel_count = real_channel_count; // 初始无辅助通道

        Self {
            filters: Vec::new(),
            all_samples: vec![vec![0.0f32; max_frame_count]; all_channel_count],
            all_samples2: vec![vec![0.0f32; max_frame_count]; all_channel_count],
            real_channel_count,
            all_channel_count,
            max_frame_count,
            all_channel_names: channel_names.clone(),
            current_channel_names: channel_names.clone(),
            last_channel_names: Vec::new(),
            cached_channel_map: None,
        }
    }

    // ── 缓冲区访问 ──────────────────────────────────────────────────────────

    /// 主缓冲区引用。
    pub fn all_samples(&self) -> &[Vec<f32>] {
        &self.all_samples
    }

    /// 主缓冲区可变引用。
    pub fn all_samples_mut(&mut self) -> &mut [Vec<f32>] {
        &mut self.all_samples
    }

    /// 辅助缓冲区引用。
    pub fn all_samples2(&self) -> &[Vec<f32>] {
        &self.all_samples2
    }

    /// 辅助缓冲区可变引用。
    pub fn all_samples2_mut(&mut self) -> &mut [Vec<f32>] {
        &mut self.all_samples2
    }

    /// 交换主缓冲区和辅助缓冲区。
    ///
    /// 非原地过滤器处理完成后调用，将结果提升为主缓冲区。
    pub fn swap_buffers(&mut self) {
        std::mem::swap(&mut self.all_samples, &mut self.all_samples2);
    }

    /// 实际音频通道数。
    pub fn real_channel_count(&self) -> usize {
        self.real_channel_count
    }

    /// 总通道数（含辅助通道）。
    pub fn all_channel_count(&self) -> usize {
        self.all_channel_count
    }

    /// 最大帧数。
    pub fn max_frame_count(&self) -> usize {
        self.max_frame_count
    }

    // ── 过滤器链操作 ────────────────────────────────────────────────────────

    /// 过滤器链长度。
    pub fn filter_count(&self) -> usize {
        self.filters.len()
    }

    /// 过滤器链是否为空。
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }

    /// 获取过滤器链引用。
    pub fn filters(&self) -> &[FilterInfo] {
        &self.filters
    }

    /// 获取过滤器链可变引用。
    pub fn filters_mut(&mut self) -> &mut [FilterInfo] {
        &mut self.filters
    }

    /// 清空过滤器链。
    pub fn clear_filters(&mut self) {
        self.filters.clear();
    }

    /// 添加过滤器到链末尾。
    pub fn push_filter(&mut self, info: FilterInfo) {
        self.filters.push(info);
    }

    /// 获取所有过滤器的延迟总和。
    pub fn total_latency(&self) -> u32 {
        self.filters.iter().map(|f| f.filter.latency()).sum()
    }

    // ── 通道名称管理 ────────────────────────────────────────────────────────

    /// 所有通道名称列表（含辅助通道）。
    pub fn all_channel_names(&self) -> &[String] {
        &self.all_channel_names
    }

    /// 当前活动通道名称子集。
    pub fn current_channel_names(&self) -> &[String] {
        &self.current_channel_names
    }

    /// 设置当前活动通道名称（`Channel:` 命令调用）。
    pub fn set_current_channel_names(&mut self, names: Vec<String>) {
        self.current_channel_names = names;
    }

    /// 保存当前通道名称快照（`parser.rs` 在解析文件前调用，Note 51）。
    pub fn save_channel_snapshot(&mut self) -> Vec<String> {
        self.current_channel_names.clone()
    }

    /// 恢复通道名称快照（`parser.rs` 在解析文件后调用，Note 51）。
    pub fn restore_channel_snapshot(&mut self, snapshot: Vec<String>) {
        self.current_channel_names = snapshot;
    }

    // ── 通道映射（Note 45） ─────────────────────────────────────────────────

    /// 将 `currentChannelNames` 映射到 `allChannelNames` 中的索引。
    ///
    /// Note 45：如果 `currentChannelNames` 与上次相同，复用缓存结果。
    pub fn resolve_channel_map(&mut self) -> Vec<usize> {
        // 检查缓存
        if self.current_channel_names == self.last_channel_names {
            if let Some(ref cached) = self.cached_channel_map {
                return cached.clone();
            }
        }

        // 计算新映射
        let map = self.compute_channel_map();

        // 更新缓存
        self.last_channel_names = self.current_channel_names.clone();
        self.cached_channel_map = Some(map.clone());

        map
    }

    /// 计算通道名称 → 索引映射。
    fn compute_channel_map(&self) -> Vec<usize> {
        self.current_channel_names
            .iter()
            .map(|name| {
                self.all_channel_names
                    .iter()
                    .position(|n| n == name)
                    .unwrap_or(usize::MAX) // 未找到标记为 MAX
            })
            .collect()
    }

    /// 清除映射缓存（通道名称变更后调用）。
    pub fn invalidate_channel_cache(&mut self) {
        self.cached_channel_map = None;
        self.last_channel_names.clear();
    }

    // ── 辅助通道扩展（Note 52） ─────────────────────────────────────────────

    /// 检查并添加新的辅助通道。
    ///
    /// `Copy` 命令的 `initialize()` 可能创建新的通道名（如辅助通道）。
    /// `chain.rs` 的 `addFilters` 检测到 `allChannelNames` 中不存在的新通道时，
    /// 将其追加到末尾，扩大 `allSamples` / `allSamples2` 的通道数组（Note 52）。
    pub fn ensure_channel_exists(&mut self, channel_name: &str) -> usize {
        if let Some(idx) = self.all_channel_names.iter().position(|n| n == channel_name) {
            return idx;
        }

        // 追加新通道
        let new_idx = self.all_channel_count;
        self.all_channel_names.push(channel_name.to_owned());
        self.all_samples.push(vec![0.0f32; self.max_frame_count]);
        self.all_samples2.push(vec![0.0f32; self.max_frame_count]);
        self.all_channel_count += 1;

        // 通道变更，清除映射缓存
        self.invalidate_channel_cache();

        new_idx
    }

    // ── 通道掩码 ────────────────────────────────────────────────────────────

    /// 获取指定通道在 `allSamples` 中的索引。
    ///
    /// 未找到返回 `None`。
    pub fn channel_index(&self, name: &str) -> Option<usize> {
        self.all_channel_names.iter().position(|n| n == name)
    }

    /// 获取指定通道索引的名称。
    pub fn channel_name(&self, index: usize) -> Option<&str> {
        self.all_channel_names.get(index).map(|s| s.as_str())
    }

    /// 重置链到初始状态（保留缓冲区分配）。
    ///
    /// 用于配置热重载时清空旧过滤器链。
    pub fn reset(&mut self, channel_names: Vec<String>) {
        self.filters.clear();
        self.real_channel_count = channel_names.len();
        self.all_channel_count = channel_names.len();
        self.all_channel_names = channel_names.clone();
        self.current_channel_names = channel_names;
        self.last_channel_names.clear();
        self.cached_channel_map = None;

        // 重置缓冲区（保留已分配的 Vec 容量，只截断长度）
        self.all_samples.truncate(self.all_channel_count);
        self.all_samples2.truncate(self.all_channel_count);
        for ch in self.all_samples.iter_mut().chain(self.all_samples2.iter_mut()) {
            for s in ch.iter_mut() {
                *s = 0.0;
            }
        }
    }

    // ── 过滤器处理（pipeline 调用） ────────────────────────────────────────

    /// 遍历过滤器链执行处理。
    ///
    /// 内部解构字段，避免外部调用时的借用冲突。
    /// Note 13b：`inPlace` 过滤器直接操作主缓冲区，
    /// 非原地过滤器写入辅助缓冲区后交换。
    pub fn process_filters(&mut self, frame_count: usize) {
        let Chain {
            ref mut filters,
            ref mut all_samples,
            ref mut all_samples2,
            ..
        } = *self;

        for filter_info in filters.iter_mut() {
            if filter_info.in_place {
                filter_info.filter.process(all_samples, frame_count);
            } else {
                for c in 0..all_samples.len() {
                    all_samples2[c][..frame_count]
                        .copy_from_slice(&all_samples[c][..frame_count]);
                }
                filter_info.filter.process(all_samples2, frame_count);
                std::mem::swap(all_samples, all_samples2);
            }
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::filter::PassthroughFilter;

    fn stereo_names() -> Vec<String> {
        vec!["L".to_owned(), "R".to_owned()]
    }

    fn surround_names() -> Vec<String> {
        vec![
            "L".to_owned(), "R".to_owned(), "C".to_owned(),
            "LFE".to_owned(), "RL".to_owned(), "RR".to_owned(),
        ]
    }

    // ── Chain 创建 ──────────────────────────────────────────────────────────

    #[test]
    fn chain_new_stereo() {
        let chain = Chain::new(2, 480, stereo_names());
        assert_eq!(chain.real_channel_count(), 2);
        assert_eq!(chain.all_channel_count(), 2);
        assert_eq!(chain.max_frame_count(), 480);
        assert_eq!(chain.all_channel_names(), &["L", "R"]);
        assert_eq!(chain.current_channel_names(), &["L", "R"]);
        assert!(chain.is_empty());
    }

    #[test]
    fn chain_new_51() {
        let chain = Chain::new(6, 960, surround_names());
        assert_eq!(chain.real_channel_count(), 6);
        assert_eq!(chain.all_channel_count(), 6);
        assert_eq!(chain.all_samples().len(), 6);
        assert_eq!(chain.all_samples2().len(), 6);
    }

    #[test]
    fn chain_buffers_zero_initialized() {
        let chain = Chain::new(2, 480, stereo_names());
        for ch in chain.all_samples() {
            assert!(ch.iter().all(|&v| v == 0.0));
        }
        for ch in chain.all_samples2() {
            assert!(ch.iter().all(|&v| v == 0.0));
        }
    }

    // ── 过滤器链操作 ────────────────────────────────────────────────────────

    #[test]
    fn push_filter() {
        let mut chain = Chain::new(2, 480, stereo_names());
        assert!(chain.is_empty());

        chain.push_filter(FilterInfo::new(
            Box::new(PassthroughFilter),
            vec![0, 1],
            vec![0, 1],
            true,
        ));
        assert_eq!(chain.filter_count(), 1);
        assert!(!chain.is_empty());
    }

    #[test]
    fn push_multiple_filters() {
        let mut chain = Chain::new(2, 480, stereo_names());
        for _ in 0..5 {
            chain.push_filter(FilterInfo::new(
                Box::new(PassthroughFilter),
                vec![0, 1],
                vec![0, 1],
                true,
            ));
        }
        assert_eq!(chain.filter_count(), 5);
    }

    #[test]
    fn clear_filters() {
        let mut chain = Chain::new(2, 480, stereo_names());
        chain.push_filter(FilterInfo::new(
            Box::new(PassthroughFilter),
            vec![0, 1],
            vec![0, 1],
            true,
        ));
        chain.clear_filters();
        assert!(chain.is_empty());
    }

    // ── FilterInfo ──────────────────────────────────────────────────────────

    #[test]
    fn filter_info_channels() {
        let info = FilterInfo::new(
            Box::new(PassthroughFilter),
            vec![0, 1],
            vec![0],
            true,
        );
        assert_eq!(info.input_channel_count(), 2);
        assert_eq!(info.output_channel_count(), 1);
        assert!(info.in_place);
    }

    #[test]
    fn filter_info_non_inplace() {
        let info = FilterInfo::new(
            Box::new(PassthroughFilter),
            vec![0],
            vec![0],
            false,
        );
        assert!(!info.in_place);
    }

    // ── 缓冲区交换 ──────────────────────────────────────────────────────────

    #[test]
    fn swap_buffers() {
        let mut chain = Chain::new(2, 480, stereo_names());

        // 写入主缓冲区
        chain.all_samples_mut()[0][0] = 1.0;
        chain.all_samples_mut()[1][0] = 2.0;

        // 辅助缓冲区写入不同值
        chain.all_samples2_mut()[0][0] = 10.0;
        chain.all_samples2_mut()[1][0] = 20.0;

        chain.swap_buffers();

        // 交换后主缓冲区应有辅助的值
        assert_eq!(chain.all_samples()[0][0], 10.0);
        assert_eq!(chain.all_samples()[1][0], 20.0);

        // 辅助缓冲区应有主的值
        assert_eq!(chain.all_samples2()[0][0], 1.0);
        assert_eq!(chain.all_samples2()[1][0], 2.0);
    }

    // ── 通道名称管理 ────────────────────────────────────────────────────────

    #[test]
    fn set_current_channel_names() {
        let mut chain = Chain::new(6, 480, surround_names());
        chain.set_current_channel_names(vec!["L".to_owned(), "R".to_owned()]);
        assert_eq!(chain.current_channel_names(), &["L", "R"]);
    }

    #[test]
    fn save_restore_channel_snapshot() {
        let mut chain = Chain::new(6, 480, surround_names());
        let snapshot = chain.save_channel_snapshot();

        chain.set_current_channel_names(vec!["C".to_owned()]);
        assert_eq!(chain.current_channel_names(), &["C"]);

        chain.restore_channel_snapshot(snapshot);
        assert_eq!(chain.current_channel_names(), &["L", "R", "C", "LFE", "RL", "RR"]);
    }

    // ── 通道映射缓存（Note 45） ─────────────────────────────────────────────

    #[test]
    fn resolve_channel_map_basic() {
        let mut chain = Chain::new(2, 480, stereo_names());
        let map = chain.resolve_channel_map();
        assert_eq!(map, vec![0, 1]); // L→0, R→1
    }

    #[test]
    fn resolve_channel_map_subset() {
        let mut chain = Chain::new(6, 480, surround_names());
        chain.set_current_channel_names(vec!["C".to_owned(), "LFE".to_owned()]);
        let map = chain.resolve_channel_map();
        assert_eq!(map, vec![2, 3]); // C→2, LFE→3
    }

    #[test]
    fn resolve_channel_map_cache_hit() {
        let mut chain = Chain::new(2, 480, stereo_names());

        let map1 = chain.resolve_channel_map();
        let map2 = chain.resolve_channel_map();

        // 第二次应命中缓存，结果相同
        assert_eq!(map1, map2);
        assert!(chain.cached_channel_map.is_some());
    }

    #[test]
    fn resolve_channel_map_cache_miss_on_change() {
        let mut chain = Chain::new(6, 480, surround_names());

        let map1 = chain.resolve_channel_map();
        assert_eq!(map1, vec![0, 1, 2, 3, 4, 5]);

        chain.set_current_channel_names(vec!["L".to_owned(), "R".to_owned()]);
        let map2 = chain.resolve_channel_map();
        assert_eq!(map2, vec![0, 1]); // 只有 L, R
    }

    #[test]
    fn invalidate_channel_cache() {
        let mut chain = Chain::new(2, 480, stereo_names());
        chain.resolve_channel_map();
        assert!(chain.cached_channel_map.is_some());

        chain.invalidate_channel_cache();
        assert!(chain.cached_channel_map.is_none());
        assert!(chain.last_channel_names.is_empty());
    }

    #[test]
    fn resolve_channel_map_unknown_name() {
        let mut chain = Chain::new(2, 480, stereo_names());
        chain.set_current_channel_names(vec!["UNKNOWN".to_owned()]);
        let map = chain.resolve_channel_map();
        assert_eq!(map, vec![usize::MAX]); // 未找到
    }

    // ── 辅助通道扩展（Note 52） ─────────────────────────────────────────────

    #[test]
    fn ensure_channel_exists_new() {
        let mut chain = Chain::new(2, 480, stereo_names());
        assert_eq!(chain.all_channel_count(), 2);

        let idx = chain.ensure_channel_exists("AUX1");
        assert_eq!(idx, 2);
        assert_eq!(chain.all_channel_count(), 3);
        assert_eq!(chain.all_channel_names()[2], "AUX1");
        assert_eq!(chain.all_samples().len(), 3);
        assert_eq!(chain.all_samples2().len(), 3);
    }

    #[test]
    fn ensure_channel_exists_existing() {
        let mut chain = Chain::new(2, 480, stereo_names());
        let idx = chain.ensure_channel_exists("L");
        assert_eq!(idx, 0);
        assert_eq!(chain.all_channel_count(), 2); // 不变
    }

    #[test]
    fn ensure_channel_exists_multiple() {
        let mut chain = Chain::new(2, 480, stereo_names());
        chain.ensure_channel_exists("AUX1");
        chain.ensure_channel_exists("AUX2");
        chain.ensure_channel_exists("AUX3");
        assert_eq!(chain.all_channel_count(), 5);
        assert_eq!(chain.all_channel_names(), &["L", "R", "AUX1", "AUX2", "AUX3"]);
    }

    #[test]
    fn ensure_channel_exists_invalidate_cache() {
        let mut chain = Chain::new(2, 480, stereo_names());
        chain.resolve_channel_map();
        assert!(chain.cached_channel_map.is_some());

        chain.ensure_channel_exists("AUX1");
        // 新通道会清除缓存
        assert!(chain.cached_channel_map.is_none());
    }

    // ── 通道查询 ────────────────────────────────────────────────────────────

    #[test]
    fn channel_index_found() {
        let chain = Chain::new(2, 480, stereo_names());
        assert_eq!(chain.channel_index("L"), Some(0));
        assert_eq!(chain.channel_index("R"), Some(1));
    }

    #[test]
    fn channel_index_not_found() {
        let chain = Chain::new(2, 480, stereo_names());
        assert_eq!(chain.channel_index("C"), None);
        assert_eq!(chain.channel_index("UNKNOWN"), None);
    }

    #[test]
    fn channel_name_found() {
        let chain = Chain::new(2, 480, stereo_names());
        assert_eq!(chain.channel_name(0), Some("L"));
        assert_eq!(chain.channel_name(1), Some("R"));
    }

    #[test]
    fn channel_name_out_of_bounds() {
        let chain = Chain::new(2, 480, stereo_names());
        assert_eq!(chain.channel_name(2), None);
    }

    // ── 延迟汇总 ────────────────────────────────────────────────────────────

    #[test]
    fn total_latency_empty() {
        let chain = Chain::new(2, 480, stereo_names());
        assert_eq!(chain.total_latency(), 0);
    }

    #[test]
    fn total_latency_with_passthrough() {
        let mut chain = Chain::new(2, 480, stereo_names());
        chain.push_filter(FilterInfo::new(
            Box::new(PassthroughFilter),
            vec![0, 1],
            vec![0, 1],
            true,
        ));
        // PassthroughFilter latency = 0
        assert_eq!(chain.total_latency(), 0);
    }

    // ── Reset ───────────────────────────────────────────────────────────────

    #[test]
    fn reset_clears_state() {
        let mut chain = Chain::new(6, 480, surround_names());
        chain.push_filter(FilterInfo::new(
            Box::new(PassthroughFilter),
            vec![0, 1, 2, 3, 4, 5],
            vec![0, 1, 2, 3, 4, 5],
            true,
        ));
        chain.set_current_channel_names(vec!["L".to_owned()]);
        chain.resolve_channel_map();

        chain.reset(stereo_names());

        assert!(chain.is_empty());
        assert_eq!(chain.real_channel_count(), 2);
        assert_eq!(chain.all_channel_count(), 2);
        assert_eq!(chain.current_channel_names(), &["L", "R"]);
        assert!(chain.cached_channel_map.is_none());
    }

    // ── 模拟 pipeline 处理 ──────────────────────────────────────────────────

    #[test]
    fn simulate_inplace_filter() {
        let mut chain = Chain::new(2, 480, stereo_names());

        // 写入测试数据
        chain.all_samples_mut()[0][0] = 1.0;
        chain.all_samples_mut()[1][0] = 2.0;

        // 模拟原地过滤器：直接操作主缓冲区
        chain.push_filter(FilterInfo::new(
            Box::new(PassthroughFilter),
            vec![0, 1],
            vec![0, 1],
            true,
        ));

        chain.process_filters(480);

        // 数据不变（passthrough）
        assert_eq!(chain.all_samples()[0][0], 1.0);
        assert_eq!(chain.all_samples()[1][0], 2.0);
    }

    #[test]
    fn simulate_non_inplace_filter() {
        let mut chain = Chain::new(2, 480, stereo_names());

        // 写入测试数据
        chain.all_samples_mut()[0][0] = 1.0;
        chain.all_samples_mut()[1][0] = 2.0;

        // 非原地过滤器：写入辅助缓冲区后交换
        chain.push_filter(FilterInfo::new(
            Box::new(PassthroughFilter),
            vec![0, 1],
            vec![0, 1],
            false,
        ));

        chain.process_filters(480);

        // 数据不变（passthrough），但经历了复制+交换
        assert_eq!(chain.all_samples()[0][0], 1.0);
        assert_eq!(chain.all_samples()[1][0], 2.0);
    }

    // ── Debug ───────────────────────────────────────────────────────────────

    #[test]
    fn filter_info_debug() {
        let info = FilterInfo::new(
            Box::new(PassthroughFilter),
            vec![0, 1],
            vec![0, 1],
            true,
        );
        let debug = format!("{info:?}");
        assert!(debug.contains("FilterInfo"));
        assert!(debug.contains("in_place"));
    }
}