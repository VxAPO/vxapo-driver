use super::*;

fn stereo_channels() -> Vec<String> {
    vec!["L".into(), "R".into()]
}

fn make_stereo_buf(frame_count: usize) -> Vec<Vec<f32>> {
    vec![vec![0.0f32; frame_count]; 2]
}

// ── BiquadCoeffs ────────────────────────────────────────────────────────

#[test]
fn coeffs_bypass() {
    let c = BiquadCoeffs::BYPASS;
    assert_eq!(c.b0, 1.0);
    assert_eq!(c.b1, 0.0);
    assert!(c.is_valid());
}

#[test]
fn coeffs_default_is_bypass() {
    let c = BiquadCoeffs::default();
    assert_eq!(c, BiquadCoeffs::BYPASS);
}

// ── BiquadState 布局 ────────────────────────────────────────────────────

#[test]
fn biquad_state_layout_is_16_bytes() {
    assert_eq!(std::mem::size_of::<BiquadState>(), 16);
    assert_eq!(std::mem::align_of::<BiquadState>(), 8);
}

#[test]
fn biquad_state_clear_after_non_finite() {
    let mut state = BiquadState::new();
    let coeffs = BiquadCoeffs {
        b0: f32::INFINITY,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };
    let out = state.process_sample(&coeffs, 1.0);
    assert_eq!(out, 0.0);
    assert_eq!(state.s1, 0.0);
    assert_eq!(state.s2, 0.0);
}

// ── compute_coeffs ──────────────────────────────────────────────────────

#[test]
fn peaking_zero_gain_is_passthrough() {
    let c = compute_coeffs(BiquadType::Peaking, 1000.0, 0.0, 1.0, 48000);
    assert!(c.is_valid());
    let mut filter = BiquadFilter::new(c, BiquadStructure::DirectFormIITransposed);
    filter.initialize(48000, &vec!["L".to_owned()]);
    let mut samples = vec![vec![0.5, -0.3, 0.8, -0.1, 0.0]];
    let input = samples[0].clone();
    filter.process(&mut samples, 5);
    for f in 0..5 {
        assert!(
            (samples[0][f] - input[f]).abs() < 0.01,
            "0dB peaking should passthrough: frame {} got {} expected {}",
            f, samples[0][f], input[f]
        );
    }
}

#[test]
fn lowpass_coeffs_valid() {
    let c = compute_coeffs(BiquadType::LowPass, 1000.0, 0.0, 0.707, 48000);
    assert!(c.is_valid());
}

#[test]
fn highpass_coeffs_valid() {
    let c = compute_coeffs(BiquadType::HighPass, 1000.0, 0.0, 0.707, 48000);
    assert!(c.is_valid());
}

#[test]
fn pass_filters_gain_scales_passband() {
    // HPF 通带在 Nyquist（z=-1）、LPF 通带在 DC（z=1）：
    // |H| 应等于线性增益，0 dB 恒为 1（纯滤波），+6 dB ≈ 1.995。
    fn mag_at(coeffs: &BiquadCoeffs, z: f64) -> f64 {
        let num = coeffs.b0 as f64 + coeffs.b1 as f64 * z + coeffs.b2 as f64 * z * z;
        let den = 1.0 + coeffs.a1 as f64 * z + coeffs.a2 as f64 * z * z;
        (num / den).abs()
    }

    let hp0 = compute_coeffs(BiquadType::HighPass, 1000.0, 0.0, 0.707, 48000);
    let hp6 = compute_coeffs(BiquadType::HighPass, 1000.0, 6.0, 0.707, 48000);
    let unity = mag_at(&hp0, -1.0);
    let ratio = mag_at(&hp6, -1.0) / unity;
    assert!(
        (ratio - 10f64.powf(6.0 / 20.0)).abs() < 1e-3,
        "HPF 通带增益应为 +6 dB，实际 ratio={ratio}"
    );

    let lp0 = compute_coeffs(BiquadType::LowPass, 1000.0, 0.0, 0.707, 48000);
    let lp6 = compute_coeffs(BiquadType::LowPass, 1000.0, 6.0, 0.707, 48000);
    let unity_lp = mag_at(&lp0, 1.0);
    let ratio_lp = mag_at(&lp6, 1.0) / unity_lp;
    assert!(
        (ratio_lp - 10f64.powf(6.0 / 20.0)).abs() < 1e-3,
        "LPF 通带增益应为 +6 dB，实际 ratio={ratio_lp}"
    );
}

#[test]
fn lowshelf_coeffs_valid() {
    let c = compute_coeffs(BiquadType::LowShelf, 200.0, 6.0, 0.707, 48000);
    assert!(c.is_valid());
}

#[test]
fn highshelf_coeffs_valid() {
    let c = compute_coeffs(BiquadType::HighShelf, 5000.0, -3.0, 0.707, 48000);
    assert!(c.is_valid());
}

#[test]
fn bandpass_coeffs_valid() {
    let c = compute_coeffs(BiquadType::BandPass, 1000.0, 0.0, 2.0, 48000);
    assert!(c.is_valid());
}

#[test]
fn notch_coeffs_valid() {
    let c = compute_coeffs(BiquadType::Notch, 1000.0, 0.0, 10.0, 48000);
    assert!(c.is_valid());
}

#[test]
fn allpass_coeffs_valid() {
    let c = compute_coeffs(BiquadType::AllPass, 1000.0, 0.0, 1.0, 48000);
    assert!(c.is_valid());
}

// ── P0 边界护栏 ─────────────────────────────────────────────────────────

#[test]
fn extreme_gain_clamps_and_stays_finite() {
    // +1000 dB → clamp 到 +48 dB：系数必须有限且稳定（不再溢出/贴单位圆）。
    let c = compute_coeffs(BiquadType::Peaking, 1000.0, 1000.0, 1.0, 48000);
    assert!(c.is_valid());
    assert!(is_stable_biquad(c.a1, c.a2));

    let c2 = compute_coeffs(BiquadType::LowShelf, 1000.0, 1000.0, 1.0, 48000);
    assert!(c2.is_valid());
    assert!(is_stable_biquad(c2.a1, c2.a2));
}

#[test]
fn extreme_cut_uses_stable_floor_not_bypass() {
    // -1000 dB → clamp 到 -120 dB → 极点贴单位圆不稳定 → 回退 -60 dB 稳定深切，
    // 而非整段直通（直通会让极端衰减看起来“没生效”）。
    let c = compute_coeffs(BiquadType::Peaking, 1000.0, -1000.0, 1.0, 48000);
    assert_ne!(c, BiquadCoeffs::BYPASS);
    assert!(c.is_valid());
    assert!(is_stable_biquad(c.a1, c.a2));
    assert!(c.a2.abs() < 0.99, "深切地板后极点不应再贴单位圆，a2={}", c.a2);
}

#[test]
fn invalid_inputs_fall_back_to_bypass() {
    assert_eq!(compute_coeffs(BiquadType::Peaking, 1000.0, 0.0, 1.0, 0), BiquadCoeffs::BYPASS);
    assert_eq!(
        compute_coeffs(BiquadType::Peaking, f32::NAN, 0.0, 1.0, 48000),
        BiquadCoeffs::BYPASS
    );
}

#[test]
fn fc_beyond_nyquist_clamps() {
    // 100 kHz @ 48 kHz → clamp 到 0.45*48k = 21.6 kHz，系数仍有效。
    let c = compute_coeffs(BiquadType::LowPass, 100_000.0, 0.0, 0.707, 48000);
    assert!(c.is_valid());
}

#[test]
fn impulse_response_decays_no_whistle() {
    // +48 dB peaking（clamp 后最大合法增益）：脉冲响应必须在有限时间内衰减。
    let c = compute_coeffs(BiquadType::Peaking, 1000.0, 1000.0, 1.0, 48000);
    let mut filter = BiquadFilter::new(c, BiquadStructure::DirectFormIITransposed);
    filter.initialize(48000, &vec!["L".to_owned()]);

    let mut samples = vec![vec![0.0f32; 8192]];
    samples[0][0] = 1.0;
    filter.process(&mut samples, 8192);

    let tail = &samples[0][4096..];
    let peak = tail.iter().fold(0.0f32, |m, &v| m.max(v.abs()));
    assert!(
        peak < 1e-3,
        "impulse response must decay, peak tail = {peak}"
    );
}

// ── BiquadFilter — Direct Form I ────────────────────────────────────────

#[test]
fn df1_bypass_passthrough() {
    let mut filter = BiquadFilter::new(BiquadCoeffs::BYPASS, BiquadStructure::DirectFormI);
    filter.initialize(48000, &stereo_channels());

    let mut samples = make_stereo_buf(4);
    samples[0] = vec![1.0, 2.0, 3.0, 4.0];
    samples[1] = vec![0.5, 1.0, 1.5, 2.0];

    filter.process(&mut samples, 4);

    for f in 0..4 {
        assert!((samples[0][f] - (f as f32 + 1.0)).abs() < 0.001);
    }
}

#[test]
fn df1_silence_stays_silent() {
    let coeffs = compute_coeffs(BiquadType::Peaking, 1000.0, 6.0, 1.0, 48000);
    let mut filter = BiquadFilter::new(coeffs, BiquadStructure::DirectFormI);
    filter.initialize(48000, &stereo_channels());

    let mut samples = make_stereo_buf(100);
    filter.process(&mut samples, 100);

    for f in 0..100 {
        assert!(samples[0][f].abs() < 1e-10);
    }
}

// ── BiquadFilter — Direct Form II ───────────────────────────────────────

#[test]
fn df2_bypass_passthrough() {
    let mut filter = BiquadFilter::new(BiquadCoeffs::BYPASS, BiquadStructure::DirectFormII);
    filter.initialize(48000, &stereo_channels());

    let mut samples = make_stereo_buf(4);
    samples[0] = vec![1.0, 2.0, 3.0, 4.0];

    filter.process(&mut samples, 4);

    for f in 0..4 {
        assert!((samples[0][f] - (f as f32 + 1.0)).abs() < 0.001);
    }
}

// ── BiquadFilter — Direct Form II Transposed ────────────────────────────

#[test]
fn df2t_bypass_passthrough() {
    let mut filter = BiquadFilter::new(
        BiquadCoeffs::BYPASS,
        BiquadStructure::DirectFormIITransposed,
    );
    filter.initialize(48000, &stereo_channels());

    let mut samples = make_stereo_buf(4);
    samples[0] = vec![1.0, 2.0, 3.0, 4.0];

    filter.process(&mut samples, 4);

    for f in 0..4 {
        assert!((samples[0][f] - (f as f32 + 1.0)).abs() < 0.001);
    }
}

// ── 三种结构结果一致性 ──────────────────────────────────────────────────

#[cfg(target_arch = "x86_64")]
#[test]
fn stereo_simd_matches_scalar_channels() {
    // 立体声 SIMD 路径（左右同系数）必须与两个独立单声道标量路径一致。
    let coeffs = compute_coeffs(BiquadType::Peaking, 1000.0, 6.0, 1.0, 48000);
    let len = 512;
    let input_l: Vec<f32> = (0..len)
        .map(|i| ((i as f32 * 0.07).sin() * 0.5) as f32)
        .collect();
    let input_r: Vec<f32> = (0..len)
        .map(|i| ((i as f32 * 0.13).cos() * 0.4) as f32)
        .collect();

    // SIMD 路径：2 通道 BiquadFilter。
    let mut stereo = BiquadFilter::new(coeffs, BiquadStructure::DirectFormIITransposed);
    stereo.initialize(48000, &vec!["L".to_owned(), "R".to_owned()]);
    let mut samples = vec![input_l.clone(), input_r.clone()];
    stereo.process(&mut samples, len);

    // 标量路径：每通道一个 1 通道 BiquadFilter。
    let mut mono_l = BiquadFilter::new(coeffs, BiquadStructure::DirectFormIITransposed);
    mono_l.initialize(48000, &vec!["L".to_owned()]);
    let mut sl = vec![input_l.clone()];
    mono_l.process(&mut sl, len);

    let mut mono_r = BiquadFilter::new(coeffs, BiquadStructure::DirectFormIITransposed);
    mono_r.initialize(48000, &vec!["R".to_owned()]);
    let mut sr = vec![input_r.clone()];
    mono_r.process(&mut sr, len);

    for f in 0..len {
        assert!(
            (samples[0][f] - sl[0][f]).abs() < 1e-9,
            "L mismatch at {f}: simd={} scalar={}",
            samples[0][f],
            sl[0][f]
        );
        assert!(
            (samples[1][f] - sr[0][f]).abs() < 1e-9,
            "R mismatch at {f}: simd={} scalar={}",
            samples[1][f],
            sr[0][f]
        );
    }
}

#[cfg(target_arch = "x86_64")]
#[test]
fn non_stereo_falls_back_to_scalar() {
    // 3 通道不回退到 SIMD，标量路径仍正确。
    let coeffs = compute_coeffs(BiquadType::Peaking, 500.0, 3.0, 1.0, 48000);
    let mut filter = BiquadFilter::new(coeffs, BiquadStructure::DirectFormIITransposed);
    let names = vec!["L".to_owned(), "R".to_owned(), "C".to_owned()];
    filter.initialize(48000, &names);

    let mut samples = vec![vec![0.0f32; 100]; 3];
    samples[0][0] = 1.0;
    samples[1][0] = 0.5;
    samples[2][0] = 0.25;
    filter.process(&mut samples, 100);

    // 三通道都处理且输出有限。
    for ch in samples.iter() {
        assert!(ch.iter().all(|v| v.is_finite()));
        assert!(ch.iter().any(|&v| v != 0.0));
    }
}

#[test]
fn single_channel_scope_leaves_other_channel_untouched() {
    // 模拟 `Channel: L` 语义：initialize 只收到 1 个通道名 → num_channels == 1，
    // 即使 samples 是 2 缓冲也不触发 SIMD，且作用域外通道必须保持原样。
    let coeffs = compute_coeffs(BiquadType::Peaking, 1000.0, 6.0, 1.0, 48000);
    let mut filter = BiquadFilter::new(coeffs, BiquadStructure::DirectFormIITransposed);
    filter.initialize(48000, &vec!["L".to_owned()]);

    let mut samples = vec![vec![0.5f32; 64], vec![0.25f32; 64]];
    let r_orig = samples[1].clone();
    filter.process(&mut samples, 64);

    assert_eq!(samples[1], r_orig, "作用域外的通道必须保持原样");
    assert!(
        samples[0].iter().any(|&v| v != 0.5),
        "作用域内通道应被滤波（瞬态段应有变化）"
    );
}

#[test]
fn channel_indices_route_to_non_leading_slot() {
    // 模拟 `Channel: R`：槽位索引 [1]，即使缓冲是 2 通道也只处理 R。
    let coeffs = compute_coeffs(BiquadType::Peaking, 1000.0, 6.0, 1.0, 48000);
    let mut filter = BiquadFilter::new(coeffs, BiquadStructure::DirectFormIITransposed);
    filter.set_channel_indices(&[1]);
    filter.initialize(48000, &vec!["R".to_owned()]);

    let mut samples = vec![vec![0.5f32; 64], vec![0.5f32; 64]];
    let l_orig = samples[0].clone();
    filter.process(&mut samples, 64);

    assert_eq!(samples[0], l_orig, "槽位 0（L）不能被处理");
    assert!(
        samples[1].iter().any(|&v| v != 0.5),
        "槽位 1（R）应被滤波（瞬态段应有变化）"
    );
}

#[cfg(target_arch = "x86_64")]
#[test]
fn stereo_simd_with_non_leading_slots_matches_scalar() {
    // 选中槽位 [2, 1]（如 `Channel: C R`）：SIMD 两路必须作用于正确槽位。
    let coeffs = compute_coeffs(BiquadType::Peaking, 1000.0, 6.0, 1.0, 48000);
    let len = 256;
    let input_c: Vec<f32> = (0..len).map(|i| ((i as f32 * 0.05).sin() * 0.3) as f32).collect();
    let input_r: Vec<f32> = (0..len).map(|i| ((i as f32 * 0.11).cos() * 0.4) as f32).collect();

    // SIMD 路径：2 通道作用域，槽位 [2, 1]。
    let mut stereo = BiquadFilter::new(coeffs, BiquadStructure::DirectFormIITransposed);
    stereo.set_channel_indices(&[2, 1]);
    stereo.initialize(48000, &vec!["C".to_owned(), "R".to_owned()]);
    let mut samples = vec![vec![0.5f32; len], input_r.clone(), input_c.clone()];
    let l_orig = samples[0].clone();
    stereo.process(&mut samples, len);

    // 标量参考：单通道滤波器分别作用在槽位 2 和槽位 1。
    let mut mono_c = BiquadFilter::new(coeffs, BiquadStructure::DirectFormIITransposed);
    mono_c.set_channel_indices(&[2]);
    mono_c.initialize(48000, &vec!["C".to_owned()]);
    let mut sc = vec![
        vec![0.0f32; len],
        vec![0.0f32; len],
        input_c.clone(),
    ];
    mono_c.process(&mut sc, len);

    let mut mono_r = BiquadFilter::new(coeffs, BiquadStructure::DirectFormIITransposed);
    mono_r.set_channel_indices(&[1]);
    mono_r.initialize(48000, &vec!["R".to_owned()]);
    let mut sr = vec![vec![0.0f32; len], input_r.clone()];
    mono_r.process(&mut sr, len);

    assert_eq!(samples[0], l_orig, "槽位 0 不在作用域，必须保持不变");
    for f in 0..len {
        assert!(
            (samples[1][f] - sr[1][f]).abs() < 1e-9,
            "R(slot1) mismatch at {f}: simd={} scalar={}",
            samples[1][f],
            sr[1][f]
        );
        assert!(
            (samples[2][f] - sc[2][f]).abs() < 1e-9,
            "C(slot2) mismatch at {f}: simd={} scalar={}",
            samples[2][f],
            sc[2][f]
        );
    }
}

#[test]
fn three_structures_same_output() {
    let coeffs = compute_coeffs(BiquadType::Peaking, 1000.0, 6.0, 1.0, 48000);
    let input = vec![0.5, -0.3, 0.8, -0.1, 0.0, 0.2, -0.7, 0.4];

    let mut results = Vec::new();
    for structure in &[
        BiquadStructure::DirectFormI,
        BiquadStructure::DirectFormII,
        BiquadStructure::DirectFormIITransposed,
    ] {
        let mut filter = BiquadFilter::new(coeffs, *structure);
        filter.initialize(48000, &vec!["L".to_owned()]);
        let mut samples = vec![input.clone()];
        filter.process(&mut samples, input.len());
        results.push(samples[0].clone());
    }

    for f in 0..input.len() {
        assert!(
            (results[0][f] - results[1][f]).abs() < 1e-6,
            "DF1 vs DF2 diff at frame {}: {} vs {}",
            f, results[0][f], results[1][f]
        );
        assert!(
            (results[0][f] - results[2][f]).abs() < 1e-6,
            "DF1 vs DF2T diff at frame {}: {} vs {}",
            f, results[0][f], results[2][f]
        );
    }
}

// ── set_coeffs / reset_state ─────────────────────────────────────────────

#[test]
fn update_coeffs() {
    let mut filter = BiquadFilter::new(BiquadCoeffs::BYPASS, BiquadStructure::DirectFormI);
    filter.initialize(48000, &stereo_channels());

    let new_coeffs = compute_coeffs(BiquadType::LowPass, 500.0, 0.0, 0.707, 48000);
    filter.set_coeffs(new_coeffs);
    assert_eq!(filter.coeffs(), new_coeffs);
}

#[test]
fn reset_clears_state() {
    let coeffs = compute_coeffs(BiquadType::Peaking, 1000.0, 6.0, 1.0, 48000);
    let mut filter = BiquadFilter::new(coeffs, BiquadStructure::DirectFormIITransposed);
    filter.initialize(48000, &stereo_channels());

    let mut samples = make_stereo_buf(10);
    samples[0][0] = 1.0;
    filter.process(&mut samples, 10);

    filter.reset_state();

    let mut zeros = make_stereo_buf(10);
    filter.process(&mut zeros, 10);
    for f in 0..10 {
        assert!(zeros[0][f].abs() < 1e-10);
    }
}

// ── latency ─────────────────────────────────────────────────────────────

#[test]
fn biquad_zero_latency() {
    let filter = BiquadFilter::new(BiquadCoeffs::BYPASS, BiquadStructure::DirectFormI);
    assert_eq!(filter.latency(), 0);
}
