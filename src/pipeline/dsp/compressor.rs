//! Compressor（动态范围压缩器）
//!
//! 按「合适、好懂、负增益」重做的参数模型，共五个参数：
//! `threshold_db` / `ratio` / `lift` / `attack_ms` / `mix`。
//!
//! 算法结构（每帧，全声道联动检测）：
//! 1. 全声道瞬时功率 → dB 电平；
//! 2. **峰值包络**（瞬时 attack + 深度加权速率释放，dB 域表针式）：对稳态低频近似平直，
//!    旧版「对称单极点功率平均」残留的 2× 频率纹波（低频增益调制、听感发毛的来源）
//!    不再进入增益；不设侧链高通，低频照常参与触发。释放速率随压制深度加权：
//!    深压 30dB/s（基准，低频纹波停在掩蔽区），浅压加速到 180dB/s——否则快节奏
//!    音符间隙里目标回落不够，双极点再怎么松也追不出呼吸感；
//! 3. 静态曲线（固定 3dB 软膝，膝宽不作参数——效果只在阈值附近一条缝里可感，
//!    属于「不知道调了会干嘛」的旋钮）：阈值右侧按 `1 - 1/ratio` 削减（压大），
//!    阈值左侧按 `lift` 朝阈值抬升（放小，抬升量 = `u × min(阈值-电平, 24dB)`，
//!    恒不过阈值）；膝区用端点值/斜率匹配的 cubic Hermite 过渡，
//!    `lift = 0` 时**逐位退化**为原 x² 插值，默认行为不变；
//! 4. 目标增益在 dB 域平滑：attack（压缩增加）快、release 慢；
//!    抬升生效走 release（慢、平滑）、退出走 attack（快、不漏瞬态）；
//!    **释放固定自动，双极点权重随压制深度加权**：快极点 100ms、慢极点 600ms，
//!    深压（≥`AUTO_RELEASE_DEPTH_FULL_DB`）按 0.3/0.7 混合——慢尾巴保证持续段
//!    与低频不泵浦；浅压慢权重随深度立方退到 0、基本纯走快极点——快节奏的
//!    浅层压制在拍间隔内能喘气（固定权重时浅压也拖 600ms 尾巴，快节奏会越压越闷）。
//!    无需手调，也不存在手动档；
//! 5. 输出 = 输入 × 10^(-gr/20)，按 `mix` 与原声等比混合（dry = 1 - mix）。
//!
//! **不削波的自然保证**：lift=0 时 `10^(-gr/20) ≤ 1` 恒成立、混合权重和恒为 1，
//! 恒有 `|out| ≤ |in|`；lift>0 时包络恒 ≥ 瞬时电平，抬升量按「阈值-包络」封顶，
//! 输出电平恒 ≤ 阈值（≤0dBFS）。两种情况都不需要 makeup、不需要限幅。
//!
//! config 语法（`[[effects]]` 段）：
//! `type = "compressor"` + `threshold_db` / `ratio` / `lift` / `attack_ms` /
//! `mix`（旧 `wet` / `dry` 字段忽略；`release_ms` 已移除——释放固定自动，
//! 该键未声明，配置中出现会按未知键拒绝）。

use crate::pipeline::dsp::filter::Filter;

/// 固定软膝宽度（dB）。
const KNEE_DB: f32 = 3.0;
/// 抬升量的电平差上限（dB）：再深的安静部分也不再多抬。
const LIFT_CAP_DB: f32 = 24.0;
/// 自动释放的快极点时常数（ms）：给回弹的「头部」，瞬态段快速松开。
const AUTO_RELEASE_FAST_MS: f32 = 100.0;
/// 自动释放的慢极点时常数（ms）：给平缓的「尾巴」，持续段不泵浦（光学式两段手感）。
const AUTO_RELEASE_SLOW_MS: f32 = 600.0;
/// 慢极点的满深权重（深压口径 0.3/0.7：快极点权重 = 1 − 此值）：慢极点主导（柔），
/// 快极点提供初始回弹速度。
const AUTO_RELEASE_SLOW_W: f32 = 0.7;
/// 慢极点权重拉满所需的压制深度（dB）。≥ 此深度保持完整 0.7 慢权重（深压不泵浦）；
/// 浅压按深度立方退到 0，释放基本纯走 100ms 快极点（快节奏拍间喘得动）。
const AUTO_RELEASE_DEPTH_FULL_DB: f32 = 6.0;
/// 包络释放速率（dB/s，深压基准）。折中：40Hz 稳态纹波 ≈0.4dB（速率停在掩蔽区），
/// 越过阈值后回到阈值约 `(峰值-阈值)/速率` 秒，与球迹 release 同量级。
const ENVELOPE_RELEASE_DB_PER_S: f32 = 30.0;
/// 包络释放速率（dB/s，浅压值）。浅压时目标回落更快——快节奏的八分音符间隙
/// （≈107ms）里固定 30dB/s 只够目标降 2~3dB，极点再快也追不出呼吸感；
/// 浅压加速到此值让目标在间隙内归零。深压保持基准速率（低频不泵浦口径不变）。
/// 上限由 20Hz 浅压稳态纹波（速率×半周期 ≤2.5dB）反推，不破低频掩蔽口径。
const ENVELOPE_RELEASE_SHALLOW_DB_PER_S: f32 = 180.0;
/// 检测电平下限（dBFS）：静音时包络停在这里。
const LEVEL_FLOOR_DB: f32 = -140.0;

#[derive(Debug, Clone, Copy)]
pub struct CompressorParams {
    /// 阈值（dBFS，-48..0）。检测的是峰值包络电平：包络超过它才开始压。
    pub threshold_db: f32,
    /// 压缩比（1..20，1 = 不压）：超出阈值的部分按 x:1 压掉。
    pub ratio: f32,
    /// 抬升（0..1，0 = 纯压大）：阈值以下的安静部分朝阈值抬
    /// `u × min(阈值-电平, 24dB)`，恒不过阈值。
    pub lift: f32,
    /// 起音时间（ms，1..100）：压下去的速度。
    pub attack_ms: f32,
    /// 干湿混合（0..1，1 = 完全压过的信号，0 = 原声）。
    pub mix: f32,
}

impl Default for CompressorParams {
    fn default() -> Self {
        Self {
            threshold_db: -12.0,
            ratio: 3.0,
            lift: 0.0,
            attack_ms: 10.0,
            mix: 1.0,
        }
    }
}

#[derive(Debug)]
pub struct CompressorFilter {
    params: CompressorParams,
    channel_indices: Vec<usize>,
    /// 平滑后的目标增益改动（dB，可为负=抬升）：双极点的加权混合。
    gr_db: f32,
    /// 自动释放的快极点状态（dB）。
    gr_fast: f32,
    /// 自动释放的慢极点状态（dB）。
    gr_slow: f32,
    /// 峰值包络（dBFS）。
    env_db: f32,
    /// 包络每样本的释放步进（dB）。
    env_rel_step: f32,
    attack_c: f32,
    release_fast_c: f32,
    release_slow_c: f32,
}

impl CompressorFilter {
    pub fn new(params: CompressorParams) -> Self {
        Self {
            params,
            channel_indices: Vec::new(),
            gr_db: 0.0,
            gr_fast: 0.0,
            gr_slow: 0.0,
            env_db: LEVEL_FLOOR_DB,
            env_rel_step: 0.0,
            attack_c: 0.0,
            release_fast_c: 0.0,
            release_slow_c: 0.0,
        }
    }

    /// 静态曲线：给定检测电平返回目标增益改动（dB，可为负=抬升）。
    #[inline]
    fn curve(level_db: f32, threshold_db: f32, ratio: f32, knee_db: f32, lift: f32) -> f32 {
        let over = level_db - threshold_db;
        let half = knee_db * 0.5;
        let slope = 1.0 - 1.0 / ratio.max(1.0);
        if over <= -half {
            // 阈下：抬升 = u × min(阈值-电平, 上限)，恒不过阈值。
            if lift <= 0.0 {
                0.0
            } else {
                -lift * (-over).min(LIFT_CAP_DB)
            }
        } else if over >= half {
            over * slope
        } else {
            // 膝区：cubic Hermite——两端值取左右分支在边缘的取值、两端斜率与左右
            // 分支一致（C1 连续）。lift=0 时退化为原 x² 插值（h'(0)=0、h'(1)=2）。
            let x = (over + half) / knee_db.max(1e-3);
            let xx = x * x;
            let xxx = xx * x;
            let v0 = -lift * half;
            let v1 = half * slope;
            let n0 = lift * knee_db;
            let n1 = slope * knee_db;
            (2.0 * xxx - 3.0 * xx + 1.0) * v0
                + (xxx - 2.0 * xx + x) * n0
                + (-2.0 * xxx + 3.0 * xx) * v1
                + (xxx - xx) * n1
        }
    }
}

impl Filter for CompressorFilter {
    fn initialize(&mut self, sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        if self.channel_indices.is_empty() {
            self.channel_indices = (0..channel_names.len()).collect();
        }
        let sr = sample_rate.max(1) as f32;
        let p = self.params;
        let attack_s = p.attack_ms.clamp(1.0, 100.0) * 0.001;
        self.attack_c = 1.0 - (-1.0 / (attack_s * sr)).exp();
        // 双极点自动释放：100ms 头部 + 600ms 尾巴，按固定权重混合。
        let fast = AUTO_RELEASE_FAST_MS * 0.001;
        let slow = AUTO_RELEASE_SLOW_MS * 0.001;
        self.release_fast_c = 1.0 - (-1.0 / (fast * sr)).exp();
        self.release_slow_c = 1.0 - (-1.0 / (slow * sr)).exp();
        self.env_rel_step = ENVELOPE_RELEASE_DB_PER_S / sr;
        self.gr_db = 0.0;
        self.gr_fast = 0.0;
        self.gr_slow = 0.0;
        self.env_db = LEVEL_FLOOR_DB;
        None
    }

    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        if self.channel_indices.is_empty() {
            return;
        }
        let n = self.channel_indices.len();
        let first = self.channel_indices[0];
        if first >= samples.len() {
            return;
        }
        let frame_count = frame_count.min(samples[first].len());
        let threshold = self.params.threshold_db.clamp(-48.0, 0.0);
        let ratio = self.params.ratio.clamp(1.0, 20.0);
        let lift = self.params.lift.clamp(0.0, 1.0);
        let mix = self.params.mix.clamp(0.0, 1.0);
        let dry = 1.0 - mix;
        // 包络释放速率按压制深度在浅/深两档间线性插值：浅压加速（快节奏音符
        // 间隙里目标能回落到位），深压基准（低频纹波与不泵浦口径不变）。
        let shallow_ratio = ENVELOPE_RELEASE_SHALLOW_DB_PER_S / ENVELOPE_RELEASE_DB_PER_S;

        for f in 0..frame_count {
            // 1) 全声道瞬时功率 → dB 电平。
            let mut sum_sq = 0.0f64;
            for k in 0..n {
                let slot = self.channel_indices[k];
                if slot < samples.len() {
                    let x = samples[slot][f] as f64;
                    sum_sq += x * x;
                }
            }
            let level_db = (10.0 * (sum_sq / n as f64).max(1e-14).log10()) as f32;

            // 2) 峰值包络：瞬时 attack，深度加权速率释放。
            let depth01 = (self.gr_db.abs() / AUTO_RELEASE_DEPTH_FULL_DB).min(1.0);
            let rel_step = self.env_rel_step * (1.0 + (shallow_ratio - 1.0) * (1.0 - depth01));
            self.env_db = if level_db > self.env_db {
                level_db
            } else {
                (self.env_db - rel_step).max(level_db)
            };

            // 3) 静态曲线（固定软膝）→ 目标削减。
            let target_gr = Self::curve(self.env_db, threshold, ratio, KNEE_DB, lift);

            // 4) dB 域平滑：attack 快、release 慢。双极点并行——快/慢极点各自朝
            //    目标平滑，无切换点。慢极点权重随当前压制深度加权：深压 0.7
            //    （600ms 尾巴，持续段与低频不泵浦），浅压按深度立方退到 0（快极点
            //    100ms，快节奏拍间能恢复）。attack 阶段两极点同系数，混合权重
            //    不影响攻击速度（d(gr)/dt = c·(target−gr)），仅释放阶段生效。
            let w_slow = AUTO_RELEASE_SLOW_W * depth01 * depth01 * depth01;
            let cf = if target_gr > self.gr_fast {
                self.attack_c
            } else {
                self.release_fast_c
            };
            let cs = if target_gr > self.gr_slow {
                self.attack_c
            } else {
                self.release_slow_c
            };
            self.gr_fast += (target_gr - self.gr_fast) * cf;
            self.gr_slow += (target_gr - self.gr_slow) * cs;
            self.gr_db = (1.0 - w_slow) * self.gr_fast + w_slow * self.gr_slow;

            // 5) 增益应用 + 等比干湿混合（权重和恒为1；lift=0 时 |out| ≤ |in|）。
            let gain = 10.0f32.powf(-self.gr_db / 20.0);
            for k in 0..n {
                let slot = self.channel_indices[k];
                if slot >= samples.len() {
                    continue;
                }
                let input = samples[slot][f];
                let out = input * gain;
                samples[slot][f] = if out.is_finite() {
                    out * mix + input * dry
                } else {
                    0.0
                };
            }
        }
    }

    fn set_channel_indices(&mut self, indices: &[usize]) {
        self.channel_indices = indices.to_vec();
    }

    fn reset(&mut self) {
        self.gr_db = 0.0;
        self.gr_fast = 0.0;
        self.gr_slow = 0.0;
        self.env_db = LEVEL_FLOOR_DB;
    }
}

#[cfg(test)]
mod tests {
    // 测试用例按帧索引并行双声道缓冲并复用 `i` 计算相位：索引形式与公式一一对应。
    #![allow(clippy::needless_range_loop)]

    use super::*;

    fn names() -> Vec<String> {
        vec!["L".into(), "R".into()]
    }

    fn sine(amp: f32, hz: f32, n: usize, sr: f32) -> Vec<Vec<f32>> {
        let mut s = vec![vec![0.0f32; n]; 2];
        for i in 0..n {
            let v = amp * (core::f32::consts::TAU * hz * i as f32 / sr).sin();
            s[0][i] = v;
            s[1][i] = v;
        }
        s
    }

    fn rms_db(samples: &[Vec<f32>], start: usize) -> f32 {
        let mut sum = 0.0f64;
        let mut count = 0.0f64;
        for ch in samples {
            for (i, &v) in ch.iter().enumerate() {
                if i >= start {
                    sum += (v as f64) * (v as f64);
                    count += 1.0;
                }
            }
        }
        if count <= 0.0 {
            return -120.0;
        }
        (10.0 * (sum / count).max(1e-14).log10()) as f32
    }

    #[test]
    fn mix_zero_is_passthrough() {
        let mut f = CompressorFilter::new(CompressorParams {
            mix: 0.0,
            ..Default::default()
        });
        f.initialize(48000, &names());
        let mut samples = vec![vec![0.3f32; 480], vec![0.2f32; 480]];
        let before = samples.clone();
        f.process(&mut samples, 480);
        for (a, b) in samples.iter().zip(before.iter()) {
            for (x, y) in a.iter().zip(b.iter()) {
                assert!((x - y).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn silence_stays_silent() {
        let mut f = CompressorFilter::new(CompressorParams::default());
        f.initialize(48000, &names());
        let mut samples = vec![vec![0.0f32; 4800], vec![0.0f32; 4800]];
        f.process(&mut samples, 4800);
        for ch in &samples {
            for &v in ch {
                assert_eq!(v, 0.0);
            }
        }
    }

    #[test]
    fn below_threshold_is_identity() {
        // 阈下信号：不压、不抬，恒等透过（负增益模式：gain ≤ 1，无 makeup）。
        let mut f = CompressorFilter::new(CompressorParams {
            threshold_db: -12.0,
            ..Default::default()
        });
        f.initialize(48000, &names());
        let mut s = sine(10.0f32.powf(-40.0 / 20.0), 220.0, 48_000, 48000.0);
        let before = s.clone();
        f.process(&mut s, 48_000);
        for (a, b) in s.iter().zip(before.iter()) {
            for (x, y) in a.iter().zip(b.iter()) {
                assert!((x - y).abs() < 1e-6, "threshold 下必须恒等透过");
            }
        }
        assert_eq!(f.gr_db, 0.0);
    }

    #[test]
    fn above_threshold_is_compressed_by_ratio() {
        // 稳态超阈值信号：峰值包络 ≈ 峰值电平（-6dBFS），阈值 -18 → over 12dB。
        // ratio 4 → gr 9dB；输入 RMS -9dB → 输出 ≈ -18dB。
        let run = |ratio: f32| -> f32 {
            let mut f = CompressorFilter::new(CompressorParams {
                threshold_db: -18.0,
                ratio,
                attack_ms: 1.0,
                ..Default::default()
            });
            f.initialize(48000, &names());
            let mut s = sine(10.0f32.powf(-6.0 / 20.0), 220.0, 96_000, 48000.0);
            f.process(&mut s, 96_000);
            rms_db(&s, 48_000)
        };
        let out = run(4.0);
        assert!(
            (out + 18.0).abs() < 0.7,
            "4:1 compression wrong: got {out:.1}, expected ≈-18.0"
        );
        // ratio 更高 → 压缩更多、输出更静。
        let out20 = run(20.0);
        assert!(
            out20 < out,
            "higher ratio must compress more: {out20:.1} vs {out:.1}"
        );
    }

    #[test]
    fn attack_is_smoothed_no_jump() {
        // 阶跃超阈值输入：增益削减逐帧逼近，首2ms 内不得逼近目标（无瞬时跳变）。
        let mut f = CompressorFilter::new(CompressorParams {
            threshold_db: -12.0,
            ratio: 10.0,
            attack_ms: 50.0,
            ..Default::default()
        });
        f.initialize(48000, &names());
        let mut s = sine(0.7, 220.0, 4800, 48000.0);
        f.process(&mut s, 96); // 2ms
        let early = f.gr_db;
        assert!(
            early > 0.0 && early < 1.5,
            "gr must rise smoothly from0, got {early}"
        );
        f.process(&mut s, 4800 - 96);
        // 目标 ≈8.0dB，50ms 起音走完100ms ≈86.5% → ≈6.9dB。
        assert!(
            f.gr_db > early && f.gr_db < 7.5,
            "gr must keep rising smoothly, got {}",
            f.gr_db
        );
    }

    #[test]
    fn soft_knee_smooths_transition() {
        // 在阈值附近：软膝输入削减连续（硬膝在阈值处从 0 跳变）。
        let th = -18.0f32;
        let ratio = 4.0f32;
        let knee = 6.0f32;
        let below = CompressorFilter::curve(-18.0 - 3.1, th, ratio, knee, 0.0);
        let at = CompressorFilter::curve(-18.0, th, ratio, knee, 0.0);
        let above = CompressorFilter::curve(-18.0 + 3.1, th, ratio, knee, 0.0);
        assert_eq!(below, 0.0);
        assert!(at > 0.0 && at < above);
        assert!(above > 0.0);
    }

    #[test]
    fn knee_with_lift_is_value_continuous() {
        // 膝区密集采样：lift>0 时值必须连续（无台阶）；
        // lift=0 必须与解析式 x²×half×slope 逐点一致（默认行为逐位退化校验）。
        let th = -12.0f32;
        let ratio = 3.0f32;
        let knee = 3.0f32;
        let mut prev = CompressorFilter::curve(th - knee, th, ratio, knee, 0.8);
        for i in 1..=600 {
            let level = th - knee + (i as f32) * (knee / 600.0);
            let v = CompressorFilter::curve(level, th, ratio, knee, 0.8);
            assert!(
                (v - prev).abs() < 0.05,
                "膝区值不连续 @level={level}: {prev} → {v}"
            );
            prev = v;
        }
        for i in 0..=60 {
            let over = -knee * 0.5 + (i as f32) * (knee / 60.0);
            let x = (over + knee * 0.5) / knee;
            let expect = x * x * (knee * 0.5) * (1.0 - 1.0 / ratio);
            let got = CompressorFilter::curve(th + over, th, ratio, knee, 0.0);
            assert!(
                (got - expect).abs() < 1e-5,
                "lift=0 未退化为 x² 插值 @over={over}: got {got}, expect {expect}"
            );
        }
    }

    #[test]
    fn lift_boosts_quiet_toward_threshold() {
        // 阈下 -30dBFS 正弦、u=0.5：抬升 = 0.5 × (-12 - (-30)) = 9dB。
        // 抬升生效走 release——自动释放慢极点 τ=600ms，3s 才充分收敛。
        let mut f = CompressorFilter::new(CompressorParams {
            threshold_db: -12.0,
            ratio: 3.0,
            lift: 0.5,
            attack_ms: 1.0,
            mix: 1.0,
        });
        f.initialize(48000, &names());
        let mut s = sine(10.0f32.powf(-30.0 / 20.0), 220.0, 144_000, 48000.0);
        let in_rms = rms_db(&s, 72_000);
        f.process(&mut s, 144_000);
        let out = rms_db(&s, 72_000);
        assert!(
            (out - (in_rms + 9.0)).abs() < 0.5,
            "lift=0.5 应抬升 9dB: in {in_rms:.1} → out {out:.1}"
        );
        assert!(f.gr_db < -8.0, "gr 应为负（抬升），got {}", f.gr_db);
    }

    #[test]
    fn lift_never_exceeds_threshold() {
        // u=1、电平在阈下 4dB：抬升 4dB 后恰好回到阈值，输出峰值不得越过阈值线。
        // 同上：自动释放收敛需要时间，3s 后测稳态。
        let th_lin = 10.0f32.powf(-12.0 / 20.0);
        let mut f = CompressorFilter::new(CompressorParams {
            threshold_db: -12.0,
            ratio: 3.0,
            lift: 1.0,
            attack_ms: 1.0,
            mix: 1.0,
        });
        f.initialize(48000, &names());
        let mut s = sine(10.0f32.powf(-16.0 / 20.0), 220.0, 144_000, 48000.0);
        f.process(&mut s, 144_000);
        let peak = s.iter().flatten().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(
            peak <= th_lin * 1.03,
            "lift 后峰值 {peak:.4} 越过阈值线 {th_lin:.4}"
        );
        let out = rms_db(&s, 72_000);
        assert!(
            (out - (-15.0)).abs() < 0.5,
            "输入 RMS ≈-19dB、抬升 4dB → 期望 ≈-15dB，实测 {out:.1}"
        );
    }

    #[test]
    fn bass_envelope_ripple_is_small() {
        // 低频稳态正弦：释放纹波必须有界。env 纹波 ≈ 当前速率×半周期——本条件
        // （gr≈4dB，浅压插值速率 ≈80dB/s）20Hz ≈2.0dB、40Hz ≈1.0dB，按速率感知
        // 口径断言；**可听的是 GR 纹波**（两级分离正是为把 env 纹波滤掉），
        // 维持原严口径。旧版对称单极点是2× 频率、±1dB 级的增益调制（低频发毛的来源）。
        for (hz, env_bound, gr_bound) in [(20.0f32, 2.6f32, 1.2f32), (40.0, 1.4, 0.8)] {
            let mut f = CompressorFilter::new(CompressorParams {
                threshold_db: -12.0,
                ratio: 3.0,
                attack_ms: 1.0,
                ..Default::default()
            });
            f.initialize(48000, &names());
            let n = 48000usize; //1s
            let mut s = sine(10.0f32.powf(-6.0 / 20.0), hz, n, 48000.0);
            //1ms 一块推进（process 只从头处理，分块以便在块边界采样状态）。
            let mut env_min = f32::MAX;
            let mut env_max = f32::MIN;
            let mut gr_min = f32::MAX;
            let mut gr_max = f32::MIN;
            let step = 48usize;
            let mut i = 0;
            while i < n {
                let take = step.min(n - i);
                let mut chunk: Vec<Vec<f32>> = s.iter().map(|c| c[i..i + take].to_vec()).collect();
                f.process(&mut chunk, take);
                for c in 0..2 {
                    s[c][i..i + take].copy_from_slice(&chunk[c]);
                }
                i += take;
                if i > 4800 {
                    // 跳过前100ms建立期
                    env_min = env_min.min(f.env_db);
                    env_max = env_max.max(f.env_db);
                    gr_min = gr_min.min(f.gr_db);
                    gr_max = gr_max.max(f.gr_db);
                }
            }
            let env_ripple = env_max - env_min;
            let gr_ripple = gr_max - gr_min;
            assert!(
                env_ripple <= env_bound,
                "{hz}Hz 包络纹波 {env_ripple:.3}dB 超界（≤{env_bound}）"
            );
            assert!(
                gr_ripple <= gr_bound,
                "{hz}Hz GR 纹波 {gr_ripple:.3}dB 超界（≤{gr_bound}）"
            );
        }
    }

    #[test]
    fn output_never_exceeds_input() {
        // 负增益模式的机器证明：激进参数 + 满湿，任何采样 |out| ≤ |in|。
        let mut f = CompressorFilter::new(CompressorParams {
            threshold_db: -48.0,
            ratio: 20.0,
            lift: 0.0,
            attack_ms: 1.0,
            mix: 1.0,
        });
        f.initialize(48000, &names());
        let n = 48000usize;
        let mut s = vec![vec![0.0f32; n]; 2];
        for i in 0..n {
            let t = i as f32 / 48000.0;
            let v = 0.9 * (core::f32::consts::TAU * 220.0 * t).sin()
                + 0.3 * (core::f32::consts::TAU * 55.0 * t).sin();
            s[0][i] = v;
            s[1][i] = v * 0.7;
        }
        let before = s.clone();
        f.process(&mut s, n);
        for (a, b) in s.iter().zip(before.iter()) {
            for (x, y) in a.iter().zip(b.iter()) {
                assert!(
                    x.abs() <= y.abs() + 1e-6,
                    "|out|={:.6} 超过 |in|={:.6}",
                    x.abs(),
                    y.abs()
                );
            }
        }
    }

    #[test]
    fn mix_is_exact_blend_of_dry_and_full_wet() {
        // gr 轨迹与 mix 无关 → mix=0.5 的输出必须是 mix=0 与 mix=1 的精确中点。
        let input = sine(0.5, 220.0, 4800, 48000.0);
        let run = |mix: f32| -> Vec<Vec<f32>> {
            let mut f = CompressorFilter::new(CompressorParams {
                threshold_db: -18.0,
                ratio: 4.0,
                lift: 0.0,
                attack_ms: 5.0,
                mix,
            });
            f.initialize(48000, &names());
            let mut s = input.clone();
            f.process(&mut s, 4800);
            s
        };
        let dry = run(0.0);
        let wet = run(1.0);
        let half = run(0.5);
        for c in 0..2 {
            for i in 0..4800 {
                let expect = (dry[c][i] + wet[c][i]) * 0.5;
                assert!((half[c][i] - expect).abs() < 1e-6, "mix=0.5 不是干湿中点");
                assert!(half[c][i].abs() <= dry[c][i].abs().max(wet[c][i].abs()) + 1e-6);
            }
        }
    }

    #[test]
    fn deterministic_and_finite() {
        let run = || -> Vec<f32> {
            let mut f = CompressorFilter::new(CompressorParams::default());
            f.initialize(48000, &names());
            let mut s = vec![vec![0.0f32; 2048], vec![0.0f32; 2048]];
            for i in 0..2048 {
                let v = 0.5 * (core::f32::consts::TAU * 440.0 * i as f32 / 48000.0).sin();
                s[0][i] = v;
                s[1][i] = v * 0.6;
            }
            f.process(&mut s, 2048);
            s.into_iter().flatten().collect()
        };
        let a = run();
        let b = run();
        for (x, y) in a.iter().zip(b.iter()) {
            assert!(x.is_finite());
            assert!((x - y).abs() < 1e-9);
        }
    }

    #[test]
    fn reset_clears_gain_reduction() {
        let mut f = CompressorFilter::new(CompressorParams {
            threshold_db: -30.0,
            ratio: 10.0,
            ..Default::default()
        });
        f.initialize(48000, &names());
        let mut s = sine(0.8, 220.0, 4800, 48000.0);
        f.process(&mut s, 4800);
        assert!(f.gr_db > 0.0);
        f.reset();
        assert_eq!(f.gr_db, 0.0);
        assert_eq!(f.gr_fast, 0.0);
        assert_eq!(f.gr_slow, 0.0);
        assert_eq!(f.env_db, LEVEL_FLOOR_DB);
    }

    #[test]
    fn auto_release_is_always_dual_pole() {
        // 释放固定自动：初始化后必为双极点（快极点系数 > 慢极点系数 > 0），
        // 不存在手动单极点模式。
        let mut f = CompressorFilter::new(CompressorParams::default());
        f.initialize(48000, &names());
        assert!(f.release_fast_c > f.release_slow_c);
        assert!(f.release_slow_c > 0.0);
    }

    #[test]
    fn auto_release_has_head_and_tail() {
        // 同一信号（2s 过阈值建立 GR → 骤降 400ms）：双极点结构必须可检出——
        // 快极点（头部）比慢极点（尾巴）释放得多，输出严格介于两者之间。
        let mut f = CompressorFilter::new(CompressorParams {
            threshold_db: -12.0,
            ratio: 4.0,
            lift: 0.0,
            attack_ms: 1.0,
            mix: 1.0,
        });
        f.initialize(48000, &names());
        let mut burst = sine(0.5, 220.0, 96_000, 48000.0); //2s
        f.process(&mut burst, 96_000);
        let gr_peak = f.gr_db;
        assert!(gr_peak > 1.0, "burst 应建立 GR，got {gr_peak}");
        let mut dip = sine(0.001, 220.0, 19_200, 48000.0); //400ms
        f.process(&mut dip, 19_200);
        // 头部已明显松开、尾巴还挂着，输出按深度加权权重落在两者之间。
        assert!(
            f.gr_fast < f.gr_slow,
            "快极点应比慢极点多释放：fast={:.3} slow={:.3}",
            f.gr_fast,
            f.gr_slow
        );
        assert!(
            f.gr_fast < f.gr_db && f.gr_db < f.gr_slow,
            "输出应严格介于两极点之间：fast={:.3} db={:.3} slow={:.3}",
            f.gr_fast,
            f.gr_db,
            f.gr_slow
        );
        let rec = gr_peak - f.gr_db;
        assert!(rec > 0.2, "400ms 内应有实质释放，got {rec:.3}dB");
    }

    #[test]
    fn shallow_release_much_faster_than_deep() {
        // 深度加权自动释放：同样骤降到静音，浅压（≈2dB）到 10% 残留的用时
        // 必须显著短于深压（≈9dB）——浅压靠快极点在拍间喘气（快节奏不发闷），
        // 深压保留慢尾巴（持续段与低频不泵浦）。固定 0.3/0.7 的旧口径两者同速。
        let t10_after_drop = |amp: f32| -> (f32, f32) {
            let mut f = CompressorFilter::new(CompressorParams {
                threshold_db: -12.0,
                ratio: 4.0,
                lift: 0.0,
                attack_ms: 1.0,
                mix: 1.0,
            });
            f.initialize(48000, &names());
            let mut burst = sine(amp, 220.0, 96_000, 48000.0); // 2s 建立 GR
            f.process(&mut burst, 96_000);
            let peak = f.gr_db;
            // 骤降为静音，5ms 一块推进并采样（process 只从头处理，分块取状态）。
            let total = 192_000usize; // 4s 上限
            let mut i = 0usize;
            while i < total && f.gr_db > peak * 0.1 {
                let take = 240usize.min(total - i);
                let mut chunk = vec![vec![0.0f32; take]; 2];
                f.process(&mut chunk, take);
                i += take;
            }
            (peak, i as f32 / 48000.0)
        };
        let (shallow_peak, shallow_t10) = t10_after_drop(10.0f32.powf(-9.0 / 20.0));
        let (deep_peak, deep_t10) = t10_after_drop(1.0);
        assert!(
            shallow_peak > 1.5 && shallow_peak < 4.0,
            "浅压峰值应 ≈2dB，got {shallow_peak:.2}"
        );
        assert!(deep_peak > 6.0, "深压峰值应 ≥6dB，got {deep_peak:.2}");
        assert!(
            shallow_t10 < 0.5,
            "浅压 t10 应 ≤500ms（实测 310ms，旧口径 ≈1.2s），got {:.0}ms",
            shallow_t10 * 1000.0
        );
        assert!(
            deep_t10 > 0.45,
            "深压仍须慢于浅压、保留尾巴，got {:.0}ms",
            deep_t10 * 1000.0
        );
        assert!(
            deep_t10 > shallow_t10 * 1.5,
            "深压释放应显著慢于浅压：deep={:.0}ms shallow={:.0}ms",
            deep_t10 * 1000.0,
            shallow_t10 * 1000.0
        );
    }

    #[test]
    fn fast_tempo_gr_does_not_accumulate() {
        // 快节奏场景回归：160BPM 八分音符连打（107ms 音 + 107ms 静音 ×20），
        // 每个间隙浅压必须恢复大半——旧口径（固定 0.3/0.7，t10≈1.2s）在
        // 间隙内只恢复约 4 成，GR 跨拍累积成持续压制（听感发闷）。
        let mut f = CompressorFilter::new(CompressorParams {
            threshold_db: -12.0,
            ratio: 4.0,
            lift: 0.0,
            attack_ms: 5.0,
            mix: 1.0,
        });
        f.initialize(48000, &names());
        let amp = 10.0f32.powf(-7.0 / 20.0); // 峰值包络 ≈-7dB → 超阈 5dB → GR ≈3.75dB
        let hit = 5_136usize; // 107ms
        let gap = 5_136usize;
        let mut min_in_gap = f32::MAX;
        let mut peak_first = 0.0f32;
        for cycle in 0..20 {
            let mut loud = sine(amp, 220.0, hit, 48000.0);
            f.process(&mut loud, hit);
            if cycle == 0 {
                peak_first = f.gr_db;
            }
            min_in_gap = f.gr_db.min(min_in_gap);
            let mut quiet = vec![vec![0.0f32; gap]; 2];
            f.process(&mut quiet, gap);
            min_in_gap = f.gr_db.min(min_in_gap);
        }
        assert!(peak_first > 2.0, "首个音应建立 GR，got {peak_first:.2}");
        // 间隙末端是 GR 最低点：连打全程不得抬到持续压制的量级。
        assert!(
            min_in_gap < peak_first * 0.5,
            "八分音符间隙内 GR 应恢复过半：min={min_in_gap:.2}dB peak={peak_first:.2}dB"
        );
    }
}
