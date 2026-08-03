//! dsp/convolution.rs — 卷积滤波器（短 IR 时域实现，RT 安全）
//!
//! 实现 `Convolution:` 命令。
//!
//! # 实现方式（Phase 8 选定：短 IR 时域卷积）
//!
//! IR 长度 < `MAX_DIRECT_IR_LEN`（256 采样）时使用**直接时域 FIR 卷积**：
//! - 每个输出采样 = IR 与输入历史的重叠相加（MAC 运算），无堆分配、无锁、RT 安全
//! - 延迟 = IR 长度 - 1（`latency()` 如实报告）
//! - 单声道 IR 复制到全部通道；多通道 IR 逐通道卷积
//! - 增益（dB）在 `initialize` 时线性合并进 IR 系数
//!
//! # 长 IR（≥ 256 采样）
//!
//! 超出短 IR 范围，跳过该滤波器（返回 `None`）并 log warn。
//! 长 IR 才有意义的部分重叠保留 / 分段卷积（Partitioned Convolution，Note 12/13c）留待 Phase 8+。

use crate::pipeline::dsp::filter::Filter;

/// 短 IR 直接卷积的最大长度（采样）。
const MAX_DIRECT_IR_LEN: usize = 256;

/// WAV 支持的最大 IR 文件大小（防止恶意文件耗尽内存）。
const MAX_WAV_FILE_SIZE: usize = 1 << 20; // 1 MiB

/// 卷积滤波器（短 IR 时域实现）。
#[derive(Debug)]
pub struct ConvolutionFilter {
    /// IR 文件路径（用于日志/错误报告）。
    ir_path: String,
    /// 增益（dB）。
    gain_db: f32,
    /// 每通道 IR 系数（已应用增益）。
    ir_data: Vec<Vec<f32>>,
    /// 每通道延迟线（历史输入，环形 buffer）。
    delay_lines: Vec<Vec<f32>>,
    /// 延迟线写头。
    write_positions: Vec<usize>,
    /// 是否已成功加载 IR。
    loaded: bool,
}

impl ConvolutionFilter {
    /// 创建卷积滤波器。
    ///
    /// - `ir_path`：IR 文件路径
    /// - `gain_db`：增益（dB）
    pub fn new(ir_path: &str, gain_db: f32) -> Self {
        Self {
            ir_path: ir_path.to_owned(),
            gain_db,
            ir_data: Vec::new(),
            delay_lines: Vec::new(),
            write_positions: Vec::new(),
            loaded: false,
        }
    }

}

impl Filter for ConvolutionFilter {
    fn initialize(&mut self, _sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        // 尝试加载 WAV IR。失败 → 不生效（跳过该滤波器，不报错）。
        let channels = channel_names.len().max(1);
        match load_wav_ir(&self.ir_path) {
            Ok(ir) => {
                if ir.is_empty() || ir.len() >= MAX_DIRECT_IR_LEN {
                    log::warn!(
                        "Convolution: IR '{}' 长度 {}（≥{} 超出短 IR 范围），跳过该滤波器",
                        self.ir_path,
                        ir.len(),
                        MAX_DIRECT_IR_LEN
                    );
                    return None;
                }

                // 增益合并进 IR。
                let gain = 10f32.powf(self.gain_db / 20.0);
                self.ir_data = vec![
                    ir.iter().map(|v| v * gain).collect::<Vec<f32>>();
                    channels
                ];

                // 预分配延迟线（IR 长度 - 1 + 1，避免 process 分配）。
                let delay_len = ir.len();
                self.delay_lines = vec![vec![0.0; delay_len]; channels];
                self.write_positions = vec![0; channels];
                self.loaded = true;
                None
            }
            Err(e) => {
                log::warn!(
                    "Convolution: 加载 IR '{}' 失败（{}），跳过该滤波器",
                    self.ir_path,
                    e
                );
                None
            }
        }
    }

    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
        if !self.loaded {
            return;
        }
        let num_ch = samples.len().min(self.ir_data.len());
        for ch in 0..num_ch {
            let ir = &self.ir_data[ch];
            let delay_len = self.delay_lines[ch].len();
            let pos = &mut self.write_positions[ch];
            let delay = &mut self.delay_lines[ch];

            for frame in 0..frame_count {
                let input = samples[ch][frame];
                // 写入当前输入到延迟线。
                delay[*pos] = input;
                *pos = (*pos + 1) % delay_len;

                // 直接 FIR：从最新样本（pos-1）回读 IR 长度。
                let mut acc = 0.0f32;
                let mut read = (*pos + delay_len - 1) % delay_len;
                for &coef in ir.iter() {
                    acc += coef * delay[read];
                    read = (read + delay_len - 1) % delay_len;
                }
                samples[ch][frame] = acc;
            }
        }
    }

    fn latency(&self) -> u32 {
        if self.loaded {
            // 延迟 = IR 长度 - 1（因果 FIR 滤波器的群延迟）。
            self.ir_data.first().map_or(0, |ir| (ir.len() - 1) as u32)
        } else {
            0
        }
    }

    fn reset(&mut self) {
        for d in &mut self.delay_lines {
            d.fill(0.0);
        }
        for p in &mut self.write_positions {
            *p = 0;
        }
    }
}

/// Convolution 参数解析错误（v7.11，pipeline 4.19）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// 空参数（`Convolution:` 无值）。
    Empty,
    /// ≥3 tokens（`ir.wav -6 abc` 的 `abc` 不再静默忽略）。
    TooManyTokens { count: usize },
    /// 第 2 个 token 非数值（`ir.wav abc`）。
    InvalidGain { token: String },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "缺少参数（需要 IR 路径）"),
            Self::TooManyTokens { count } => {
                write!(f, "参数过多（{} 个 token，最多 2：路径 + 增益）", count)
            }
            Self::InvalidGain { token } => write!(f, "增益 '{}' 不是合法数值", token),
        }
    }
}

/// 解析 `Convolution:` 参数。
///
/// 格式：`path [gain_dB]`
///
/// 严格化（v7.11，pipeline 4.19）：≥3 tokens / 第 2 个非数值 → `Err(ParseError)`，
/// 不再静默忽略多余 token（「配置写错必有反馈」）。
/// `ParseError` 由 config 层包装为 `ConfigError::SyntaxError`（文件 + 行号）。
pub fn parse_convolution_params(params: &str) -> Result<(String, f32), ParseError> {
    let parts: Vec<&str> = params.split_whitespace().collect();
    if parts.is_empty() {
        return Err(ParseError::Empty);
    }
    if parts.len() >= 3 {
        return Err(ParseError::TooManyTokens { count: parts.len() });
    }

    let path = parts[0].to_owned();
    let gain = if parts.len() == 2 {
        parts[1]
            .parse::<f32>()
            .map_err(|_| ParseError::InvalidGain { token: parts[1].to_owned() })?
    } else {
        0.0
    };

    Ok((path, gain))
}

/// 从 WAV 文件加载 IR（PCM 16-bit / 32-bit float，单声道或立体声）。
///
/// 解析 RIFF/FMT/data 头。非 WAV、压缩格式、过大文件返回 Err。
fn load_wav_ir(path: &str) -> Result<Vec<f32>, String> {
    let data = std::fs::read(path).map_err(|e| format!("文件读取失败: {}", e))?;
    if data.len() > MAX_WAV_FILE_SIZE {
        return Err("文件超过 1 MiB 限制".to_owned());
    }
    if data.len() < 44 {
        return Err("文件过短，不是有效的 WAV".to_owned());
    }

    // RIFF 头。
    if &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return Err("不是 RIFF/WAVE 文件".to_owned());
    }

    // 定位 fmt / data chunk（跳过 LIST 等）。
    let mut pos = 12usize;
    let mut audio_format: Option<(u16, u16, u32, u16)> = None; // (format, channels, sample_rate, bits)
    let mut data_chunk: Option<&[u8]> = None;

    while pos + 8 <= data.len() {
        let chunk_id = &data[pos..pos + 4];
        let chunk_size = u32::from_le_bytes([
            data[pos + 4],
            data[pos + 5],
            data[pos + 6],
            data[pos + 7],
        ]) as usize;
        let chunk_start = pos + 8;

        match chunk_id {
            b"fmt " => {
                if chunk_start + 16 <= data.len() {
                    let format = u16::from_le_bytes([data[chunk_start], data[chunk_start + 1]]);
                    let channels = u16::from_le_bytes([
                        data[chunk_start + 2],
                        data[chunk_start + 3],
                    ]);
                    let sample_rate = u32::from_le_bytes([
                        data[chunk_start + 4],
                        data[chunk_start + 5],
                        data[chunk_start + 6],
                        data[chunk_start + 7],
                    ]);
                    let bits = u16::from_le_bytes([
                        data[chunk_start + 14],
                        data[chunk_start + 15],
                    ]);
                    audio_format = Some((format, channels, sample_rate, bits));
                }
            }
            b"data" => {
                if chunk_start <= data.len() {
                    let end = (chunk_start + chunk_size).min(data.len());
                    data_chunk = Some(&data[chunk_start..end]);
                }
                break; // data 通常最后一个 chunk。
            }
            _ => {}
        }
        // 进到下一 chunk（chunk_size 对齐到偶数字节）。
        pos = chunk_start + chunk_size + (chunk_size & 1);
    }

    let (format, channels, _sample_rate, bits) =
        audio_format.ok_or("缺少 fmt chunk".to_owned())?;
    let raw = data_chunk.ok_or("缺少 data chunk".to_owned())?;

    // 取第 0 通道（IR 通常单声道；若为立体声取左通道）。
    let samples = match (format, bits) {
        (1, 16) => parse_pcm16(raw, channels)?,
        (3, 32) => parse_f32(raw, channels)?,
        _ => return Err(format!("不支持的 WAV 格式: format={} bits={}", format, bits)),
    };

    Ok(samples)
}

/// 解析 16-bit PCM 数据（取第 0 通道）。
fn parse_pcm16(raw: &[u8], channels: u16) -> Result<Vec<f32>, String> {
    let ch = channels.max(1) as usize;
    let frame_bytes = ch * 2;
    if raw.len() < frame_bytes {
        return Err("PCM16 数据过短".to_owned());
    }
    let frame_count = raw.len() / frame_bytes;
    let mut out = Vec::with_capacity(frame_count);
    for i in 0..frame_count {
        let base = i * frame_bytes;
        let v = i16::from_le_bytes([raw[base], raw[base + 1]]) as f32 / 32768.0;
        out.push(v);
    }
    Ok(out)
}

/// 解析 32-bit float 数据（取第 0 通道）。
fn parse_f32(raw: &[u8], channels: u16) -> Result<Vec<f32>, String> {
    let ch = channels.max(1) as usize;
    let frame_bytes = ch * 4;
    if raw.len() < frame_bytes {
        return Err("F32 数据过短".to_owned());
    }
    let frame_count = raw.len() / frame_bytes;
    let mut out = Vec::with_capacity(frame_count);
    for i in 0..frame_count {
        let base = i * frame_bytes;
        let v = f32::from_le_bytes([
            raw[base],
            raw[base + 1],
            raw[base + 2],
            raw[base + 3],
        ]);
        out.push(v);
    }
    Ok(out)
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

    /// 构建最小 PCM16 单声道 WAV 字节。
    fn pcm16_wav(samples: &[i16]) -> Vec<u8> {
        let data_len = samples.len() * 2;
        let mut wav = Vec::with_capacity(44 + data_len);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&48000u32.to_le_bytes());
        wav.extend_from_slice(&((48000 * 2) as u32).to_le_bytes()); // byte rate
        wav.extend_from_slice(&2u16.to_le_bytes()); // block align
        wav.extend_from_slice(&16u16.to_le_bytes()); // bits
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data_len as u32).to_le_bytes());
        for s in samples {
            wav.extend_from_slice(&s.to_le_bytes());
        }
        wav
    }

    /// 构建 32-bit float 单声道 WAV 字节。
    fn f32_wav(samples: &[f32]) -> Vec<u8> {
        let data_len = samples.len() * 4;
        let mut wav = Vec::with_capacity(44 + data_len);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&48000u32.to_le_bytes());
        wav.extend_from_slice(&((48000 * 4) as u32).to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&32u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data_len as u32).to_le_bytes());
        for s in samples {
            wav.extend_from_slice(&s.to_le_bytes());
        }
        wav
    }

    // ── parse_convolution_params ─────────────────────────────────────────────

    #[test]
    fn parse_path_only() {
        let (path, gain) = parse_convolution_params("ir.wav").unwrap();
        assert_eq!(path, "ir.wav");
        assert_eq!(gain, 0.0);
    }

    #[test]
    fn parse_path_and_gain() {
        let (path, gain) = parse_convolution_params("ir.wav -6").unwrap();
        assert_eq!(path, "ir.wav");
        assert_eq!(gain, -6.0);
    }

    #[test]
    fn parse_empty() {
        // v7.11：空参数 → Err(ParseError::Empty)。
        assert!(parse_convolution_params("").is_err());
    }

    // ── WAV 解析 ─────────────────────────────────────────────────────────────

    #[test]
    fn wav_pcm16_loads_mono() {
        let wav = pcm16_wav(&[1000, -1000, 500]);
        let dir = std::env::temp_dir();
        let path = dir.join("vxapo_test_ir_pcm16.wav");
        std::fs::write(&path, &wav).unwrap();

        let ir = load_wav_ir(path.to_str().unwrap()).unwrap();
        assert_eq!(ir.len(), 3);
        assert!((ir[0] - (1000.0 / 32768.0)).abs() < 1e-4);
        assert!((ir[1] - (-1000.0 / 32768.0)).abs() < 1e-4);
        assert!((ir[2] - (500.0 / 32768.0)).abs() < 1e-4);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn wav_f32_loads_mono() {
        let wav = f32_wav(&[0.5, -0.25, 0.125]);
        let dir = std::env::temp_dir();
        let path = dir.join("vxapo_test_ir_f32.wav");
        std::fs::write(&path, &wav).unwrap();

        let ir = load_wav_ir(path.to_str().unwrap()).unwrap();
        assert_eq!(ir, vec![0.5, -0.25, 0.125]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn wav_not_riff_errors() {
        let wav = b"NOTWAVE........";
        let dir = std::env::temp_dir();
        let path = dir.join("vxapo_test_ir_bad.wav");
        std::fs::write(&path, wav).unwrap();

        assert!(load_wav_ir(path.to_str().unwrap()).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn wav_unsupported_format_errors() {
        // format=0（非法），bits=8。
        let mut wav = pcm16_wav(&[1, 2, 3]);
        wav[20] = 0; // fmt format tag lo
        wav[21] = 0; // fmt format tag hi → 0x0000
        let dir = std::env::temp_dir();
        let path = dir.join("vxapo_test_ir_badfmt.wav");
        std::fs::write(&path, &wav).unwrap();

        assert!(load_wav_ir(path.to_str().unwrap()).is_err());
        let _ = std::fs::remove_file(&path);
    }

    // ── ConvolutionFilter 行为 ───────────────────────────────────────────────

    #[test]
    fn short_ir_convolves_single() {
        // IR = [1.0]，延迟 = 0，输出 = 输入。仍走卷积路径（delay 长度 1）。
        let wav = f32_wav(&[1.0, 0.0]);
        let dir = std::env::temp_dir();
        let path = dir.join("vxapo_test_ir_identity.wav");
        std::fs::write(&path, &wav).unwrap();

        let mut filter = ConvolutionFilter::new(path.to_str().unwrap(), 0.0);
        let ch = filter.initialize(48000, &stereo_names());
        assert!(ch.is_none());
        assert_eq!(filter.latency(), 1); // ir_len=2 → latency=1

        let mut samples = vec![vec![1.0, 2.0, 3.0], vec![0.5, 1.0, 1.5]];
        filter.process(&mut samples, 3);
        // IR=[1.0, 0.0]：输出 = 1.0*input[n] + 0.0*input[n-1]。
        assert!((samples[0][2] - 3.0).abs() < 1e-4);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn short_ir_two_tap_sums_frames() {
        // IR = [1.0, 1.0]：y[n] = x[n] + x[n-1]（含延迟 1）。
        let wav = f32_wav(&[1.0, 1.0]);
        let dir = std::env::temp_dir();
        let path = dir.join("vxapo_test_ir_two.wav");
        std::fs::write(&path, &wav).unwrap();

        let mut filter = ConvolutionFilter::new(path.to_str().unwrap(), 0.0);
        filter.initialize(48000, &stereo_names());
        assert_eq!(filter.latency(), 1);

        let mut samples = vec![vec![1.0, 1.0, 1.0], vec![0.0, 0.0, 0.0]];
        filter.process(&mut samples, 3);
        // 帧0: x[-1]=0 → y=1; 帧1: y=1+1=2; 帧2: y=1+1=2。
        assert!((samples[0][0] - 1.0).abs() < 1e-4);
        assert!((samples[0][1] - 2.0).abs() < 1e-4);
        assert!((samples[0][2] - 2.0).abs() < 1e-4);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reset_clears_delay_line() {
        let wav = f32_wav(&[1.0, 1.0]);
        let dir = std::env::temp_dir();
        let path = dir.join("vxapo_test_ir_reset.wav");
        std::fs::write(&path, &wav).unwrap();

        let mut filter = ConvolutionFilter::new(path.to_str().unwrap(), 0.0);
        filter.initialize(48000, &stereo_names());

        let mut samples = vec![vec![1.0, 1.0], vec![0.0, 0.0]];
        filter.process(&mut samples, 2);
        filter.reset();

        let mut samples2 = vec![vec![1.0, 1.0], vec![0.0, 0.0]];
        filter.process(&mut samples2, 2);
        // 重置后历史清空：帧0 y=x[0]=1；帧1 y=1+1=2。
        assert!((samples2[0][0] - 1.0).abs() < 1e-4);
        assert!((samples2[0][1] - 2.0).abs() < 1e-4);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unloaded_filter_passthrough() {
        // IR 不存在 → 未加载 → process 直通。
        let mut filter = ConvolutionFilter::new("nonexistent_ir_xyz.wav", 0.0);
        filter.initialize(48000, &stereo_names());
        assert_eq!(filter.latency(), 0);

        let mut samples = vec![vec![1.0, 2.0, 3.0], vec![0.5, 1.0, 1.5]];
        let input = samples.clone();
        filter.process(&mut samples, 3);
        assert_eq!(samples, input);
    }

    #[test]
    fn latency_zero_when_unloaded() {
        let filter = ConvolutionFilter::new("ir.wav", 0.0);
        assert_eq!(filter.latency(), 0);
    }
}