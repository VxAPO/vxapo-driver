//! pipeline/buffer.rs — 缓冲区描述与状态判定（v6.3 规范 4.2）

use crate::sys::com::apo_types::{APO_BUFFER_FLAGS, APO_CONNECTION_PROPERTY, BUFFER_INVALID, BUFFER_SILENT, BUFFER_VALID};

/// 静音阈值（-200 dBFS 以下）。
const SILENCE_THRESHOLD: f32 = 1e-10;

/// 缓冲区信息——封装 APO_CONNECTION_PROPERTY 的字段。
pub struct BufferInfo {
    pub ptr: *mut f32,
    pub valid_frames: usize,
    pub flags: APO_BUFFER_FLAGS,
    pub channels: usize,
}

impl BufferInfo {
    pub fn new(ptr: *mut f32, valid_frames: usize, flags: APO_BUFFER_FLAGS, channels: usize) -> Self {
        Self { ptr, valid_frames, flags, channels }
    }

    pub fn from_prop(prop: &APO_CONNECTION_PROPERTY, channels: usize) -> Self {
        Self {
            ptr: prop.pBuffer as *mut f32,
            valid_frames: prop.u32ValidFrameCount as usize,
            flags: prop.u32BufferFlags,
            channels,
        }
    }

    pub fn from_prop_mut(prop: &mut APO_CONNECTION_PROPERTY, channels: usize) -> Self {
        Self {
            ptr: prop.pBuffer as *mut f32,
            valid_frames: prop.u32ValidFrameCount as usize,
            flags: prop.u32BufferFlags,
            channels,
        }
    }

    pub fn is_valid(&self) -> bool {
        self.flags == BUFFER_VALID
    }

    pub fn is_silent(&self) -> bool {
        self.flags == BUFFER_SILENT
    }

    pub fn total_samples(&self) -> usize {
        self.valid_frames * self.channels
    }

    pub fn bytes(&self) -> usize {
        self.total_samples() * std::mem::size_of::<f32>()
    }

    /// 交织格式连续切片。
    ///
    /// # Safety
    /// 调用方必须保证 ptr 指向至少 total_samples() 个 f32 元素。
    pub unsafe fn as_slice(&self) -> &[f32] {
        std::slice::from_raw_parts(self.ptr, self.total_samples())
    }

    /// 交织格式连续可变切片。
    ///
    /// # Safety
    /// 调用方必须保证 ptr 指向至少 total_samples() 个 f32 元素。
    pub unsafe fn as_slice_mut(&mut self) -> &mut [f32] {
        std::slice::from_raw_parts_mut(self.ptr, self.total_samples())
    }

    /// 清零缓冲区。
    pub fn zero(&mut self) {
        unsafe {
            std::ptr::write_bytes(self.ptr, 0, self.total_samples());
        }
    }
}

/// 缓冲区处理动作（v6.3 规范 4.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferAction {
    /// 正常处理。
    Process,
    /// 跳过（Invalid）。
    Skip,
    /// 静音（Silent 且不允许修改）。
    Silent,
}

/// 根据输入标志和 allowSilentBuffer 确定处理动作。
pub fn evaluate_buffer(flags: APO_BUFFER_FLAGS, allow_silent_buffer: bool) -> (BufferAction, APO_BUFFER_FLAGS) {
    if flags == BUFFER_INVALID {
        (BufferAction::Skip, BUFFER_INVALID)
    } else if flags == BUFFER_SILENT && !allow_silent_buffer {
        (BufferAction::Silent, BUFFER_SILENT)
    } else if flags == BUFFER_SILENT {
        (BufferAction::Process, BUFFER_SILENT)
    } else {
        (BufferAction::Process, BUFFER_VALID)
    }
}

/// 检查去交织平面缓冲区中所有采样是否为静音。
pub fn is_silent(samples: &[Vec<f32>], frame_count: usize) -> bool {
    samples.iter().all(|ch| ch[..frame_count].iter().all(|&v| v.abs() <= SILENCE_THRESHOLD))
}

/// 将去交织平面缓冲区所有通道清零。
pub fn zero_buffers(buffers: &mut [Vec<f32>], frame_count: usize) {
    for ch in buffers.iter_mut() {
        ch[..frame_count].fill(0.0);
    }
}

/// 将单个通道清零。
pub fn zero_channel(channel: &mut [f32], frame_count: usize) {
    channel[..frame_count].fill(0.0);
}

/// 将源缓冲区内容复制到目标缓冲区（避免隐式堆分配）。
pub fn copy_buffers(src: &[Vec<f32>], dst: &mut [Vec<f32>], frame_count: usize) {
    let n = src.len().min(dst.len());
    for i in 0..n {
        dst[i][..frame_count].copy_from_slice(&src[i][..frame_count]);
    }
}

/// 调试摘要（非实时路径）。
#[derive(Debug, Clone)]
pub struct BufferSummary {
    pub channels: usize,
    pub frame_count: usize,
    pub is_silent: bool,
    pub peak_level: f32,
    pub rms_level: f32,
}

/// 生成缓冲区摘要（非实时路径）。
pub fn summarize(buffers: &[Vec<f32>], frame_count: usize) -> BufferSummary {
    let mut peak = 0.0f32;
    let mut sum_sq = 0.0f32;
    let mut total = 0usize;
    for ch in buffers {
        for &v in &ch[..frame_count] {
            peak = peak.max(v.abs());
            sum_sq += v * v;
            total += 1;
        }
    }
    BufferSummary {
        channels: buffers.len(),
        frame_count,
        is_silent: is_silent(buffers, frame_count),
        peak_level: peak,
        rms_level: if total > 0 { (sum_sq / total as f32).sqrt() } else { 0.0 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluate_valid_process() {
        assert_eq!(evaluate_buffer(BUFFER_VALID, false), (BufferAction::Process, BUFFER_VALID));
    }

    #[test]
    fn evaluate_invalid_skip() {
        assert_eq!(evaluate_buffer(BUFFER_INVALID, true), (BufferAction::Skip, BUFFER_INVALID));
    }

    #[test]
    fn evaluate_silent_no_allow() {
        assert_eq!(evaluate_buffer(BUFFER_SILENT, false), (BufferAction::Silent, BUFFER_SILENT));
    }

    #[test]
    fn evaluate_silent_allow() {
        assert_eq!(evaluate_buffer(BUFFER_SILENT, true), (BufferAction::Process, BUFFER_SILENT));
    }

    #[test]
    fn is_silent_detects() {
        assert!(is_silent(&[vec![0.0, 0.0], vec![0.0, 0.0]], 2));
        assert!(!is_silent(&[vec![0.0, 1.0], vec![0.0, 0.0]], 2));
    }

    #[test]
    fn zero_and_copy() {
        let mut b = vec![vec![1.0, 2.0], vec![3.0, 4.0]];
        zero_buffers(&mut b, 2);
        assert!(b.iter().all(|ch| ch.iter().all(|&v| v == 0.0)));

        let src = vec![vec![5.0, 6.0], vec![7.0, 8.0]];
        copy_buffers(&src, &mut b, 2);
        assert_eq!(b[0][0], 5.0);
        assert_eq!(b[1][1], 8.0);
    }
}