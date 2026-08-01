//! pipeline/dsp/factory.rs — FilterRegistry + 工厂注册 + 工厂遍历匹配（v6.3 规范 4.10）
//!
//! 职责：注册表持有按优先级排序的工厂列表，按遍历顺序尝试创建过滤器。
//! 同时定义内置 DSP 工厂（IIR/Biquad/Preamp/Delay/Copy/...）与 `register_builtin_filters`。
//!
//! 引用来源：
//! - `crate::pipeline::dsp::filter::*`
//! - `crate::utils::vx_error::VxApoError`
//!
//! 导出给：`config/commands/*.rs`、`config/parser.rs`。

use crate::pipeline::dsp::biquad::{BiquadCoeffs, BiquadFilter, BiquadStructure, BiquadType, compute_coeffs};
use crate::pipeline::dsp::convolution::{parse_convolution_params, ConvolutionFilter};
use crate::pipeline::dsp::copy::{parse_copy_ops, CopyFilter};
use crate::pipeline::dsp::delay::DelayFilter;
use crate::pipeline::dsp::filter::{
    ConfigLoader, DspContext, Filter, FilterCreateResult, FilterFactory,
};
use crate::pipeline::dsp::graphic_eq::{parse_graphic_eq_params, GraphicEqFilter};
use crate::pipeline::dsp::hp_lp::HighLowPassFilter;
use crate::pipeline::dsp::loudness::{parse_loudness_params, LoudnessFilter};
use crate::pipeline::dsp::peq::PeakingFilter;

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
// 工厂索引常量（v6.3 规范 4.10）
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

/// `IIR:` 命令工厂 → 参数化滤波器（PK/LP/HP/LS/HS/AP/NO）。
///
/// 语法（Filter/REW 传入的已是 `TYPE 参数...` 形式）：
/// - `PK Fc 1000 Hz Gain +3.0 dB Q 1.0`
/// - `LP Fc 1000 Hz Q 0.707`
///
/// 类型映射：
/// - PK → `PeakingFilter`
/// - LP/HP → `HighLowPassFilter`
/// - LS/HS/AP/NO → `compute_coeffs` + `BiquadFilter`（临时封装）
/// - Modal → `NoMatch`（无 DSP 实现，留作未来扩展）
#[derive(Debug)]
pub struct IirFactory;

/// 解析 `Fc <val> Hz` / `Gain <val> dB` / `Q <val>` 键值流。
///
/// 返回 `(fc, gain_db, q)`。任一必需参数缺失或非法时返回 `None`。
fn parse_iir_params(tokens: &[&str]) -> Option<(f32, f32, f32)> {
    let mut fc: Option<f32> = None;
    let mut gain_db: f32 = 0.0;
    let mut q: Option<f32> = None;

    let mut i = 0;
    while i < tokens.len() {
        match tokens[i].to_ascii_uppercase().as_str() {
            "FC" => {
                let v = tokens.get(i + 1)?.parse::<f32>().ok()?;
                fc = Some(v);
                i += 2;
                // 跳过可选的 "Hz" 单位
                if tokens.get(i).is_some_and(|t| t.eq_ignore_ascii_case("hz")) {
                    i += 1;
                }
            }
            "GAIN" => {
                gain_db = tokens.get(i + 1)?.parse::<f32>().ok()?;
                i += 2;
                // 跳过可选的 "dB" 单位
                if tokens.get(i).is_some_and(|t| t.eq_ignore_ascii_case("db")) {
                    i += 1;
                }
            }
            "Q" => {
                let v = tokens.get(i + 1)?.parse::<f32>().ok()?;
                q = Some(v);
                i += 2;
            }
            _ => return None,
        }
    }

    let fc = fc?;
    let q = q?;
    if !fc.is_finite() || fc <= 0.0 || !q.is_finite() || q <= 0.0 || !gain_db.is_finite() {
        return None;
    }
    Some((fc, gain_db, q))
}

impl FilterFactory for IirFactory {
    fn create_filter(
        &self,
        params: &str,
        ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        let tokens: Vec<&str> = params.split_whitespace().collect();
        if tokens.is_empty() {
            return FilterCreateResult::NoMatch;
        }

        let ftype = match tokens[0].to_ascii_uppercase().as_str() {
            "PK" | "PEAK" | "PEAKING" => BiquadType::Peaking,
            "LP" | "LOWPASS" => BiquadType::LowPass,
            "HP" | "HIGHPASS" => BiquadType::HighPass,
            "LS" | "LOWSHELF" => BiquadType::LowShelf,
            "HS" | "HIGHSHELF" => BiquadType::HighShelf,
            "AP" | "ALLPASS" => BiquadType::AllPass,
            "NO" | "NOTCH" => BiquadType::Notch,
            // Modal：无 DSP 实现，留作未来扩展（不报错，继续尝试下一工厂）。
            "MODAL" => return FilterCreateResult::NoMatch,
            _ => return FilterCreateResult::NoMatch,
        };

        let (fc, gain_db, q) = match parse_iir_params(&tokens[1..]) {
            Some(v) => v,
            None => return FilterCreateResult::NoMatch,
        };

        let filter: Box<dyn Filter> = match ftype {
            BiquadType::Peaking => Box::new(PeakingFilter::new(fc, gain_db, q)),
            BiquadType::LowPass | BiquadType::HighPass => {
                Box::new(HighLowPassFilter::new(ftype, fc, q))
            }
            // LS/HS/AP/NO：直接由系数构造 BiquadFilter（无需专用 Filter 类型）。
            _ => {
                let coeffs = compute_coeffs(ftype, fc, gain_db, q, ctx.sample_rate);
                Box::new(BiquadFilter::new(
                    coeffs,
                    BiquadStructure::DirectFormIITransposed,
                ))
            }
        };

        FilterCreateResult::Filter(filter)
    }

    fn command_name(&self) -> &str {
        "IIR"
    }
}

/// `Biquad:` 命令工厂 → 原始系数输入（调试/自定义滤波器底层接口）。
///
/// 语法：`Biquad: b0 b1 b2 a1 a2`
/// 与 `IirFactory`（参数化）用途不同：直接暴露系数。
#[derive(Debug)]
pub struct BiquadFactory;

impl FilterFactory for BiquadFactory {
    fn create_filter(
        &self,
        params: &str,
        _ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        let tokens: Vec<&str> = params.split_whitespace().collect();
        if tokens.len() != 5 {
            return FilterCreateResult::NoMatch;
        }

        let b0 = match tokens[0].parse::<f32>() {
            Ok(v) if v.is_finite() => v,
            _ => return FilterCreateResult::NoMatch,
        };
        let b1 = match tokens[1].parse::<f32>() {
            Ok(v) if v.is_finite() => v,
            _ => return FilterCreateResult::NoMatch,
        };
        let b2 = match tokens[2].parse::<f32>() {
            Ok(v) if v.is_finite() => v,
            _ => return FilterCreateResult::NoMatch,
        };
        let a1 = match tokens[3].parse::<f32>() {
            Ok(v) if v.is_finite() => v,
            _ => return FilterCreateResult::NoMatch,
        };
        let a2 = match tokens[4].parse::<f32>() {
            Ok(v) if v.is_finite() => v,
            _ => return FilterCreateResult::NoMatch,
        };

        let coeffs = BiquadCoeffs { b0, b1, b2, a1, a2 };
        FilterCreateResult::Filter(Box::new(BiquadFilter::new(
            coeffs,
            BiquadStructure::DirectFormIITransposed,
        )))
    }

    fn command_name(&self) -> &str {
        "Biquad"
    }
}

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
            Ok(db) if db.is_finite() => {
                FilterCreateResult::Filter(Box::new(crate::pipeline::dsp::gain::GainFilter::new(db)))
            }
            _ => FilterCreateResult::NoMatch,
        }
    }

    fn command_name(&self) -> &str {
        "Preamp"
    }
}

/// `Delay:` 命令工厂 → DelayFilter。
///
/// 语法：`Delay: 500 ms`（可选 "ms" 后缀，无需后缀的纯数字也接受）。
#[derive(Debug)]
pub struct DelayFactory;

impl FilterFactory for DelayFactory {
    fn create_filter(
        &self,
        params: &str,
        _ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        let trimmed = params.trim();
        // 移除可选的 "ms" 后缀。
        let num = trimmed
            .strip_suffix("ms")
            .map(str::trim)
            .unwrap_or(trimmed);
        match num.parse::<f32>() {
            Ok(ms) if ms.is_finite() => FilterCreateResult::Filter(Box::new(DelayFilter::new(ms))),
            _ => FilterCreateResult::NoMatch,
        }
    }

    fn command_name(&self) -> &str {
        "Delay"
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

/// `Convolution:` 命令工厂 → ConvolutionFilter。
///
/// 语法：`Convolution: ir.wav -6`（路径 + 可选增益 dB）。
#[derive(Debug)]
pub struct ConvolutionFactory;

impl FilterFactory for ConvolutionFactory {
    fn create_filter(
        &self,
        params: &str,
        _ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        match parse_convolution_params(params) {
            Some((path, gain_db)) => {
                FilterCreateResult::Filter(Box::new(ConvolutionFilter::new(&path, gain_db)))
            }
            None => FilterCreateResult::NoMatch,
        }
    }

    fn command_name(&self) -> &str {
        "Convolution"
    }
}

/// `GraphicEQ:` 命令工厂 → GraphicEqFilter。
///
/// 语法：`GraphicEQ: 25 0; 40 -3; 63 6; ...`
#[derive(Debug)]
pub struct GraphicEqFactory;

impl FilterFactory for GraphicEqFactory {
    fn create_filter(
        &self,
        params: &str,
        _ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        match parse_graphic_eq_params(params) {
            Some(bands) => FilterCreateResult::Filter(Box::new(GraphicEqFilter::new(bands))),
            None => FilterCreateResult::NoMatch,
        }
    }

    fn command_name(&self) -> &str {
        "GraphicEQ"
    }
}

/// `VSTPlugin:` 命令工厂（预留入口）。
///
/// 当前行为：**恒返回 NoMatch**（VST 插件加载暂不需要，见 `dsp/vst.rs` 注释）。
/// 未来如需 VST2/VST3 支持，在此处恢复参数解析并创建对应 Filter。
///
/// 语法（预留）：`VSTPlugin: "plugin_name" "path/to/plugin.dll" [param=value ...]`
#[derive(Debug)]
pub struct VstFactory;

impl FilterFactory for VstFactory {
    fn create_filter(
        &self,
        _params: &str,
        _ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        // 预留：VST 不支持，静默跳过（NoMatch → 继续尝试下一工厂 / 最终 Unmatched）。
        FilterCreateResult::NoMatch
    }

    fn command_name(&self) -> &str {
        "VSTPlugin"
    }
}

/// `LoudnessCorrection:` 命令工厂 → LoudnessFilter。
///
/// 语法：`LoudnessCorrection: 40 [80]`（phon [reference_phon]）。
#[derive(Debug)]
pub struct LoudnessFactory;

impl FilterFactory for LoudnessFactory {
    fn create_filter(
        &self,
        params: &str,
        _ctx: &DspContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        match parse_loudness_params(params) {
            Some((phon, reference_phon)) => {
                FilterCreateResult::Filter(Box::new(LoudnessFilter::new(phon, reference_phon)))
            }
            None => FilterCreateResult::NoMatch,
        }
    }

    fn command_name(&self) -> &str {
        "LoudnessCorrection"
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// register_builtin_filters
// ══════════════════════════════════════════════════════════════════════════════

/// 注册所有内置 Filter 工厂到 FilterRegistry（v6.3 规范 4.10）。
///
/// 注册顺序与 `index` 常量保持一致（优先级从高到低）：
/// IIR → BIQUAD → PREAMP → DELAY → COPY → CONVOLUTION → GRAPHIC_EQ → VST_PLUGIN → LOUDNESS_CORRECTION
pub fn register_builtin_filters(registry: &mut FilterRegistry) {
    registry.register(Box::new(IirFactory));
    registry.register(Box::new(BiquadFactory));
    registry.register(Box::new(PreampFactory));
    registry.register(Box::new(DelayFactory));
    registry.register(Box::new(CopyFactory));
    registry.register(Box::new(ConvolutionFactory));
    registry.register(Box::new(GraphicEqFactory));
    registry.register(Box::new(VstFactory));
    registry.register(Box::new(LoudnessFactory));
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

    // ── IIR 工厂 ────────────────────────────────────────────────────────────

    #[test]
    fn iir_parses_pk() {
        let factory = IirFactory;
        let result = factory.create_filter(
            "PK Fc 1000 Hz Gain +3.0 dB Q 1.0",
            &test_ctx(),
            &NullLoader,
        );
        assert!(matches!(result, FilterCreateResult::Filter(_)));
    }

    #[test]
    fn iir_parses_lp() {
        let factory = IirFactory;
        // LP 无 Gain：默认 0dB。
        let result = factory.create_filter("LP Fc 1000 Hz Q 0.707", &test_ctx(), &NullLoader);
        assert!(matches!(result, FilterCreateResult::Filter(_)));
    }

    #[test]
    fn iir_parses_hp() {
        let factory = IirFactory;
        let result = factory.create_filter("HP Fc 100 Hz Q 0.707", &test_ctx(), &NullLoader);
        assert!(matches!(result, FilterCreateResult::Filter(_)));
    }

    #[test]
    fn iir_parses_ls_hs() {
        let factory = IirFactory;
        let ls = factory.create_filter("LS Fc 200 Hz Gain 6.0 dB Q 0.707", &test_ctx(), &NullLoader);
        assert!(matches!(ls, FilterCreateResult::Filter(_)));
        let hs = factory.create_filter("HS Fc 5000 Hz Gain -3.0 dB Q 0.707", &test_ctx(), &NullLoader);
        assert!(matches!(hs, FilterCreateResult::Filter(_)));
    }

    #[test]
    fn iir_parses_ap_no() {
        let factory = IirFactory;
        let ap = factory.create_filter("AP Fc 1000 Hz Gain 0 dB Q 1.0", &test_ctx(), &NullLoader);
        assert!(matches!(ap, FilterCreateResult::Filter(_)));
        let no = factory.create_filter("NO Fc 1000 Hz Gain 0 dB Q 1.0", &test_ctx(), &NullLoader);
        assert!(matches!(no, FilterCreateResult::Filter(_)));
    }

    #[test]
    fn iir_modal_returns_no_match() {
        let factory = IirFactory;
        // Modal 无 DSP 实现 → 不报错，返回 NoMatch（留作未来扩展）。
        let result = factory.create_filter("Modal Fc 100 Hz Q 10", &test_ctx(), &NullLoader);
        assert!(matches!(result, FilterCreateResult::NoMatch));
    }

    #[test]
    fn iir_invalid_no_match() {
        let factory = IirFactory;
        let result = factory.create_filter("PK no params", &test_ctx(), &NullLoader);
        assert!(matches!(result, FilterCreateResult::NoMatch));
    }

    // ── Biquad 工厂 ─────────────────────────────────────────────────────────

    #[test]
    fn biquad_parses_coefficients() {
        let factory = BiquadFactory;
        let result = factory.create_filter("1.0 0.0 0.0 0.0 0.0", &test_ctx(), &NullLoader);
        assert!(matches!(result, FilterCreateResult::Filter(_)));
    }

    #[test]
    fn biquad_invalid_no_match() {
        let factory = BiquadFactory;
        // 非 5 个系数。
        assert!(matches!(
            factory.create_filter("1.0 0.0 0.0", &test_ctx(), &NullLoader),
            FilterCreateResult::NoMatch
        ));
        // 非法数值。
        assert!(matches!(
            factory.create_filter("1.0 0.0 0.0 0.0 abc", &test_ctx(), &NullLoader),
            FilterCreateResult::NoMatch
        ));
    }

    // ── Delay 工厂 ──────────────────────────────────────────────────────────

    #[test]
    fn delay_parses_ms() {
        let factory = DelayFactory;
        let result = factory.create_filter("500 ms", &test_ctx(), &NullLoader);
        assert!(matches!(result, FilterCreateResult::Filter(_)));
    }

    #[test]
    fn delay_parses_plain_number() {
        let factory = DelayFactory;
        let result = factory.create_filter("12.5", &test_ctx(), &NullLoader);
        assert!(matches!(result, FilterCreateResult::Filter(_)));
    }

    #[test]
    fn delay_invalid_no_match() {
        let factory = DelayFactory;
        let result = factory.create_filter("oops", &test_ctx(), &NullLoader);
        assert!(matches!(result, FilterCreateResult::NoMatch));
    }

    // ── GraphicEQ 工厂 ──────────────────────────────────────────────────────

    #[test]
    fn graphic_eq_parses_bands() {
        let factory = GraphicEqFactory;
        let result = factory.create_filter("25 0; 40 -3; 63 6", &test_ctx(), &NullLoader);
        assert!(matches!(result, FilterCreateResult::Filter(_)));
    }

    #[test]
    fn graphic_eq_invalid_no_match() {
        let factory = GraphicEqFactory;
        let result = factory.create_filter("", &test_ctx(), &NullLoader);
        assert!(matches!(result, FilterCreateResult::NoMatch));
    }

    // ── Convolution 工厂 ────────────────────────────────────────────────────

    #[test]
    fn convolution_parses_path() {
        let factory = ConvolutionFactory;
        let result = factory.create_filter("ir.wav", &test_ctx(), &NullLoader);
        assert!(matches!(result, FilterCreateResult::Filter(_)));
    }

    #[test]
    fn convolution_parses_gain() {
        let factory = ConvolutionFactory;
        let result = factory.create_filter("ir.wav -6", &test_ctx(), &NullLoader);
        assert!(matches!(result, FilterCreateResult::Filter(_)));
    }

    #[test]
    fn convolution_invalid_no_match() {
        let factory = ConvolutionFactory;
        let result = factory.create_filter("", &test_ctx(), &NullLoader);
        assert!(matches!(result, FilterCreateResult::NoMatch));
    }

    // ── VST 工厂（预留 NoMatch，无测试） ──────────────────────────────────
    // 当前行为恒 NoMatch（见 VstFactory 注释）。未来实现 VST 后补测试。

    // ── Loudness 工厂 ───────────────────────────────────────────────────────

    #[test]
    fn loudness_parses_phon() {
        let factory = LoudnessFactory;
        let result = factory.create_filter("40", &test_ctx(), &NullLoader);
        assert!(matches!(result, FilterCreateResult::Filter(_)));
    }

    #[test]
    fn loudness_parses_reference() {
        let factory = LoudnessFactory;
        let result = factory.create_filter("60 80", &test_ctx(), &NullLoader);
        assert!(matches!(result, FilterCreateResult::Filter(_)));
    }

    #[test]
    fn loudness_invalid_no_match() {
        let factory = LoudnessFactory;
        let result = factory.create_filter("", &test_ctx(), &NullLoader);
        assert!(matches!(result, FilterCreateResult::NoMatch));
    }

    // ── Preamp / Copy 工厂 ──────────────────────────────────────────────────

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

    // ── 注册与遍历 ──────────────────────────────────────────────────────────

    #[test]
    fn register_registers_known_factories() {
        let mut registry = FilterRegistry::new();
        register_builtin_filters(&mut registry);
        let names = registry.factory_names();
        assert!(names.contains(&"IIR"));
        assert!(names.contains(&"Biquad"));
        assert!(names.contains(&"Preamp"));
        assert!(names.contains(&"Delay"));
        assert!(names.contains(&"Copy"));
        assert!(names.contains(&"Convolution"));
        assert!(names.contains(&"GraphicEQ"));
        assert!(names.contains(&"VSTPlugin"));
        assert!(names.contains(&"LoudnessCorrection"));
        assert_eq!(registry.len(), 9);
    }

    #[test]
    fn registration_order_matches_index() {
        let mut registry = FilterRegistry::new();
        register_builtin_filters(&mut registry);
        let names = registry.factory_names();

        // 与 index 常量顺序一致：IIR → BIQUAD → PREAMP → DELAY → COPY → CONVOLUTION → GRAPHIC_EQ → VST_PLUGIN → LOUDNESS_CORRECTION
        let expected = [
            index::IIR,
            index::BIQUAD,
            index::PREAMP,
            index::DELAY,
            index::COPY,
            index::CONVOLUTION,
            index::GRAPHIC_EQ,
            index::VST_PLUGIN,
            index::LOUDNESS_CORRECTION,
        ];

        for (i, &idx) in expected.iter().enumerate() {
            assert_eq!(
                names[i],
                factory_name_for_index(idx),
                "factory at registry position {i} should match index constant {idx}"
            );
        }
    }

    /// 根据 index 常量映射工厂命令名（测试用）。
    fn factory_name_for_index(idx: usize) -> &'static str {
        match idx {
            index::IIR => "IIR",
            index::BIQUAD => "Biquad",
            index::PREAMP => "Preamp",
            index::DELAY => "Delay",
            index::COPY => "Copy",
            index::CONVOLUTION => "Convolution",
            index::GRAPHIC_EQ => "GraphicEQ",
            index::VST_PLUGIN => "VSTPlugin",
            index::LOUDNESS_CORRECTION => "LoudnessCorrection",
            _ => "<unregistered>",
        }
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
    fn try_create_iir_dispatch() {
        let mut registry = FilterRegistry::new();
        register_builtin_filters(&mut registry);
        // "PK ..." 应命中 IirFactory（第一个注册的 DSP 工厂）。
        let outcome = registry.try_create(
            "PK Fc 1000 Hz Gain +3.0 dB Q 1.0",
            &test_ctx(),
            &NullLoader,
        );
        assert!(matches!(outcome.result, OutcomeKind::FilterAdded(_)));
        assert_eq!(outcome.factory_name.as_deref(), Some("IIR"));
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