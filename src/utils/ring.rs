//! utils/ring.rs — SPSC 无锁环形缓冲区（规范 4.8）

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

/// SPSC 无锁环形缓冲区，存储固定大小、实现 `Copy + Default` 的类型。
pub struct RingBuffer<T: Copy + Default> {
    data: UnsafeCell<Box<[T]>>,
    capacity: usize,
    write_pos: AtomicUsize,
    read_pos: AtomicUsize,
}

// SAFETY: SPSC 设计，push/pop 由不同线程调用，原子操作提供内存同步。
unsafe impl<T: Copy + Default> Send for RingBuffer<T> {}
// SAFETY: `Sync` 的前提是 &self 接口不给出跨界别名——本类型的 &self 方法只有
// push/pop/len 等，槽位的写权限按 SPSC 协议限定在 push 侧、读权限限定在 pop 侧，
// 且 T: Copy 保证读出的值不携带对内部槽位的引用。
unsafe impl<T: Copy + Default> Sync for RingBuffer<T> {}

impl<T: Copy + Default> RingBuffer<T> {
    /// 创建指定容量（向上取整为 2 的幂）的环形缓冲。
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.next_power_of_two().max(2);
        let data = (0..cap)
            .map(|_| T::default())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            data: UnsafeCell::new(data),
            capacity: cap,
            write_pos: AtomicUsize::new(0),
            read_pos: AtomicUsize::new(0),
        }
    }

    /// 推入元素。满时返回 false。
    pub fn push(&self, value: T) -> bool {
        let write = self.write_pos.load(Ordering::Relaxed);
        let read = self.read_pos.load(Ordering::Acquire);
        if write.wrapping_sub(read) >= self.capacity {
            return false;
        }
        // SAFETY: `data` 是 UnsafeCell；本函数是唯一的写者（SPSC），`write` 位掩码
        // 落在已初始化容量内，且该槽位此刻不被读者持有（read 侧未推进到这里）。
        unsafe {
            (*self.data.get())[write & (self.capacity - 1)] = value;
        }
        self.write_pos
            .store(write.wrapping_add(1), Ordering::Release);
        true
    }

    /// 弹出元素。空时返回 None。
    #[cfg(test)]
    pub fn pop(&self) -> Option<T> {
        let read = self.read_pos.load(Ordering::Relaxed);
        let write = self.write_pos.load(Ordering::Acquire);
        if read == write {
            return None;
        }
        // SAFETY: 同上——本函数是唯一的读者（SPSC），`read` 位掩码落在容量内，
        // 且该槽位必已由 push 侧写入并发布（write_pos 的 Release/Acquire 配对）。
        let value = unsafe { (*self.data.get())[read & (self.capacity - 1)] };
        self.read_pos.store(read.wrapping_add(1), Ordering::Release);
        Some(value)
    }

    #[cfg(test)]
    #[allow(dead_code)] // cfg(test) 专用但当前连测试都未引用：待整链清理后删除
    pub fn is_empty(&self) -> bool {
        self.read_pos.load(Ordering::Acquire) == self.write_pos.load(Ordering::Acquire)
    }

    #[cfg(test)]
    #[allow(dead_code)] // cfg(test) 专用但当前连测试都未引用：待整链清理后删除
    pub fn is_full(&self) -> bool {
        let write = self.write_pos.load(Ordering::Relaxed);
        let read = self.read_pos.load(Ordering::Acquire);
        write.wrapping_sub(read) >= self.capacity
    }

    #[allow(dead_code)] // 死簇：仅被已死的调用链引用，删除需整链评估
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}
