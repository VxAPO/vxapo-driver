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
        EffectParams::Wide(w) => assert_eq!(w.air, 0.5),
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
        EffectParams::Wide(w) => assert_eq!(w.air, WideParams::default().air),
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
fn compressor_range_rejected() {
    let err = convert("[[effects]]\ntype = \"compressor\"\nthreshold_db = 5.0\n")
        .unwrap_err();
    assert!(err.to_string().contains("threshold_db"));
    let err2 = convert("[[effects]]\ntype = \"compressor\"\nratio = 50.0\n")
        .unwrap_err();
    assert!(err2.to_string().contains("ratio"));
}

#[test]
fn legacy_maximizer_and_leveler_map_to_compressor() {
    // 旧 maximizer / leveler 段落：类型映射为 compressor，旧参数忽略。
    let model = convert(
        "[[effects]]\ntype = \"maximizer\"\ngain_boost_db = 6.0\nmax_output_db = -0.3\nrelease_ms = 10.0\ntarget = 0.32\nlookahead_ms = 0.75\ndither = \"shaped\"\n",
    )
    .unwrap();
    assert_eq!(model.effects.len(), 1);
    assert_eq!(model.effects[0].kind, EffectType::Compressor);
    match &model.effects[0].params {
        EffectParams::Compressor(p) => {
            assert_eq!(p.threshold_db, CompressorParams::default().threshold_db);
            assert_eq!(p.ratio, CompressorParams::default().ratio);
        }
        _ => panic!("expected compressor"),
    }
    // leveler 旧段落同样映射。
    let model2 = convert(
        "[[effects]]\ntype = \"leveler\"\ntarget_rms_db = -12.0\nresponse_s = 5.0\n",
    )
    .unwrap();
    assert_eq!(model2.effects[0].kind, EffectType::Compressor);
    match &model2.effects[0].params {
        EffectParams::Compressor(p) => {
            assert_eq!(p.threshold_db, CompressorParams::default().threshold_db);
        }
        _ => panic!("expected compressor"),
    }
}

#[test]
fn toml_syntax_error_reported() {
    let err = convert("[[effects]\ntype = \"peq\"\n").unwrap_err();
    assert!(err.to_string().contains("TOML error"));
}
