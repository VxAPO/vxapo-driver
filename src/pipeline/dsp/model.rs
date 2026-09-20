//! pipeline/dsp/model.rs — DSP 层配置模型（ChainModel）
//!
//! config 层把 TOML 反序列化的 FileModel 转换到这里（丢弃 `name` / `group` /
//! `meta` 等 APP 元数据，校验范围/段数/声道名），`factory` 按此构造 Filter。
//! 依赖方向：config → dsp，dsp 层不引用 config。
//!
//! 各效果器的参数类型随其实现文件（gain / loudness / peq_hybrid / aural /
//! reverb / compressor / wide），本模块只做聚合 re-export，外部路径
//! `dsp::model::*Params` 保持不变。

pub use crate::pipeline::dsp::aural::AuralParams;
pub use crate::pipeline::dsp::compressor::CompressorParams;
pub use crate::pipeline::dsp::gain::PreampParams;
pub use crate::pipeline::dsp::loudness::LoudnessParams;
pub use crate::pipeline::dsp::peq_hybrid::{PeqBand, PeqBandType, PeqParams};
pub use crate::pipeline::dsp::reverb::ReverbParams;
pub use crate::pipeline::dsp::wide::WideParams;

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
    Compressor,
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
            // 旧名 maximizer / leveler 兼容映射为压缩器。
            "compressor" | "leveler" | "maximizer" => Some(Self::Compressor),
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
            Self::Compressor => "compressor",
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
                "room={:.6};decay={:.6};damp={:.6};bw={:.6};dens={:.6};lat5={:.6};lat6={:.6};pd={:.6};mr={:.6};md={:.6};lc={:.1};wet={:.6};dry={:.6}",
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
                p.low_cut_hz,
                p.wet,
                p.dry
            )),
            EffectParams::Compressor(p) => s.push_str(&format!(
                "th={:.6};ratio={:.6};knee={:.6};att={:.6};rel={:.6};mg={:.6};wet={:.6};dry={:.6}",
                p.threshold_db, p.ratio, p.knee_db, p.attack_ms, p.release_ms, p.makeup_gain_db, p.wet, p.dry
            )),
            EffectParams::Wide(p) => s.push_str(&format!(
                "gain={:.6};air={:.6};air_side={:.6};mix={:.6};xover={:.1}",
                p.gain, p.air, p.air_side, p.mix, p.crossover_hz
            )),
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
    Compressor(CompressorParams),
    Wide(WideParams),
    Loudness(LoudnessParams),
}
