use super::*;
use crate::pipeline::dsp::model::{PeqBand, PeqBandType};

fn sr_amp(filter: &mut HybridPeqFilter, freq: f32, amp: f32, sr: u32, frames: usize) -> f32 {
    let mut samples = vec![vec![0.0f32; frames], vec![0.0f32; frames]];
    for i in 0..frames {
        let v = amp * (std::f32::consts::TAU * freq * i as f32 / sr as f32).sin();
        samples[0][i] = v;
        samples[1][i] = v * 0.5;
    }
    filter.process(&mut samples, frames);
    // 稳态段 RMS（跳过预热）。
    let start = frames / 2;
    // 低频（如 20 Hz @96k）周期很长：取整周期窗口，避免 RMS 测量误差。
    let period = (sr as f32 / freq).round() as usize;
    let span = ((frames - start) / period.max(1)) * period.max(1);
    let sum: f32 = samples[0][start..start + span].iter().map(|x| x * x).sum();
    (sum / span.max(1) as f32).sqrt()
}

fn target_db(bands: &[PeqBand], freq: f32, sr: u32) -> f32 {
    bands
        .iter()
        .map(|b| band_response_db(b, freq, sr))
        .sum()
}

#[test]
fn peaking_response_math() {
    // 单段 1 kHz +6 dB：中心 = 6 dB，远离中心 ≈ 0。
    let at_center = peaking_response_db(1000.0, 6.0, 1.0, 1000.0, 48000.0);
    let far = peaking_response_db(1000.0, 6.0, 1.0, 100.0, 48000.0);
    assert!((at_center - 6.0).abs() < 0.05, "center {at_center}");
    assert!(far.abs() < 0.1, "far {far}");
}

#[test]
fn shelf_and_pass_response_math() {
    // 低架 +6 dB：fc 以下接近 +6 dB，远高于 fc 接近 0。
    let ls = PeqBand { fc: 200.0, gain_db: 6.0, q: 0.707, kind: PeqBandType::LowShelf };
    assert!((band_response_db(&ls, 50.0, 48000) - 6.0).abs() < 0.3);
    assert!(band_response_db(&ls, 10000.0, 48000).abs() < 0.2);
    // 高通：fc 以下显著衰减，fc 以上接近 0 dB。
    let hp = PeqBand { fc: 1000.0, gain_db: 0.0, q: 0.707, kind: PeqBandType::HighPass };
    assert!(band_response_db(&hp, 50.0, 48000) < -20.0);
    assert!(band_response_db(&hp, 10000.0, 48000).abs() < 0.2);
    // 低通：fc 以上显著衰减，fc 以下接近 0 dB。
    let lp = PeqBand { fc: 1000.0, gain_db: 0.0, q: 0.707, kind: PeqBandType::LowPass };
    assert!(band_response_db(&lp, 10000.0, 48000) < -20.0);
    assert!(band_response_db(&lp, 50.0, 48000).abs() < 0.2);
    // 高架 -6 dB：fc 以上接近 -6 dB，远低于 fc 接近 0。
    let hs = PeqBand { fc: 6000.0, gain_db: -6.0, q: 0.707, kind: PeqBandType::HighShelf };
    assert!((band_response_db(&hs, 12000.0, 48000) + 6.0).abs() < 0.3);
    assert!(band_response_db(&hs, 50.0, 48000).abs() < 0.2);
}

#[test]
fn hybrid_shelf_and_pass_match_target() {
    let bands = vec![
        PeqBand { fc: 120.0, gain_db: 6.0, q: 0.707, kind: PeqBandType::LowShelf },
        PeqBand { fc: 6000.0, gain_db: -4.0, q: 0.707, kind: PeqBandType::HighShelf },
        PeqBand { fc: 1200.0, gain_db: 0.0, q: 0.707, kind: PeqBandType::LowPass },
        PeqBand { fc: 80.0, gain_db: 0.0, q: 0.707, kind: PeqBandType::HighPass },
    ];
    for sr in [44_100u32, 48_000, 96_000] {
        let mut f = HybridPeqFilter::new(PeqParams {
            crossover_hz: CROSSOVER_HZ,
            bands: bands.clone(),
        });
        f.initialize(sr, &["L".into(), "R".into()]);
        assert_eq!(f.iir.len(), 4, "shelf/pass 段必须全部走 IIR");
        for freq in [30.0f32, 60.0, 100.0, 120.0, 1000.0, 1200.0, 6000.0, 12000.0] {
            let frames = 12000usize;
            let out_rms = sr_amp(&mut f, freq, 0.25, sr, frames);
            let in_rms = 0.25 / std::f32::consts::SQRT_2;
            let measured = 20.0 * (out_rms / in_rms).log10();
            let target = target_db(&bands, freq, sr);
            assert!(
                (measured - target).abs() < 0.7,
                "sr {sr} freq {freq}: measured {measured:.2} dB vs target {target:.2} dB"
            );
        }
    }
}

#[test]
fn hybrid_matches_target_across_crossover() {
    // 多段（含跨 200 Hz）：级联输出频响 ≈ 目标 ±0.5 dB。
    let bands = vec![
        PeqBand { fc: 100.0, gain_db: -3.0, q: 1.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 200.0, gain_db: 4.0, q: 1.2, kind: PeqBandType::Peaking },
        PeqBand { fc: 1000.0, gain_db: 6.0, q: 1.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 4000.0, gain_db: -2.0, q: 2.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 8000.0, gain_db: 3.0, q: 1.5, kind: PeqBandType::Peaking },
        PeqBand { fc: 16000.0, gain_db: -1.0, q: 1.0, kind: PeqBandType::Peaking },
    ];
    for sr in [44_100u32, 48_000, 96_000] {
        let mut f = HybridPeqFilter::new(PeqParams {
            crossover_hz: CROSSOVER_HZ,
            bands: bands.clone(),
        });
        f.initialize(sr, &["L".into(), "R".into()]);
        for freq in [
            20.0f32, 31.5, 50.0, 60.0, 100.0, 125.0, 200.0, 500.0, 1000.0, 4000.0, 8000.0,
            16000.0,
        ] {
            let frames = 12000usize;
            let out_rms = sr_amp(&mut f, freq, 0.25, sr, frames);
            let in_rms = 0.25 / std::f32::consts::SQRT_2;
            let measured = 20.0 * (out_rms / in_rms).log10();
            let target = target_db(&bands, freq, sr);
            assert!(
                (measured - target).abs() < 0.5,
                "sr {sr} freq {freq}: measured {measured:.2} dB vs target {target:.2} dB"
            );
        }
    }
}

#[test]
fn low_band_uses_iir_high_band_fir() {
    // fc=100 的段：低频 IIR 承担；高频路径（fir_ir）应接近 0 dB 补偿。
    let bands = vec![PeqBand { fc: 100.0, gain_db: -6.0, q: 1.0, kind: PeqBandType::Peaking }];
    let mut f = HybridPeqFilter::new(PeqParams {
        crossover_hz: CROSSOVER_HZ,
        bands: bands.clone(),
    });
    f.initialize(48000, &["L".into()]);
    assert_eq!(f.iir.len(), 1);
    // 100 Hz 处目标 -6 dB；5 kHz 处目标 ≈ 0。
    let out100 = sr_amp(&mut f, 100.0, 0.25, 48000, 8192);
    let in_rms = 0.25 / std::f32::consts::SQRT_2;
    let m100 = 20.0 * (out100 / in_rms).log10();
    assert!((m100 + 6.0).abs() < 0.5, "100Hz {m100}");
    let out5k = sr_amp(&mut f, 5000.0, 0.25, 48000, 8192);
    let m5k = 20.0 * (out5k / in_rms).log10();
    assert!(m5k.abs() < 0.5, "5k {m5k}");
}

#[test]
fn wide_q_crossing_band_goes_to_iir() {
    // 宽 Q 段（fc=250, q=0.6）影响范围跨过分频点 → IIR 主实现；
    // 窄 Q 段（fc=300, q=5）影响完全在 200 Hz 以上 → FIR。
    let bands = vec![
        PeqBand { fc: 250.0, gain_db: -6.0, q: 0.6, kind: PeqBandType::Peaking },
        PeqBand { fc: 300.0, gain_db: -6.0, q: 8.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 400.0, gain_db: -6.0, q: 2.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 1000.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 2000.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 4000.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
    ];
    let mut f = HybridPeqFilter::new(PeqParams {
        crossover_hz: CROSSOVER_HZ,
        bands,
    });
    f.initialize(48000, &["L".into()]);
    assert_eq!(f.iir.len(), 2, "250/q0.6 与 400/q2.0 应进 IIR，300/q8 不进");
}

#[test]
fn crossing_band_fits_across_crossover() {
    // 宽 Q 段跨分频点：低频由 IIR 精确、高频由 FIR 补偿，总响应 = 目标。
    let bands = vec![
        PeqBand { fc: 150.0, gain_db: -3.0, q: 0.8, kind: PeqBandType::Peaking },
        PeqBand { fc: 250.0, gain_db: -6.0, q: 0.6, kind: PeqBandType::Peaking },
        PeqBand { fc: 1000.0, gain_db: 3.0, q: 1.5, kind: PeqBandType::Peaking },
        PeqBand { fc: 4000.0, gain_db: -2.0, q: 2.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 8000.0, gain_db: 2.0, q: 1.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 16000.0, gain_db: -1.0, q: 1.0, kind: PeqBandType::Peaking },
    ];
    for sr in [48_000u32, 96_000] {
        let mut f = HybridPeqFilter::new(PeqParams {
            crossover_hz: CROSSOVER_HZ,
            bands: bands.clone(),
        });
        f.initialize(sr, &["L".into(), "R".into()]);
        for freq in [
            60.0f32, 100.0, 150.0, 180.0, 220.0, 250.0, 300.0, 400.0, 600.0, 1000.0, 4000.0,
        ] {
            let frames = 12000usize;
            let out_rms = sr_amp(&mut f, freq, 0.25, sr, frames);
            let in_rms = 0.25 / std::f32::consts::SQRT_2;
            let measured = 20.0 * (out_rms / in_rms).log10();
            let target = target_db(&bands, freq, sr);
            assert!(
                (measured - target).abs() < 0.6,
                "sr {sr} freq {freq}: measured {measured:.2} dB vs target {target:.2} dB"
            );
        }
    }
}

#[test]
fn silence_stays_silent() {
    let bands = vec![
        PeqBand { fc: 100.0, gain_db: 6.0, q: 1.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 1000.0, gain_db: -6.0, q: 1.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 4000.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 8000.0, gain_db: -3.0, q: 1.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 12000.0, gain_db: 1.0, q: 1.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 16000.0, gain_db: -1.0, q: 1.0, kind: PeqBandType::Peaking },
    ];
    let mut f = HybridPeqFilter::new(PeqParams {
        crossover_hz: CROSSOVER_HZ,
        bands,
    });
    f.initialize(48000, &["L".into(), "R".into()]);
    let mut samples = vec![vec![0.0f32; 2048], vec![0.0f32; 2048]];
    f.process(&mut samples, 2048);
    for ch in &samples {
        for &v in ch {
            assert_eq!(v, 0.0);
        }
    }
}

#[test]
fn mute_recovery_fades_in_without_glitch() {
    // 静音 500 帧 → 恢复正弦：输出应从 0 线性淡入（前 8 ms），无阶跃。
    let bands = vec![
        PeqBand { fc: 160.0, gain_db: -2.0, q: 2.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 600.0, gain_db: -6.0, q: 1.5, kind: PeqBandType::Peaking },
        PeqBand { fc: 1000.0, gain_db: 6.0, q: 2.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 2000.0, gain_db: 2.0, q: 1.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 4000.0, gain_db: 1.0, q: 2.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 8000.0, gain_db: -2.0, q: 1.5, kind: PeqBandType::Peaking },
    ];
    let mut f = HybridPeqFilter::new(PeqParams {
        crossover_hz: CROSSOVER_HZ,
        bands,
    });
    f.initialize(48000, &["L".into(), "R".into()]);
    let n = 2048usize;
    let mut samples = vec![vec![0.0f32; n], vec![0.0f32; n]];
    for i in 500..n {
        let v = 0.25 * (std::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin();
        samples[0][i] = v;
        samples[1][i] = v;
    }
    f.process(&mut samples, n);
    // 恢复第 1 帧：淡入起点 ≈ 0（无阶跃）。
    assert!(
        samples[0][500].abs() < 0.01,
        "fade-in start should be ~0, got {}",
        samples[0][500]
    );
    // 约 8 ms（384 帧）后淡入完成，幅度恢复正常（目标 1000 Hz +6 dB）。
    let later = samples[0][500 + 400].abs();
    assert!(later > 0.05, "fade should complete, got {later}");
}

#[test]
fn extreme_params_finite_and_deterministic() {
    let mut bands = Vec::new();
    for i in 0..31 {
        let fc = 20.0 * (i as f32 + 1.0) * 1.6;
        bands.push(PeqBand {
            fc: fc.min(20000.0),
            gain_db: if i % 2 == 0 { 30.0 } else { -30.0 },
            q: if i % 3 == 0 { 12.0 } else { 0.1 },
            kind: PeqBandType::Peaking,
        });
    }
    for sr in [48_000u32, 96_000, 192_000] {
        let mut f = HybridPeqFilter::new(PeqParams {
            crossover_hz: CROSSOVER_HZ,
            bands: bands.clone(),
        });
        f.initialize(sr, &["L".into(), "R".into()]);
        let mut a = vec![vec![0.0f32; 2048], vec![0.0f32; 2048]];
        let mut b = vec![vec![0.0f32; 2048], vec![0.0f32; 2048]];
        for i in 0..2048 {
            let v = 0.5 * (std::f32::consts::TAU * 440.0 * i as f32 / sr as f32).sin();
            a[0][i] = v;
            a[1][i] = v * 0.3;
            b[0][i] = v;
            b[1][i] = v * 0.3;
        }
        f.process(&mut a, 2048);
        f.reset();
        f.process(&mut b, 2048);
        for ch in 0..2 {
            for i in 0..2048 {
                assert!(a[ch][i].is_finite(), "finite @sr {sr}");
                assert!((a[ch][i] - b[ch][i]).abs() < 1e-9, "deterministic @sr {sr}");
            }
        }
    }
}

#[test]
fn latency_reports_fir_len_div_4() {
    let bands = vec![
        PeqBand { fc: 100.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 1000.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 2000.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 4000.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 8000.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 16000.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
    ];
    let mut f = HybridPeqFilter::new(PeqParams {
        crossover_hz: CROSSOVER_HZ,
        bands,
    });
    f.initialize(48_000, &["L".into()]);
    // 1024 抽头最小相位：保守群延迟估计 = 1024/4 = 256。
    assert_eq!(f.latency(), 256);
    let mut f2 = HybridPeqFilter::new(PeqParams {
        crossover_hz: CROSSOVER_HZ,
        bands: vec![
            PeqBand { fc: 100.0, gain_db: 0.0, q: 1.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 200.0, gain_db: 0.0, q: 1.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 300.0, gain_db: 0.0, q: 1.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 400.0, gain_db: 0.0, q: 1.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 500.0, gain_db: 0.0, q: 1.0, kind: PeqBandType::Peaking },
            PeqBand { fc: 600.0, gain_db: 0.0, q: 1.0, kind: PeqBandType::Peaking },
        ],
    });
    f2.initialize(192_000, &["L".into()]);
    // 192k → 4096 抽头 > 2048 阈值 → 分块 FFT，延迟 = 块大小 128 - 1。
    assert_eq!(f2.latency(), 127);
}

/// M16+ 实机配置波形回归：分块喂入（模拟 APOProcess 每 480 帧一调），
/// 稳态输出必须保持正弦（频率不变、无长零段、RMS 符合目标）。
#[test]
fn sine_waveform_preserved_chunked_real_config() {
    let bands = vec![
        PeqBand { fc: 1500.0, gain_db: 1.0, q: 1.5, kind: PeqBandType::Peaking },
        PeqBand { fc: 2000.0, gain_db: 1.0, q: 1.5, kind: PeqBandType::Peaking },
        PeqBand { fc: 4500.0, gain_db: 1.0, q: 1.5, kind: PeqBandType::Peaking },
        PeqBand { fc: 5047.1, gain_db: 1.0, q: 1.5, kind: PeqBandType::Peaking },
        PeqBand { fc: 6000.0, gain_db: 3.0, q: 2.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 7812.0, gain_db: -6.0, q: 1.5, kind: PeqBandType::Peaking },
        PeqBand { fc: 10000.0, gain_db: 3.0, q: 2.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 13000.0, gain_db: -2.0, q: 2.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 16268.0, gain_db: -1.0, q: 2.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 20000.0, gain_db: -3.0, q: 3.0, kind: PeqBandType::Peaking },
    ];
    for sr in [44_100u32, 48_000, 96_000] {
        let mut f = HybridPeqFilter::new(PeqParams {
            crossover_hz: CROSSOVER_HZ,
            bands: bands.clone(),
        });
        f.initialize(sr, &["L".into(), "R".into()]);

        let freq = 1000.0f32;
        let chunk = 480usize;
        let total = 48000usize; // 1 秒
        let mut out = vec![vec![0.0f32; total], vec![0.0f32; total]];
        for start in (0..total).step_by(chunk) {
            let n = chunk.min(total - start);
            let mut block = vec![vec![0.0f32; n], vec![0.0f32; n]];
            for i in 0..n {
                let v = 0.25
                    * (std::f32::consts::TAU * freq * (start + i) as f32 / sr as f32).sin();
                block[0][i] = v;
                block[1][i] = v * 0.5;
            }
            f.process(&mut block, n);
            out[0][start..start + n].copy_from_slice(&block[0]);
            out[1][start..start + n].copy_from_slice(&block[1]);
        }

        let st = 8192usize; // 跳过预热（FIR 1024 + 淡入 384）
        let tail = &out[0][st..];
        // 1) 全有限
        assert!(tail.iter().all(|v| v.is_finite()), "finite @sr {sr}");
        // 2) 稳态无长零段（>128 连续零 = 异常静音/掉帧）
        let mut zeros = 0usize;
        for &v in tail {
            if v.abs() < 1e-6 {
                zeros += 1;
                assert!(zeros <= 128, "long zero run @sr {sr} len={zeros}");
            } else {
                zeros = 0;
            }
        }
        // 3) 过零率保持 2×freq（±10%，FIR 窗边缘不计）
        let period = (sr as f32 / freq).round() as usize;
        let crossings = tail
            .windows(2)
            .filter(|w| (w[0] < 0.0 && w[1] >= 0.0) || (w[0] >= 0.0 && w[1] < 0.0))
            .count();
        let expect = (tail.len() as f32 * 2.0 * freq / sr as f32).round() as usize;
        assert!(
            (crossings as i64 - expect as i64).unsigned_abs() <= (expect as i64 / 10).unsigned_abs() as u64,
            "crossings @sr {sr}: got {crossings}, expect ~{expect}"
        );
        // 4) 稳态 RMS ≈ 目标（1k 处 ≈ -1.17 dB）
        let span = (tail.len() / period) * period;
        let rms: f32 = (tail[..span].iter().map(|x| x * x).sum::<f32>() / span as f32).sqrt();
        let db = 20.0 * (rms / (0.25 / std::f32::consts::SQRT_2)).log10();
        let target = target_db(&bands, freq, sr);
        assert!(
            (db - target).abs() < 0.5,
            "rms @sr {sr}: {db:.2} dB vs target {target:.2} dB"
        );
    }
}

/// APP 实际序列化形态：每段一个 `[[effects]] type="peq"` 块 → 驱动级联
/// 10 个单段 HybridPeqFilter。分块喂入必须保持正弦（无慢放/电流音）。
#[test]
fn sine_waveform_preserved_per_band_blocks_cascaded() {
    use crate::pipeline::chain::Chain;
    use crate::pipeline::dsp::filter::Filter;

    let bands = vec![
        PeqBand { fc: 1500.0, gain_db: 1.0, q: 1.5, kind: PeqBandType::Peaking },
        PeqBand { fc: 2000.0, gain_db: 1.0, q: 1.5, kind: PeqBandType::Peaking },
        PeqBand { fc: 4500.0, gain_db: 1.0, q: 1.5, kind: PeqBandType::Peaking },
        PeqBand { fc: 5047.1, gain_db: 1.0, q: 1.5, kind: PeqBandType::Peaking },
        PeqBand { fc: 6000.0, gain_db: 3.0, q: 2.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 7812.0, gain_db: -6.0, q: 1.5, kind: PeqBandType::Peaking },
        PeqBand { fc: 10000.0, gain_db: 3.0, q: 2.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 13000.0, gain_db: -2.0, q: 2.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 16268.0, gain_db: -1.0, q: 2.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 20000.0, gain_db: -3.0, q: 3.0, kind: PeqBandType::Peaking },
    ];
    let sr = 48_000u32;
    let mut chain = Chain::new();
    for band in &bands {
        chain
            .add_filter(Box::new(HybridPeqFilter::new(PeqParams {
                crossover_hz: CROSSOVER_HZ,
                bands: vec![*band],
            })))
            .unwrap();
    }
    chain.initialize(sr, &["L".into(), "R".into()]);

    let freq = 1000.0f32;
    let chunk = 480usize;
    let total = 48000usize;
    let mut out = vec![vec![0.0f32; total], vec![0.0f32; total]];
    for start in (0..total).step_by(chunk) {
        let n = chunk.min(total - start);
        let mut block = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v =
                0.25 * (std::f32::consts::TAU * freq * (start + i) as f32 / sr as f32).sin();
            block[0][i] = v;
            block[1][i] = v * 0.5;
        }
        chain.process(&mut block, n).unwrap();
        out[0][start..start + n].copy_from_slice(&block[0]);
        out[1][start..start + n].copy_from_slice(&block[1]);
    }

    let st = 16384usize; // 10 条 FIR 预热更久，跳过前 1/3
    let tail = &out[0][st..];
    assert!(tail.iter().all(|v| v.is_finite()), "finite");
    let mut zeros = 0usize;
    for &v in tail {
        if v.abs() < 1e-6 {
            zeros += 1;
            assert!(zeros <= 128, "long zero run len={zeros}");
        } else {
            zeros = 0;
        }
    }
    let period = (sr as f32 / freq).round() as usize;
    let crossings = tail
        .windows(2)
        .filter(|w| (w[0] < 0.0 && w[1] >= 0.0) || (w[0] >= 0.0 && w[1] < 0.0))
        .count();
    let expect = (tail.len() as f32 * 2.0 * freq / sr as f32).round() as usize;
    assert!(
        (crossings as i64 - expect as i64).unsigned_abs()
            <= (expect as i64 / 10).unsigned_abs() as u64,
        "crossings: got {crossings}, expect ~{expect}"
    );
    let span = (tail.len() / period) * period;
    let rms: f32 = (tail[..span].iter().map(|x| x * x).sum::<f32>() / span as f32).sqrt();
    let db = 20.0 * (rms / (0.25 / std::f32::consts::SQRT_2)).log10();
    let target = target_db(&bands, freq, sr);
    assert!(
        (db - target).abs() < 0.5,
        "rms: {db:.2} dB vs target {target:.2} dB"
    );
}

/// 复用回归：旧流（大音量）跑完后 reset()，再喂新流（小音量）——
/// 输出必须与全新链一致，前段不得混入旧流尾巴（复用缓存电流声根因）。
#[test]
fn reset_clears_stale_fir_tail_before_reuse() {
    let bands = vec![
        PeqBand { fc: 1000.0, gain_db: 6.0, q: 1.5, kind: PeqBandType::Peaking },
        PeqBand { fc: 4000.0, gain_db: -6.0, q: 2.0, kind: PeqBandType::Peaking },
        PeqBand { fc: 8000.0, gain_db: 3.0, q: 1.0, kind: PeqBandType::Peaking },
    ];
    let sr = 48_000u32;
    let make = || {
        let mut f = HybridPeqFilter::new(PeqParams {
            crossover_hz: CROSSOVER_HZ,
            bands: bands.clone(),
        });
        f.initialize(sr, &["L".into(), "R".into()]);
        f
    };
    let mut reused = make();
    let mut fresh = make();

    // 旧流：大音量 440Hz，跑 5000 帧填满 FIR 延迟线。
    let loud_n = 5000usize;
    let mut loud = vec![vec![0.0f32; loud_n], vec![0.0f32; loud_n]];
    for i in 0..loud_n {
        let v = 0.9 * (std::f32::consts::TAU * 440.0 * i as f32 / sr as f32).sin();
        loud[0][i] = v;
        loud[1][i] = v;
    }
    reused.process(&mut loud, loud_n);
    // 模拟复用：清状态不清结构。
    reused.reset();

    // 新流：小音量 1kHz，分块喂入（真实引擎帧型）。
    let total = 4000usize;
    let chunk = 480usize;
    let mut out_a = vec![vec![0.0f32; total], vec![0.0f32; total]];
    let mut out_b = vec![vec![0.0f32; total], vec![0.0f32; total]];
    for start in (0..total).step_by(chunk) {
        let n = chunk.min(total - start);
        let mut a = vec![vec![0.0f32; n], vec![0.0f32; n]];
        let mut b = vec![vec![0.0f32; n], vec![0.0f32; n]];
        for i in 0..n {
            let v = 0.01 * (std::f32::consts::TAU * 1000.0 * (start + i) as f32 / sr as f32).sin();
            a[0][i] = v;
            a[1][i] = v;
            b[0][i] = v;
            b[1][i] = v;
        }
        reused.process(&mut a, n);
        fresh.process(&mut b, n);
        out_a[0][start..start + n].copy_from_slice(&a[0]);
        out_b[0][start..start + n].copy_from_slice(&b[0]);
    }

    // 从头到尾必须一致（含前 1024 帧：旧流尾巴若残留，此处必然发散）。
    for i in 0..total {
        assert!(
            (out_a[0][i] - out_b[0][i]).abs() < 1e-4,
            "i={i}: reused {} vs fresh {}",
            out_a[0][i],
            out_b[0][i]
        );
    }
}

/// 纯 FIR 配置遇真实静音空隙（>1.3ms 零输入）：不得触发静音门控淡入——
/// 恢复帧必须立即出声（h[0] 主导），否则音乐空隙处反复静音/淡入 = 电流感。
#[test]
fn fir_only_skips_silence_gate_on_gaps() {
    let bands = vec![
        PeqBand { fc: 1000.0, gain_db: 6.0, q: 1.5, kind: PeqBandType::Peaking },
        PeqBand { fc: 4000.0, gain_db: -6.0, q: 2.0, kind: PeqBandType::Peaking },
    ];
    let sr = 48_000u32;
    let mut f = HybridPeqFilter::new(PeqParams {
        crossover_hz: CROSSOVER_HZ,
        bands,
    });
    f.initialize(sr, &["L".into(), "R".into()]);
    assert!(f.iir.is_empty(), "本用例必须为纯 FIR 配置");

    // 信号：1200 帧响 → 400 帧真实静音（>SILENCE_HOLD_FRAMES）→ 1200 帧响。
    let n = 2800usize;
    let mut samples = vec![vec![0.0f32; n], vec![0.0f32; n]];
    for i in 0..n {
        if (1200..1600).contains(&i) {
            continue;
        }
        let v = 0.25 * (std::f32::consts::TAU * 1000.0 * i as f32 / sr as f32).sin();
        samples[0][i] = v;
        samples[1][i] = v;
    }
    f.process(&mut samples, n);

    // 静音段允许 FIR 自然衰减尾巴（物理正确的滤波器行为，任何 EQ 均有），
    // 关键是不得出现「静音门控 → 恢复淡入」的阶梯：恢复首帧必须立即出声。
    // 首帧输出 ≈ h[0]·x ≈ x（最小相位 FIR 前载），下限取 0.5×输入幅度。
    let v0 = samples[0][1600].abs();
    assert!(
        v0 > 0.12,
        "recovery first frame should be immediate (gate skipped), got {v0}"
    );
    // 稳态 RMS 仍符合目标（+6 dB @1k：0.25/√2 → 0.5/√2）。
    let sum: f32 = samples[0][2000..2600].iter().map(|x| x * x).sum();
    let rms = (sum / 600.0).sqrt();
    let db = 20.0 * (rms / (0.25 / std::f32::consts::SQRT_2)).log10();
    assert!((db - 6.0).abs() < 0.5, "steady rms {db:.2} dB vs +6");
}
