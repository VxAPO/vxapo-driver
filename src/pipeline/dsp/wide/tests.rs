// 测试用例按帧索引并行双声道缓冲并复用 `i` 计算相位：索引形式与公式一一对应；
// 迭代器化需 split_at_mut + zip + enumerate 组合，反而降低可读性。
#![allow(clippy::needless_range_loop)]

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
        side_itd: 0.0,
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
fn gain_cuts_side_low_and_leaves_highs_flat() {
    // 新语义：宽度不再靠提升侧信号能量。Gain 只衰减侧低频
    // （side_low_gain = 1 − 1.0·Gain，衰减位置 = crossover_hz），
    // 高频段不做任何提升：低频侧能量随 Gain 单调下降、高频侧能量不变。
    let side_width = |freq: f32, gain: f32| -> f32 {
        let mut f = WideFilter::new(WideParams {
            gain,
            ..Default::default()
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let n = 9600usize;
        let mut samples = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = (core::f32::consts::TAU * freq * i as f32 / 48000.0).sin();
            samples[0][i] = 0.5 * v;
            samples[1][i] = 0.1 * v;
        }
        f.process(&mut samples, n);
        width_rms(&samples, 4800)
    };

    // 低频（60Hz，分频点以下，直通支路）：侧低频随 Gain 单调下降。
    let lo_base = side_width(60.0, 0.0);
    let lo_half = side_width(60.0, 0.5);
    let lo_full = side_width(60.0, 1.0);
    assert!(
        lo_half < lo_base && lo_full < lo_half,
        "side low must fall monotonically with Gain: {lo_base} < {lo_half} < {lo_full}"
    );
    // 侧低频的下降与干声同步（同一个低架），低频立体声像不被破坏；
    // 60Hz 处低架约给到九成深度，故阈值取 0.6。
    assert!(
        lo_full < lo_base * 0.6,
        "full Gain should cut side low together with dry: {lo_full} vs {lo_base}"
    );

    // 高频（5kHz，ITD 泛音区）：不提升，随 Gain 基本不变。
    let hi_base = side_width(5000.0, 0.0);
    let hi_full = side_width(5000.0, 1.0);
    assert!(
        (hi_full / hi_base - 1.0).abs() < 0.05,
        "highs must not be boosted by Gain: full={hi_full} base={hi_base}"
    );
}

// 增益映射（低架深度 = low_shelf_depth_db·Gain，拐点 = 分频点，Q = 0.707）
// 已由 bass_tilts_down_per_low_shelf_and_highs_stay_flat 直接按实测响应覆盖，
// 不再单独断言内部增益字段。

#[test]
fn air_depth_drives_center_attenuation() {
    // 空气吸收深度只由 air 参数控制：8k 纯中置正弦，
    // air 越大衰减越深，air=0 时保持原样。
    fn center_8k_energy(air: f32) -> f32 {
        let mut f = WideFilter::new(WideParams {
            air,
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

// 侧向空气（air_side）已从参数模型中移除：它与中置空气语义重叠、
// 默认 0 时不起作用，独立价值不足。相关测试一并删除。

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
fn bass_tilts_down_per_low_shelf_and_highs_stay_flat() {
    // 新语义：低频不再「保持能量」，而是按 low_shelf_gain = 1 − 0.5·Gain
    // 整体下压（Gain=1 → −6dB）让出相对高频感；高频不提升。
    // 纯中央低频（L==R）只走直通搁架：输出 ≈ 输入 · low_shelf_gain。
    let centered_low_rms = |gain: f32| -> f32 {
        let mut f = WideFilter::new(WideParams {
            gain,
            ..Default::default()
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let n = 9600usize;
        let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = 0.5 * (core::f32::consts::TAU * 60.0 * i as f32 / 48000.0).sin();
            s[0][i] = v;
            s[1][i] = v;
        }
        f.process(&mut s, n);
        let mut sum = 0.0f32;
        for i in 4800..n {
            sum += s[0][i] * s[0][i];
        }
        (sum / 4800.0).sqrt()
    };
    let r0 = centered_low_rms(0.0);
    let r5 = centered_low_rms(0.5);
    let r1 = centered_low_rms(1.0);
    // 低架 Q=0.707、拐点 200Hz：60Hz 处约为满深度的九成，故容差放宽到 0.07。
    assert!(
        (r5 / r0 - 0.75).abs() < 0.07,
        "Gain=0.5 → low shelf ≈ −2.5dB (ratio {}), r5={r5} r0={r0}",
        r5 / r0
    );
    assert!(
        (r1 / r0 - 0.5).abs() < 0.07,
        "Gain=1 → low shelf −6dB (ratio {}), r1={r1} r0={r0}",
        r1 / r0
    );

    // 5kHz 侧成分：不提升（宽度不靠抬高频）。
    let high_diff = |gain: f32| -> f32 {
        let mut f = WideFilter::new(WideParams {
            gain,
            ..Default::default()
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let n = 4800usize;
        let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = (core::f32::consts::TAU * 5000.0 * i as f32 / 48000.0).sin();
            s[0][i] = 0.5 * v;
            s[1][i] = 0.1 * v;
        }
        f.process(&mut s, n);
        let mut d = 0.0f32;
        for i in (n / 2)..n {
            d = d.max((s[0][i] - s[1][i]).abs());
        }
        d
    };
    let d0 = high_diff(0.0);
    let d1 = high_diff(1.0);
    assert!(d0 > 0.0);
    assert!(
        (d1 / d0 - 1.0).abs() < 0.05,
        "highs must not be boosted by Gain: full={d1} base={d0}"
    );
}

#[test]
fn fir_split_reconstructs_delayed_input() {
    // 线性相位 FIR 完美重建：低频支路 + 高频支路 = 延迟 center 帧的原信号
    // （逐样本，含相位——这是 FIR 相对 IIR 分频的核心优势）。
    let mut f = WideFilter::new(WideParams { air: 1.0, ..Default::default() });
    f.initialize(48000, &["L".into(), "R".into()]);
    // 只测 FIR 分频本身：用 FIR 群延迟中心，不含 Haas 延迟。
    let center = (wide_fir_len(48000, 200.0) - 1) / 2;
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
    let center = (wide_fir_len(48000, 200.0) - 1) / 2;
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
            crossover_hz: xover,
            ..Default::default()
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
        crossover_hz: 99999.0,
        ..Default::default()
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
fn itd_delay_follows_time_and_amount() {
    // 侧向去相关：延迟按**时间**折算（0.10 / 0.15 ms，Δτ = 0.05 ms），
    // 换采样率时时间量恒定；强度 0..1 连续控制延迟长度。
    // 48k → 4.8 / 7.2 采样；192k → 19.2 / 28.8 采样。
    for (sr, want_l, want_r) in [(48_000.0f32, 4.8f32, 7.2f32), (192_000.0, 19.2, 28.8)] {
        let mut l = ItdDelay::new(ITD_TIME_L_SECS * sr);
        let mut r = ItdDelay::new(ITD_TIME_R_SECS * sr);
        l.set_amount(1.0);
        r.set_amount(1.0);
        // 分数延迟时冲激会分到相邻两点，用"重心"作为有效延迟。
        let (mut ls, mut lw, mut rs, mut rw) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for i in 0..128 {
            let x = if i == 0 { 1.0f32 } else { 0.0 };
            let yl = l.process(x);
            let yr = r.process(x);
            ls += yl * i as f32;
            lw += yl;
            rs += yr * i as f32;
            rw += yr;
        }
        let (dl, dr) = (ls / lw, rs / rw);
        assert!((dl - want_l).abs() < 0.1, "sr={sr} 左延迟 {dl} 应为 {want_l}");
        assert!((dr - want_r).abs() < 0.1, "sr={sr} 右延迟 {dr} 应为 {want_r}");
        assert!(
            (dr - dl - 0.05 * sr / 1000.0).abs() < 0.1,
            "sr={sr} Δτ 应恒为 0.05ms（{dle} 采样）",
            dle = dr - dl
        );
        // 强度 0 ⇒ 精确直通（不动相位、不改幅度）
        let mut z = ItdDelay::new(ITD_TIME_R_SECS * sr);
        z.set_amount(0.0);
        assert_eq!(z.process(0.7), 0.7, "sr={sr} 强度 0 必须精确直通");
    }
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
fn hard_panned_hf_keeps_its_position() {
    // 诊断：硬左右（纯侧）5kHz，air=0、gain=0，只变 side_itd。
    // 期望：任何档位下侧能量都不该塌、mid 不该涨（α=0 时 mid 应≈0）。
    let measure = |side_itd: f32| -> (f32, f32) {
        let mut f = WideFilter::new(WideParams {
            air: 0.0,
            gain: 0.0,
            side_itd,
            ..Default::default()
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let n = 9600usize;
        let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = 0.3 * (core::f32::consts::TAU * 5000.0 * i as f32 / 48000.0).sin();
            s[0][i] = v;
            s[1][i] = -v;
        }
        f.process(&mut s, n);
        let (mut es, mut em) = (0.0f32, 0.0f32);
        for i in 4800..n {
            let side = (s[0][i] - s[1][i]) * 0.5;
            let mid = (s[0][i] + s[1][i]) * 0.5;
            es += side * side;
            em += mid * mid;
        }
        ((es / 4800.0).sqrt(), (em / 4800.0).sqrt())
    };
    for a in [0.0f32, 0.01, 0.1, 0.5, 1.0] {
        let (side_rms, mid_rms) = measure(a);
        println!("side_itd={a}: side_rms={side_rms:.4} mid_rms={mid_rms:.4}");
    }
    let in_rms = 0.3 / (2.0f32).sqrt();
    let (s0, m0) = measure(0.0);
    assert!((s0 - in_rms).abs() < 0.05, "α=0 应≈输入侧电平 {in_rms}: {s0}");
    assert!(m0 < 0.05, "α=0 不该产生 mid: {m0}");
    // （α=1 的侧能量下降属去相关本身，见下方按「能量守恒」的判据。）

    // 位置判据：α=0 必须**逐样本**等于延迟后的原信号（干路一次都不能经过
    // 分离器——两支路各带自己的群延迟，用它当干声会让侧高频相对中置平移）。
    let pos_err = |side_itd: f32| -> f32 {
        // air 不能为 0：gain/air/side_itd 全零会让效果判为 inactive 直接直通，
        // 那样测不到链路本身。中置空气对纯侧信号无影响，只用来让链路跑起来。
        let mut f = WideFilter::new(WideParams {
            air: 0.354331,
            gain: 0.0,
            side_itd,
            ..Default::default()
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        // 方案 A 后总延迟 = 主分离器中心 + 1（HPF 群延迟补偿）+ 侧分离器中心 D2。
        let center = (wide_fir_len(48000, 200.0) - 1) / 2
            + 1
            + (side_fir_len(48000) - 1) / 2;
        let n = 4800usize;
        let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = 0.3 * (core::f32::consts::TAU * 5000.0 * i as f32 / 48000.0).sin();
            s[0][i] = v;
            s[1][i] = -v;
        }
        let mut out = s.clone();
        f.process(&mut out, n);
        let mut max_err = 0.0f32;
        for ch in 0..2 {
            for i in center..n {
                max_err = max_err.max((out[ch][i] - s[ch][i - center]).abs());
            }
        }
        max_err
    };
    let (e0, e1) = (pos_err(0.0), pos_err(0.01));
    println!("α=0 逐样本误差={e0:.2e}  α=0.01 误差={e1:.2e}");
    assert!(e0 < 1e-4, "α=0 必须等于延迟后的原信号: err={e0}");
    assert!(e1 < 0.05, "α=0.01 只该有个位数量级的分量: err={e1}");
    // 去相关的本质是能量在 mid / side 之间重新分配（总量守恒），
    // 要守的是两条：不放大（旧实现 α=1 会涨到 2 倍）、mid 不失控；
    // 侧能量随 α 下降是去相关本身，不是塌陷。
    for a in [0.0f32, 0.01, 0.1, 0.5, 1.0] {
        let (s, m) = measure(a);
        let total = (s * s + m * m).sqrt();
        assert!(
            total < in_rms * 1.1,
            "side_itd={a}: 总能量不该被放大 total={total} in={in_rms}"
        );
        assert!(m < in_rms, "side_itd={a}: mid 不该超过输入侧电平: {m}");
    }
    let (s1, _) = measure(1.0);
    assert!(s1 > in_rms * 0.5, "α=1 侧能量不该塌掉: {s1}");
}

#[test]
fn measure_splitter_group_delay() {
    // 只读测量：线性相位分离器的群延迟（喂冲激，看 LP+HP 的峰值落在第几个样本）。
    // 方案 A 需要把 mid_h / hl / hr 平移「侧分离器」的这个延迟量。
    let sr = 48000u32;
    for (name, fc, taps) in [
        ("main", 200.0f32, wide_fir_len(sr, 200.0)),
        ("side", SIDE_ITD_CROSSOVER_HZ, side_fir_len(sr)),
    ] {
        let ir = design_lowpass_ir(fc, sr, taps);
        let mut f = FirSplit::new(ir, 1);
        let n = taps * 2;
        let (mut peak, mut best) = (0usize, 0.0f32);
        let mut energy = 0.0f32;
        for i in 0..n {
            let x = if i == 0 { 1.0f32 } else { 0.0 };
            let (lp, hp) = f.split_channel(0, x);
            let y = (lp + hp).abs();
            energy += y * y;
            if y > best {
                best = y;
                peak = i;
            }
        }
        println!("{name}: fc={fc}Hz taps={taps} 群延迟={peak} 采样 ({:.3} ms) 峰值={best:.4}",
            peak as f32 * 1000.0 / sr as f32);
        assert!(best > 0.5, "{name}: 冲激响应峰值异常 {best}");
        assert!(energy > 0.9, "{name}: 重建能量异常 {energy}");
    }
}

#[test]
fn side_itd_controls_decorrelation() {
    // 侧向时间差（Side ITD）= 侧通道 1.5kHz 以上去相关的干湿比。
    // 表征用「侧输出相对干声的相位延迟」（3kHz，避开相位折叠且位于 ITD 带内）：
    // α=0 必须为 0（完全不动相位），α 增大相位延迟单调增大。
    let phase = |side_itd: f32| -> f32 {
        let mut f = WideFilter::new(WideParams {
            air: 0.354331,
            side_itd,
            ..Default::default()
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let n = 9600usize;
        let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = 0.3 * (core::f32::consts::TAU * 3000.0 * i as f32 / 48000.0).sin();
            s[0][i] = v;
            s[1][i] = -v;
        }
        f.process(&mut s, n);
        let center = (wide_fir_len(48000, 200.0) - 1) / 2 + 1;
        let (mut re, mut im) = (0.0f32, 0.0f32);
        for i in 4800..n {
            let side_out = (s[0][i] - s[1][i]) * 0.5;
            let t = (i - center) as f32 / 48000.0;
            re += side_out * (core::f32::consts::TAU * 3000.0 * t).sin();
            im += side_out * (core::f32::consts::TAU * 3000.0 * t).cos();
        }
        im.atan2(re)
    };
    let p0 = phase(0.0).abs();
    let p_mid = phase(0.5).abs();
    let p1 = phase(1.0).abs();
    // α=0 时侧通道仍带约 1 个采样的群延迟（side_fir 的中心与主分频中心不重合，
    // 3kHz 下 ≈0.4 rad），所以只要求它足够小；关键是随 α 单调增大。
    assert!(p0.abs() < 0.5, "α=0 的残余相位应不足 1.5 采样: p0={p0}");
    assert!(
        p_mid > p0 && p1 > p_mid && p1 > 1.0,
        "相位延迟应随侧向时间差单调增大: p0={p0} p_mid={p_mid} p1={p1}"
    );
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
fn output_is_linear_below_knee_and_never_clips() {
    // 新语义：链路不再过 tanh。输出未触及软膝（0.9）时处理是线性的——
    // 输入等比放大、输出峰值等比放大；超过软膝由 output_soft_clip 兜底，峰值恒 ≤ 1.0。
    let peak = |amp: f32| -> f32 {
        let mut f = WideFilter::new(WideParams {
            air: 1.0,
            gain: 1.0,
            ..Default::default()
        });
        f.initialize(48000, &["L".into(), "R".into()]);
        let n = 4800usize;
        let mut s = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = 0.9 * (core::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin();
            s[0][i] = amp * v;
            s[1][i] = -amp * v;
        }
        f.process(&mut s, n);
        let mut pk = 0.0f32;
        for ch in &s {
            for &x in &ch[(n / 2)..] {
                pk = pk.max(x.abs());
            }
        }
        pk
    };
    let p1 = peak(0.1);
    let p2 = peak(0.2);
    let p4 = peak(0.4);
    assert!(p1 > 0.0);
    assert!(p4 < 0.9, "test loads must stay below the soft knee: peak {p4}");
    assert!(
        (p2 / p1 - 2.0).abs() < 0.15 && (p4 / p2 - 2.0).abs() < 0.15,
        "below knee output must scale linearly: p1={p1} p2={p2} p4={p4}"
    );
    // 大电平 / 极端反相：软限幅兜底，峰值恒 ≤ 1.0。
    for amp in [1.0f32, 2.0, 5.0] {
        let p = peak(amp);
        assert!(
            p.is_finite() && p <= 1.0 + 1e-6,
            "output must stay ≤ 1.0: amp {amp} → peak {p}"
        );
    }
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
