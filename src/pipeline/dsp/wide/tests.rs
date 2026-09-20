use super::*;

/// 临时诊断：Kaiser 分频在各采样率的停带表现（500Hz 衰减应 ≥-55dB）。
#[test]
fn kaiser_crossover_response() {
    fn lp_gain_db(ir: &[f32], freq: f32, sr: u32) -> f32 {
        let w = std::f32::consts::TAU * freq / sr as f32;
        let mut re = 0.0f32;
        let mut im = 0.0f32;
        for (i, &h) in ir.iter().enumerate() {
            re += h * (w * i as f32).cos();
            im -= h * (w * i as f32).sin();
        }
        20.0 * (re * re + im * im).sqrt().max(1e-6).log10()
    }
    for sr in [44_100u32, 48_000, 96_000, 192_000, 384_000] {
        let n = wide_fir_len(sr, 200.0);
        let ir = design_lowpass_ir(200.0, sr, n);
        eprintln!(
            "DIAG kaiser sr={} n={} lp200={:.1}dB lp500={:.1}dB",
            sr,
            n,
            lp_gain_db(&ir, 200.0, sr),
            lp_gain_db(&ir, 500.0, sr),
        );
    }
}

#[test]
fn all_zero_is_passthrough() {
    let mut f = WideFilter::new(WideParams {
        gain: 0.0,
        air: 0.0,
        ..Default::default()
    });
    f.initialize(48000, &["L".into(), "R".into()]);
    let mut samples = vec![vec![0.4f32; 256], vec![0.2f32; 256]];
    let before = samples.clone();
    f.process(&mut samples, 256);
    for (a, b) in samples.iter().zip(before.iter()) {
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(*x, *y, "air 0 / gain 0 must be bit-exact passthrough");
        }
    }
}

#[test]
fn mono_is_passthrough() {
    let mut f = WideFilter::new(WideParams { air: 1.0, ..Default::default() });
    f.initialize(48000, &["Mono".into()]);
    let mut samples = vec![vec![0.8f32; 64]];
    f.process(&mut samples, 64);
    for &v in &samples[0] {
        assert_eq!(v, 0.8);
    }
}

#[test]
fn silence_stays_silent() {
    let mut f = WideFilter::new(WideParams { air: 1.0, ..Default::default() });
    f.initialize(48000, &["L".into(), "R".into()]);
    let mut samples = vec![vec![0.0f32; 4800], vec![0.0f32; 4800]];
    f.process(&mut samples, 4800);
    for ch in &samples {
        for &v in ch {
            assert_eq!(v, 0.0);
        }
    }
}

/// 稳态段上 |L-R| 的 RMS（宽度度量）。
fn width_rms(samples: &[Vec<f32>], start: usize) -> f32 {
    let n = samples[0].len() - start;
    let mut sum = 0.0f32;
    for i in start..samples[0].len() {
        let d = samples[0][i] - samples[1][i];
        sum += d * d;
    }
    (sum / n as f32).sqrt()
}

#[test]
fn side_signal_is_widened() {
    // 低频（300Hz，直通路径）与高频（5kHz，ITD 路径）带侧成分：
    // 宽度应随用户 Gain 单调递增（避开线性相位 FIR 分离的
    // 梳状谷频率，如 945Hz/1.9kHz）。
    let run = |gain: f32| -> f32 {
        let mut f = WideFilter::new(WideParams {
            gain,
            mix: 1.0,
            ..Default::default()
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let n = 4800usize;
        let mut samples = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let t = i as f32 / 48000.0;
            let v = 0.4 * (core::f32::consts::TAU * 300.0 * t).sin()
                + 0.3 * (core::f32::consts::TAU * 5000.0 * t).sin();
            samples[0][i] = 0.2 * v;
            samples[1][i] = 0.04 * v;
        }
        f.process(&mut samples, n);
        width_rms(&samples, 2000)
    };

    let base = run(0.0);
    let quarter = run(0.25);
    let half = run(0.5);
    let full = run(1.0);
    assert!(
        quarter > base && half > quarter && full > half,
        "width should grow monotonically with Gain: {base} < {quarter} < {half} < {full}"
    );
    assert!(
        full / base > 1.35,
        "full Gain should clearly widen: {full} vs {base}"
    );
    assert!(
        half / base > 1.1,
        "mid Gain should already widen: {half} vs {base}"
    );
}

#[test]
fn gain_mapping_curve() {
    // 侧增益只随用户 Gain（1 + 1.5·Gain）。
    let setup = |gain: f32| -> f32 {
        let mut f = WideFilter::new(WideParams {
            gain,
            ..Default::default()
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        f.gain_side_high
    };
    assert!((setup(0.0) - 1.0).abs() < 1e-5, "Gain=0 side must stay flat");
    assert!((setup(0.5) - 1.75).abs() < 1e-4);
    assert!((setup(1.0) - 2.5).abs() < 1e-4, "Gain=1 side gain should be 2.5x");
}

#[test]
fn air_depth_drives_center_attenuation() {
    // 空气吸收深度只由 air 参数控制：8k 纯中置正弦，
    // air 越大衰减越深，air=0 时保持原样。
    fn center_8k_energy(air: f32) -> f32 {
        let mut f = WideFilter::new(WideParams {
            air,
            mix: 1.0,
            ..Default::default()
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let n = 9600usize;
        let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = 0.3 * (core::f32::consts::TAU * 8000.0 * i as f32 / 48000.0).sin();
            s[0][i] = v;
            s[1][i] = v;
        }
        f.process(&mut s, n);
        let mut e = 0.0f32;
        for i in 4800..n {
            let m = (s[0][i] + s[1][i]) * 0.5;
            e += m * m;
        }
        e
    }
    let e_none = center_8k_energy(0.0);
    let e_half = center_8k_energy(0.5);
    let e_full = center_8k_energy(1.0);
    assert!(e_none > 0.0);
    assert!(
        e_half < e_none * 0.95 && e_full < e_half * 0.95,
        "air should attenuate monotonically: none={e_none} half={e_half} full={e_full}"
    );
}

#[test]
fn side_air_attenuates_side_only() {
    // air_side：纯侧 8k 信号被衰减（且只影响侧、不影响 mid）；
    // air_side=0 时侧保持原样。
    fn side_8k_energy(air_side: f32) -> f32 {
        let mut f = WideFilter::new(WideParams {
            air_side,
            gain: 0.0,
            air: 0.0,
            ..Default::default()
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let n = 9600usize;
        let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = 0.3 * (core::f32::consts::TAU * 8000.0 * i as f32 / 48000.0).sin();
            s[0][i] = v;
            s[1][i] = -v;
        }
        f.process(&mut s, n);
        let mut e = 0.0f32;
        for i in 4800..n {
            let d = (s[0][i] - s[1][i]) * 0.5;
            e += d * d;
        }
        e
    }
    let e0 = side_8k_energy(0.0);
    let e1 = side_8k_energy(1.0);
    assert!(e0 > 0.0);
    assert!(
        e1 < e0 * 0.85,
        "air_side should attenuate side highs: {e1} vs {e0}"
    );

    // air_side 不影响纯中心信号。
    let mut f = WideFilter::new(WideParams {
        air_side: 1.0,
        air: 0.0,
        ..Default::default()
    });
    f.initialize(48000, &["L".into(), "R".into()]);
    let n = 9600usize;
    let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
    for i in 0..n {
        let v = 0.3 * (core::f32::consts::TAU * 8000.0 * i as f32 / 48000.0).sin();
        s[0][i] = v;
        s[1][i] = v;
    }
    f.process(&mut s, n);
    let mut e = 0.0f32;
    for i in 4800..n {
        let m = (s[0][i] + s[1][i]) * 0.5;
        e += m * m;
    }
    // 中心无 mid air：能量应接近输入（air_side 不碰 mid）。
    assert!(
        e > 0.05,
        "air_side must not affect center, energy {e}"
    );
}

#[test]
fn center_signal_preserved_without_air_and_symmetric() {
    // 纯中央信号、无空气吸收：mid 不做任何静态负增益，
    // 输出应保持原电平（无中频糊感来源）。
    let mut f = WideFilter::new(WideParams { air: 0.0, ..Default::default() });
    f.initialize(48000, &["L".into(), "R".into()]);
    let n = 4800usize;
    let mut samples = vec![vec![0.0f32; n], vec![0.0f32; n]];
    for i in 0..n {
        let v = 0.1 * (core::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin();
        samples[0][i] = v;
        samples[1][i] = v;
    }
    f.process(&mut samples, n);
    let peak = samples[0][2000..]
        .iter()
        .fold(0.0f32, |m, &v| m.max(v.abs()));
    for i in 2000..n {
        assert!(
            (samples[0][i] - samples[1][i]).abs() < 1e-6,
            "center signal must stay symmetric"
        );
    }
    // 峰值应接近输入 0.1（无负增益、无梳状、无压缩）。
    assert!(
        peak > 0.095 && peak < 0.105,
        "center must be preserved without air, peak {peak}"
    );
}

#[test]
fn bass_keeps_energy_and_width_highs_widened() {
    // 低频干净直通：60 Hz 带侧成分原样通过（线性相位 FIR 不破坏瞬态）；
    // 1 kHz 侧成分被用户 Gain 放大。
    let run = |freq: f32, n: usize| -> (f32, f32) {
        let mut f = WideFilter::new(WideParams {
            gain: 1.0,
            mix: 1.0,
            ..Default::default()
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let mut samples = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = (core::f32::consts::TAU * freq * i as f32 / 48000.0).sin();
            samples[0][i] = 0.5 * v;
            samples[1][i] = 0.1 * v;
        }
        f.process(&mut samples, n);
        let mut diff = 0.0f32;
        let mut peak_l = 0.0f32;
        for i in (n / 2)..n {
            diff = diff.max((samples[0][i] - samples[1][i]).abs());
            peak_l = peak_l.max(samples[0][i].abs());
        }
        (diff, peak_l)
    };

    let (diff_low, peak_low) = run(60.0, 9600);
    let (diff_high, _) = run(5000.0, 4800);
    assert!(
        (peak_low - 0.5).abs() < 0.02,
        "bass must stay at original level: peak {peak_low}"
    );
    assert!(
        (diff_low - 0.4).abs() < 0.03,
        "bass keeps original stereo info: diff {diff_low}"
    );
    // 5kHz 在 ITD 泛音区（避开线性相位 FIR 分离的梳状谷）。
    assert!(diff_high > 0.45, "highs should be widened: high {diff_high}");
}

#[test]
fn fir_split_reconstructs_delayed_input() {
    // 线性相位 FIR 完美重建：低频支路 + 高频支路 = 延迟 center 帧的原信号
    // （逐样本，含相位——这是 FIR 相对 IIR 分频的核心优势）。
    let mut f = WideFilter::new(WideParams { air: 1.0, ..Default::default() });
    f.initialize(48000, &["L".into(), "R".into()]);
    // 只测 FIR 分频本身：用 FIR 群延迟中心，不含 Haas 延迟。
    let center = ((wide_fir_len(48000, 200.0) - 1) / 2) as usize;
    let n = 4800usize;
    let input: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 / 48000.0;
            0.3 * (core::f32::consts::TAU * 60.0 * t).sin()
                + 0.4 * (core::f32::consts::TAU * 1000.0 * t).sin()
                + 0.2 * (core::f32::consts::TAU * 6000.0 * t).sin()
        })
        .collect();
    let mut max_err = 0.0f32;
    for i in 0..n {
        let (lp, hp) = f.fir.split_channel(0, input[i]);
        if i >= center {
            max_err = max_err.max((lp + hp - input[i - center]).abs());
        }
    }
    assert!(
        max_err < 1.0e-4,
        "FIR split must reconstruct delayed input, max err {max_err}"
    );
}

#[test]
fn fir_split_reconstructs_partitioned_high_rate() {
    // 192k：4096 抽头走分块 FFT——低通支路 + 互补高通必须仍等于
    // 延迟 latency 帧的原信号（延迟对齐环正确性）。
    let mut f = WideFilter::new(WideParams { air: 1.0, ..Default::default() });
    f.initialize(192_000, &["L".into(), "R".into()]);
    // 192k/200Hz：8192 抽头走分块 FFT；分块延迟 = 报告延迟 − 1
    // （报告值含 1 样本整体补偿）。
    let latency = (f.latency() - 1) as usize;
    let n = 4000usize;
    let input: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 / 192_000.0;
            0.3 * (core::f32::consts::TAU * 60.0 * t).sin()
                + 0.4 * (core::f32::consts::TAU * 1000.0 * t).sin()
                + 0.2 * (core::f32::consts::TAU * 6000.0 * t).sin()
        })
        .collect();
    let mut max_err = 0.0f32;
    for i in 0..n {
        let (lp, hp) = f.fir.split_channel(0, input[i]);
        if i >= latency {
            max_err = max_err.max((lp + hp - input[i - latency]).abs());
        }
    }
    assert!(
        max_err < 1.0e-4,
        "partitioned split must reconstruct delayed input, max err {max_err}"
    );
}

#[test]
fn fir_delays_by_center_samples() {
    // 单脉冲经低通 FIR 的主峰应出现在 center 帧（对称 FIR 群延迟）。
    let mut f = WideFilter::new(WideParams { air: 1.0, ..Default::default() });
    f.initialize(48000, &["L".into(), "R".into()]);
    let center = ((wide_fir_len(48000, 200.0) - 1) / 2) as usize;
    let n = center + 128;
    let mut best = 0usize;
    let mut best_v = 0.0f32;
    for i in 0..n {
        let x = if i == 0 { 1.0 } else { 0.0 };
        let (lp, _) = f.fir.split_channel(0, x);
        if lp.abs() > best_v {
            best_v = lp.abs();
            best = i;
        }
    }
    assert_eq!(best, center, "LP peak should be at FIR center");
}

#[test]
fn latency_is_fir_center() {
    let mut f = WideFilter::new(WideParams { air: 1.0, ..Default::default() });
    f.initialize(48000, &["L".into(), "R".into()]);
    // FIR 中心 + 1 样本（HPF 群延迟补偿）。
    assert_eq!(f.latency(), ((wide_fir_len(48000, 200.0) - 1) / 2) as u32 + 1);
}

#[test]
fn air_absorption_follows_physical_curve() {
    // 纯中置输入（L==R）：空气吸收应随频率升高衰减加深（物理曲线形状），
    // 且比原单极点低通柔和——1k 处衰减不超过 -3dB，16k 处明显更深。
    fn tone_attenuation_db(air: f32, freq: f32) -> f32 {
        let mut f = WideFilter::new(WideParams {
            air,
            mix: 1.0,
            ..Default::default()
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let n = 19200usize;
        let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = 0.5 * (core::f32::consts::TAU * freq * i as f32 / 48000.0).sin();
            s[0][i] = v;
            s[1][i] = v;
        }
        f.process(&mut s, n);
        // 稳态段（跳过 FIR 中心 + 滤波器暂态）RMS。
        let start = 9600usize;
        let mut sum = 0.0f32;
        for i in start..n {
            let m = (s[0][i] + s[1][i]) * 0.5;
            sum += m * m;
        }
        let out_rms = (sum / (n - start) as f32).sqrt();
        let in_rms = 0.5 / core::f32::consts::SQRT_2;
        20.0 * (out_rms / in_rms).max(1e-6).log10()
    }

    let att_1k = tone_attenuation_db(1.0, 1000.0);
    let att_4k = tone_attenuation_db(1.0, 4000.0);
    let att_8k = tone_attenuation_db(1.0, 8000.0);
    let att_16k = tone_attenuation_db(1.0, 16000.0);
    // 物理曲线形状：衰减随频率单调加深，且 16k 明显深于 1k。
    assert!(
        att_4k < att_1k - 1.0 && att_8k < att_4k - 0.5 && att_16k < att_8k - 1.5,
        "air curve should deepen with frequency: 1k={att_1k:.2} 4k={att_4k:.2} \
         8k={att_8k:.2} 16k={att_16k:.2} dB"
    );
    // 柔和性：满档也不至于完全闷死。
    assert!(
        att_16k < -8.0 && att_16k > -20.0,
        "16k attenuation out of physical range: {att_16k:.2} dB"
    );
    // 单调性：air 越大衰减越深。
    fn air_only_attenuation_db(air: f32) -> f32 {
        let mut f = WideFilter::new(WideParams {
            air,
            mix: 1.0,
            ..Default::default()
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let n = 9600usize;
        let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = 0.5 * (core::f32::consts::TAU * 8000.0 * i as f32 / 48000.0).sin();
            s[0][i] = v;
            s[1][i] = v;
        }
        f.process(&mut s, n);
        let mut sum = 0.0f32;
        for i in 4800..n {
            let m = (s[0][i] + s[1][i]) * 0.5;
            sum += m * m;
        }
        let out_rms = (sum / 4800.0).sqrt();
        let in_rms = 0.5 / core::f32::consts::SQRT_2;
        20.0 * (out_rms / in_rms).max(1e-6).log10()
    }
    let att_8k_half = air_only_attenuation_db(0.5);
    assert!(att_8k_half > att_8k, "more air should attenuate deeper: {att_8k_half} vs {att_8k}");
}

#[test]
fn air_zero_is_center_passthrough() {
    // air=0 时空气吸收级联完全旁路：中声道逐样本位精确直通（FIR 分频除外）。
    let mut f = WideFilter::new(WideParams {
        air: 0.0,
        ..Default::default()
    });
    f.initialize(48000, &["L".into(), "R".into()]);
    // 跳过 FIR 分频 + 梳状延迟的启动瞬态，只验证稳态对称性。
    let n = 4096usize;
    let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
    for i in 0..n {
        let v = (core::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin() * 0.3;
        s[0][i] = v;
        s[1][i] = v;
    }
    f.process(&mut s, n);
    for i in 2048..n {
        assert!(
            (s[0][i] - s[1][i]).abs() < 1e-6,
            "air=0 must not break mid symmetry at {i}"
        );
    }
}

#[test]
fn air_adds_negligible_harmonics() {
    // 空气吸收失真检查：纯中置 1k 正弦 + 满空气，输出谐波应极小。
    // （RBJ 架 + Butterworth 低通都是线性滤波器；只要 tanh 未饱和就不产生谐波。）
    let mut f = WideFilter::new(WideParams {
        air: 1.0,
        ..Default::default()
    });
    f.initialize(48000, &["L".into(), "R".into()]);
    let n = 9600usize;
    let mut samples = vec![vec![0.0f32; n], vec![0.0f32; n]];
    for i in 0..n {
        let v = 0.2 * (core::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin();
        samples[0][i] = v;
        samples[1][i] = v;
    }
    f.process(&mut samples, n);

    // 输出稳态段 2k/3k 谐波幅度（4800..9600 = 100 个 1k 整周期）。
    let amp = |freq: f32| -> f32 {
        let mut re = 0.0f32;
        let mut im = 0.0f32;
        for i in 4800..n {
            let t = i as f32 / 48000.0;
            let w = core::f32::consts::TAU * freq * t;
            re += samples[0][i] * w.cos();
            im += samples[0][i] * w.sin();
        }
        2.0 * (re * re + im * im).sqrt() / 4800.0
    };
    assert!(
        amp(2000.0) < 2.0e-3,
        "air must not add 2nd harmonic: {}",
        amp(2000.0)
    );
    assert!(
        amp(3000.0) < 2.0e-3,
        "air must not add 3rd harmonic: {}",
        amp(3000.0)
    );
}

#[test]
fn crossover_endpoints_are_bounded() {
    for xover in [200.0f32, 1000.0] {
        let mut f = WideFilter::new(WideParams {
            gain: 1.0,
            air: 1.0,
            air_side: 1.0,
            mix: 1.0,
            crossover_hz: xover,
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let n = 4800usize;
        let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = 0.9 * (core::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin();
            s[0][i] = v;
            s[1][i] = -v;
        }
        f.process(&mut s, n);
        for ch in &s {
            for &v in ch {
                assert!(v.is_finite());
                assert!(v.abs() < 4.0, "crossover {xover}: bounded, got {v}");
            }
        }
    }
}

#[test]
fn out_of_range_params_are_clamped() {
    let mut f = WideFilter::new(WideParams {
        gain: 2.0,
        air: -1.0,
        air_side: 2.0,
        mix: 2.0,
        crossover_hz: 99999.0,
    });
    f.initialize(48000, &["L".into(), "R".into()]);
    let n = 4800usize;
    let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
    for i in 0..n {
        let v = 0.8 * (core::f32::consts::TAU * 500.0 * i as f32 / 48000.0).sin();
        s[0][i] = v;
        s[1][i] = v * 0.5;
    }
    f.process(&mut s, n);
    for ch in &s {
        for &v in ch {
            assert!(v.is_finite(), "clamped params must stay finite");
        }
    }
}

#[test]
fn low_level_is_transparent() {
    // 低电平下 tanh 近似线性：2 次/3 次谐波应可忽略。
    let mut f = WideFilter::new(WideParams { air: 1.0, ..Default::default() });
    f.initialize(48000, &["L".into(), "R".into()]);
    let n = 9600usize;
    let mut samples = vec![vec![0.0f32; n], vec![0.0f32; n]];
    for i in 0..n {
        let v = 0.05 * (core::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin();
        samples[0][i] = v;
        samples[1][i] = 0.01 * v;
    }
    f.process(&mut samples, n);

    // 计算左声道 2k/3k 谱幅度（窗口 4800..9600 = 100 整周期）。
    let amp = |freq: f32| -> f32 {
        let mut re = 0.0f32;
        let mut im = 0.0f32;
        for i in 4800..n {
            let t = i as f32 / 48000.0;
            let w = core::f32::consts::TAU * freq * t;
            re += samples[0][i] * w.cos();
            im += samples[0][i] * w.sin();
        }
        2.0 * (re * re + im * im).sqrt() / 4800.0
    };
    assert!(amp(2000.0) < 1.0e-3, "2nd harmonic should be absent");
    assert!(amp(3000.0) < 1.0e-3, "3rd harmonic should be absent");
}

#[test]
fn first_order_hpf_response() {
    // 一阶高通安全锁：截止频率处 -3dB；半截止处约 -7dB
    // （6dB/oct 是渐近近似，单极点真实值 20log10(0.5/√1.25)≈-7dB）。
    fn hpf_gain_db(fc: f32, freq: f32) -> f32 {
        let mut h = FirstOrderHpf::new(fc, 48000);
        let n = 96_000usize;
        let mut sum = 0.0f32;
        let mut i = 0;
        while i < n {
            let x = (core::f32::consts::TAU * freq * i as f32 / 48000.0).sin();
            let y = h.process(x);
            if i >= 48_000 {
                sum += y * y;
            }
            i += 1;
        }
        let out_rms = (sum / 48_000.0).sqrt();
        let in_rms = 1.0 / core::f32::consts::SQRT_2;
        20.0 * (out_rms / in_rms).log10()
    }
    for fc in [200.0f32, 500.0, 1000.0] {
        let at_fc = hpf_gain_db(fc, fc);
        let at_half = hpf_gain_db(fc, fc * 0.5);
        assert!(
            (at_fc + 3.0).abs() < 0.5,
            "HPF at Fc={fc} should be -3dB, got {at_fc:.2}"
        );
        assert!(
            (at_half + 7.0).abs() < 0.5,
            "HPF at 0.5Fc={} should be about -7dB, got {at_half:.2}",
            fc * 0.5
        );
    }
}

#[test]
fn itd_delays_differ_by_two_samples() {
    // ITD 去相关：左 +5、右 +7 样本，脉冲峰值位置差 2。
    let mut l = ItdDelay::new(ITD_DELAY_L);
    let mut r = ItdDelay::new(ITD_DELAY_R);
    let mut l_peak = usize::MAX;
    let mut r_peak = usize::MAX;
    for i in 0..32 {
        let x = if i == 0 { 1.0 } else { 0.0 };
        if l.process(x) > 0.5 {
            l_peak = i;
        }
        if r.process(x) > 0.5 {
            r_peak = i;
        }
    }
    assert_eq!(l_peak, ITD_DELAY_L);
    assert_eq!(r_peak, ITD_DELAY_R);
    assert_eq!(r_peak - l_peak, 2, "L/R ITD must differ by 2 samples");
}

#[test]
fn side_fir_splits_at_1k5() {
    // ITD 限频的线性相位 FIR 分离：500Hz 归低频支路（hp≈0），
    // 4kHz 归高频支路（lp≈0），保证 ITD 只作用于泛音区。
    fn hp_energy_ratio(freq: f32) -> f32 {
        let ir = design_lowpass_ir(SIDE_ITD_CROSSOVER_HZ, 48000, side_fir_len(48000));
        let mut fir = FirSplit::new(ir, 1);
        let n = 4800usize;
        let mut lp_sum = 0.0f32;
        let mut hp_sum = 0.0f32;
        for i in 0..n {
            let x = 0.5 * (core::f32::consts::TAU * freq * i as f32 / 48000.0).sin();
            let (lp, hp) = fir.split_channel(0, x);
            if i >= 2400 {
                lp_sum += lp * lp;
                hp_sum += hp * hp;
            }
        }
        (hp_sum / (lp_sum + hp_sum + 1e-9)).sqrt()
    }
    assert!(
        hp_energy_ratio(500.0) < 0.1,
        "500Hz must stay in the straight-through path"
    );
    assert!(
        hp_energy_ratio(4000.0) > 0.9,
        "4kHz must go through the ITD path"
    );
}

#[test]
fn mix_zero_is_delayed_passthrough() {
    // Mix=0：增量完全不入，整条链路退化为纯延迟——
    // 输出 = 输入延迟 (FIR 中心 + 1) 帧（忽略 FIR 浮点重建误差）。
    let mut f = WideFilter::new(WideParams {
        air: 0.354331,
        mix: 0.0,
        ..Default::default()
    });
    f.initialize(48000, &["L".into(), "R".into()]);
    let center = ((wide_fir_len(48000, 200.0) - 1) / 2) as usize + 1;
    let n = 4800usize;
    let mut input = vec![vec![0.0f32; n], vec![0.0f32; n]];
    for i in 0..n {
        let t = i as f32 / 48000.0;
        input[0][i] = 0.2 * (core::f32::consts::TAU * 440.0 * t).sin()
            + 0.1 * (core::f32::consts::TAU * 2200.0 * t).sin();
        input[1][i] = 0.15 * (core::f32::consts::TAU * 330.0 * t).sin()
            + 0.08 * (core::f32::consts::TAU * 5000.0 * t).sin();
    }
    let mut out = input.clone();
    f.process(&mut out, n);
    for ch in 0..2 {
        for i in center..n {
            let err = (out[ch][i] - input[ch][i - center]).abs();
            assert!(
                err < 1e-5,
                "mix=0 must be delayed passthrough: ch{ch} i={i} err={err}"
            );
        }
    }
}

#[test]
fn reset_clears_all_state() {
    // 处理非零信号后 reset，再喂静音必须无状态残留。
    let mut f = WideFilter::new(WideParams {
        air: 1.0,
        gain: 1.0,
        ..Default::default()
    });
    f.initialize(48000, &["L".into(), "R".into()]);
    let mut s = vec![vec![0.0f32; 2048], vec![0.0f32; 2048]];
    for i in 0..2048 {
        let v = 0.5 * (core::f32::consts::TAU * 300.0 * i as f32 / 48000.0).sin();
        s[0][i] = v;
        s[1][i] = -v;
    }
    f.process(&mut s, 2048);
    f.reset();
    let mut z = vec![vec![0.0f32; 256], vec![0.0f32; 256]];
    f.process(&mut z, 256);
    for ch in &z {
        for &v in ch {
            assert_eq!(v, 0.0, "reset must clear all filter state");
        }
    }
}

#[test]
fn extreme_antiphase_is_bounded() {
    // 1 kHz 纯反相、用户 Gain 满档：输出应被软限幅约束且明显放大。
    let mut f = WideFilter::new(WideParams {
        air: 1.0,
        gain: 1.0,
        mix: 1.0,
        ..Default::default()
    });
    f.initialize(48000, &["L".into(), "R".into()]);
    let n = 4800usize;
    let mut samples = vec![vec![0.0f32; n], vec![0.0f32; n]];
    for i in 0..n {
        let v = 0.9 * (core::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin();
        samples[0][i] = v;
        samples[1][i] = -v;
    }
    f.process(&mut samples, n);
    let mut peak = 0.0f32;
    for ch in &samples {
        for &v in ch {
            assert!(v.is_finite());
            // 低频支路原样相加允许 ±1.1 级轻微超限（高频支路已被 tanh 限制）。
            assert!(v.abs() <= 1.1 + 1e-6, "soft limiter must bound output: {v}");
            peak = peak.max(v.abs());
        }
    }
    assert!(peak > 0.9, "full-width antiphase should be strongly widened, peak {peak}");
}

#[test]
fn wide_material_preserves_dynamics() {
    // 动态 M/S：低于包络阈值的侧信号保持线性（动态不被压），
    // 超过阈值后自动收增益（防削波），这是动态 M/S 的核心行为。
    let run = |amp: f32| -> f32 {
        let mut f = WideFilter::new(WideParams {
            air: 1.0,
            gain: 1.0,
            mix: 1.0,
            ..Default::default()
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let n = 9600usize;
        let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = 0.9 * (core::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin();
            s[0][i] = amp * v;
            s[1][i] = -amp * v;
        }
        f.process(&mut s, n);
        let pk = |ch: &[f32]| ch[4800..].iter().fold(0.0f32, |m, &x| m.max(x.abs()));
        (pk(&s[0]) + pk(&s[1])) * 0.5
    };
    let p_lo = run(0.1);
    let p_mid = run(0.2);
    let p_hi = run(0.5);
    // 低电平（阈值下）：2 倍输入 → 输出接近 2 倍。
    let ratio_linear = p_mid / p_lo;
    assert!(
        ratio_linear > 1.7 && ratio_linear < 2.3,
        "below threshold dynamics should scale linearly: ratio {ratio_linear}"
    );
    // 高电平（阈值上）：动态增益介入，输出不再线性放大。
    let ratio_compressed = p_hi / p_mid;
    assert!(
        ratio_compressed < 2.2,
        "loud side should be dynamically tamed (linear would be 2.5): ratio {ratio_compressed}"
    );
}

#[test]
fn deterministic_and_reproducible() {
    let run = || -> Vec<f32> {
        let mut f = WideFilter::new(WideParams { air: 0.7, ..Default::default() });
        f.initialize(48000, &["L".into(), "R".into()]);
        let mut samples = vec![vec![0.0f32; 2048], vec![0.0f32; 2048]];
        for i in 0..2048 {
            let v = 0.4 * (core::f32::consts::TAU * 440.0 * i as f32 / 48000.0).sin();
            samples[0][i] = v;
            samples[1][i] = v * 0.5;
        }
        f.process(&mut samples, 2048);
        samples.into_iter().flatten().collect()
    };
    let a = run();
    let b = run();
    for (x, y) in a.iter().zip(b.iter()) {
        assert!((x - y).abs() < 1e-9, "same params must be reproducible");
    }
}

#[test]
fn finite_across_params_and_sample_rates() {
    for sr in [44_100u32, 48_000, 96_000] {
        for air in [0.0f32, 0.354331, 0.7, 1.0] {
            let mut f = WideFilter::new(WideParams { air, ..Default::default() });
            f.initialize(sr, &["L".into(), "R".into()]);
            let mut samples = vec![vec![0.0f32; 480], vec![0.0f32; 480]];
            for i in 0..480 {
                samples[0][i] =
                    (core::f32::consts::TAU * 440.0 * i as f32 / sr as f32).sin() * 0.9;
                samples[1][i] = -samples[0][i] * 0.6;
            }
            f.process(&mut samples, 480);
            for ch in &samples {
                for &v in ch {
                    assert!(v.is_finite());
                }
            }
        }
    }
}
