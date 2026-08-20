//! pipeline/dsp/model.rs — DSP 层配置模型（ChainModel）
//!
//! config 层把 TOML 反序列化的 FileModel 转换到这里（丢弃 `name` / `group` /
//! `meta` 等 APP 元数据，校验范围/段数/声道名），`factory` 按此构造 Filter。
//! 依赖方向：config → dsp，dsp 层不引用 config。

use crate::pipeline::dsp::aural::AuralParams;
use crate::pipeline::dsp::maximizer::MaximizerParams;
use crate::pipeline::dsp::reverb::ReverbParams;
use crate::pipeline::dsp::wide::WideParams;

/// 分频点（Hz）：`Fc < CROSSOVER_HZ` 归 IIR，`Fc >= CROSSOVER_HZ` 归 FIR。
pub const CROSSOVER_HZ: f32 = 200.0;
/// PEQ 单块最小段数（UI 卡片模型允许 1 段卡 / 无组裸 band，见 UI 设计规范 01）。
pub const MIN_PEQ_BANDS: usize = 1;
/// PEQ 单块最大段数（沿用 GraphicEQ 上限；跨块全局合计 ≤ MAX，config 层校验）。
pub const MAX_PEQ_BANDS: usize = 31;

/// 完整 DSP 链模型（有序效果器列表）。
#[derive(Debug, Clone)]
pub struct ChainModel {
    pub effects: Vec<EffectConfig>,
}

/// 单个效果器配置（DSP 语义，无 APP 元数据）。
#[derive(Debug, Clone)]
pub struct EffectConfig {
    pub kind: EffectType,
    pub enabled: bool,
    /// 作用声道（None = 全部通道）。
    pub channels: Option<Vec<String>>,
    pub params: EffectParams,
}

/// 效果器类型（保留集）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectType {
    Peq,
    Preamp,
    Aural,
    Reverb,
    Maximizer,
    Wide,
    Loudness,
}

impl EffectType {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "peq" => Some(Self::Peq),
            "preamp" => Some(Self::Preamp),
            "aural" | "auralenhancer" => Some(Self::Aural),
            "reverb" => Some(Self::Reverb),
            "maximizer" => Some(Self::Maximizer),
            "wide" => Some(Self::Wide),
            "loudness" | "loudnesscorrection" => Some(Self::Loudness),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Peq => "peq",
            Self::Preamp => "preamp",
            Self::Aural => "aural",
            Self::Reverb => "reverb",
            Self::Maximizer => "maximizer",
            Self::Wide => "wide",
            Self::Loudness => "loudness",
        }
    }
}

impl EffectConfig {
    /// 稳定配置指纹（热重载比较用；`name`/`group` 不参与）。
    pub fn spec(&self) -> String {
        let mut s = format!("{}:{}", self.kind.as_str(), self.enabled);
        if let Some(ch) = &self.channels {
            s.push(':');
            s.push_str(&ch.join(","));
        }
        s.push('|');
        match &self.params {
            EffectParams::Peq(p) => {
                s.push_str(&format!("crossover={:.6}", p.crossover_hz));
                for b in &p.bands {
                    s.push_str(&format!(
                        ";{}:{:.6},{:.6},{:.6}",
                        b.kind.as_str(),
                        b.fc,
                        b.gain_db,
                        b.q
                    ));
                }
            }
            EffectParams::Preamp(p) => s.push_str(&format!("gain={:.6}", p.gain_db)),
            EffectParams::Aural(p) => s.push_str(&format!(
                "tune={:.6};drive={:.6};odd={:.6};even={:.6};wet={:.6};dry={:.6}",
                p.tune_hz, p.drive, p.odd, p.even, p.wet, p.dry
            )),
            EffectParams::Reverb(p) => s.push_str(&format!(
                "room={:.6};decay={:.6};damp={:.6};bw={:.6};dens={:.6};lat5={:.6};lat6={:.6};pd={:.6};mr={:.6};md={:.6};wet={:.6};dry={:.6}",
                p.room_size,
                p.decay,
                p.damping,
                p.bandwidth,
                p.density,
                p.lat5,
                p.lat6,
                p.pre_delay_ms,
                p.motion_rate,
                p.motion_depth,
                p.wet,
                p.dry
            )),
            EffectParams::Maximizer(p) => s.push_str(&format!(
                "gb={:.6};mo={:.6};rel={:.6};tgt={:.6};la={:.6};dith={:?};wet={:.6};dry={:.6}",
                p.gain_boost_db, p.max_output_db, p.release_ms, p.target, p.lookahead_ms, p.dither, p.wet, p.dry
            )),
            EffectParams::Wide(p) => s.push_str(&format!("intensity={:.6}", p.intensity)),
            EffectParams::Loudness(p) => s.push_str(&format!(
                "phon={:.6};ref={:.6}",
                p.phon, p.reference_phon
            )),
        }
        s
    }
}

/// 效果器参数（按类型携带）。
#[derive(Debug, Clone)]
pub enum EffectParams {
    Peq(PeqParams),
    Preamp(PreampParams),
    Aural(AuralParams),
    Reverb(ReverbParams),
    Maximizer(MaximizerParams),
    Wide(WideParams),
    Loudness(LoudnessParams),
}

/// 混合式 PEQ 参数。
#[derive(Debug, Clone)]
pub struct PeqParams {
    pub crossover_hz: f32,
    pub bands: Vec<PeqBand>,
}

/// PEQ 段滤波器类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PeqBandType {
    #[default]
    Peaking,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
}

impl PeqBandType {
    /// TOML 字符串 → 类型（缺省/未知回 `Peaking` 由 config 层决定是否报错）。
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "peaking" | "peak" | "peq" => Some(Self::Peaking),
            "low_shelf" | "lowshelf" | "low-shelf" => Some(Self::LowShelf),
            "high_shelf" | "highshelf" | "high-shelf" => Some(Self::HighShelf),
            "low_pass" | "lowpass" | "low-pass" => Some(Self::LowPass),
            "high_pass" | "highpass" | "high-pass" => Some(Self::HighPass),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Peaking => "peaking",
            Self::LowShelf => "low_shelf",
            Self::HighShelf => "high_shelf",
            Self::LowPass => "low_pass",
            Self::HighPass => "high_pass",
        }
    }
}

/// 单段滤波器（默认 peaking）。
#[derive(Debug, Clone, Copy)]
pub struct PeqBand {
    pub fc: f32,
    pub gain_db: f32,
    pub q: f32,
    pub kind: PeqBandType,
}

/// 全局增益。
#[derive(Debug, Clone, Copy)]
pub struct PreampParams {
    pub gain_db: f32,
}

/// 等响校正。
#[derive(Debug, Clone, Copy)]
pub struct LoudnessParams {
    pub phon: f32,
    pub reference_phon: f32,
}
