//! config/model.rs — 文件格式模型（FileModel）
//!
//! TOML 反序列化目标：`version` / `[meta]` / `[[effects]]`（含 APP 元数据
//! `name` / `group`）。`into_chain_model()` 丢弃 APP 元数据并完成范围/段数/
//! 声道名校验，转换为 dsp 层 `ChainModel`——**转换是 config 层职责**，
//! 依赖方向保持 `config → pipeline/dsp`。

use std::collections::HashMap;

use serde::Deserialize;

use crate::config::error::ConfigError;
use crate::pipeline::dsp::aural::AuralParams;
use crate::pipeline::dsp::maximizer::{DitherType, MaximizerParams};
use crate::pipeline::dsp::model::{
    ChainModel, EffectConfig, EffectParams, EffectType, LoudnessParams, PeqBand, PeqBandType,
    PeqParams, PreampParams, MAX_PEQ_BANDS, MIN_PEQ_BANDS,
};
use crate::pipeline::dsp::reverb::ReverbParams;
use crate::pipeline::dsp::wide::WideParams;

/// 顶层 TOML 文件模型。
#[derive(Debug, Clone, Deserialize)]
pub struct FileModel {
    #[serde(default = "default_version")]
    pub version: u32,
    /// 总开关：`false` = 整链 passthrough，但文件内容保留、不参与校验。
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub meta: Option<Meta>,
    #[serde(default)]
    pub effects: Vec<FileEffect>,
}

fn default_version() -> u32 {
    1
}

/// APP 元数据（driver 忽略）。
#[derive(Debug, Clone, Deserialize)]
pub struct Meta {
    #[serde(default)]
    pub app: Option<String>,
    #[serde(default)]
    pub schema: Option<u32>,
}

/// 单个效果器文件表示（参数平铺；`name`/`group` 仅供 APP）。
#[derive(Debug, Clone, Deserialize)]
pub struct FileEffect {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub channels: Option<Vec<String>>,

    // —— 各效果器参数（按 type 取用；不适用字段视为错误）——
    #[serde(default)]
    pub gain_db: Option<f32>,
    #[serde(default)]
    pub crossover_hz: Option<f32>,
    #[serde(default)]
    pub depth: Option<f32>,
    #[serde(default)]
    pub air: Option<f32>,
    #[serde(default)]
    pub gain: Option<f32>,
    #[serde(default)]
    pub bands: Option<Vec<FilePeqBand>>,
    #[serde(default)]
    pub tune_hz: Option<f32>,
    #[serde(default)]
    pub drive: Option<f32>,
    #[serde(default)]
    pub odd: Option<f32>,
    #[serde(default)]
    pub even: Option<f32>,
    #[serde(default)]
    pub wet: Option<f32>,
    #[serde(default)]
    pub dry: Option<f32>,
    #[serde(default)]
    pub room_size: Option<f32>,
    #[serde(default)]
    pub decay: Option<f32>,
    #[serde(default)]
    pub damping: Option<f32>,
    #[serde(default)]
    pub bandwidth: Option<f32>,
    #[serde(default)]
    pub density: Option<f32>,
    #[serde(default)]
    pub lat5: Option<f32>,
    #[serde(default)]
    pub lat6: Option<f32>,
    #[serde(default)]
    pub pre_delay_ms: Option<f32>,
    #[serde(default)]
    pub motion_rate: Option<f32>,
    #[serde(default)]
    // 旧配置写的是 motion_depth_ms；该参数实际是归一化值（0..2，2=论文满调制），
    // 改名 motion_depth 并兼容旧键。
    #[serde(alias = "motion_depth_ms")]
    pub motion_depth: Option<f32>,
    #[serde(default)]
    pub gain_boost_db: Option<f32>,
    #[serde(default)]
    pub max_output_db: Option<f32>,
    #[serde(default)]
    pub release_ms: Option<f32>,
    #[serde(default)]
    pub target: Option<f32>,
    #[serde(default)]
    pub lookahead_ms: Option<f32>,
    #[serde(default)]
    pub dither: Option<String>,
    #[serde(default)]
    pub intensity: Option<f32>,
    #[serde(default)]
    pub phon: Option<f32>,
    #[serde(default)]
    pub reference_phon: Option<f32>,

    /// 未声明键（严格校验用）。
    #[serde(flatten)]
    pub extra: HashMap<String, toml::Value>,
}

fn default_true() -> bool {
    true
}

/// PEQ 段（TOML `[[effects.bands]]`）。
#[derive(Debug, Clone, Deserialize)]
pub struct FilePeqBand {
    pub fc: f32,
    pub gain_db: f32,
    pub q: f32,
    /// 段类型：`peaking`（默认）/ `low_shelf` / `high_shelf` / `low_pass` / `high_pass`。
    #[serde(rename = "type", default)]
    pub band_type: Option<String>,
}

impl FileModel {
    /// FileModel → ChainModel（丢弃 APP 元数据 + 校验）。
    pub fn into_chain_model(&self, file: &str) -> Result<ChainModel, ConfigError> {
        if !self.enabled {
            return Ok(ChainModel { effects: Vec::new() });
        }
        let mut effects = Vec::with_capacity(self.effects.len());
        for (idx, fe) in self.effects.iter().enumerate() {
            effects.push(fe.into_effect_config(file, idx)?);
        }
        // peq 段数按声道分组统计。有 `channels` 的段计入对应声道，无
        // `channels` 的段计入共享预算（各声道 / 共享分别 ≤ MAX_PEQ_BANDS）。
        let mut shared_peq_bands = 0usize;
        let mut channel_peq_bands: HashMap<String, usize> = HashMap::new();
        for e in &effects {
            let EffectParams::Peq(p) = &e.params else { continue };
            let n = p.bands.len();
            match &e.channels {
                Some(names) if !names.is_empty() => {
                    for name in names {
                        *channel_peq_bands.entry(name.clone()).or_insert(0) += n;
                    }
                }
                _ => shared_peq_bands += n,
            }
        }
        if shared_peq_bands > MAX_PEQ_BANDS {
            return Err(model_err(
                file,
                format!(
                    "unscoped 'peq' bands count {shared_peq_bands} exceeds max {MAX_PEQ_BANDS}"
                ),
            ));
        }
        if let Some((name, count)) = channel_peq_bands.iter().find(|(_, c)| **c > MAX_PEQ_BANDS) {
            return Err(model_err(
                file,
                format!("channel '{name}' peq bands count {count} exceeds max {MAX_PEQ_BANDS}"),
            ));
        }
        Ok(ChainModel { effects })
    }
}

impl FileEffect {
    fn into_effect_config(&self, file: &str, idx: usize) -> Result<EffectConfig, ConfigError> {
        let kind = EffectType::from_str(&self.kind).ok_or_else(|| {
            model_err(
                file,
                format!("effects[{idx}]: unknown type '{}'", self.kind),
            )
        })?;
        self.check_keys(kind, file, idx)?;

        let params = match kind {
            EffectType::Peq => EffectParams::Peq(self.into_peq(file, idx)?),
            EffectType::Preamp => EffectParams::Preamp(self.into_preamp(file, idx)?),
            EffectType::Aural => EffectParams::Aural(self.into_aural(file, idx)?),
            EffectType::Reverb => EffectParams::Reverb(self.into_reverb(file, idx)?),
            EffectType::Maximizer => EffectParams::Maximizer(self.into_maximizer(file, idx)?),
            EffectType::Wide => EffectParams::Wide(self.into_wide(file, idx)?),
            EffectType::Loudness => EffectParams::Loudness(self.into_loudness(file, idx)?),
        };
        let channels = self.into_channels(file, idx)?;
        Ok(EffectConfig {
            kind,
            enabled: self.enabled,
            channels,
            params,
        })
    }

    /// 未知键 + 不适用字段检查。
    fn check_keys(&self, kind: EffectType, file: &str, idx: usize) -> Result<(), ConfigError> {
        if let Some(k) = self.extra.keys().next() {
            return Err(model_err(
                file,
                format!("effects[{idx}]: unknown key '{k}'"),
            ));
        }
        let allowed = match kind {
            EffectType::Peq => &["crossover_hz", "bands"][..],
            EffectType::Preamp => &["gain_db"][..],
            EffectType::Aural => &["tune_hz", "drive", "odd", "even", "wet", "dry"][..],
            EffectType::Reverb => &[
                "room_size",
                "decay",
                "damping",
                "bandwidth",
                "density",
                "lat5",
                "lat6",
                "pre_delay_ms",
                "motion_rate",
                "motion_depth",
                "motion_depth_ms",
                "wet",
                "dry",
            ][..],
            EffectType::Maximizer => &[
                "gain_boost_db",
                "max_output_db",
                "release_ms",
                "target",
                "lookahead_ms",
                "dither",
                "wet",
                "dry",
            ][..],
            EffectType::Wide => &[
                "intensity",
                "depth",
                "crossover_hz",
                "air",
                "gain",
            ][..],
            EffectType::Loudness => &["phon", "reference_phon"][..],
        };
        for field in self.set_fields() {
            if !allowed.contains(&field) {
                return Err(model_err(
                    file,
                    format!("effects[{idx}]: key '{field}' does not apply to '{}'", kind.as_str()),
                ));
            }
        }
        Ok(())
    }

    /// 已显式设置的参数字段名。
    fn set_fields(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        for (name, set) in [
            ("gain_db", self.gain_db.is_some()),
            ("crossover_hz", self.crossover_hz.is_some()),
            ("bands", self.bands.is_some()),
            ("tune_hz", self.tune_hz.is_some()),
            ("drive", self.drive.is_some()),
            ("odd", self.odd.is_some()),
            ("even", self.even.is_some()),
            ("wet", self.wet.is_some()),
            ("dry", self.dry.is_some()),
            ("room_size", self.room_size.is_some()),
            ("decay", self.decay.is_some()),
            ("damping", self.damping.is_some()),
            ("bandwidth", self.bandwidth.is_some()),
            ("density", self.density.is_some()),
            ("lat5", self.lat5.is_some()),
            ("lat6", self.lat6.is_some()),
            ("pre_delay_ms", self.pre_delay_ms.is_some()),
            ("motion_rate", self.motion_rate.is_some()),
            ("motion_depth", self.motion_depth.is_some()),
            ("motion_depth_ms", self.motion_depth.is_some()),
            ("gain_boost_db", self.gain_boost_db.is_some()),
            ("max_output_db", self.max_output_db.is_some()),
            ("release_ms", self.release_ms.is_some()),
            ("target", self.target.is_some()),
            ("lookahead_ms", self.lookahead_ms.is_some()),
            ("dither", self.dither.is_some()),
            ("intensity", self.intensity.is_some()),
            ("depth", self.depth.is_some()),
            ("air", self.air.is_some()),
            ("gain", self.gain.is_some()),
            ("phon", self.phon.is_some()),
            ("reference_phon", self.reference_phon.is_some()),
        ] {
            if set {
                v.push(name);
            }
        }
        v
    }

    fn into_channels(&self, file: &str, idx: usize) -> Result<Option<Vec<String>>, ConfigError> {
        let Some(channels) = &self.channels else {
            return Ok(None);
        };
        if channels.is_empty() {
            return Err(model_err(
                file,
                format!("effects[{idx}]: 'channels' must not be empty"),
            ));
        }
        for c in channels {
            if c.trim().is_empty() {
                return Err(model_err(
                    file,
                    format!("effects[{idx}]: 'channels' contains an empty name"),
                ));
            }
        }
        let mut seen = std::collections::HashSet::new();
        for c in channels {
            if !seen.insert(c.to_ascii_uppercase()) {
                return Err(model_err(
                    file,
                    format!("effects[{idx}]: duplicate channel '{c}'"),
                ));
            }
        }
        Ok(Some(channels.clone()))
    }

    fn into_peq(&self, file: &str, idx: usize) -> Result<PeqParams, ConfigError> {
        let crossover_hz = self
            .crossover_hz
            .unwrap_or(crate::pipeline::dsp::model::CROSSOVER_HZ);
        let crossover_hz = finite_range(crossover_hz, 20.0, 20_000.0, file, idx, "crossover_hz")?;

        let bands = self.bands.clone().ok_or_else(|| {
            model_err(file, format!("effects[{idx}]: 'peq' requires 'bands'"))
        })?;
        if bands.len() < MIN_PEQ_BANDS || bands.len() > MAX_PEQ_BANDS {
            return Err(model_err(
                file,
                format!(
                    "effects[{idx}]: 'peq' bands count {} out of range [{MIN_PEQ_BANDS}, {MAX_PEQ_BANDS}]",
                    bands.len()
                ),
            ));
        }
        let mut out = Vec::with_capacity(bands.len());
        for (bi, b) in bands.iter().enumerate() {
            let what = format!("bands[{bi}]");
            let fc = finite_range(b.fc, 20.0, 20_000.0, file, idx, &format!("{what}.fc"))?;
            let gain_db =
                finite_range(b.gain_db, -30.0, 30.0, file, idx, &format!("{what}.gain_db"))?;
            let q = finite_range(b.q, 0.1, 12.0, file, idx, &format!("{what}.q"))?;
            let kind = match &b.band_type {
                Some(t) => PeqBandType::from_str(t).ok_or_else(|| {
                    model_err(
                        file,
                        format!("effects[{idx}]: bands[{bi}].type '{t}' is invalid"),
                    )
                })?,
                None => PeqBandType::Peaking,
            };
            out.push(PeqBand { fc, gain_db, q, kind });
        }
        Ok(PeqParams { crossover_hz, bands: out })
    }

    fn into_preamp(&self, file: &str, idx: usize) -> Result<PreampParams, ConfigError> {
        let gain_db = self
            .gain_db
            .ok_or_else(|| model_err(file, format!("effects[{idx}]: 'preamp' requires 'gain_db'")))?;
        Ok(PreampParams {
            gain_db: finite_range(gain_db, -120.0, 48.0, file, idx, "gain_db")?,
        })
    }

    fn into_aural(&self, file: &str, idx: usize) -> Result<AuralParams, ConfigError> {
        let d = AuralParams::default();
        Ok(AuralParams {
            tune_hz: match self.tune_hz {
                Some(v) => finite_range(v, 500.0, 10_000.0, file, idx, "tune_hz")?,
                None => d.tune_hz,
            },
            drive: match self.drive {
                Some(v) => finite_range(v, 0.0, 4.25, file, idx, "drive")?,
                None => d.drive,
            },
            odd: match self.odd {
                Some(v) => finite_range(v, 0.0, 1.5, file, idx, "odd")?,
                None => d.odd,
            },
            even: match self.even {
                Some(v) => finite_range(v, 0.0, 0.75, file, idx, "even")?,
                None => d.even,
            },
            wet: match self.wet {
                Some(v) => finite_range(v, 0.0, 1.0, file, idx, "wet")?,
                None => d.wet,
            },
            dry: match self.dry {
                Some(v) => finite_range(v, 0.0, 1.0, file, idx, "dry")?,
                None => d.dry,
            },
        })
    }

    fn into_reverb(&self, file: &str, idx: usize) -> Result<ReverbParams, ConfigError> {
        let d = ReverbParams::default();
        let unit = |v: Option<f32>, name: &str| -> Result<f32, ConfigError> {
            match v {
                Some(x) => finite_range(x, 0.0, 1.0, file, idx, name),
                None => Ok(if name == "wet" { d.wet } else if name == "dry" { d.dry } else {
                    match name {
                        "decay" => d.decay,
                        "damping" => d.damping,
                        "bandwidth" => d.bandwidth,
                        "density" => d.density,
                        "lat5" => d.lat5,
                        "lat6" => d.lat6,
                        _ => 0.0,
                    }
                }),
            }
        };
        Ok(ReverbParams {
            room_size: match self.room_size {
                Some(v) => finite_range(v, 0.5, 1.5, file, idx, "room_size")?,
                None => d.room_size,
            },
            decay: unit(self.decay, "decay")?,
            damping: unit(self.damping, "damping")?,
            bandwidth: unit(self.bandwidth, "bandwidth")?,
            density: unit(self.density, "density")?,
            lat5: unit(self.lat5, "lat5")?,
            lat6: unit(self.lat6, "lat6")?,
            pre_delay_ms: match self.pre_delay_ms {
                Some(v) => finite_range(v, 0.0, 100.0, file, idx, "pre_delay_ms")?,
                None => d.pre_delay_ms,
            },
            motion_rate: match self.motion_rate {
                Some(v) => finite_range(v, 0.05, 2.0, file, idx, "motion_rate")?,
                None => d.motion_rate,
            },
            motion_depth: match self.motion_depth {
                Some(v) => finite_range(v, 0.0, 2.0, file, idx, "motion_depth")?,
                None => d.motion_depth,
            },
            wet: unit(self.wet, "wet")?,
            dry: unit(self.dry, "dry")?,
        })
    }

    fn into_maximizer(&self, file: &str, idx: usize) -> Result<MaximizerParams, ConfigError> {
        let d = MaximizerParams::default();
        let dither = match &self.dither {
            Some(s) => match s.to_ascii_lowercase().as_str() {
                "none" | "off" => DitherType::None,
                "uniform" => DitherType::Uniform,
                "triangular" | "triangle" => DitherType::Triangular,
                "shaped" => DitherType::Shaped,
                _ => {
                    return Err(model_err(
                        file,
                        format!("effects[{idx}]: invalid dither '{s}'"),
                    ))
                }
            },
            None => d.dither,
        };
        Ok(MaximizerParams {
            gain_boost_db: match self.gain_boost_db {
                Some(v) => finite_range(v, 0.0, 30.0, file, idx, "gain_boost_db")?,
                None => d.gain_boost_db,
            },
            max_output_db: match self.max_output_db {
                Some(v) => finite_range(v, -30.0, 0.0, file, idx, "max_output_db")?,
                None => d.max_output_db,
            },
            release_ms: match self.release_ms {
                Some(v) => finite_range(v, 0.1, 100.0, file, idx, "release_ms")?,
                None => d.release_ms,
            },
            target: match self.target {
                Some(v) => finite_range(v, 0.01, 1.0, file, idx, "target")?,
                None => d.target,
            },
            lookahead_ms: match self.lookahead_ms {
                Some(v) => finite_range(v, 0.0, 10.0, file, idx, "lookahead_ms")?,
                None => d.lookahead_ms,
            },
            dither,
            wet: match self.wet {
                Some(v) => finite_range(v, 0.0, 1.0, file, idx, "wet")?,
                None => d.wet,
            },
            dry: match self.dry {
                Some(v) => finite_range(v, 0.0, 1.0, file, idx, "dry")?,
                None => d.dry,
            },
        })
    }

    fn into_wide(&self, file: &str, idx: usize) -> Result<WideParams, ConfigError> {
        let d = WideParams::default();
        // 旧 `depth` 键（本会话早期版本）：映射到空气吸收。
        let depth_fallback = |what: &str| -> Result<f32, ConfigError> {
            match self.depth {
                Some(v) => finite_range(v, 0.0, 1.0, file, idx, what),
                None => Ok(0.0),
            }
        };
        Ok(WideParams {
            intensity: match self.intensity {
                Some(v) => finite_range(v, 0.0, 1.0, file, idx, "intensity")?,
                None => d.intensity,
            },
            gain: match self.gain {
                Some(v) => finite_range(v, 0.0, 1.0, file, idx, "gain")?,
                None => d.gain,
            },
            air: match self.air {
                Some(v) => finite_range(v, 0.0, 1.0, file, idx, "air")?,
                None => {
                    if self.depth.is_some() {
                        depth_fallback("depth")?
                    } else {
                        d.air
                    }
                }
            },
            crossover_hz: match self.crossover_hz {
                Some(v) => finite_range(v, 100.0, 1000.0, file, idx, "crossover_hz")?,
                None => d.crossover_hz,
            },
        })
    }

    fn into_loudness(&self, file: &str, idx: usize) -> Result<LoudnessParams, ConfigError> {
        let phon = self
            .phon
            .ok_or_else(|| model_err(file, format!("effects[{idx}]: 'loudness' requires 'phon'")))?;
        let reference = self.reference_phon.unwrap_or(80.0);
        Ok(LoudnessParams {
            phon: finite_range(phon, 0.0, 120.0, file, idx, "phon")?,
            reference_phon: finite_range(reference, 0.0, 120.0, file, idx, "reference_phon")?,
        })
    }
}

fn finite_range(
    v: f32,
    lo: f32,
    hi: f32,
    file: &str,
    idx: usize,
    what: &str,
) -> Result<f32, ConfigError> {
    if !v.is_finite() || v < lo || v > hi {
        return Err(model_err(
            file,
            format!("effects[{idx}]: '{what}' = {v} out of range [{lo}, {hi}]"),
        ));
    }
    Ok(v)
}

fn model_err(file: &str, message: String) -> ConfigError {
    ConfigError::ModelError {
        file: file.to_owned(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(toml: &str) -> Result<ChainModel, ConfigError> {
        let file: FileModel = toml::from_str(toml).map_err(|e| ConfigError::TomlError {
            file: "test.toml".into(),
            message: e.to_string(),
        })?;
        file.into_chain_model("test.toml")
    }

    #[test]
    fn parse_full_chain_with_metadata() {
        let toml = r#"
version = 1
[meta]
app = "vxapo"
schema = 1

[[effects]]
type = "preamp"
gain_db = -3.0

[[effects]]
type = "peq"
name = "主声道"
group = "FPS 预设"
crossover_hz = 200
[[effects.bands]]
fc = 1000
gain_db = 3.0
q = 1.0
[[effects.bands]]
fc = 2500
gain_db = -2.0
q = 2.0
[[effects.bands]]
fc = 4000
gain_db = 0.5
q = 0.8
[[effects.bands]]
fc = 8000
gain_db = -1.0
q = 1.1
[[effects.bands]]
fc = 12000
gain_db = 0.0
q = 1.0
[[effects.bands]]
fc = 16000
gain_db = 1.5
q = 0.9

[[effects]]
type = "wide"
intensity = 0.5
"#;
        let model = convert(toml).unwrap();
        assert_eq!(model.effects.len(), 3);
        assert_eq!(model.effects[0].kind, EffectType::Preamp);
        assert_eq!(model.effects[1].kind, EffectType::Peq);
        assert!(model.effects[1].enabled);
        match &model.effects[1].params {
            EffectParams::Peq(p) => {
                assert_eq!(p.crossover_hz, 200.0);
                assert_eq!(p.bands.len(), 6);
                assert_eq!(p.bands[0].fc, 1000.0);
            }
            _ => panic!("expected peq"),
        }
        match &model.effects[2].params {
            EffectParams::Wide(w) => assert_eq!(w.intensity, 0.5),
            _ => panic!("expected wide"),
        }
    }

    #[test]
    fn defaults_applied() {
        let toml = r#"
[[effects]]
type = "peq"
[[effects.bands]]
fc = 100
gain_db = -3.0
q = 1.0
[[effects.bands]]
fc = 200
gain_db = -3.0
q = 1.0
[[effects.bands]]
fc = 400
gain_db = -3.0
q = 1.0
[[effects.bands]]
fc = 800
gain_db = -3.0
q = 1.0
[[effects.bands]]
fc = 1600
gain_db = -3.0
q = 1.0
[[effects.bands]]
fc = 3200
gain_db = -3.0
q = 1.0

[[effects]]
type = "wide"
"#;
        let model = convert(toml).unwrap();
        match &model.effects[0].params {
            EffectParams::Peq(p) => assert_eq!(p.crossover_hz, 200.0),
            _ => panic!("expected peq"),
        }
        match &model.effects[1].params {
            EffectParams::Wide(w) => assert_eq!(w.intensity, WideParams::default().intensity),
            _ => panic!("expected wide"),
        }
    }

    #[test]
    fn band_type_parsed_and_defaulted() {
        let toml = r#"
[[effects]]
type = "peq"
[[effects.bands]]
fc = 100
gain_db = -3.0
q = 1.0
[[effects.bands]]
type = "low_shelf"
fc = 200
gain_db = 4.0
q = 0.707
[[effects.bands]]
type = "high_shelf"
fc = 5000
gain_db = -4.0
q = 0.707
[[effects.bands]]
type = "low_pass"
fc = 1200
gain_db = 0.0
q = 0.707
[[effects.bands]]
type = "high_pass"
fc = 80
gain_db = 0.0
q = 0.707
"#;
        let model = convert(toml).unwrap();
        match &model.effects[0].params {
            EffectParams::Peq(p) => {
                assert_eq!(p.bands.len(), 5);
                assert_eq!(p.bands[0].kind, PeqBandType::Peaking, "缺省 type 必须是 peaking");
                assert_eq!(p.bands[1].kind, PeqBandType::LowShelf);
                assert_eq!(p.bands[2].kind, PeqBandType::HighShelf);
                assert_eq!(p.bands[3].kind, PeqBandType::LowPass);
                assert_eq!(p.bands[4].kind, PeqBandType::HighPass);
            }
            _ => panic!("expected peq"),
        }
    }

    #[test]
    fn invalid_band_type_rejected() {
        let toml = r#"
[[effects]]
type = "peq"
[[effects.bands]]
type = "ring_mod"
fc = 1000
gain_db = 0.0
q = 1.0
"#;
        let err = convert(toml).unwrap_err();
        assert!(err.to_string().contains("bands[0].type 'ring_mod' is invalid"));
    }

    #[test]
    fn band_type_included_in_spec() {
        let base = |t: &str| {
            format!(
                "[[effects]]\ntype = \"peq\"\n[[effects.bands]]\ntype = \"{t}\"\nfc = 1000\ngain_db = 3.0\nq = 1.0\n"
            )
        };
        let a = convert(&base("peaking")).unwrap();
        let b = convert(&base("low_shelf")).unwrap();
        assert_ne!(a.effects[0].spec(), b.effects[0].spec());
        assert!(a.effects[0].spec().contains("peaking"));
        assert!(b.effects[0].spec().contains("low_shelf"));
    }

    #[test]
    fn unknown_type_rejected() {
        let err = convert("[[effects]]\ntype = \"graphiceq\"\n").unwrap_err();
        assert!(err.to_string().contains("unknown type"));
    }

    #[test]
    fn unknown_key_rejected() {
        let err = convert("[[effects]]\ntype = \"peq\"\nbogus = 1\n").unwrap_err();
        assert!(err.to_string().contains("unknown key 'bogus'"));
    }

    #[test]
    fn foreign_field_rejected() {
        let err = convert("[[effects]]\ntype = \"peq\"\ngain_db = 1.0\n").unwrap_err();
        assert!(err.to_string().contains("does not apply to 'peq'"));
    }

    #[test]
    fn band_count_out_of_range_rejected() {
        let mut s = String::from("[[effects]]\ntype = \"peq\"\n");
        for fc in (0..32).map(|i| 100.0 + i as f32 * 100.0) {
            s.push_str(&format!("[[effects.bands]]\nfc = {fc}\ngain_db = 0.0\nq = 1.0\n"));
        }
        let err = convert(&s).unwrap_err();
        assert!(err.to_string().contains("out of range [1, 31]"));
    }

    #[test]
    fn single_band_peq_accepted() {
        // 单块下限 1——允许 1 段卡 / 无组裸 band（UI 设计规范 01）。
        let toml = r#"
[[effects]]
type = "peq"
group = "FPS 预设"
name = "枪声增强"
[[effects.bands]]
fc = 3200
gain_db = 3.0
q = 2.0
"#;
        let model = convert(toml).unwrap();
        match &model.effects[0].params {
            EffectParams::Peq(p) => assert_eq!(p.bands.len(), 1),
            _ => panic!("expected peq"),
        }
    }

    #[test]
    fn total_peq_band_cap_enforced() {
        // 跨块全局合计 ≤ 31（预设块 + 无组裸 band 共享预算）。
        let mut s = String::new();
        for _ in 0..2 {
            s.push_str("[[effects]]\ntype = \"peq\"\n");
            for fc in (0..16).map(|i| 100.0 + i as f32 * 100.0) {
                s.push_str(&format!("[[effects.bands]]\nfc = {fc}\ngain_db = 0.0\nq = 1.0\n"));
            }
        }
        let err = convert(&s).unwrap_err();
        assert!(err.to_string().contains("unscoped 'peq' bands count 32 exceeds max 31"));
    }

    #[test]
    fn per_channel_peq_band_cap() {
        let mut s = String::new();
        for _ in 0..2 {
            s.push_str("[[effects]]\ntype = \"peq\"\nchannels = [\"L\"]\n");
            for fc in (0..10).map(|i| 100.0 + i as f32 * 100.0) {
                s.push_str(&format!("[[effects.bands]]\nfc = {fc}\ngain_db = 0.0\nq = 1.0\n"));
            }
        }
        for _ in 0..2 {
            s.push_str("[[effects]]\ntype = \"peq\"\nchannels = [\"R\"]\n");
            for fc in (0..10).map(|i| 100.0 + i as f32 * 100.0) {
                s.push_str(&format!("[[effects.bands]]\nfc = {fc}\ngain_db = 0.0\nq = 1.0\n"));
            }
        }
        // L 20 + R 20：每声道均 ≤31，应通过
        assert!(convert(&s).is_ok());
        // L 再补 12 段 → L = 32 超限，按声道报错
        s.push_str("[[effects]]\ntype = \"peq\"\nchannels = [\"L\"]\n");
        for fc in (0..12).map(|i| 100.0 + i as f32 * 100.0) {
            s.push_str(&format!("[[effects.bands]]\nfc = {fc}\ngain_db = 0.0\nq = 1.0\n"));
        }
        let err = convert(&s).unwrap_err();
        assert!(err.to_string().contains("channel 'L' peq bands count 32 exceeds max 31"));
    }

    #[test]
    fn total_peq_band_cap_allows_31() {
        let mut s = String::new();
        let mut count = 0usize;
        for block_bands in [15usize, 16] {
            s.push_str("[[effects]]\ntype = \"peq\"\n");
            for _ in 0..block_bands {
                count += 1;
                s.push_str(&format!(
                    "[[effects.bands]]\nfc = {}\ngain_db = 0.0\nq = 1.0\n",
                    100.0 + count as f32 * 100.0
                ));
            }
        }
        assert_eq!(count, 31);
        assert!(convert(&s).is_ok());
    }

    #[test]
    fn out_of_range_rejected() {
        let err = convert("[[effects]]\ntype = \"preamp\"\ngain_db = 60.0\n").unwrap_err();
        assert!(err.to_string().contains("'gain_db' = 60 out of range"));
    }

    #[test]
    fn missing_required_key_rejected() {
        let err = convert("[[effects]]\ntype = \"preamp\"\n").unwrap_err();
        assert!(err.to_string().contains("requires 'gain_db'"));
        let err = convert("[[effects]]\ntype = \"peq\"\n").unwrap_err();
        assert!(err.to_string().contains("requires 'bands'"));
    }

    #[test]
    fn duplicate_channel_rejected() {
        let err = convert(
            "[[effects]]\ntype = \"wide\"\nchannels = [\"FL\", \"fl\"]\nintensity = 0.5\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("duplicate channel"));
    }

    #[test]
    fn invalid_dither_rejected() {
        let err = convert(
            "[[effects]]\ntype = \"maximizer\"\ndither = \"pink\"\ngain_boost_db = 6.0\nmax_output_db = -0.3\nrelease_ms = 10.0\ntarget = 0.32\nlookahead_ms = 0.75\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("invalid dither"));
    }

    #[test]
    fn toml_syntax_error_reported() {
        let err = convert("[[effects]\ntype = \"peq\"\n").unwrap_err();
        assert!(err.to_string().contains("TOML error"));
    }
}
