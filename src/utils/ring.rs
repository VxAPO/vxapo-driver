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
unsafe impl<T: Copy + Default> Sync for RingBuffer<T> {}

impl<T: Copy + Default> RingBuffer<T> {
    /// 创建指定容量（向上取整为 2 的幂）的环形缓冲。
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.next_power_of_two().max(2);
        let data = (0..cap).map(|_| T::default()).collect::<Vec<_>>().into_boxed_slice();
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
        unsafe {
            (*self.data.get())[write & (self.capacity - 1)] = value;
        }
        self.write_pos.store(write.wrapping_add(1), Ordering::Release);
        true
    }

    /// 弹出元素。空时返回 None。
    pub fn pop(&self) -> Option<T> {
        let read = self.read_pos.load(Ordering::Relaxed);
        let write = self.write_pos.load(Ordering::Acquire);
        if read == write {
            return None;
        }
        let value = unsafe { (*self.data.get())[read & (self.capacity - 1)] };
        self.read_pos.store(read.wrapping_add(1), Ordering::Release);
        Some(value)
    }

    pub fn is_empty(&self) -> bool {
        self.read_pos.load(Ordering::Acquire) == self.write_pos.load(Ordering::Acquire)
    }

    pub fn is_full(&self) -> bool {
        let write = self.write_pos.load(Ordering::Relaxed);
        let read = self.read_pos.load(Ordering::Acquire);
        write.wrapping_sub(read) >= self.capacity
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }
}
