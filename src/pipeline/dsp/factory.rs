//! pipeline/dsp/factory.rs — 模型 → Filter 静态分派
//!
//! 参数已由 config 层完成 TOML 反序列化与校验；本层按 `EffectType` 穷尽
//! `match` 构造 Filter，不再有动态注册表 / 字符串参数解析
//! （已删除 `FilterFactory` / `FilterRegistry` / `FilterCreateResult` /
//! `OutcomeKind` / `index` 常量）。

use crate::pipeline::dsp::aural::AuralEnhancerFilter;
use crate::pipeline::dsp::compressor::CompressorFilter;
use crate::pipeline::dsp::filter::{DspContext, Filter, PassthroughFilter};
use crate::pipeline::dsp::gain::GainFilter;
use crate::pipeline::dsp::loudness::LoudnessFilter;
use crate::pipeline::dsp::model::{EffectConfig, EffectParams, EffectType};
use crate::pipeline::dsp::peq_hybrid::HybridPeqFilter;
use crate::pipeline::dsp::reverb::ReverbFilter;
use crate::pipeline::dsp::wide::WideFilter;

/// 由 DSP 模型构造 Filter（校验已前置，构造不失败）。
pub fn create_from_model(effect: &EffectConfig, ctx: &DspContext) -> Box<dyn Filter> {
    debug_assert!(matches_kind(&effect.kind, &effect.params));
    if !effect.enabled {
        return Box::new(PassthroughFilter);
    }
    match &effect.params {
        EffectParams::Peq(p) => Box::new(HybridPeqFilter::new(p.clone())),
        EffectParams::Preamp(p) => Box::new(GainFilter::new(p.gain_db)),
        EffectParams::Aural(p) => Box::new(AuralEnhancerFilter::new(*p)),
        EffectParams::Reverb(p) => Box::new(ReverbFilter::new(*p)),
        EffectParams::Compressor(p) => Box::new(CompressorFilter::new(*p)),
        EffectParams::Wide(p) => Box::new(WideFilter::new(*p)),
        EffectParams::Loudness(p) => {
            let mut f = LoudnessFilter::new(p.phon, p.reference_phon);
            f.set_enabled(ctx.loudness_enabled.get());
            Box::new(f)
        }
    }
}

fn matches_kind(kind: &EffectType, params: &EffectParams) -> bool {
    matches!(
        (kind, params),
        (EffectType::Peq, EffectParams::Peq(_))
            | (EffectType::Preamp, EffectParams::Preamp(_))
            | (EffectType::Aural, EffectParams::Aural(_))
            | (EffectType::Reverb, EffectParams::Reverb(_))
            | (EffectType::Compressor, EffectParams::Compressor(_))
            | (EffectType::Wide, EffectParams::Wide(_))
            | (EffectType::Loudness, EffectParams::Loudness(_))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::dsp::filter::{DeviceType, ProcessingStage};
    use crate::pipeline::dsp::model::{LoudnessParams, PeqBand, PeqBandType, PeqParams, PreampParams};
    use crate::pipeline::dsp::wide::WideParams;
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
            loudness_enabled: std::cell::Cell::new(true),
            rt_marker: std::marker::PhantomData,
        }
    }

    fn effect(kind: EffectType, params: EffectParams) -> EffectConfig {
        EffectConfig {
            kind,
            enabled: true,
            channels: None,
            params,
        }
    }

    fn peq_params() -> PeqParams {
        PeqParams {
            crossover_hz: 200.0,
            bands: vec![
                PeqBand { fc: 100.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
                PeqBand { fc: 200.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
                PeqBand { fc: 400.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
                PeqBand { fc: 800.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
                PeqBand { fc: 1600.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
                PeqBand { fc: 3200.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
            ],
        }
    }

    #[test]
    fn creates_all_effect_types() {
        let ctx = test_ctx();
        let cases: Vec<EffectConfig> = vec![
            effect(EffectType::Peq, EffectParams::Peq(peq_params())),
            effect(
                EffectType::Preamp,
                EffectParams::Preamp(PreampParams { gain_db: -3.0 }),
            ),
            effect(
                EffectType::Aural,
                EffectParams::Aural(crate::pipeline::dsp::aural::AuralParams::default()),
            ),
            effect(
                EffectType::Reverb,
                EffectParams::Reverb(crate::pipeline::dsp::reverb::ReverbParams::default()),
            ),
            effect(
                EffectType::Compressor,
                EffectParams::Compressor(crate::pipeline::dsp::compressor::CompressorParams::default()),
            ),
            effect(
                EffectType::Wide,
                EffectParams::Wide(WideParams::default()),
            ),
            effect(
                EffectType::Loudness,
                EffectParams::Loudness(LoudnessParams { phon: 80.0, reference_phon: 90.0 }),
            ),
        ];
        for cfg in cases {
            let f = create_from_model(&cfg, &ctx);
            assert!(format!("{f:?}").len() > 0);
        }
    }

    #[test]
    fn disabled_effect_is_passthrough() {
        let mut cfg = effect(EffectType::Wide, EffectParams::Wide(WideParams::default()));
        cfg.enabled = false;
        let mut f = create_from_model(&cfg, &test_ctx());
        // PassthroughFilter：处理不改动样本。
        let mut samples = vec![vec![0.3f32; 8], vec![0.2f32; 8]];
        let before = samples.clone();
        f.process(&mut samples, 8);
        assert_eq!(samples, before);
    }

    #[test]
    fn loudness_respects_ctx_flag() {
        let cfg = effect(
            EffectType::Loudness,
            EffectParams::Loudness(LoudnessParams { phon: 80.0, reference_phon: 90.0 }),
        );
        let ctx = test_ctx();
        ctx.loudness_enabled.set(false);
        let mut f = create_from_model(&cfg, &ctx);
        let mut samples = vec![vec![0.5f32; 64], vec![0.5f32; 64]];
        let before = samples.clone();
        f.process(&mut samples, 64);
        assert_eq!(samples, before, "loudness disabled in ctx must passthrough");
    }
}
