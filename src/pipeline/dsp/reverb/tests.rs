use super::*;

#[test]
fn silence_stays_silent() {
    let mut f = ReverbFilter::new(ReverbParams::default());
    f.initialize(48000, &["L".into(), "R".into()]);
    let mut samples = vec![vec![0.0f32; 480]; 2];
    f.process(&mut samples, 480);
    for ch in &samples {
        for &v in ch {
            assert!(v.abs() < 1e-9);
        }
    }
}

#[test]
fn dry_only_is_exact_passthrough() {
    let params = ReverbParams {
        wet: 0.0,
        dry: 1.0,
        ..Default::default()
    };
    let mut f = ReverbFilter::new(params);
    f.initialize(48000, &["L".into(), "R".into()]);
    let mut samples = vec![vec![0.0f32; 4800]; 2];
    for i in 0..4800 {
        samples[0][i] = (core::f32::consts::TAU * 440.0 * i as f32 / 48000.0).sin() * 0.5;
        samples[1][i] = samples[0][i] * 0.5;
    }
    let orig = samples.clone();
    f.process(&mut samples, 4800);
    for ch in 0..2 {
        for i in 0..4800 {
            assert_eq!(samples[ch][i], orig[ch][i]);
        }
    }
}

#[test]
fn low_cut_preserves_low_transients() {
    // 60Hz 纯音、全湿：low_cut=200 时低频被保护直通（输出≈输入）；
    // low_cut=20（关闭）时低频进混响被处理。
    let run = |low_cut_hz: f32| -> f32 {
        let mut f = ReverbFilter::new(ReverbParams {
            low_cut_hz,
            wet: 1.0,
            dry: 0.0,
            ..Default::default()
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let n = 48_000usize;
        let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = 0.4 * (core::f32::consts::TAU * 60.0 * i as f32 / 48000.0).sin();
            s[0][i] = v;
            s[1][i] = v;
        }
        f.process(&mut s, n);
        s[0][24_000..].iter().fold(0.0f32, |m, &v| m.max(v.abs()))
    };
    let input_peak = 0.4;
    let kept = run(200.0);
    let wet = run(20.0);
    assert!(
        (kept - input_peak).abs() < 0.08,
        "low_cut=200 must keep 60Hz intact, got {kept}"
    );
    assert!(
        wet < input_peak - 0.1 || wet > input_peak + 0.1,
        "low_cut=20 lets reverb process the low, got {wet}"
    );
}

#[test]
fn non_silent_is_finite() {
    let mut f = ReverbFilter::new(ReverbParams::default());
    f.initialize(48000, &["L".into(), "R".into()]);
    let mut samples = vec![vec![0.0f32; 4800]; 2];
    for i in 0..4800 {
        samples[0][i] = (core::f32::consts::TAU * 440.0 * i as f32 / 48000.0).sin() * 0.5;
        samples[1][i] = samples[0][i] * 0.5;
    }
    f.process(&mut samples, 4800);
    for ch in &samples {
        for &v in ch {
            assert!(v.is_finite());
        }
    }
}

#[test]
fn max_params_bounded_across_sample_rates() {
    let params = ReverbParams {
        room_size: 1.5,
        decay: 1.0,
        density: 1.0,
        pre_delay_ms: 100.0,
        motion_depth: 2.0,
        motion_rate: 2.0,
        wet: 1.0,
        dry: 0.0,
        ..Default::default()
    };
    for sr in [44_100u32, 48_000, 96_000] {
        let mut f = ReverbFilter::new(params);
        f.initialize(sr, &["L".into(), "R".into()]);
        let mut samples = vec![vec![0.0f32; 4800], vec![0.0f32; 4800]];
        for i in 0..4800 {
            samples[0][i] =
                (core::f32::consts::TAU * 440.0 * i as f32 / sr as f32).sin() * 0.9;
            samples[1][i] = samples[0][i] * 0.5;
        }
        f.process(&mut samples, 4800);
        for ch in &samples {
            for &v in ch {
                assert!(v.is_finite());
            }
        }
    }
}

#[test]
fn mono_has_wet_tail() {
    let mut f = ReverbFilter::new(ReverbParams::default());
    f.initialize(48000, &["Mono".into()]);
    let mut samples = vec![vec![0.0f32; 4800]];
    for i in 0..4800 {
        samples[0][i] =
            (core::f32::consts::TAU * 220.0 * i as f32 / 48000.0).sin() * 0.5;
    }
    f.process(&mut samples, 4800);
    for &v in &samples[0] {
        assert!(v.is_finite());
    }
    // 湿声应存在（非纯直通）且幅度有界。
    let peak = samples[0].iter().map(|v| v.abs()).fold(0.0f32, f32::max);
    assert!(peak > 0.001);
    assert!(peak < 2.0);
}

#[test]
fn out_of_range_params_are_clamped() {
    // 越界/非有限参数不应让环路发散：DSP 入口 clamp 后输出保持有界。
    let params = ReverbParams {
        room_size: 99.0,
        decay: 5.0,
        damping: -3.0,
        bandwidth: 2.0,
        density: -1.0,
        lat5: 4.0,
        lat6: -2.0,
        pre_delay_ms: 1e6,
        motion_rate: 0.0,
        motion_depth: 50.0,
        wet: 2.0,
        dry: -1.0,
        ..Default::default()
    };
    let mut f = ReverbFilter::new(params);
    f.initialize(48000, &["L".into(), "R".into()]);
    let mut samples = vec![vec![0.0f32; 4800], vec![0.0f32; 4800]];
    for i in 0..4800 {
        samples[0][i] = (core::f32::consts::TAU * 440.0 * i as f32 / 48000.0).sin() * 0.9;
        samples[1][i] = samples[0][i] * 0.5;
    }
    f.process(&mut samples, 4800);
    for ch in &samples {
        for &v in ch {
            assert!(v.is_finite());
            assert!(v.abs() < 4.0, "clamp 后输出应保持有界，实际 {v}");
        }
    }
}
