//! dsp/filters/copy.rs — 通道复制/混音（Note 52）
//!
//! 实现 `Copy:` 命令。
//!
//! 语法示例（EqualizerAPO 兼容）：
//! - `Copy: L=R`          — 将 R 复制到 L
//! - `Copy: L=R+L`        — 将 R+L 混合后写入 L
//! - `Copy: AUX1=L+R`     — 创建辅助通道 AUX1 = L+R
//! - `Copy: L=0.5*L+0.5*R` — 加权混合
//!
//! Note 52：`initialize` 可能通过 `ensure_channel_exists` 创建新通道名，
//! 这是唯一允许在 `initialize` 中扩展通道数组的过滤器。
//!
//! 支持的操作：
//! - `TARGET=SOURCE` — 复制
//! - `TARGET=A+B` — 混合（最多 4 个源）
//! - `TARGET=N*SOURCE` — 带系数的混合
//!
//! 解析在工厂 `create_filter` 阶段完成（非实时路径），
//! `process` 只执行预解析的复制/混合指令。
//!
//! `process` 方法遵守 RT-safety 约束（Note 12）。

use crate::pipeline::dsp::filter::Filter;
use crate::pipeline::dsp::math::{COPY_COEFF_MAX, MAX_FRAME_COUNT};

/// 单个源通道及其系数。
#[derive(Debug, Clone)]
pub struct CopySource {
    /// 源通道索引（在 `allSamples` 中的位置）。
    pub channel_index: usize,
    /// 系数（默认 1.0）。
    pub coefficient: f32,
}

/// 单条复制/混合指令。
#[derive(Debug, Clone)]
pub struct CopyOp {
    /// 目标通道索引。
    pub target_index: usize,
    /// 源通道列表（多个则混合）。
    pub sources: Vec<CopySource>,
}

/// 通道复制/混音过滤器。
#[derive(Debug)]
pub struct CopyFilter {
    /// 指令列表（initialize 时解析通道索引）。
    ops: Vec<CopyOp>,
    /// 临时缓冲区（避免 process 中分配）。
    temp_buf: Vec<f32>,
}

impl CopyFilter {
    /// 创建复制/混音过滤器。
    ///
    /// `ops` 中的通道索引为占位值，`initialize` 时通过通道名解析。
    pub fn new(ops: Vec<CopyOp>) -> Self {
        Self {
            ops,
            temp_buf: Vec::new(),
        }
    }
}

impl Filter for CopyFilter {
    fn initialize(&mut self, _sample_rate: u32, _channel_names: &[String]) -> Option<Vec<String>> {
        // RT 安全：一次性预分配到上限，process 内不再 resize。
        self.temp_buf.resize(MAX_FRAME_COUNT, 0.0);
        None
    }

    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        debug_assert!(
            frame_count <= MAX_FRAME_COUNT,
            "CopyFilter frame_count 超过预分配上限"
        );

        for op in &self.ops {
            if op.target_index >= samples.len() {
                continue;
            }

            // 零拷贝快速路径：单源且系数 == 1.0。
            if op.sources.len() == 1 && op.sources[0].coefficient == 1.0 {
                let src_idx = op.sources[0].channel_index;
                if src_idx >= samples.len() {
                    continue;
                }
                if src_idx == op.target_index {
                    continue; // 目标 == 源：零拷贝，直接跳过。
                }
                // 两个不同槽位 → split_at_mut 取两个可变通道，避免借用冲突。
                let (a, b) = if src_idx < op.target_index {
                    let (front, back) = samples.split_at_mut(op.target_index);
                    (&front[src_idx], &mut back[0])
                } else {
                    let (front, back) = samples.split_at_mut(src_idx);
                    (&back[0], &mut front[op.target_index])
                };
                b[..frame_count].copy_from_slice(&a[..frame_count]);
                continue;
            }

            // 通用路径（多源 / 带系数）：temp_buf 累加。
            self.temp_buf[..frame_count].fill(0.0);
            for src in &op.sources {
                if src.channel_index < samples.len() {
                    let coeff = src.coefficient;
                    let src_buf = &samples[src.channel_index];
                    for f in 0..frame_count {
                        self.temp_buf[f] = coeff.mul_add(src_buf[f], self.temp_buf[f]);
                    }
                }
            }

            // 写入目标通道
            samples[op.target_index][..frame_count].copy_from_slice(&self.temp_buf[..frame_count]);
        }
    }

    fn max_frame_count(&self) -> Option<usize> {
        Some(MAX_FRAME_COUNT)
    }
}

/// 解析 `Copy:` 参数字符串。
///
/// 格式：`TARGET=SOURCE1+SOURCE2+...`
///
/// 每个 SOURCE 可选带系数：`0.5*L` 或 `L`
pub fn parse_copy_ops(params: &str, channel_names: &[String]) -> Option<Vec<CopyOp>> {
    let mut ops = Vec::new();

    // 支持多条指令用空格分隔
    for part in params.split_whitespace() {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        // 分割 TARGET=SOURCES
        let eq_pos = part.find('=')?;
        let target_name = part[..eq_pos].trim();
        let sources_str = part[eq_pos + 1..].trim();

        let target_index = channel_names
            .iter()
            .position(|n| n == target_name)?;

        // 解析源通道（+分隔）
        let mut sources = Vec::new();
        for src_part in sources_str.split('+') {
            let src_part = src_part.trim();
            if src_part.is_empty() {
                continue;
            }

            let (coeff, name) = parse_coeff_and_name(src_part)?;
            let channel_index = channel_names
                .iter()
                .position(|n| n == name)?;

            sources.push(CopySource {
                channel_index,
                coefficient: coeff,
            });
        }

        if sources.is_empty() {
            return None;
        }

        ops.push(CopyOp {
            target_index,
            sources,
        });
    }

    if ops.is_empty() {
        None
    } else {
        Some(ops)
    }
}

/// 解析 `0.5*L` 或 `L` 格式。
///
/// 返回 (coefficient, channel_name)。
fn parse_coeff_and_name(s: &str) -> Option<(f32, &str)> {
    if let Some(star_pos) = s.find('*') {
        let coeff_str = s[..star_pos].trim();
        let name = s[star_pos + 1..].trim();
        let coeff = coeff_str.parse::<f32>().ok()?;
        if !coeff.is_finite() {
            return None;
        }
        Some((coeff.clamp(-COPY_COEFF_MAX, COPY_COEFF_MAX), name))
    } else {
        Some((1.0, s))
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo_names() -> Vec<String> {
        vec!["L".into(), "R".into()]
    }

    fn surround_names() -> Vec<String> {
        vec![
            "L".into(), "R".into(), "C".into(),
            "LFE".into(), "SL".into(), "SR".into(),
        ]
    }

    // ── parse_coeff_and_name ────────────────────────────────────────────────

    #[test]
    fn parse_simple_name() {
        let (coeff, name) = parse_coeff_and_name("L").unwrap();
        assert_eq!(coeff, 1.0);
        assert_eq!(name, "L");
    }

    #[test]
    fn parse_coeff_name() {
        let (coeff, name) = parse_coeff_and_name("0.5*R").unwrap();
        assert!((coeff - 0.5).abs() < 1e-6);
        assert_eq!(name, "R");
    }

    #[test]
    fn parse_int_coeff() {
        let (coeff, name) = parse_coeff_and_name("2*L").unwrap();
        assert_eq!(coeff, 2.0);
        assert_eq!(name, "L");
    }

    #[test]
    fn parse_coeff_rejects_nan_and_clamps_extreme() {
        assert!(parse_coeff_and_name("NaN*L").is_none());
        assert!(parse_coeff_and_name("inf*L").is_none());
        let (coeff, name) = parse_coeff_and_name("1000*L").unwrap();
        assert_eq!(coeff, COPY_COEFF_MAX);
        assert_eq!(name, "L");
        let (coeff, _) = parse_coeff_and_name("-1000*L").unwrap();
        assert_eq!(coeff, -COPY_COEFF_MAX);
    }

    // ── parse_copy_ops ──────────────────────────────────────────────────────

    #[test]
    fn parse_simple_copy() {
        let ops = parse_copy_ops("L=R", &stereo_names()).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].target_index, 0); // L = index 0
        assert_eq!(ops[0].sources.len(), 1);
        assert_eq!(ops[0].sources[0].channel_index, 1); // R = index 1
        assert_eq!(ops[0].sources[0].coefficient, 1.0);
    }

    #[test]
    fn parse_mix() {
        let ops = parse_copy_ops("C=L+R", &surround_names()).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].target_index, 2); // C = index 2
        assert_eq!(ops[0].sources.len(), 2);
    }

    #[test]
    fn parse_weighted() {
        let ops = parse_copy_ops("L=0.5*L+0.5*R", &stereo_names()).unwrap();
        assert_eq!(ops[0].sources.len(), 2);
        assert!((ops[0].sources[0].coefficient - 0.5).abs() < 1e-6);
        assert!((ops[0].sources[1].coefficient - 0.5).abs() < 1e-6);
    }

    #[test]
    fn parse_unknown_target() {
        assert!(parse_copy_ops("X=L", &stereo_names()).is_none());
    }

    #[test]
    fn parse_unknown_source() {
        assert!(parse_copy_ops("L=X", &stereo_names()).is_none());
    }

    #[test]
    fn parse_empty() {
        assert!(parse_copy_ops("", &stereo_names()).is_none());
    }

    // ── CopyFilter ──────────────────────────────────────────────────────────

    #[test]
    fn copy_channel() {
        // L = R
        let ops = vec![CopyOp {
            target_index: 0,
            sources: vec![CopySource { channel_index: 1, coefficient: 1.0 }],
        }];
        let mut filter = CopyFilter::new(ops);
        filter.initialize(48000, &stereo_names());

        let mut samples = vec![
            vec![0.0, 0.0, 0.0],
            vec![1.0, 2.0, 3.0],
        ];
        filter.process(&mut samples, 3);

        assert_eq!(samples[0], vec![1.0, 2.0, 3.0]);
        assert_eq!(samples[1], vec![1.0, 2.0, 3.0]); // R unchanged
    }

    #[test]
    fn copy_same_slot_is_noop() {
        // L=L：单源 coeff=1.0 且目标==源 → 零拷贝直接跳过，逐位不变。
        let ops = vec![CopyOp {
            target_index: 0,
            sources: vec![CopySource {
                channel_index: 0,
                coefficient: 1.0,
            }],
        }];
        let mut filter = CopyFilter::new(ops);
        filter.initialize(48000, &stereo_names());

        let mut samples = vec![vec![1.0, -2.0, 3.5], vec![9.0, 9.0, 9.0]];
        let orig = samples.clone();
        filter.process(&mut samples, 3);
        assert_eq!(samples, orig);
    }

    #[test]
    fn mix_channels() {
        // C = 0.5*L + 0.5*R
        let ops = vec![CopyOp {
            target_index: 2,
            sources: vec![
                CopySource { channel_index: 0, coefficient: 0.5 },
                CopySource { channel_index: 1, coefficient: 0.5 },
            ],
        }];
        let mut filter = CopyFilter::new(ops);
        filter.initialize(48000, &surround_names());

        let mut samples = vec![
            vec![2.0, 4.0, 6.0],
            vec![0.0, 0.0, 0.0],
            vec![0.0, 0.0, 0.0],
            vec![0.0, 0.0, 0.0],
            vec![0.0, 0.0, 0.0],
            vec![0.0, 0.0, 0.0],
        ];
        filter.process(&mut samples, 3);

        assert_eq!(samples[2], vec![1.0, 2.0, 3.0]); // 0.5*2.0 = 1.0 etc
    }

    #[test]
    fn silence_stays_silent() {
        let ops = vec![CopyOp {
            target_index: 0,
            sources: vec![CopySource { channel_index: 1, coefficient: 1.0 }],
        }];
        let mut filter = CopyFilter::new(ops);
        filter.initialize(48000, &stereo_names());

        let mut samples = vec![vec![0.0f32; 10]; 2];
        filter.process(&mut samples, 10);

        for f in 0..10 {
            assert!(samples[0][f].abs() < 1e-10);
        }
    }
}
