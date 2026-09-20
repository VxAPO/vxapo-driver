//! utils/align.rs — SIMD 宽度对齐内存分配
//!
//! 提供对齐到指定字节边界（16 / 32 字节）的内存分配，用于 SIMD 操作。
//! 缓冲区在初始化阶段分配，实时线程直接使用已分配内存，无堆分配开销。
//!
//! 此模块为纯工具函数，不依赖 Windows API。

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::ptr::NonNull;

use crate::utils::vx_error::VxApoError;

/// 默认 SIMD 对齐字节数（SSE = 16，AVX = 32）。
pub(crate) const SIMD_ALIGN: usize = 32;

/// 对齐到指定边界的连续内存块。
///
/// # 用途
///
/// `engine/buffer.rs` 中的音频采样缓冲区在初始化时分配，实时线程直接使用指针。
/// `align` 确保首地址对齐到 SIMD 宽度，使 `f32x4` / `f32x8` 加载不跨缓存行。
///
/// # 内存布局
///
/// ```text
/// ┌──────────────┬──────────────────────────┐
/// │ padding │ usable(len × sizeof) │
/// └──────────────┴──────────────────────────┘
/// ^
/// returned pointer
/// ```
///
/// # 实时安全
///
/// 分配发生在非实时路径（`initialize`），分配后所有操作仅涉及指针读写，
/// 不触发堆分配。Drop 时释放内存。
#[derive(Debug)]
pub(crate) struct AlignedBuffer<T: Copy> {
    ptr: NonNull<T>,
    len: usize,
    layout: Layout,
}

impl<T: Copy> AlignedBuffer<T> {
    /// 创建一个长度为 `len`、对齐到 `align` 字节的零初始化缓冲区。
    ///
    /// 尺寸溢出 / 布局超出 `Layout` 可表示范围时返回错误（不 panic）。
    ///
    /// # Panics
    ///
    /// - `align` 不是 2 的幂
    /// - `align` 不是 `size_of::<T>()` 的倍数
    /// - 系统内存不足（OOM，`handle_alloc_error`）
    pub fn new_zeroed(len: usize, align: usize) -> Result<Self, VxApoError> {
        assert!(align.is_power_of_two(), "align must be a power of two");
        assert!(
            align % std::mem::size_of::<T>() == 0,
            "align must be a multiple of size_of::<T>()"
        );

        if len == 0 {
            // 零长度：用 dangling 指针，Drop 不释放
            let Ok(layout) = Layout::from_size_align(0, align) else {
                return Err(VxApoError::internal("AlignedBuffer zero-length layout invalid"));
            };
            return Ok(Self {
                ptr: NonNull::dangling(),
                len: 0,
                layout,
            });
        }

        let elem_size = std::mem::size_of::<T>();
        let total_bytes = len
            .checked_mul(elem_size)
            .ok_or_else(|| VxApoError::internal("AlignedBuffer size overflow"))?;
        let layout = Layout::from_size_align(total_bytes, align)
            .map_err(|e| VxApoError::internal(&format!("AlignedBuffer layout invalid: {e}")))?;

        // SAFETY: layout.size() > 0（因为 len > 0 且 elem_size > 0），
        // align 是 2 的幂且 >= size_of::<T>()，由 assert 保证。
        let raw = unsafe { alloc_zeroed(layout) };
        let ptr = NonNull::new(raw).unwrap_or_else(|| {
            std::alloc::handle_alloc_error(layout);
        }).cast::<T>();

        Ok(Self { ptr, len, layout })
    }

    /// 创建用 `SIMD_ALIGN`（32 字节）对齐的零初始化缓冲区。
    pub fn new(len: usize) -> Result<Self, VxApoError> {
        Self::new_zeroed(len, SIMD_ALIGN)
    }

    /// 返回指向缓冲区首元素的裸指针。
    ///
    /// 供实时路径中通过裸指针直接操作音频采样。
    pub fn as_ptr(&self) -> *mut T {
        self.ptr.as_ptr()
    }

    /// 返回缓冲区元素数量。
    pub fn len(&self) -> usize {
        self.len
    }

    /// 返回缓冲区是否为空。
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 以切片形式访问缓冲区内容（非实时路径调试/测试用）。
    pub fn as_slice(&self) -> &[T] {
        // SAFETY: ptr 由 alloc_zeroed 分配，len 个元素均有效且已初始化（全零），
        // 且生命周期内不会被释放（&self 约束）。
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    /// 以可变切片形式访问缓冲区内容（非实时路径调试/测试用）。
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        // SAFETY: 同上，且 &mut self 保证独占访问。
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl<T: Copy> Drop for AlignedBuffer<T> {
    fn drop(&mut self) {
        if self.layout.size() > 0 {
            // SAFETY: ptr 由 alloc_zeroed 使用相同的 layout 分配，
            // layout.size() > 0 保证 pointer 来自有效分配。
            unsafe {
                dealloc(self.ptr.as_ptr() as *mut u8, self.layout);
            }
        }
    }
}

// 禁止 Send + Sync 以外的隐式跨线程使用——
// 实时线程通过 Arc/裸指针显式传递所有权。
unsafe impl<T: Copy + Send> Send for AlignedBuffer<T> {}
unsafe impl<T: Copy + Send + Sync> Sync for AlignedBuffer<T> {}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_length() {
        let buf = AlignedBuffer::<f32>::new(0).unwrap();
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn alignment_f32_32() {
        let buf = AlignedBuffer::<f32>::new(256).unwrap();
        let ptr = buf.as_ptr() as usize;
        assert_eq!(ptr % 32, 0, "pointer {ptr:#x} not 32-byte aligned");
    }

    #[test]
    fn alignment_f32_16() {
        let buf = AlignedBuffer::<f32>::new_zeroed(64, 16).unwrap();
        let ptr = buf.as_ptr() as usize;
        assert_eq!(ptr % 16, 0, "pointer {ptr:#x} not 16-byte aligned");
    }

    #[test]
    fn zero_initialized() {
        let buf = AlignedBuffer::<f32>::new(1024).unwrap();
        let slice = buf.as_slice();
        assert!(slice.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn as_mut_slice_write_read() {
        let mut buf = AlignedBuffer::<f32>::new(4).unwrap();
        let slice = buf.as_mut_slice();
        slice[0] = 1.0;
        slice[1] = 2.0;
        slice[2] = 3.0;
        slice[3] = 4.0;
        assert_eq!(buf.as_slice(), &[1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn large_buffer() {
        // 模拟 8 通道 × 48000 采样（1 秒 @ 48kHz）
        let buf = AlignedBuffer::<f32>::new(8 * 48000).unwrap();
        assert_eq!(buf.len(), 384_000);
        let ptr = buf.as_ptr() as usize;
        assert_eq!(ptr % 32, 0);
    }

    #[test]
    fn drop_releases_memory() {
        // 基本的 drop 不 panic 测试
        let buf = AlignedBuffer::<f32>::new(1024).unwrap();
        drop(buf);
    }

    #[test]
    #[should_panic(expected = "power of two")]
    fn align_not_power_of_two_panics() {
        let _ = AlignedBuffer::<f32>::new_zeroed(10, 24);
    }

    #[test]
    fn u8_buffer() {
        let mut buf = AlignedBuffer::<u8>::new(16).unwrap();
        let slice = buf.as_mut_slice();
        for i in 0..16 {
            slice[i] = i as u8;
        }
        assert_eq!(buf.as_slice(), &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
    }

    #[test]
    fn i16_buffer() {
        let buf = AlignedBuffer::<i16>::new_zeroed(32, 32).unwrap();
        assert_eq!(buf.len(), 32);
        let ptr = buf.as_ptr() as usize;
        assert_eq!(ptr % 32, 0);
    }

    #[test]
    fn size_overflow_returns_error() {
        // len × size_of::<T>() 溢出 usize → 错误（而非 panic：panic=abort 下不可恢复）。
        let r = AlignedBuffer::<f32>::new_zeroed(usize::MAX, 32);
        assert!(r.is_err());
    }

    #[test]
    fn layout_too_large_returns_error() {
        // u8 逐字节不溢出，但超出 Layout 可表示范围（> isize::MAX）→ 错误。
        let r = AlignedBuffer::<u8>::new_zeroed(isize::MAX as usize + 1, 32);
        assert!(r.is_err());
    }
}
