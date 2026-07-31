//! pipeline/chain.rs — Filter 链执行 + 延迟累计（v6.2 规范 4.5）
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

    /// 总延迟（采样数）。
    pub fn total_latency(&self) -> u32 {
        self.total_latency
    }

    /// 过滤器数量。
    pub fn filter_count(&self) -> usize {
        self.filters.len()
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
    fn validate_frame_count() {
        let mut c = Chain::new();
        c.add_filter(Box::new(PassthroughFilter)).unwrap();
        assert!(c.validate_frame_count(1000));
    }
}