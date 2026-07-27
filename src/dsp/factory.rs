//! dsp/factory.rs — 工厂注册机制（Note 14）
//!
//! 按硬编码顺序注册 15 个工厂。`host/parse/parser.rs` 按下标顺序遍历，
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

use crate::dsp::filter::{
    ConfigLoader, DspContext, Filter, FilterCreateResult, FilterFactory,
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
        ctx: &DspContext,
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
/// Index 0–5 为命令工厂（由 parser.rs 直接处理，此处占位）。
/// Index 6–14 为 DSP 工厂（Phase 8 实现）。
pub fn create_default_registry() -> Vec<Box<dyn FilterFactory>> {
    let mut factories: Vec<Box<dyn FilterFactory>> = Vec::with_capacity(FACTORY_COUNT);

    // Index 0–5：命令工厂（parser.rs 直接处理，此处 NoMatch 占位）
    for _ in 0..6 {
        factories.push(Box::new(NoMatchFactory));
    }

    // Index 6–14：DSP 工厂
    factories.push(Box::new(IirFactory));
    factories.push(Box::new(BiquadFactory));
    factories.push(Box::new(PreampFactory));
    factories.push(Box::new(DelayFactory));
    factories.push(Box::new(CopyFactory));
    factories.push(Box::new(ConvolutionFactory));
    factories.push(Box::new(GraphicEqFactory));
    factories.push(Box::new(VstPluginFactory));
    factories.push(Box::new(LoudnessCorrectionFactory));

    debug_assert_eq!(factories.len(), FACTORY_COUNT);
    factories
}

// ══════════════════════════════════════════════════════════════════════════════
// DSP 工厂实现（Phase 8）
//
// Index 6–14 的真实工厂，替换 PassthroughFactory 占位。
// ══════════════════════════════════════════════════════════════════════════════

use crate::dsp::filters::biquad::{self, BiquadType};
use crate::dsp::filters::gain::GainFilter;
use crate::dsp::filters::delay::DelayFilter;
use crate::dsp::filters::copy::{self, CopyFilter};
use crate::dsp::filters::convolution::{self, ConvolutionFilter};
use crate::dsp::filters::hp_lp::HighLowPassFilter;
use crate::dsp::filters::graph_eq::{self, GraphicEqFilter};
use crate::dsp::filters::loudness::{self, LoudnessFilter};
use crate::dsp::filters::vst::{self, VstFilter};

// ── IIR 工厂 (index 6) ───────────────────────────────────────────────────

/// IIR 滤波器工厂。
///
/// 匹配 `IIR, HP, ...` / `IIR, LP, ...` / `IIR, PK, ...` 等格式。
pub struct IirFactory;

impl FilterFactory for IirFactory {
    fn create_filter(
        &self,
        params: &str,
        ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        let upper = params.to_uppercase();
        if !upper.starts_with("IIR") {
            return FilterCreateResult::NoMatch;
        }

        // 解析 "IIR, TYPE, Fc=X, Gain=Y, Q=Z"
        let parts: Vec<&str> = params.splitn(2, ',').collect();
        if parts.len() < 2 {
            return FilterCreateResult::NoMatch;
        }
        let rest = parts[1].trim();

        // 提取类型
        let (type_str, rest) = if let Some(pos) = rest.find(',') {
            (&rest[..pos].trim(), rest[pos + 1..].trim())
        } else {
            return FilterCreateResult::NoMatch;
        };

        let filter_type = match type_str.to_uppercase().as_str() {
            "HP" => BiquadType::HighPass,
            "LP" => BiquadType::LowPass,
            "PK" | "PEQ" | "P" => BiquadType::Peaking,
            "LS" => BiquadType::LowShelf,
            "HS" => BiquadType::HighShelf,
            "BP" => BiquadType::BandPass,
            "NO" | "NOTCH" => BiquadType::Notch,
            "AP" => BiquadType::AllPass,
            _ => return FilterCreateResult::NoMatch,
        };

        // 解析键值对 "Fc=1000, Gain=3, Q=1.41"
        let mut fc = 1000.0f32;
        let mut gain = 0.0f32;
        let mut q = 0.707f32;

        for kv in rest.split(',') {
            let kv = kv.trim();
            let eq = match kv.find('=') {
                Some(p) => p,
                None => continue,
            };
            let key = kv[..eq].trim().to_uppercase();
            let val = kv[eq + 1..].trim();

            match key.as_str() {
                "FC" | "F" => fc = val.parse().unwrap_or(1000.0),
                "GAIN" | "G" => gain = val.parse().unwrap_or(0.0),
                "Q" => q = val.parse().unwrap_or(0.707),
                _ => {}
            }
        }

        if fc <= 0.0 || !fc.is_finite() {
            log::warn!("IIR: invalid Fc={}, skipping", fc);
            return FilterCreateResult::NoMatch;
        }

        match filter_type {
            BiquadType::HighPass | BiquadType::LowPass => {
                let filter = HighLowPassFilter::new(filter_type, fc, q);
                FilterCreateResult::Filter(Box::new(filter))
            }
            BiquadType::Peaking | BiquadType::LowShelf | BiquadType::HighShelf => {
                let coeffs = biquad::compute_coeffs(
                    filter_type, fc, gain, q, ctx.sample_rate,
                );
                let filter = biquad::BiquadFilter::new(
                    coeffs,
                    biquad::BiquadStructure::DirectFormIITransposed,
                );
                FilterCreateResult::Filter(Box::new(filter))
            }
            _ => {
                let coeffs = biquad::compute_coeffs(
                    filter_type, fc, gain, q, ctx.sample_rate,
                );
                let filter = biquad::BiquadFilter::new(
                    coeffs,
                    biquad::BiquadStructure::DirectFormIITransposed,
                );
                FilterCreateResult::Filter(Box::new(filter))
            }
        }
    }

    fn command_name(&self) -> &str {
        "IIR:"
    }
}

// ── Biquad 工厂 (index 7) ────────────────────────────────────────────────

/// Biquad 直接系数工厂。
///
/// 匹配 `Biquad: b0=X, b1=Y, b2=Z, a1=W, a2=V` 格式。
pub struct BiquadFactory;

impl FilterFactory for BiquadFactory {
    fn create_filter(
        &self,
        params: &str,
        _ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        if !params.to_uppercase().starts_with("BIQUAD") {
            return FilterCreateResult::NoMatch;
        }

        let rest = match params.find(',') {
            Some(p) => params[p + 1..].trim(),
            None => return FilterCreateResult::NoMatch,
        };

        let mut coeffs = biquad::BiquadCoeffs::BYPASS;
        for kv in rest.split(',') {
            let kv = kv.trim();
            let eq = match kv.find('=') {
                Some(p) => p,
                None => continue,
            };
            let key = kv[..eq].trim().to_uppercase();
            let val: f32 = match kv[eq + 1..].trim().parse() {
                Ok(v) => v,
                Err(_) => continue,
            };

            match key.as_str() {
                "B0" => coeffs.b0 = val,
                "B1" => coeffs.b1 = val,
                "B2" => coeffs.b2 = val,
                "A1" => coeffs.a1 = val,
                "A2" => coeffs.a2 = val,
                _ => {}
            }
        }

        if !coeffs.is_valid() {
            log::warn!("Biquad: invalid coefficients");
            return FilterCreateResult::NoMatch;
        }

        let filter = biquad::BiquadFilter::new(
            coeffs,
            biquad::BiquadStructure::DirectFormIITransposed,
        );
        FilterCreateResult::Filter(Box::new(filter))
    }

    fn command_name(&self) -> &str {
        "Biquad:"
    }
}

// ── Preamp 工厂 (index 8) ────────────────────────────────────────────────

/// 增益工厂。
///
/// 匹配 `Preamp: X dB` 或 `Preamp: X` 格式。
pub struct PreampFactory;

impl FilterFactory for PreampFactory {
    fn create_filter(
        &self,
        params: &str,
        _ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        let upper = params.to_uppercase();
        if !upper.starts_with("PREAMP") {
            return FilterCreateResult::NoMatch;
        }

        let rest = match params.find(':') {
            Some(p) => params[p + 1..].trim(),
            None => match params.find(' ') {
                Some(p) => params[p..].trim(),
                None => return FilterCreateResult::NoMatch,
            },
        };

        // 解析 "6 dB" 或 "-3" 或 "0.5 dB"
        let num_str = rest
            .split_whitespace()
            .next()
            .unwrap_or(rest);

        let gain_db: f32 = match num_str.parse() {
            Ok(v) => v,
            Err(_) => {
                log::warn!("Preamp: invalid value '{}'", rest);
                return FilterCreateResult::NoMatch;
            }
        };

        FilterCreateResult::Filter(Box::new(GainFilter::new(gain_db)))
    }

    fn command_name(&self) -> &str {
        "Preamp:"
    }
}

// ── Delay 工厂 (index 9) ─────────────────────────────────────────────────

/// 延迟线工厂。
///
/// 匹配 `Delay: X` 格式（毫秒）。
pub struct DelayFactory;

impl FilterFactory for DelayFactory {
    fn create_filter(
        &self,
        params: &str,
        _ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        if !params.to_uppercase().starts_with("DELAY") {
            return FilterCreateResult::NoMatch;
        }

        let rest = match params.find(':') {
            Some(p) => params[p + 1..].trim(),
            None => match params.find(' ') {
                Some(p) => params[p..].trim(),
                None => return FilterCreateResult::NoMatch,
            },
        };

        let delay_ms: f32 = match rest.split_whitespace().next().unwrap_or(rest).parse() {
            Ok(v) => v,
            Err(_) => {
                log::warn!("Delay: invalid value '{}'", rest);
                return FilterCreateResult::NoMatch;
            }
        };

        if delay_ms < 0.0 {
            log::warn!("Delay: negative value {} not supported", delay_ms);
            return FilterCreateResult::NoMatch;
        }

        FilterCreateResult::Filter(Box::new(DelayFilter::new(delay_ms)))
    }

    fn command_name(&self) -> &str {
        "Delay:"
    }
}

// ── Copy 工厂 (index 10) ─────────────────────────────────────────────────

/// 通道复制/混音工厂。
///
/// 匹配 `Copy: TARGET=SOURCE+...` 格式。
pub struct CopyFactory;

impl FilterFactory for CopyFactory {
    fn create_filter(
        &self,
        params: &str,
        ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        if !params.to_uppercase().starts_with("COPY") {
            return FilterCreateResult::NoMatch;
        }

        let rest = match params.find(':') {
            Some(p) => params[p + 1..].trim(),
            None => match params.find(' ') {
                Some(p) => params[p..].trim(),
                None => return FilterCreateResult::NoMatch,
            },
        };

        match copy::parse_copy_ops(rest, &ctx.channel_names) {
            Some(ops) => FilterCreateResult::Filter(Box::new(CopyFilter::new(ops))),
            None => {
                log::warn!("Copy: failed to parse '{}'", rest);
                FilterCreateResult::NoMatch
            }
        }
    }

    fn command_name(&self) -> &str {
        "Copy:"
    }
}

// ── Convolution 工厂 (index 11) ──────────────────────────────────────────

/// 卷积工厂。
///
/// 匹配 `Convolution: path [gain_dB]` 格式。
pub struct ConvolutionFactory;

impl FilterFactory for ConvolutionFactory {
    fn create_filter(
        &self,
        params: &str,
        _ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        if !params.to_uppercase().starts_with("CONVOLUTION") {
            return FilterCreateResult::NoMatch;
        }

        let rest = match params.find(':') {
            Some(p) => params[p + 1..].trim(),
            None => match params.find(' ') {
                Some(p) => params[p..].trim(),
                None => return FilterCreateResult::NoMatch,
            },
        };

        match convolution::parse_convolution_params(rest) {
            Some((path, gain)) => {
                FilterCreateResult::Filter(Box::new(ConvolutionFilter::new(&path, gain)))
            }
            None => {
                log::warn!("Convolution: failed to parse '{}'", rest);
                FilterCreateResult::NoMatch
            }
        }
    }

    fn command_name(&self) -> &str {
        "Convolution:"
    }
}

// ── GraphicEQ 工厂 (index 12) ────────────────────────────────────────────

/// 图形均衡器工厂。
///
/// 匹配 `GraphicEQ: freq1 gain1; freq2 gain2; ...` 格式。
pub struct GraphicEqFactory;

impl FilterFactory for GraphicEqFactory {
    fn create_filter(
        &self,
        params: &str,
        _ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        if !params.to_uppercase().starts_with("GRAPHICEQ") {
            return FilterCreateResult::NoMatch;
        }

        let rest = match params.find(':') {
            Some(p) => params[p + 1..].trim(),
            None => match params.find(' ') {
                Some(p) => params[p..].trim(),
                None => return FilterCreateResult::NoMatch,
            },
        };

        match graph_eq::parse_graphic_eq_params(rest) {
            Some(bands) => {
                FilterCreateResult::Filter(Box::new(GraphicEqFilter::new(bands)))
            }
            None => {
                log::warn!("GraphicEQ: failed to parse '{}'", rest);
                FilterCreateResult::NoMatch
            }
        }
    }

    fn command_name(&self) -> &str {
        "GraphicEQ:"
    }
}

// ── VSTPlugin 工厂 (index 13) ────────────────────────────────────────────

/// VST 插件工厂。
///
/// 匹配 `VSTPlugin: "name" "path.dll"` 格式。
pub struct VstPluginFactory;

impl FilterFactory for VstPluginFactory {
    fn create_filter(
        &self,
        params: &str,
        _ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        if !params.to_uppercase().starts_with("VSTPLUGIN") {
            return FilterCreateResult::NoMatch;
        }

        let rest = match params.find(':') {
            Some(p) => params[p + 1..].trim(),
            None => match params.find(' ') {
                Some(p) => params[p..].trim(),
                None => return FilterCreateResult::NoMatch,
            },
        };

        match vst::parse_vst_params(rest) {
            Some((name, path, _raw)) => {
                FilterCreateResult::Filter(Box::new(VstFilter::new(&path, &name)))
            }
            None => {
                log::warn!("VSTPlugin: failed to parse '{}'", rest);
                FilterCreateResult::NoMatch
            }
        }
    }

    fn command_name(&self) -> &str {
        "VSTPlugin:"
    }
}

// ── LoudnessCorrection 工厂 (index 14) ───────────────────────────────────

/// 等响曲线工厂。
///
/// 匹配 `LoudnessCorrection: phon [reference_phon]` 格式。
pub struct LoudnessCorrectionFactory;

impl FilterFactory for LoudnessCorrectionFactory {
    fn create_filter(
        &self,
        params: &str,
        _ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        if !params.to_uppercase().starts_with("LOUDNESSCORRECTION") {
            return FilterCreateResult::NoMatch;
        }

        let rest = match params.find(':') {
            Some(p) => params[p + 1..].trim(),
            None => match params.find(' ') {
                Some(p) => params[p..].trim(),
                None => return FilterCreateResult::NoMatch,
            },
        };

        match loudness::parse_loudness_params(rest) {
            Some((phon, ref_phon)) => {
                FilterCreateResult::Filter(Box::new(LoudnessFilter::new(phon, ref_phon)))
            }
            None => {
                log::warn!("LoudnessCorrection: failed to parse '{}'", rest);
                FilterCreateResult::NoMatch
            }
        }
    }

    fn command_name(&self) -> &str {
        "LoudnessCorrection:"
    }
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
        _ctx: &DspContext,
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

/// 占位工厂：永远匹配，返回 `NoFilter`（消费行但不产生过滤器）。
///
/// 用于测试中作为兜底/后备工厂。
#[derive(Debug)]
pub struct PassthroughFactory;

impl FilterFactory for PassthroughFactory {
    fn create_filter(
        &self,
        _params: &str,
        _ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        FilterCreateResult::NoFilter
    }

    fn command_name(&self) -> &str {
        "<passthrough>"
    }
}

/// 占位工厂：永不匹配。
///
/// 用于 index 0–5（命令工厂），这些命令由 parser.rs 直接处理，
/// 不通过 registry 匹配。
#[derive(Debug)]
pub struct NoMatchFactory;

impl FilterFactory for NoMatchFactory {
    fn create_filter(
        &self,
        _params: &str,
        _ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        FilterCreateResult::NoMatch
    }

    fn command_name(&self) -> &str {
        "<command>"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::filter::{DeviceType, PassthroughFilter, ProcessingStage};

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
        }
    }

    struct NullLoader;
    impl ConfigLoader for NullLoader {
        fn load_config(&self, _path: &str, _ctx: &DspContext) -> Vec<Box<dyn Filter>> {
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
        // Index 0–5 命令占位
        for i in 0..6 {
            assert_eq!(names[i], "<command>", "index {} should be NoMatch", i);
        }
        assert_eq!(names[6], "IIR:");
        assert_eq!(names[7], "Biquad:");
        assert_eq!(names[8], "Preamp:");
        assert_eq!(names[9], "Delay:");
        assert_eq!(names[10], "Copy:");
        assert_eq!(names[11], "Convolution:");
        assert_eq!(names[12], "GraphicEQ:");
        assert_eq!(names[13], "VSTPlugin:");
        assert_eq!(names[14], "LoudnessCorrection:");
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
        assert_eq!(reg.command_name_at(0), Some("<command>"));
        assert_eq!(reg.command_name_at(99), None);
    }

    // ── try_create — （Index 6–14）──────────────────────

    #[test]
    fn try_create_preamp() {
        let reg = Registry::default_registry();
        let ctx = test_ctx();
        let outcome = reg.try_create("Preamp: -6", &ctx, &NullLoader);
        assert!(matches!(outcome.result, OutcomeKind::FilterAdded(_)));
        assert_eq!(outcome.factory_index, Some(index::PREAMP));
    }

    #[test]
    fn try_create_delay() {
        let reg = Registry::default_registry();
        let ctx = test_ctx();
        let outcome = reg.try_create("Delay: 5", &ctx, &NullLoader);
        assert!(matches!(outcome.result, OutcomeKind::FilterAdded(_)));
        assert_eq!(outcome.factory_index, Some(index::DELAY));
    }

    #[test]
    fn try_create_iir_lp() {
        let reg = Registry::default_registry();
        let ctx = test_ctx();
        let outcome = reg.try_create("Iir, LP, Fc=1000, Q=0.707", &ctx, &NullLoader);
        assert!(matches!(outcome.result, OutcomeKind::FilterAdded(_)));
        assert_eq!(outcome.factory_index, Some(index::IIR));
    }

    #[test]
    fn try_create_graphic_eq() {
        let reg = Registry::default_registry();
        let ctx = test_ctx();
        let outcome = reg.try_create("GraphicEQ: 100 0; 1000 3; 10000 -3", &ctx, &NullLoader);
        assert!(matches!(outcome.result, OutcomeKind::FilterAdded(_)));
        assert_eq!(outcome.factory_index, Some(index::GRAPHIC_EQ));
    }

    #[test]
    fn try_create_unknown_returns_unmatched() {
        let reg = Registry::default_registry();
        let ctx = test_ctx();
        let outcome = reg.try_create("Foobar: something", &ctx, &NullLoader);
        // 命令工厂 NoMatch → DSP 工厂 NoMatch → Unmatched
        assert!(matches!(outcome.result, OutcomeKind::Unmatched));
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