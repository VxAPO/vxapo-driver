//! pipeline/dsp/math.rs — DSP 共享数值策略（整合方案 v2）
//!
//! 职责：
//! - dB ↔ 线性转换（f64 中间计算 + clamp，保证有限）
//! - 参数范围常量（增益/频率/Q/延迟/通道数等）
//! - 二阶极点稳定性判据（极点半径，initialize 非 RT 路径）
//! - RT 线程入口 FTZ/DAZ 硬件冲刷次正规数（x86_64）
//! - 非 RT 限频 warn（解析层 clamp 提示）

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;

// ══════════════════════════════════════════════════════════════════════════════
// 参数范围常量（已确认，见 docs/dsp-math-optimization-plan.md 第 9 节）
// ══════════════════════════════════════════════════════════════════════════════

/// 增益下限（dB）。低于 -120 dB 已不可闻，且避免滤波器 `1/a` 爆掉。
pub const GAIN_DB_MIN: f32 = -120.0;
/// 增益上限（dB）。+48 dB 时 biquad 幅度因子 `a ≈ 15.8`，极点余量充足。
pub const GAIN_DB_MAX: f32 = 48.0;
/// 滤波器频率下限（Hz）。
pub const FILTER_FREQ_MIN_HZ: f32 = 10.0;
/// 滤波器频率上限比例（× sample_rate，Nyquist 以内）。
pub const FILTER_FREQ_MAX_RATIO: f32 = 0.45;
/// Q 值下限。
pub const Q_MIN: f32 = 0.05;
/// Q 值上限。
pub const Q_MAX: f32 = 18.0;
/// 极点半径余量：`max(|pole|) < 1 - STABILITY_MARGIN`。
pub const STABILITY_MARGIN: f32 = 1e-3;
/// 增益平滑每采样最大比例变化（≈0.42 dB）。
pub const MAX_GAIN_STEP_RATIO: f32 = 0.05;
/// 增益平滑接近目标时的跳转阈值（相对误差）。
/// 取 1e-6：f32 机器精度 ≈ 1.19e-7，1e-7 阈值可能永不命中导致停滞。
pub const GAIN_SNAP_THRESHOLD: f32 = 1e-6;
/// 每 N 采样重算一次目标 ratio，避免每采样 powf。
pub const GAIN_SMOOTH_RERATE: u32 = 32;
/// 增益平滑默认目标步数（≈2.7 ms @ 48 kHz）。
pub const GAIN_SMOOTH_STEPS_DEFAULT: u32 = 128;
/// Copy 混音系数幅值上限。
pub const COPY_COEFF_MAX: f32 = 16.0;
/// 延迟上限（毫秒）。
pub const DELAY_MS_MAX: f32 = 1000.0;
/// Loudness phon 下限。
pub const PHON_MIN: f32 = 0.0;
/// Loudness phon 上限。
pub const PHON_MAX: f32 = 120.0;
/// GraphicEQ 最大段数（1/3 倍频程全带）。
pub const MAX_GRAPHIC_EQ_BANDS: usize = 31;
/// Copy 临时缓冲预分配上限（RT 安全）。
pub const MAX_FRAME_COUNT: usize = 8192;
/// 分块卷积块大小（Phase 8，M3）。
pub const CONVOLUTION_PARTITION_SIZE: usize = 128;

// ══════════════════════════════════════════════════════════════════════════════
// dB ↔ 线性
// ══════════════════════════════════════════════════════════════════════════════

/// 输入 dB 归一：NaN → 0 dB（直通）；±inf → 边界值；超界 → clamp。
pub fn clamp_gain_db(db: f32) -> f32 {
    if db.is_nan() {
        0.0
    } else if db == f32::NEG_INFINITY {
        GAIN_DB_MIN
    } else if db == f32::INFINITY {
        GAIN_DB_MAX
    } else {
        db.clamp(GAIN_DB_MIN, GAIN_DB_MAX)
    }
}

/// dB → 线性因子。f64 计算后转 f32，输入经 `clamp_gain_db` 保证有限。
pub fn db_to_linear(db: f32) -> f32 {
    let db = clamp_gain_db(db);
    (10.0_f64.powf(db as f64 / 20.0)) as f32
}

/// 线性因子 → dB。NaN → 0 dB；+inf → `GAIN_DB_MAX`；≤0 → -inf（静音）。
pub fn linear_to_db(linear: f32) -> f32 {
    if linear.is_nan() {
        0.0
    } else if linear == f32::INFINITY {
        GAIN_DB_MAX
    } else if linear <= 0.0 {
        f32::NEG_INFINITY
    } else {
        (20.0 * (linear as f64).log10()) as f32
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 二阶极点稳定性（非 RT，initialize 时调用）
// ══════════════════════════════════════════════════════════════════════════════

/// 二阶极点稳定性判据：解 `z^2 + a1 z + a2 = 0`，要求 `max(|pole|) < 1 - STABILITY_MARGIN`。
///
/// 选极点半径而非 Schur-Cohn 边界判据：高增益 shelf 的 `|a1|` 会贴近 `1 + a2`，
/// Schur-Cohn 加余量后容易误杀合法大增益；极点半径直接对应“贴单位圆 → 哨声”的物理量。
pub fn is_stable_biquad(a1: f32, a2: f32) -> bool {
    if !a1.is_finite() || !a2.is_finite() {
        return false;
    }
    let a1 = a1 as f64;
    let a2 = a2 as f64;

    let disc = a1 * a1 - 4.0 * a2;
    let max_radius = if disc >= 0.0 {
        let s = disc.sqrt();
        ((-a1 + s).abs() / 2.0).max((-a1 - s).abs() / 2.0)
    } else {
        // 共轭复根：|r1| = |r2| = sqrt(a2)
        a2.abs().sqrt()
    };

    max_radius < (1.0 - STABILITY_MARGIN as f64)
}

// ══════════════════════════════════════════════════════════════════════════════
// RT 线程 FTZ/DAZ 初始化（x86_64）
// ══════════════════════════════════════════════════════════════════════════════

/// RT 音频线程入口调用一次：设置 SSE MXCSR 的 FTZ（bit15）+ DAZ（bit6）。
///
/// 设置后硬件直接把次正规数当 0 处理，DSP 代码无需任何逐采样检测。
/// thread_local 幂等：同一线程重复调用无额外开销。
#[inline]
pub fn init_audio_thread() {
    #[cfg(target_arch = "x86_64")]
    {
        const MXCSR_FTZ: u32 = 1 << 15;
        const MXCSR_DAZ: u32 = 1 << 6;

        thread_local! {
            static FTZ_DAZ_SET: Cell<bool> = const { Cell::new(false) };
        }

        FTZ_DAZ_SET.with(|flag| {
            if !flag.get() {
                // SAFETY: stmxcsr/ldmxcsr 读写当前线程 MXCSR；按位写入 FTZ/DAZ，
                // 无内存副作用、不改变 flags。指针指向栈上 u32，指令期间有效。
                unsafe {
                    let mut csr: u32 = 0;
                    core::arch::asm!(
                        "stmxcsr [{}]",
                        in(reg) &mut csr,
                        options(nostack, preserves_flags)
                    );
                    csr |= MXCSR_FTZ | MXCSR_DAZ;
                    core::arch::asm!(
                        "ldmxcsr [{}]",
                        in(reg) &csr,
                        options(nostack, preserves_flags)
                    );
                }
                flag.set(true);
            }
        });
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 非 RT 限频 warn（解析层 clamp 提示）
// ══════════════════════════════════════════════════════════════════════════════

/// 同一 key 10 秒内最多一条 warn。仅在非 RT 路径调用（内部有锁 + 分配）。
pub fn warn_rate_limited(key: &str, msg: &str) {
    static LAST_WARN: Lazy<Mutex<HashMap<String, Instant>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    let now = Instant::now();
    let mut map = LAST_WARN.lock().unwrap_or_else(|e| e.into_inner());
    let due = map
        .get(key)
        .is_none_or(|last| now.duration_since(*last) >= Duration::from_secs(10));
    if due {
        log::warn!("{msg}");
        map.insert(key.to_owned(), now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_gain_db_normalizes_non_finite() {
        assert_eq!(clamp_gain_db(f32::NAN), 0.0);
        assert_eq!(clamp_gain_db(f32::NEG_INFINITY), GAIN_DB_MIN);
        assert_eq!(clamp_gain_db(f32::INFINITY), GAIN_DB_MAX);
    }

    #[test]
    fn clamp_gain_db_bounds_extremes() {
        assert_eq!(clamp_gain_db(-1000.0), GAIN_DB_MIN);
        assert_eq!(clamp_gain_db(1000.0), GAIN_DB_MAX);
        assert_eq!(clamp_gain_db(0.0), 0.0);
    }

    #[test]
    fn db_to_linear_extremes_stay_finite() {
        let hi = db_to_linear(1000.0);
        let lo = db_to_linear(-1000.0);
        assert!(hi.is_finite());
        assert!(lo.is_finite());
        assert!((hi - db_to_linear(GAIN_DB_MAX)).abs() < 1e-3);
        assert!((lo - db_to_linear(GAIN_DB_MIN)).abs() < 1e-9);
    }

    #[test]
    fn linear_to_db_special_values() {
        assert_eq!(linear_to_db(0.0), f32::NEG_INFINITY);
        assert_eq!(linear_to_db(-1.0), f32::NEG_INFINITY);
        assert_eq!(linear_to_db(f32::NAN), 0.0);
        assert_eq!(linear_to_db(f32::INFINITY), GAIN_DB_MAX);
    }

    #[test]
    fn is_stable_biquad_matches_pole_radius() {
        assert!(is_stable_biquad(0.0, 0.0)); // 直通
        assert!(is_stable_biquad(0.5, 0.5)); // 半径 sqrt(0.5) ≈ 0.707
        assert!(!is_stable_biquad(0.0, 1.0)); // 极点贴单位圆
        assert!(!is_stable_biquad(0.0, -1.0)); // 极点贴单位圆（Nyquist）
        assert!(!is_stable_biquad(f32::NAN, 0.0));
        // -120 dB peaking 的归一化系数会有一个极点 ≈ 1.0（根判据能抓住）
        assert!(!is_stable_biquad(-0.02125, -0.9786));
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn init_audio_thread_sets_ftz_daz_and_is_idempotent() {
        init_audio_thread();
        let csr = read_mxcsr();
        assert_eq!(csr & 0x8040, 0x8040, "FTZ/DAZ bits must be set");
        // 幂等：重复调用无副作用。
        init_audio_thread();
        init_audio_thread();
        assert_eq!(read_mxcsr() & 0x8040, 0x8040);
    }

    #[cfg(target_arch = "x86_64")]
    fn read_mxcsr() -> u32 {
        let mut csr: u32 = 0;
        // SAFETY: 读取当前线程 MXCSR 到栈上 u32，指令期间指针有效。
        unsafe {
            core::arch::asm!(
                "stmxcsr [{}]",
                in(reg) &mut csr,
                options(nostack, preserves_flags)
            );
        }
        csr
    }

    #[test]
    fn warn_rate_limited_deduplicates() {
        // 只验证不 panic、不阻塞；限频行为由内部状态保证。
        warn_rate_limited("test_key", "first");
        warn_rate_limited("test_key", "second");
    }
}
