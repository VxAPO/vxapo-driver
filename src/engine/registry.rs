//! engine/registry.rs — 工厂注册机制（Note 14）
//!
//! 按硬编码顺序注册 15 个工厂。`parser.rs` 按下标顺序遍历，
//! 第一个返回 `Filter` 或 `NoFilter` 的工厂胜出。
//!
//! 工厂优先级（高 → 低）：
//!
//! | 序号 | 命令                  | 类型     | 说明                             |
//! |------|-----------------------|----------|----------------------------------|
//! | 1    | `Device:`             | 命令     | 设备匹配，不匹配则终止当前文件    |
//! | 2    | `If:`                 | 命令     | 条件分支                         |
//! | 3    | `Eval:`               | 命令     | 数学表达式变量设置               |
//! | 4    | `Include:`            | 命令     | 递归加载子配置文件               |
//! | 5    | `Stage:`              | 命令     | 阶段标志设置                     |
//! | 6    | `Channel:`            | 命令     | 通道选择（唯一产生 Filter 的非 DSP 命令） |
//! | 7    | `IIR:`                | DSP      | IIR 滤波器                       |
//! | 8    | `Biquad:`             | DSP      | 双二阶滤波器                     |
//! | 9    | `Preamp:`             | DSP      | 增益                             |
//! | 10   | `Delay:`              | DSP      | 延迟线                           |
//! | 11   | `Copy:`               | DSP      | 通道复制/混音                    |
//! | 12   | `Convolution:`        | DSP      | 卷积                             |
//! | 13   | `GraphicEQ:`          | DSP      | 图形均衡器                       |
//! | 14   | `VSTPlugin:`          | DSP      | VST 插件加载（feature gate）     |
//! | 15   | `LoudnessCorrection:` | DSP      | 等响曲线                         |
//!
//! Phase 3 初期全部使用 `PassthroughFactory` 占位。
//! Phase 7 补全命令工厂（1–6），Phase 8 补全 DSP 工厂（7–15）。
//!
//! 此模块同时定义 `FilterFactory` trait、`FilterCreateResult` 枚举与
//! `ConfigLoader` trait（Note 14/49）。工厂内部的非致命错误通过日志记录，
//! 返回 `NoMatch`，不使用 `Result`（Note 57）。

use crate::engine::filter::{
    ConfigLoader, EngineContext, Filter, FilterCreateResult, FilterFactory, PassthroughFactory,
};

// ══════════════════════════════════════════════════════════════════════════════
// 工厂序号常量
// ══════════════════════════════════════════════════════════════════════════════

/// 工厂总数（固定 15 个，与 EqualizerAPO 兼容）。
pub const FACTORY_COUNT: usize = 15;

/// 工厂索引常量——对应 `create_default_registry()` 中的位置。
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
// Registry — 过滤器工厂注册表
// ══════════════════════════════════════════════════════════════════════════════

/// 过滤器工厂注册表。
///
/// 持有按优先级排序的工厂列表。`parser.rs` 对每一行配置文本
/// 调用 `try_create`，按序尝试所有工厂直到匹配。
///
/// # 查找逻辑（Note 14/56）
///
/// ```text
/// for factory in registry.iter() {
///     match factory.create_filter(params, ctx, loader) {
///         Filter(f)  → add to chain, break
///         NoFilter   → break (matched, no filter needed)
///         AbortFile  → return (stop parsing this file)
///         NoMatch    → continue (try next factory)
///     }
/// }
/// ```
pub struct Registry {
    factories: Vec<Box<dyn FilterFactory>>,
}

impl Registry {
    /// 创建新的注册表。
    pub fn new(factories: Vec<Box<dyn FilterFactory>>) -> Self {
        Self { factories }
    }

    /// 创建默认注册表（15 个工厂，Phase 3 全部占位）。
    pub fn default_registry() -> Self {
        Self::new(create_default_registry())
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

    /// 按索引获取工厂的命令名。
    pub fn command_name_at(&self, index: usize) -> Option<&str> {
        self.get(index).map(|f| f.command_name())
    }

    /// 尝试为配置行创建过滤器。
    ///
    /// `params` 是冒号后的值部分（已 trim）。
    ///
    /// 返回 `TryCreateOutcome`，包含结果和匹配到的工厂索引（用于日志）。
    pub fn try_create(
        &self,
        params: &str,
        ctx: &EngineContext,
        loader: &dyn ConfigLoader,
    ) -> TryCreateOutcome {
        for (i, factory) in self.factories.iter().enumerate() {
            match factory.create_filter(params, ctx, loader) {
                FilterCreateResult::Filter(filter) => {
                    return TryCreateOutcome {
                        result: OutcomeKind::FilterAdded(filter),
                        factory_index: Some(i),
                        factory_name: Some(factory.command_name().to_owned()),
                    };
                }
                FilterCreateResult::NoFilter => {
                    return TryCreateOutcome {
                        result: OutcomeKind::MatchedNoFilter,
                        factory_index: Some(i),
                        factory_name: Some(factory.command_name().to_owned()),
                    };
                }
                FilterCreateResult::AbortFile => {
                    return TryCreateOutcome {
                        result: OutcomeKind::Aborted,
                        factory_index: Some(i),
                        factory_name: Some(factory.command_name().to_owned()),
                    };
                }
                FilterCreateResult::NoMatch => {
                    continue;
                }
            }
        }

        // 所有工厂都不匹配——静默忽略（配置行可能是注释或未知命令）
        TryCreateOutcome {
            result: OutcomeKind::Unmatched,
            factory_index: None,
            factory_name: None,
        }
    }

    /// 遍历所有工厂的命令名（用于调试输出）。
    pub fn factory_names(&self) -> Vec<&str> {
        self.factories.iter().map(|f| f.command_name()).collect()
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// TryCreateOutcome — 查找结果
// ══════════════════════════════════════════════════════════════════════════════

/// `try_create` 的返回结果。
#[derive(Debug)]
pub struct TryCreateOutcome {
    /// 结果类型。
    pub result: OutcomeKind,
    /// 匹配到的工厂索引（`Unmatched` 时为 `None`）。
    pub factory_index: Option<usize>,
    /// 匹配到的工厂命令名（`Unmatched` 时为 `None`）。
    pub factory_name: Option<String>,
}

/// `try_create` 的结果类型。
pub enum OutcomeKind {
    /// 匹配成功，产生了过滤器实例。
    FilterAdded(Box<dyn Filter>),
    /// 匹配成功，但不需要过滤器。
    MatchedNoFilter,
    /// 当前文件应停止解析。
    Aborted,
    /// 所有工厂都不匹配。
    Unmatched,
}

impl std::fmt::Debug for OutcomeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FilterAdded(_) => write!(f, "FilterAdded(..)"),
            Self::MatchedNoFilter => write!(f, "MatchedNoFilter"),
            Self::Aborted => write!(f, "Aborted"),
            Self::Unmatched => write!(f, "Unmatched"),
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// create_default_registry — 按硬编码顺序注册 15 个工厂
//
// Phase 3 初期全部用 PassthroughFactory 占位。
// Phase 7 逐个替换为真正的命令工厂。
// Phase 8 补全 DSP 工厂。
// ══════════════════════════════════════════════════════════════════════════════

/// 创建默认工厂列表（15 个，按优先级排序）。
///
/// Phase 3 初期全部用 `PassthroughFactory` 占位。
/// 后续阶段逐个替换：
///
/// | 索引 | Phase | 工厂 |
/// |------|-------|------|
/// | 0    | 7     | `cmd_device::DeviceFactory` |
/// | 1    | 7     | `cmd_cond::IfFactory` |
/// | 2    | 7     | `cmd_expr::EvalFactory` |
/// | 3    | 7     | `cmd_include::IncludeFactory` |
/// | 4    | 7     | `cmd_stage::StageFactory` |
/// | 5    | 7     | `cmd_channel::ChannelFactory` |
/// | 6    | 8     | DSP IIR factory |
/// | 7    | 8     | DSP BiQuad factory |
/// | 8    | 8     | DSP Preamp factory |
/// | 9    | 8     | DSP Delay factory |
/// | 10   | 8     | DSP Copy factory |
/// | 11   | 8     | DSP Convolution factory |
/// | 12   | 8     | DSP GraphicEQ factory |
/// | 13   | 8     | DSP VSTPlugin factory |
/// | 14   | 8     | DSP LoudnessCorrection factory |
pub fn create_default_registry() -> Vec<Box<dyn FilterFactory>> {
    // Phase 3：全部占位
    (0..FACTORY_COUNT)
        .map(|_| Box::new(PassthroughFactory) as Box<dyn FilterFactory>)
        .collect()
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试用 Mock 工厂
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
/// 测试用工厂：按关键词匹配，返回预设结果。
pub struct MockFactory {
    pub keyword: String,
    pub outcome: fn(&str) -> FilterCreateResult,
}

#[cfg(test)]
impl MockFactory {
    pub fn new(keyword: &str, outcome: fn(&str) -> FilterCreateResult) -> Self {
        Self {
            keyword: keyword.to_owned(),
            outcome,
        }
    }
}

#[cfg(test)]
impl FilterFactory for MockFactory {
    fn create_filter(
        &self,
        params: &str,
        _ctx: &EngineContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        if params.starts_with(&self.keyword) {
            (self.outcome)(params)
        } else {
            FilterCreateResult::NoMatch
        }
    }

    fn command_name(&self) -> &str {
        &self.keyword
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::filter::{DeviceType, PassthroughFilter, ProcessingStage};

    fn test_ctx() -> EngineContext {
        EngineContext {
            sample_rate: 48000,
            channel_count: 2,
            channel_mask: 0x3,
            max_frame_count: 480,
            bits_per_sample: 32,
            device_type: DeviceType::Render,
            stage: ProcessingStage::None,
        }
    }

    struct NullLoader;
    impl ConfigLoader for NullLoader {
        fn load_config(&self, _path: &str, _ctx: &EngineContext) -> Vec<Box<dyn Filter>> {
            vec![]
        }
    }

    // ── Registry 基础 ───────────────────────────────────────────────────────

    #[test]
    fn default_registry_has_15_factories() {
        let reg = Registry::default_registry();
        assert_eq!(reg.len(), FACTORY_COUNT);
        assert!(!reg.is_empty());
    }

    #[test]
    fn default_registry_factory_names() {
        let reg = Registry::default_registry();
        let names = reg.factory_names();
        assert_eq!(names.len(), FACTORY_COUNT);
        // Phase 3 全部占位，都是 "*"
        assert!(names.iter().all(|n| *n == "*"));
    }

    #[test]
    fn registry_get_by_index() {
        let reg = Registry::default_registry();
        assert!(reg.get(0).is_some());
        assert!(reg.get(14).is_some());
        assert!(reg.get(15).is_none());
    }

    #[test]
    fn registry_command_name_at() {
        let reg = Registry::default_registry();
        assert_eq!(reg.command_name_at(0), Some("*"));
        assert_eq!(reg.command_name_at(99), None);
    }

    // ── try_create — 占位阶段（全部 PassthroughFactory）──────────────────────

    #[test]
    fn try_create_all_passthrough_returns_nofilter() {
        let reg = Registry::default_registry();
        let ctx = test_ctx();
        let outcome = reg.try_create("anything", &ctx, &NullLoader);

        // PassthroughFactory 匹配一切返回 NoFilter
        assert!(matches!(outcome.result, OutcomeKind::MatchedNoFilter));
        assert_eq!(outcome.factory_index, Some(0));
    }

    #[test]
    fn try_create_nofilter_stops_at_first_factory() {
        let reg = Registry::default_registry();
        let ctx = test_ctx();
        let outcome = reg.try_create("Preamp: 6", &ctx, &NullLoader);

        // 第一个工厂（index 0）就返回了 NoFilter，不会走到后面的工厂
        assert_eq!(outcome.factory_index, Some(0));
    }

    // ── try_create — Mock 工厂测试 ──────────────────────────────────────────

    #[test]
    fn try_create_with_mock_filter() {
        let factories: Vec<Box<dyn FilterFactory>> = vec![
            Box::new(MockFactory::new("Device:", |_| FilterCreateResult::NoMatch)),
            Box::new(MockFactory::new("Preamp:", |params| {
                FilterCreateResult::Filter(Box::new(PassthroughFilter))
            })),
            Box::new(PassthroughFactory),
        ];
        let reg = Registry::new(factories);
        let ctx = test_ctx();

        let outcome = reg.try_create("Preamp: 6", &ctx, &NullLoader);
        assert!(matches!(outcome.result, OutcomeKind::FilterAdded(_)));
        assert_eq!(outcome.factory_index, Some(1));
        assert_eq!(outcome.factory_name.as_deref(), Some("Preamp:"));
    }

    #[test]
    fn try_create_skips_nomatch() {
        let factories: Vec<Box<dyn FilterFactory>> = vec![
            Box::new(MockFactory::new("Device:", |_| FilterCreateResult::NoMatch)),
            Box::new(MockFactory::new("Channel:", |_| FilterCreateResult::NoMatch)),
            Box::new(MockFactory::new("Preamp:", |_| {
                FilterCreateResult::NoFilter
            })),
        ];
        let reg = Registry::new(factories);
        let ctx = test_ctx();

        let outcome = reg.try_create("Preamp: 6", &ctx, &NullLoader);
        assert!(matches!(outcome.result, OutcomeKind::MatchedNoFilter));
        assert_eq!(outcome.factory_index, Some(2));
    }

    #[test]
    fn try_create_abort_stops() {
        let factories: Vec<Box<dyn FilterFactory>> = vec![
            Box::new(MockFactory::new("Device:", |_| FilterCreateResult::AbortFile)),
            Box::new(PassthroughFactory),
        ];
        let reg = Registry::new(factories);
        let ctx = test_ctx();

        let outcome = reg.try_create("Device: Headphones", &ctx, &NullLoader);
        assert!(matches!(outcome.result, OutcomeKind::Aborted));
        assert_eq!(outcome.factory_index, Some(0));
    }

    #[test]
    fn try_create_all_nomatch_returns_unmatched() {
        let factories: Vec<Box<dyn FilterFactory>> = vec![
            Box::new(MockFactory::new("A:", |_| FilterCreateResult::NoMatch)),
            Box::new(MockFactory::new("B:", |_| FilterCreateResult::NoMatch)),
        ];
        let reg = Registry::new(factories);
        let ctx = test_ctx();

        let outcome = reg.try_create("unknown command", &ctx, &NullLoader);
        assert!(matches!(outcome.result, OutcomeKind::Unmatched));
        assert!(outcome.factory_index.is_none());
        assert!(outcome.factory_name.is_none());
    }

    // ── 优先级测试 ──────────────────────────────────────────────────────────

    #[test]
    fn first_matching_factory_wins() {
        // 两个工厂都匹配 "X:"，但第一个胜出
        let factories: Vec<Box<dyn FilterFactory>> = vec![
            Box::new(MockFactory::new("X:", |_| FilterCreateResult::NoFilter)),
            Box::new(MockFactory::new("X:", |_| {
                FilterCreateResult::Filter(Box::new(PassthroughFilter))
            })),
        ];
        let reg = Registry::new(factories);
        let ctx = test_ctx();

        let outcome = reg.try_create("X: value", &ctx, &NullLoader);
        // 第一个返回 NoFilter → 胜出，不会到第二个
        assert!(matches!(outcome.result, OutcomeKind::MatchedNoFilter));
        assert_eq!(outcome.factory_index, Some(0));
    }

    #[test]
    fn empty_registry_returns_unmatched() {
        let reg = Registry::new(vec![]);
        let ctx = test_ctx();

        let outcome = reg.try_create("anything", &ctx, &NullLoader);
        assert!(matches!(outcome.result, OutcomeKind::Unmatched));
    }

    // ── 工厂索引常量 ────────────────────────────────────────────────────────

    #[test]
    fn factory_index_constants() {
        assert_eq!(index::DEVICE, 0);
        assert_eq!(index::IF, 1);
        assert_eq!(index::EVAL, 2);
        assert_eq!(index::INCLUDE, 3);
        assert_eq!(index::STAGE, 4);
        assert_eq!(index::CHANNEL, 5);
        assert_eq!(index::IIR, 6);
        assert_eq!(index::BIQUAD, 7);
        assert_eq!(index::PREAMP, 8);
        assert_eq!(index::DELAY, 9);
        assert_eq!(index::COPY, 10);
        assert_eq!(index::CONVOLUTION, 11);
        assert_eq!(index::GRAPHIC_EQ, 12);
        assert_eq!(index::VST_PLUGIN, 13);
        assert_eq!(index::LOUDNESS_CORRECTION, 14);
    }

    #[test]
    fn factory_index_count_matches() {
        assert_eq!(FACTORY_COUNT, 15);
    }
}