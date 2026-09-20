//! config/model/convert.rs — 文件模型校验与 ChainModel 转换

//! into_chain_model() 丢弃 APP 元数据并完成范围/段数/声道名校验。

use super::*;
use super::types::*;

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
            EffectType::Compressor => EffectParams::Compressor(self.into_compressor(file, idx)?),
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
                "low_cut_hz",
                "wet",
                "dry",
            ][..],
            // 旧 maximizer / leveler 字段一并允许（旧配置映射时忽略）。
            EffectType::Compressor => &[
                "threshold_db",
                "ratio",
                "knee_db",
                "attack_ms",
                "release_ms",
                "makeup_gain_db",
                "wet",
                "dry",
                // —— 旧 maximizer 兼容（忽略）——
                "gain_boost_db",
                "max_output_db",
                "target",
                "lookahead_ms",
                "dither",
                // —— 旧 leveler 兼容（忽略）——
                "target_rms_db",
                "response_s",
                "max_gain_db",
                "dynamic_preserve",
                "noise_gate_db",
                "peak_limit_db",
            ][..],
            EffectType::Wide => &[
                "intensity",
                "depth",
                "crossover_hz",
                "air",
                "air_side",
                "mix",
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
            ("low_cut_hz", self.low_cut_hz.is_some()),
            ("gain_boost_db", self.gain_boost_db.is_some()),
            ("max_output_db", self.max_output_db.is_some()),
            ("release_ms", self.release_ms.is_some()),
            ("target", self.target.is_some()),
            ("lookahead_ms", self.lookahead_ms.is_some()),
            ("dither", self.dither.is_some()),
            ("target_rms_db", self.target_rms_db.is_some()),
            ("response_s", self.response_s.is_some()),
            ("max_gain_db", self.max_gain_db.is_some()),
            ("dynamic_preserve", self.dynamic_preserve.is_some()),
            ("noise_gate_db", self.noise_gate_db.is_some()),
            ("peak_limit_db", self.peak_limit_db.is_some()),
            ("threshold_db", self.threshold_db.is_some()),
            ("ratio", self.ratio.is_some()),
            ("knee_db", self.knee_db.is_some()),
            ("attack_ms", self.attack_ms.is_some()),
            ("makeup_gain_db", self.makeup_gain_db.is_some()),
            ("intensity", self.intensity.is_some()),
            ("depth", self.depth.is_some()),
            ("air", self.air.is_some()),
            ("air_side", self.air_side.is_some()),
            ("mix", self.mix.is_some()),
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
            low_cut_hz: match self.low_cut_hz {
                Some(v) => finite_range(v, 20.0, 250.0, file, idx, "low_cut_hz")?,
                None => d.low_cut_hz,
            },
            wet: unit(self.wet, "wet")?,
            dry: unit(self.dry, "dry")?,
        })
    }

    fn into_compressor(&self, file: &str, idx: usize) -> Result<CompressorParams, ConfigError> {
        let d = CompressorParams::default();
        // 旧 maximizer / leveler 字段全部忽略，走新默认值。
        Ok(CompressorParams {
            threshold_db: match self.threshold_db {
                Some(v) => finite_range(v, -60.0, 0.0, file, idx, "threshold_db")?,
                None => d.threshold_db,
            },
            ratio: match self.ratio {
                Some(v) => finite_range(v, 1.0, 20.0, file, idx, "ratio")?,
                None => d.ratio,
            },
            knee_db: match self.knee_db {
                Some(v) => finite_range(v, 0.0, 12.0, file, idx, "knee_db")?,
                None => d.knee_db,
            },
            attack_ms: match self.attack_ms {
                Some(v) => finite_range(v, 0.1, 100.0, file, idx, "attack_ms")?,
                None => d.attack_ms,
            },
            release_ms: match self.release_ms {
                Some(v) => finite_range(v, 10.0, 1000.0, file, idx, "release_ms")?,
                None => d.release_ms,
            },
            makeup_gain_db: match self.makeup_gain_db {
                Some(v) => finite_range(v, 0.0, 24.0, file, idx, "makeup_gain_db")?,
                None => d.makeup_gain_db,
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
            gain: match self.gain {
                Some(v) => finite_range(v, 0.0, 1.0, file, idx, "gain")?,
                None => d.gain,
            },
            // 空气吸收：优先显式 air；旧配置的 depth / intensity 依次回退映射。
            air: match self.air {
                Some(v) => finite_range(v, 0.0, 1.0, file, idx, "air")?,
                None => {
                    if self.depth.is_some() {
                        depth_fallback("depth")?
                    } else if let Some(intensity) = self.intensity {
                        finite_range(intensity, 0.0, 1.0, file, idx, "intensity")?
                    } else {
                        d.air
                    }
                }
            },
            air_side: match self.air_side {
                Some(v) => finite_range(v, 0.0, 1.0, file, idx, "air_side")?,
                None => d.air_side,
            },
            mix: match self.mix {
                Some(v) => finite_range(v, 0.0, 1.0, file, idx, "mix")?,
                None => d.mix,
            },
            crossover_hz: match self.crossover_hz {
                Some(v) => finite_range(v, 200.0, 1000.0, file, idx, "crossover_hz")?,
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

pub(super) fn finite_range(
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

pub(super) fn model_err(file: &str, message: String) -> ConfigError {
    ConfigError::ModelError {
        file: file.to_owned(),
        message,
    }
}

