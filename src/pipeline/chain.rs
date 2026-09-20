//! pipeline/chain.rs — Filter 链执行 + 延迟累计（规范 4.5）
//!
//! 工作在去交织空间。不拥有缓冲区，接受外部传入。

use crate::pipeline::dsp::filter::Filter;

/// Filter 链——按序执行 Filter。
pub struct Chain {
    filters: Vec<Box<dyn Filter>>,
    total_latency: u32,
}

impl Chain {
    /// 创建空链。
    pub fn new() -> Self {
        Self {
            filters: Vec::new(),
            total_latency: 0,
        }
    }

    /// 添加过滤器并累计延迟。
    pub fn add_filter(&mut self, filter: Box<dyn Filter>) -> crate::utils::vx_error::Result<()> {
        self.total_latency += filter.latency();
        self.filters.push(filter);
        Ok(())
    }

    /// 初始化整条链：按顺序调用每个 Filter 的 `initialize`。
    ///
    /// 部分 DSP（GraphicEQ/PEQ/IIR/Delay/Convolution 等）依赖 `initialize`
    /// 预计算系数/分配状态；不调用会变成空处理（声音不变）。
    /// `Channel:` 类滤波器可能返回新的通道名列表，后续滤波器按更新后的名字初始化。
    pub fn initialize(&mut self, sample_rate: u32, channel_names: &[String]) {
        // 通道选择语义：`Channel:` 可能把作用域缩到任意子集（如只选 R）。
        // 这里按当前选中名字计算它们在原始平面缓冲中的槽位号，下发给每个滤波器，
        // 避免滤波器按“前 N 个槽位”误处理（如 `Channel: R` 处理成 L）。
        let base_names = channel_names.to_vec();
        let mut names = base_names.clone();
        for filter in self.filters.iter_mut() {
            // per-effect `channels` 固定槽位优先，否则按当前通道名自动计算。
            let indices: Vec<usize> = match filter.fixed_channel_indices() {
                Some(fixed) => fixed,
                None => names
                    .iter()
                    .filter_map(|name| base_names.iter().position(|base| base == name))
                    .collect(),
            };
            filter.set_channel_indices(&indices);
            if let Some(next) = filter.initialize(sample_rate, &names) {
                names = next;
            }
        }
        // 延迟可能依赖 initialize（如卷积型 GraphicEQ/Convolution 在 initialize
        // 时才确定 IR/分块长度），因此初始化完成后重算一次总延迟。
        self.total_latency = self.filters.iter().map(|f| f.latency()).sum();
    }

    /// 总延迟（采样数）。
#[cfg(test)]
    pub fn total_latency(&self) -> u32 {
        self.total_latency
    }

    /// 过滤器数量。
    pub fn filter_count(&self) -> usize {
        self.filters.len()
    }

    /// 是否为空链。
    ///
    /// `process_audio` 据此走零拷贝快路径——空链时去交织缓冲原样即输出。
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }

    /// 全链是否全部就地处理。
    ///
    /// `filters.iter().all(|f| f.is_in_place())`。调用方（process_audio）据此
    /// 决定是否可走零拷贝快路径：全链 `true` 时去交织缓冲即最终输出。
#[cfg(test)]
    pub fn is_fully_in_place(&self) -> bool {
        self.filters.iter().all(|f| f.is_in_place())
    }

    /// 在去交织空间执行 Filter 链。
    ///
    /// `samples[channel][frame]`，纯计算操作：无锁、无分配、无 I/O。
    pub fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) -> crate::utils::vx_error::Result<()> {
        for filter in self.filters.iter_mut() {
            filter.process(samples, frame_count);
        }
        Ok(())
    }

    /// 重置所有 Filter 状态。
    pub fn reset(&mut self) {
        for filter in self.filters.iter_mut() {
            filter.reset();
        }
    }

    /// 校验帧数约束（防御性检查）。
#[cfg(test)]
    pub fn validate_frame_count(&self, frame_count: usize) -> bool {
        self.filters
            .iter()
            .all(|f| f.max_frame_count().map_or(true, |max| frame_count <= max))
    }
}

impl Default for Chain {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::dsp::filter::PassthroughFilter;

    #[derive(Debug)]
    struct GainFilter {
        gain: f32,
    }

    #[derive(Debug)]
    struct InitTrackingFilter {
        initialized: bool,
    }

    /// 模拟 `Channel:` 选择的测试滤波器（不处理采样，只改作用域）。
    #[derive(Debug)]
    struct TestSelectFilter {
        channels: Vec<String>,
    }

    impl Filter for TestSelectFilter {
        fn process(&mut self, _samples: &mut [Vec<f32>], _frame_count: usize) {}

        fn initialize(
            &mut self,
            _sample_rate: u32,
            _channel_names: &[String],
        ) -> Option<Vec<String>> {
            Some(self.channels.clone())
        }

        fn is_channel_select(&self) -> bool {
            true
        }
    }

    impl Filter for InitTrackingFilter {
        fn process(&mut self, _samples: &mut [Vec<f32>], _frame_count: usize) {
            assert!(self.initialized, "filter process called before initialize");
        }
        fn initialize(&mut self, _sample_rate: u32, _channel_names: &[String]) -> Option<Vec<String>> {
            self.initialized = true;
            None
        }
    }

    impl Filter for GainFilter {
        fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
            for ch in samples.iter_mut() {
                for v in ch[..frame_count].iter_mut() {
                    *v *= self.gain;
                }
            }
        }
        fn initialize(&mut self, _sample_rate: u32, _channel_names: &[String]) -> Option<Vec<String>> {
            None
        }
    }

    #[test]
    fn new_chain_empty() {
        let c = Chain::new();
        assert_eq!(c.filter_count(), 0);
        assert_eq!(c.total_latency(), 0);
    }

    #[test]
    fn add_filter_and_latency() {
        let mut c = Chain::new();
        c.add_filter(Box::new(PassthroughFilter)).unwrap();
        assert_eq!(c.filter_count(), 1);
    }

    #[test]
    fn initialize_calls_each_filter() {
        let mut c2 = Chain::new();
        c2.add_filter(Box::new(InitTrackingFilter { initialized: false })).unwrap();
        c2.initialize(48000, &["L".into(), "R".into()]);
        assert_eq!(c2.filter_count(), 1);
        let mut samples = vec![vec![0.0f32; 4]; 2];
        c2.process(&mut samples, 4).unwrap();
    }

    #[test]
    fn process_applies_filters() {
        let mut c = Chain::new();
        c.add_filter(Box::new(GainFilter { gain: 2.0 })).unwrap();
        c.add_filter(Box::new(GainFilter { gain: 0.5 })).unwrap();
        let mut samples = vec![vec![1.0, 2.0, 3.0]];
        c.process(&mut samples, 3).unwrap();
        // 2.0 × 0.5 = 1.0
        assert_eq!(samples[0], vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn channel_selection_applies_to_selected_slot_only() {
        use crate::pipeline::dsp::gain::{GainFilter, db_to_linear};

        let mut chain = Chain::new();
        chain
            .add_filter(Box::new(TestSelectFilter {
                channels: vec!["R".into()],
            }))
            .unwrap();
        let mut gain = GainFilter::new(0.0);
        gain.set_gain_db(6.0);
        chain.add_filter(Box::new(gain)).unwrap();

        chain.initialize(48000, &["L".into(), "R".into()]);

        let mut samples = vec![vec![1.0f32; 200], vec![1.0f32; 200]];
        chain.process(&mut samples, 200).unwrap();

        // 平滑 128 步后到达 +6 dB；L（槽位 0）必须保持原样。
        assert!(
            (samples[0][199] - 1.0).abs() < 1e-6,
            "L 不应被增益，got {}",
            samples[0][199]
        );
        assert!(
            (samples[1][199] - db_to_linear(6.0)).abs() < 0.01,
            "R 应被 +6 dB，got {}",
            samples[1][199]
        );
    }

    #[test]
    fn validate_frame_count() {
        let mut c = Chain::new();
        c.add_filter(Box::new(PassthroughFilter)).unwrap();
        assert!(c.validate_frame_count(1000));
    }
}
