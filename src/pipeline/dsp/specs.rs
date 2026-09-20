//! pipeline/dsp/specs.rs — 效果器参数表（UI 契约单一来源）
//!
//! 用途：cli `effects schema --json` 透传本表，app 依据它生成
//! `src/lib/effects.generated.ts`（决策 2）——参数范围、步进与默认值只在此维护一份。
//!
//! 数值来源：**默认值运行时取自各 `*Params::default()`**（避免表与实现漂移，
//! 测试逐项锁定）；范围与步进为 UI 暴露范围，与解析层 `config/model/convert.rs`
//! 的校验范围一致。

use serde::{Deserialize, Serialize};

use crate::pipeline::dsp::aural::AuralParams;
use crate::pipeline::dsp::compressor::CompressorParams;
use crate::pipeline::dsp::math::{GAIN_DB_MAX, GAIN_DB_MIN, PHON_MAX, PHON_MIN};
use crate::pipeline::dsp::reverb::ReverbParams;
use crate::pipeline::dsp::wide::WideParams;

/// 单个参数的可调范围、步进、默认值与单位。
///
/// 注意：本表给的是 driver 的**权威数值**，不做 UI 显示精度处理——长浮点
/// （如 wide air=`0.354331`）在输入框里放不下、正常使用也不需要该精度，
/// 显示层就近取整（如 `0.3543`）属 app 自身取舍，不是漂移。
/// 同理，app 为「新增效果器」选定的 UI 起点若与 driver 默认值不同
/// （如 wide `gain` 0.05 vs 0.0），那是产品取舍，应显式声明在 app 侧。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectParamSpec {
    /// 参数键（即 config.toml 字段名）。
    pub key: &'static str,
    /// 步进（滑杆粒度）。
    pub step: f64,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    /// 单位（`dB` / `Hz` / `ms` / `dBFS` / `phon`）；无量纲为 `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<&'static str>,
}

/// 一种效果器及其全部 UI 可调参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectSpec {
    /// 效果器类型（`[[effects]] type`）。
    pub effect: &'static str,
    pub params: Vec<EffectParamSpec>,
}

/// 参数表构造（`unit = None` 表示无量纲）。
fn spec(
    key: &'static str,
    min: f64,
    max: f64,
    step: f64,
    default: f64,
    unit: Option<&'static str>,
) -> EffectParamSpec {
    EffectParamSpec {
        key,
        step,
        min,
        max,
        default,
        unit,
    }
}

/// 全部效果器的参数表（顺序即 UI 展示顺序）。
pub fn effect_param_specs() -> Vec<EffectSpec> {
    let wide = WideParams::default();
    let aural = AuralParams::default();
    let reverb = ReverbParams::default();
    let compressor = CompressorParams::default();

    vec![
        EffectSpec {
            effect: "preamp",
            params: vec![spec(
                "gain_db",
                GAIN_DB_MIN as f64,
                GAIN_DB_MAX as f64,
                0.1,
                // PreampParams 无 Default：0 dB 即直通，取中性值。
                0.0,
                Some("dB"),
            )],
        },
        EffectSpec {
            effect: "wide",
            params: vec![
                spec("gain", 0.0, 1.0, 0.01, wide.gain as f64, None),
                spec("air", 0.0, 1.0, 0.01, wide.air as f64, None),
                spec("air_side", 0.0, 1.0, 0.01, wide.air_side as f64, None),
                spec("mix", 0.0, 1.0, 0.01, wide.mix as f64, None),
                spec(
                    "crossover_hz",
                    200.0,
                    1000.0,
                    10.0,
                    wide.crossover_hz as f64,
                    Some("Hz"),
                ),
            ],
        },
        EffectSpec {
            effect: "aural",
            params: vec![
                spec(
                    "tune_hz",
                    500.0,
                    10_000.0,
                    10.0,
                    aural.tune_hz as f64,
                    Some("Hz"),
                ),
                spec("drive", 0.0, 4.25, 0.01, aural.drive as f64, None),
                spec("odd", 0.0, 1.5, 0.01, aural.odd as f64, None),
                spec("even", 0.0, 0.75, 0.01, aural.even as f64, None),
                spec("wet", 0.0, 1.0, 0.01, aural.wet as f64, None),
                spec("dry", 0.0, 1.0, 0.01, aural.dry as f64, None),
            ],
        },
        EffectSpec {
            effect: "reverb",
            params: vec![
                spec(
                    "room_size",
                    0.5,
                    1.5,
                    0.01,
                    reverb.room_size as f64,
                    None,
                ),
                spec("decay", 0.0, 1.0, 0.01, reverb.decay as f64, None),
                spec("damping", 0.0, 1.0, 0.01, reverb.damping as f64, None),
                spec(
                    "pre_delay_ms",
                    0.0,
                    100.0,
                    1.0,
                    reverb.pre_delay_ms as f64,
                    Some("ms"),
                ),
                spec(
                    "low_cut_hz",
                    20.0,
                    250.0,
                    5.0,
                    reverb.low_cut_hz as f64,
                    Some("Hz"),
                ),
                spec("wet", 0.0, 1.0, 0.01, reverb.wet as f64, None),
                spec("dry", 0.0, 1.0, 0.01, reverb.dry as f64, None),
            ],
        },
        EffectSpec {
            effect: "compressor",
            params: vec![
                spec(
                    "threshold_db",
                    -60.0,
                    0.0,
                    1.0,
                    compressor.threshold_db as f64,
                    Some("dBFS"),
                ),
                spec("ratio", 1.0, 20.0, 0.5, compressor.ratio as f64, None),
                spec("knee_db", 0.0, 12.0, 1.0, compressor.knee_db as f64, Some("dB")),
                spec(
                    "attack_ms",
                    0.1,
                    100.0,
                    0.5,
                    compressor.attack_ms as f64,
                    Some("ms"),
                ),
                spec(
                    "release_ms",
                    10.0,
                    1000.0,
                    10.0,
                    compressor.release_ms as f64,
                    Some("ms"),
                ),
                spec(
                    "makeup_gain_db",
                    0.0,
                    24.0,
                    0.5,
                    compressor.makeup_gain_db as f64,
                    Some("dB"),
                ),
                spec("wet", 0.0, 1.0, 0.01, compressor.wet as f64, None),
                spec("dry", 0.0, 1.0, 0.01, compressor.dry as f64, None),
            ],
        },
        EffectSpec {
            effect: "loudness",
            params: vec![
                spec(
                    "phon",
                    PHON_MIN as f64,
                    PHON_MAX as f64,
                    1.0,
                    // 文档典型值（LoudnessFilter::new 注释：典型 40–80）。
                    80.0,
                    Some("phon"),
                ),
                spec(
                    "reference_phon",
                    PHON_MIN as f64,
                    PHON_MAX as f64,
                    1.0,
                    // 与解析层缺省一致（config/model/convert.rs 的 eference_phon.unwrap_or(80.0)）。
                    80.0,
                    Some("phon"),
                ),
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认值必须在 `[min, max]` 内、`step > 0`、键唯一（UI 滑杆与写盘都依赖）。
    #[test]
    fn specs_are_self_consistent() {
        for effect in effect_param_specs() {
            let mut keys = Vec::new();
            for p in &effect.params {
                assert!(p.step > 0.0, "{} {}: step 必须为正", effect.effect, p.key);
                assert!(
                    p.min <= p.default && p.default <= p.max,
                    "{} {}: default {} 不在 [{}, {}] 内",
                    effect.effect,
                    p.key,
                    p.default,
                    p.min,
                    p.max
                );
                assert!(
                    p.min < p.max,
                    "{} {}: min 必须小于 max",
                    effect.effect,
                    p.key
                );
                assert!(!keys.contains(&p.key), "{}: 参数键重复 {}", effect.effect, p.key);
                keys.push(p.key);
            }
        }
    }

    /// 默认值与各效果器实现的全精度默认值一致（防止表与实现漂移，含
    /// wide air=0.354331、aural drive=1.76993、reverb damping=0.408290）。
    #[test]
    fn defaults_track_filter_impls() {
        let find = |effect: &str, key: &str| -> f64 {
            effect_param_specs()
                .iter()
                .find(|e| e.effect == effect)
                .and_then(|e| e.params.iter().find(|p| p.key == key))
                .map(|p| p.default)
                .unwrap_or_else(|| panic!("缺少 {effect}.{key}"))
        };
        let wide = WideParams::default();
        assert_eq!(find("wide", "air"), wide.air as f64);
        assert_eq!(find("wide", "mix"), wide.mix as f64);
        let aural = AuralParams::default();
        assert_eq!(find("aural", "drive"), aural.drive as f64);
        assert_eq!(find("aural", "odd"), aural.odd as f64);
        let reverb = ReverbParams::default();
        assert_eq!(find("reverb", "damping"), reverb.damping as f64);
        assert_eq!(find("reverb", "decay"), reverb.decay as f64);
        let compressor = CompressorParams::default();
        assert_eq!(find("compressor", "ratio"), compressor.ratio as f64);
        assert_eq!(find("compressor", "release_ms"), compressor.release_ms as f64);
        // loudness 无 Default 实现：默认值来自文档典型值与解析层回退（见参数表注释）。
        assert_eq!(find("loudness", "phon"), 80.0);
        assert_eq!(find("loudness", "reference_phon"), 80.0);
        // 本表给的是 driver 的**精确**默认值（0.354331 / 1.76993）。
        // UI 侧显示精度是 app 自己的取舍（输入框放不下长浮点，正常使用也不需要
        // 这么高精度），app 按自己的显示规则就近取整即可，不属漂移。
        assert_eq!(find("wide", "air"), 0.354_331_f32 as f64);
        assert_eq!(find("aural", "drive"), 1.769_93_f32 as f64);
    }

    /// 覆盖 app 需要的全部效果器（缺一个会导致 UI 少一组参数）。
    #[test]
    fn covers_all_ui_effects() {
        let effects: Vec<&str> = effect_param_specs().iter().map(|e| e.effect).collect();
        assert_eq!(
            effects,
            ["preamp", "wide", "aural", "reverb", "compressor", "loudness"]
        );
    }

    /// 序列化形状（cli `effects schema --json` 的输出）。
    #[test]
    fn serializes_expected_shape() {
        let json = serde_json::to_string(&effect_param_specs()[0]).unwrap();
        assert_eq!(
            json,
            r#"{"effect":"preamp","params":[{"key":"gain_db","step":0.1,"min":-120.0,"max":48.0,"default":0.0,"unit":"dB"}]}"#
        );
    }
}
