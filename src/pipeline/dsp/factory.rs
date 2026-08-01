//! pipeline/dsp/factory.rs — FilterRegistry + 工厂注册 + 工厂遍历匹配（v6.2 规范 4.10）
//!
//! 职责：注册表持有按优先级排序的工厂列表，按遍历顺序尝试创建过滤器。
//! 同时定义内置 DSP 工厂（Preamp/Copy/...）与 `register_builtin_filters`。
//!
//! 引用来源：
//! - `crate::pipeline::dsp::filter::*`
//! - `crate::utils::vx_error::VxApoError`
//!
//! 导出给：`config/commands/*.rs`、`config/parser.rs`。

use crate::pipeline::dsp::copy::{parse_copy_ops, CopyFilter};
use crate::pipeline::dsp::filter::{
    ConfigLoader, DspContext, Filter, FilterCreateResult, FilterFactory,
};

// ══════════════════════════════════════════════════════════════════════════════
// FilterRegistry — 工厂注册表
// ══════════════════════════════════════════════════════════════════════════════

/// 过滤器工厂注册表。
///
/// 持有按优先级排序的工厂列表。
pub struct FilterRegistry {
    factories: Vec<Box<dyn FilterFactory>>,
}

impl FilterRegistry {
    /// 创建空注册表。
    pub fn new() -> Self {
        Self {
            factories: Vec::new(),
        }
    }

    /// 注册工厂（追加到末尾，优先级 = 注册顺序）。
    pub fn register(&mut self, factory: Box<dyn FilterFactory>) {
        self.factories.push(factory);
    }

    /// 工厂数量。
    pub fn len(&self) -> usize {
        self.factories.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.factories.is_empty()
    }

    /// 按索引获取工厂引用。
    pub fn get(&self, index: usize) -> Option<&dyn FilterFactory> {
        self.factories.get(index).map(|f| f.as_ref())
    }

    /// 遍历所有工厂的命令名。
    pub fn factory_names(&self) -> Vec<&str> {
        self.factories.iter().map(|f| f.command_name()).collect()
    }

    /// 按遍历顺序尝试创建过滤器。
    ///
    /// 第一个返回 `Filter` 或 `NoFilter` 的工厂胜出。
    pub fn try_create(
        &self,
        params: &str,
        ctx: &DspContext,
        loader: &dyn ConfigLoader,
    ) -> TryCreateOutcome {
        for (index, factory) in self.factories.iter().enumerate() {
            match factory.create_filter(params, ctx, loader) {
                FilterCreateResult::Filter(f) => {
                    return TryCreateOutcome {
                        result: OutcomeKind::FilterAdded(f),
                        factory_index: Some(index),
                        factory_name: Some(factory.command_name().to_owned()),
                    };
                }
                FilterCreateResult::NoFilter => {
                    return TryCreateOutcome {
                        result: OutcomeKind::MatchedNoFilter,
                        factory_index: Some(index),
                        factory_name: Some(factory.command_name().to_owned()),
                    };
                }
                FilterCreateResult::AbortFile => {
                    return TryCreateOutcome {
                        result: OutcomeKind::Aborted,
                        factory_index: Some(index),
                        factory_name: Some(factory.command_name().to_owned()),
                    };
                }
                FilterCreateResult::NoMatch => {
                    // 继续尝试下一工厂
                }
            }
        }
        TryCreateOutcome {
            result: OutcomeKind::Unmatched,
            factory_index: None,
            factory_name: None,
        }
    }
}

impl Default for FilterRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// `try_create` 的返回结果。
#[derive(Debug)]
pub struct TryCreateOutcome {
    pub result: OutcomeKind,
    pub factory_index: Option<usize>,
    pub factory_name: Option<String>,
}

/// 匹配结果种类。
#[derive(Debug)]
pub enum OutcomeKind {
    /// 匹配成功，产生了过滤器实例。
    FilterAdded(Box<dyn Filter>),
    /// 匹配成功，无需产生过滤器。
    MatchedNoFilter,
    /// 应中止当前文件解析。
    Aborted,
    /// 无工厂匹配。
    Unmatched,
}

// ══════════════════════════════════════════════════════════════════════════════
// 工厂索引常量（v6.2 规范 4.10）
// ══════════════════════════════════════════════════════════════════════════════

/// 内置工厂总数（15）。
pub const FACTORY_COUNT: usize = 15;

/// 工厂索引常量表。
pub mod index {
    pub const DEVICE: usize = 0;
    pub const IF: usize = 1;
    pub const EVAL: usize = 2;
    pub const INCLUDE: usize = 3;
    pub const STAGE: usize = 4;
    pub const CHANNEL: usize = 5;
    pub const IIR: usize = 6;
    pub const BIQUAD: usize = 7;
    pub const PREAMP: usize = 8;
    pub const DELAY: usize = 9;
    pub const COPY: usize = 10;
    pub const CONVOLUTION: usize = 11;
    pub const GRAPHIC_EQ: usize = 12;
    pub const VST_PLUGIN: usize = 13;
    pub const LOUDNESS_CORRECTION: usize = 14;
}

// ══════════════════════════════════════════════════════════════════════════════
// 内置工厂
// ══════════════════════════════════════════════════════════════════════════════

/// `Preamp:` 命令工厂 → GainFilter。
///
/// 语法：`Preamp: -6.0 dB`（可选 "dB" 后缀）。
#[derive(Debug)]
pub struct PreampFactory;

impl FilterFactory for PreampFactory {
    fn create_filter(
        &self,
        params: &str,
        _ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        let trimmed = params.trim();
        // 移除可选的 "dB" 后缀。
        let num = trimmed
            .strip_suffix("dB")
            .map(str::trim)
            .unwrap_or(trimmed);
        match num.parse::<f32>() {
            Ok(db) => FilterCreateResult::Filter(Box::new(crate::pipeline::dsp::gain::GainFilter::new(db))),
            Err(_) => FilterCreateResult::NoMatch,
        }
    }

    fn command_name(&self) -> &str {
        "Preamp"
    }
}

/// `Copy:` 命令工厂 → CopyFilter。
///
/// 语法：`Copy: L2=L R2=R`
#[derive(Debug)]
pub struct CopyFactory;

impl FilterFactory for CopyFactory {
    fn create_filter(
        &self,
        params: &str,
        ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        match parse_copy_ops(params, &ctx.channel_names) {
            Some(ops) => FilterCreateResult::Filter(Box::new(CopyFilter::new(ops))),
            None => FilterCreateResult::NoMatch,
        }
    }

    fn command_name(&self) -> &str {
        "Copy"
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// register_builtin_filters
// ══════════════════════════════════════════════════════════════════════════════

/// 注册所有内置 Filter 工厂到 FilterRegistry（v6.2 规范 4.10）。
///
/// 当前已注册：
/// - `Preamp:` → GainFilter
/// - `Copy:` → CopyFilter
///
/// 待注册（TODO 下一批）：Delay / GraphicEQ / Filter(PK/LP/HP/...) / Convolution / VST / Loudness。
pub fn register_builtin_filters(registry: &mut FilterRegistry) {
    registry.register(Box::new(PreampFactory));
    registry.register(Box::new(CopyFactory));
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::dsp::filter::{DeviceType, ProcessingStage, PassthroughFilter};
    use std::collections::HashMap;

    fn test_ctx() -> DspContext {
        DspContext {
            sample_rate: 48000,
            channel_count: 2,
            channel_mask: 0x3,
            channel_names: vec!["L".into(), "R".into()],
            max_frame_count: 480,
            bits_per_sample: 32,
            device_type: DeviceType::Render,
            stage: ProcessingStage::None,
            variables: HashMap::new(),
        }
    }

    struct NullLoader;
    impl ConfigLoader for NullLoader {
        fn load_config(&self, _path: &str, _ctx: &DspContext) -> Vec<Box<dyn Filter>> {
            vec![]
        }
    }

    #[test]
    fn preamp_factory_parses_db() {
        let factory = PreampFactory;
        let result = factory.create_filter("-6.0 dB", &test_ctx(), &NullLoader);
        assert!(matches!(result, FilterCreateResult::Filter(_)));
    }

    #[test]
    fn preamp_factory_invalid_no_match() {
        let factory = PreampFactory;
        let result = factory.create_filter("oops", &test_ctx(), &NullLoader);
        assert!(matches!(result, FilterCreateResult::NoMatch));
    }

    #[test]
    fn copy_factory_parses_mapping() {
        let factory = CopyFactory;
        let result = factory.create_filter("L=R", &test_ctx(), &NullLoader);
        assert!(matches!(result, FilterCreateResult::Filter(_)));
    }

    #[test]
    fn copy_factory_invalid_no_match() {
        let factory = CopyFactory;
        let result = factory.create_filter("X=Y", &test_ctx(), &NullLoader);
        assert!(matches!(result, FilterCreateResult::NoMatch));
    }

    #[test]
    fn register_registers_known_factories() {
        let mut registry = FilterRegistry::new();
        register_builtin_filters(&mut registry);
        let names = registry.factory_names();
        assert!(names.contains(&"Preamp"));
        assert!(names.contains(&"Copy"));
    }

    #[test]
    fn empty_registry_unmatched() {
        let registry = FilterRegistry::new();
        let outcome = registry.try_create("x", &test_ctx(), &NullLoader);
        assert!(matches!(outcome.result, OutcomeKind::Unmatched));
    }

    #[test]
    fn register_and_len() {
        let mut registry = FilterRegistry::new();
        assert!(registry.is_empty());
        registry.register(Box::new(CopyFactory));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn factory_names_lists_all() {
        let mut registry = FilterRegistry::new();
        registry.register(Box::new(CopyFactory));
        assert_eq!(registry.factory_names(), vec!["Copy"]);
    }

    /// 匹配一切的工厂（返回 PassthroughFilter）。
    #[derive(Debug)]
    struct MatchAllFactory;

    impl FilterFactory for MatchAllFactory {
        fn create_filter(
            &self,
            _params: &str,
            _ctx: &DspContext,
            _loader: &dyn ConfigLoader,
        ) -> FilterCreateResult {
            FilterCreateResult::Filter(Box::new(PassthroughFilter))
        }

        fn command_name(&self) -> &str {
            "*"
        }
    }

    #[test]
    fn try_create_first_match_wins() {
        let mut registry = FilterRegistry::new();
        registry.register(Box::new(MatchAllFactory));
        let outcome = registry.try_create("anything", &test_ctx(), &NullLoader);
        assert!(matches!(outcome.result, OutcomeKind::FilterAdded(_)));
        assert_eq!(outcome.factory_index, Some(0));
    }

    #[test]
    fn index_constants_order() {
        assert!(index::DEVICE < index::IF);
        assert!(index::IF < index::EVAL);
        assert!(index::EVAL < index::INCLUDE);
        assert!(index::INCLUDE < index::STAGE);
        assert!(index::STAGE < index::CHANNEL);
        assert!(index::CHANNEL < index::IIR);
        assert!(index::IIR < index::BIQUAD);
        assert!(index::BIQUAD < index::PREAMP);
        assert!(index::PREAMP < index::DELAY);
        assert!(index::DELAY < index::COPY);
        assert!(index::COPY < index::CONVOLUTION);
        assert!(index::CONVOLUTION < index::GRAPHIC_EQ);
        assert!(index::GRAPHIC_EQ < index::VST_PLUGIN);
        assert!(index::VST_PLUGIN < index::LOUDNESS_CORRECTION);
        assert!(index::LOUDNESS_CORRECTION + 1 == FACTORY_COUNT);
    }
}