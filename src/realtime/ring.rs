//! realtime/ring.rs — 无锁环形缓冲区（Note 22）
//!
//! 单生产者单消费者（SPSC）无锁环形缓冲区。
//!
//! 被两个模块共同使用：
//! - `engine/`：信号量抽象，`swap.rs` 中的配置交换协调（Note 19）
//! - `telemetry/`：无锁日志，`logger.rs` 的实时安全日志写入（Note 34）
//!
//! 本模块位于 `realtime/` 目录下，定位为"实时安全基础设施"。
//! 未来扩展更多无锁数据结构时保持一致性。
//!
//! 实时安全：
//! - `push`：生产者调用，原子更新 `write_pos`
//! - `pop`：消费者调用，原子更新 `read_pos`
//! - 无堆分配、无互斥锁、无 I/O、无 panic
//!
//! 内存序：
//! - 写入端：`store(write_pos, Release)` — 保证数据写入对读端可见
//! - 读取端：`load(read_pos, Acquire)` + `load(write_pos, Acquire)` — 保证读到最新写入
//!
//! 此模块与 `engine/`、`telemetry/` 无耦合，可在任意上下文安全使用。

use std::sync::atomic::{AtomicUsize, Ordering};

// ══════════════════════════════════════════════════════════════════════════════
// RingBuffer — SPSC 无锁环形缓冲区
// ══════════════════════════════════════════════════════════════════════════════

/// SPSC 无锁环形缓冲区。
///
/// - `T`：元素类型。实时路径中通常为 `u8`（日志）或 `usize`（信号量计数）
/// - `N`：缓冲区容量（编译期常量，必须为 2 的幂）
///
/// # 容量约束
///
/// `N` 必须为 2 的幂，这样 `pos % N` 可以用 `pos & (N - 1)` 替代，
/// 避免昂贵的整数除法。编译期断言验证此约束。
///
/// # Safety
///
/// `RingBuffer` 可以安全地跨线程共享（通过 `Arc<RingBuffer<T, N>>`）。
/// 生产者和消费者必须是不同的线程——同一线程同时 push 和 pop
/// 不会导致数据损坏，但语义上不正确。
pub struct RingBuffer<T: Copy + Default, const N: usize> {
    /// 数据存储（固定大小数组）。
    data: [T; N],
    /// 写入位置（生产者原子更新）。
    write_pos: AtomicUsize,
    /// 读取位置（消费者原子更新）。
    read_pos: AtomicUsize,
}

impl<T: Copy + Default, const N: usize> RingBuffer<T, N> {
    /// 编译期断言：N 必须为 2 的幂且 > 0。
    const _ASSERT_POWER_OF_TWO: () = assert!(
        N > 0 && (N & (N - 1)) == 0,
        "RingBuffer capacity N must be a power of 2"
    );

    /// 创建新的环形缓冲区（全零初始化）。
    pub const fn new() -> Self {
        // 触发编译期断言
        let _ = Self::_ASSERT_POWER_OF_TWO;

        Self {
            // const fn 中无法用 `[T::default(); N]`（T 不一定有 const default），
            // 使用 unsafe 初始化。T: Default 保证所有位模式有效。
            //
            // SAFETY: T: Copy + Default，零初始化或 default 初始化都是有效值。
            // 这里用 unsafe zeroed —— 对于 u8/usize/f32 等 POD 类型是安全的。
            data: unsafe { std::mem::zeroed() },
            write_pos: AtomicUsize::new(0),
            read_pos: AtomicUsize::new(0),
        }
    }

    /// 缓冲区容量（编译期常量）。
    pub const fn capacity(&self) -> usize {
        N
    }

    /// 掩码（N - 1），用于替代 `% N`。
    const MASK: usize = N - 1;

    // ── 生产者接口 ──────────────────────────────────────────────────────────

    /// 尝试推入一个元素。
    ///
    /// - 成功：返回 `Ok(())`
    /// - 缓冲区满：返回 `Err(value)`，调用方保留值的 ownership
    ///
    /// # 实时安全
    ///
    /// 无锁原子操作。无堆分配。
    pub fn push(&self, value: T) -> Result<(), T> {
        let write = self.write_pos.load(Ordering::Relaxed);
        let read = self.read_pos.load(Ordering::Acquire);

        // 判断是否满：下一个写入位置 == 读取位置
        let next_write = write.wrapping_add(1);
        if next_write & Self::MASK == read & Self::MASK {
            return Err(value);
        }

        // SAFETY: write & MASK 始终在 [0, N) 范围内，且此槽位已被消费者读取
        // （因为 read_pos 尚未追上 write_pos）。
        unsafe {
            *self.data.as_ptr().cast_mut().add(write & Self::MASK) = value;
        }

        // Release 保证数据写入对消费者可见
        self.write_pos.store(next_write, Ordering::Release);

        Ok(())
    }

    /// 尝试推入多个元素。
    ///
    /// 返回实际推入的数量。缓冲区满时停止。
    pub fn push_slice(&self, values: &[T]) -> usize {
        let mut pushed = 0;
        for &v in values {
            match self.push(v) {
                Ok(()) => pushed += 1,
                Err(_) => break,
            }
        }
        pushed
    }

    // ── 消费者接口 ──────────────────────────────────────────────────────────

    /// 尝试弹出一个元素。
    ///
    /// - 成功：返回 `Some(value)`
    /// - 缓冲区空：返回 `None`
    ///
    /// # 实时安全
    ///
    /// 无锁原子操作。无堆分配。
    pub fn pop(&self) -> Option<T> {
        let read = self.read_pos.load(Ordering::Relaxed);
        let write = self.write_pos.load(Ordering::Acquire);

        if read & Self::MASK == write & Self::MASK {
            return None; // 空
        }

        // SAFETY: read & MASK 始终在 [0, N) 范围内，且此槽位已被生产者写入
        // （因为 write_pos 已越过 read_pos）。
        let value = unsafe { *self.data.as_ptr().add(read & Self::MASK) };

        // Release 保证读取完成后槽位可被生产者重用
        let next_read = read.wrapping_add(1);
        self.read_pos.store(next_read, Ordering::Release);

        Some(value)
    }

    /// 尝试弹出多个元素到缓冲区。
    ///
    /// 返回实际弹出的数量。
    pub fn pop_slice(&self, output: &mut [T]) -> usize {
        let mut popped = 0;
        for slot in output.iter_mut() {
            match self.pop() {
                Some(v) => {
                    *slot = v;
                    popped += 1;
                }
                None => break,
            }
        }
        popped
    }

    // ── 状态查询 ────────────────────────────────────────────────────────────

    /// 当前已用槽位数（近似值，可能因并发而 slightly off）。
    ///
    /// 仅用于监控/日志，不得用于精确的空/满判断。
    pub fn len(&self) -> usize {
        let write = self.write_pos.load(Ordering::Relaxed);
        let read = self.read_pos.load(Ordering::Relaxed);
        write.wrapping_sub(read)
    }

    /// 是否为空（近似值）。
    pub fn is_empty(&self) -> bool {
        let write = self.write_pos.load(Ordering::Acquire);
        let read = self.read_pos.load(Ordering::Relaxed);
        write & Self::MASK == read & Self::MASK
    }

    /// 是否已满（近似值）。
    pub fn is_full(&self) -> bool {
        let write = self.write_pos.load(Ordering::Relaxed);
        let read = self.read_pos.load(Ordering::Acquire);
        let next_write = write.wrapping_add(1);
        next_write & Self::MASK == read & Self::MASK
    }

    /// 可用容量 = N - 1（一个槽位用于区分空和满）。
    pub fn available_capacity(&self) -> usize {
        N - 1
    }

    /// 清空缓冲区（非线程安全，仅在确认无并发访问时调用）。
    ///
    /// 用于初始化和测试。
    pub fn clear(&self) {
        self.read_pos.store(0, Ordering::Relaxed);
        self.write_pos.store(0, Ordering::Relaxed);
    }
}

// Send + Sync：SPSC 设计保证安全
// SAFETY: data 通过原子操作保护，push 和 pop 分别由不同端调用，
// 原子操作提供必要的内存同步。
unsafe impl<T: Copy + Default, const N: usize> Send for RingBuffer<T, N> {}
unsafe impl<T: Copy + Default, const N: usize> Sync for RingBuffer<T, N> {}

// ══════════════════════════════════════════════════════════════════════════════
// 常用容量预设
// ══════════════════════════════════════════════════════════════════════════════

/// 小型缓冲区（64 槽位）——用于信号量抽象。
pub type Ring64<T> = RingBuffer<T, 64>;

/// 中型缓冲区（1024 槽位）——用于日志缓冲。
pub type Ring1K<T> = RingBuffer<T, 1024>;

/// 大型缓冲区（4096 槽位）——用于高吞吐量日志。
pub type Ring4K<T> = RingBuffer<T, 4096>;

/// 超大型缓冲区（65536 槽位）——用于长时间日志缓冲。
pub type Ring64K<T> = RingBuffer<T, 65536>;

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // ── 基础操作 ────────────────────────────────────────────────────────────

    #[test]
    fn new_is_empty() {
        let ring = RingBuffer::<u8, 64>::new();
        assert!(ring.is_empty());
        assert!(!ring.is_full());
        assert_eq!(ring.len(), 0);
        assert_eq!(ring.capacity(), 64);
        assert_eq!(ring.available_capacity(), 63);
    }

    #[test]
    fn push_pop_single() {
        let ring = RingBuffer::<u8, 64>::new();
        ring.push(42).unwrap();
        assert!(!ring.is_empty());
        assert_eq!(ring.pop(), Some(42));
        assert!(ring.is_empty());
    }

    #[test]
    fn push_pop_fifo() {
        let ring = RingBuffer::<u32, 64>::new();
        for i in 0..10 {
            ring.push(i).unwrap();
        }
        for i in 0..10 {
            assert_eq!(ring.pop(), Some(i));
        }
        assert!(ring.is_empty());
    }

    #[test]
    fn push_until_full() {
        let ring = RingBuffer::<u8, 8>::new(); // 容量 8，可用 7
        for i in 0..7 {
            assert!(ring.push(i).is_ok());
        }
        assert!(ring.is_full());
        assert!(ring.push(99).is_err());
    }

    #[test]
    fn pop_from_empty() {
        let ring = RingBuffer::<u8, 64>::new();
        assert!(ring.pop().is_none());
    }

    // ── wrap-around ─────────────────────────────────────────────────────────

    #[test]
    fn wrap_around_basic() {
        let ring = RingBuffer::<u8, 4>::new(); // 容量 4，可用 3

        // 填满
        ring.push(1).unwrap();
        ring.push(2).unwrap();
        ring.push(3).unwrap();

        // 弹出两个
        assert_eq!(ring.pop(), Some(1));
        assert_eq!(ring.pop(), Some(2));

        // 现在有空间，继续推入
        ring.push(4).unwrap();
        ring.push(5).unwrap();

        // 验证顺序
        assert_eq!(ring.pop(), Some(3));
        assert_eq!(ring.pop(), Some(4));
        assert_eq!(ring.pop(), Some(5));
        assert!(ring.is_empty());
    }

    #[test]
    fn wrap_around_many_cycles() {
        let ring = RingBuffer::<usize, 16>::new();

        for cycle in 0..100 {
            for i in 0..15 {
                ring.push(cycle * 15 + i).unwrap();
            }
            for i in 0..15 {
                assert_eq!(ring.pop(), Some(cycle * 15 + i));
            }
        }
    }

    // ── push_slice / pop_slice ───────────────────────────────────────────────

    #[test]
    fn push_slice_basic() {
        let ring = RingBuffer::<u8, 16>::new();
        let data = [1, 2, 3, 4, 5];
        let pushed = ring.push_slice(&data);
        assert_eq!(pushed, 5);
        assert_eq!(ring.len(), 5);
    }

    #[test]
    fn push_slice_partial() {
        let ring = RingBuffer::<u8, 8>::new(); // 可用 7
        let data = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let pushed = ring.push_slice(&data);
        assert_eq!(pushed, 7);
        assert!(ring.is_full());
    }

    #[test]
    fn pop_slice_basic() {
        let ring = RingBuffer::<u8, 16>::new();
        ring.push_slice(&[10, 20, 30]);

        let mut buf = [0u8; 5];
        let popped = ring.pop_slice(&mut buf);
        assert_eq!(popped, 3);
        assert_eq!(&buf[..3], &[10, 20, 30]);
    }

    #[test]
    fn pop_slice_from_empty() {
        let ring = RingBuffer::<u8, 16>::new();
        let mut buf = [0u8; 5];
        let popped = ring.pop_slice(&mut buf);
        assert_eq!(popped, 0);
    }

    // ── 状态查询 ────────────────────────────────────────────────────────────

    #[test]
    fn len_tracking() {
        let ring = RingBuffer::<u8, 16>::new();
        assert_eq!(ring.len(), 0);

        ring.push(1).unwrap();
        ring.push(2).unwrap();
        assert_eq!(ring.len(), 2);

        ring.pop().unwrap();
        assert_eq!(ring.len(), 1);
    }

    #[test]
    fn is_full_detection() {
        let ring = RingBuffer::<u8, 4>::new();
        assert!(!ring.is_full());

        ring.push(1).unwrap();
        ring.push(2).unwrap();
        ring.push(3).unwrap();
        assert!(ring.is_full());
    }

    // ── clear ───────────────────────────────────────────────────────────────

    #[test]
    fn clear_resets_state() {
        let ring = RingBuffer::<u8, 16>::new();
        ring.push_slice(&[1, 2, 3, 4, 5]);
        assert!(!ring.is_empty());

        ring.clear();
        assert!(ring.is_empty());
        assert_eq!(ring.len(), 0);
    }

    // ── 类型别名 ────────────────────────────────────────────────────────────

    #[test]
    fn ring64_capacity() {
        let ring = Ring64::<u8>::new();
        assert_eq!(ring.capacity(), 64);
    }

    #[test]
    fn ring1k_capacity() {
        let ring = Ring1K::<u8>::new();
        assert_eq!(ring.capacity(), 1024);
    }

    #[test]
    fn ring4k_capacity() {
        let ring = Ring4K::<u8>::new();
        assert_eq!(ring.capacity(), 4096);
    }

    // ── 并发测试 ────────────────────────────────────────────────────────────

    #[test]
    fn spsc_concurrent() {
        let ring = Arc::new(RingBuffer::<usize, 1024>::new());
        let count = 100_000;

        let ring_producer = ring.clone();
        let producer = std::thread::spawn(move || {
            for i in 0..count {
                loop {
                    if ring_producer.push(i).is_ok() {
                        break;
                    }
                    std::thread::yield_now();
                }
            }
        });

        let mut received = Vec::with_capacity(count);
        for _ in 0..count {
            loop {
                if let Some(v) = ring.pop() {
                    received.push(v);
                    break;
                }
                std::thread::yield_now();
            }
        }

        producer.join().unwrap();

        // 验证 FIFO 顺序
        assert_eq!(received.len(), count);
        for (i, &v) in received.iter().enumerate() {
            assert_eq!(v, i, "order violation at index {i}");
        }
    }

    #[test]
    fn spsc_concurrent_high_contention() {
        let ring = Arc::new(RingBuffer::<u8, 64>::new());
        let count = 50_000;

        let ring_producer = ring.clone();
        let producer = std::thread::spawn(move || {
            for i in 0..count {
                let val = (i % 256) as u8;
                loop {
                    if ring_producer.push(val).is_ok() {
                        break;
                    }
                    std::thread::yield_now();
                }
            }
        });

        let mut sum_produced: u64 = 0;
        let mut sum_consumed: u64 = 0;
        let mut consumed_count = 0;

        while consumed_count < count {
            if let Some(v) = ring.pop() {
                sum_consumed += v as u64;
                consumed_count += 1;
            }
        }

        // 计算预期的生产者总和
        for i in 0..count {
            sum_produced += (i % 256) as u64;
        }

        producer.join().unwrap();
        assert_eq!(sum_consumed, sum_produced);
    }

    // ── 边界：u8 数据类型 ───────────────────────────────────────────────────

    #[test]
    fn ring_buffer_u8() {
        let ring = RingBuffer::<u8, 16>::new();
        ring.push(0).unwrap();
        ring.push(255).unwrap();
        assert_eq!(ring.pop(), Some(0));
        assert_eq!(ring.pop(), Some(255));
    }

    // ── 边界：f32 数据类型 ──────────────────────────────────────────────────

    #[test]
    fn ring_buffer_f32() {
        let ring = RingBuffer::<f32, 16>::new();
        ring.push(1.5).unwrap();
        ring.push(-0.25).unwrap();
        assert_eq!(ring.pop(), Some(1.5));
        assert_eq!(ring.pop(), Some(-0.25));
    }

    // ── 边界：最小容量 ──────────────────────────────────────────────────────

    #[test]
    fn ring_buffer_capacity_2() {
        let ring = RingBuffer::<u8, 2>::new();
        // 容量 2，可用 1
        ring.push(42).unwrap();
        assert!(ring.is_full());
        assert!(ring.push(99).is_err());
        assert_eq!(ring.pop(), Some(42));
        assert!(ring.is_empty());
    }

    // ── 模拟信号量模式（swap.rs 使用场景） ──────────────────────────────────

    #[test]
    fn simulate_semaphore_signaling() {
        let ring = Ring64::<u8>::new();

        // 模拟：builder 线程写入"新配置可用"信号
        ring.push(1).unwrap();

        // 模拟：RT 线程检查信号
        assert_eq!(ring.pop(), Some(1));
        assert!(ring.pop().is_none()); // 无更多信号
    }

    #[test]
    fn simulate_burst_signaling() {
        let ring = Ring64::<u8>::new();

        // 快速连续发送多个信号（只保留最新的）
        for _ in 0..10 {
            ring.push(1).unwrap();
        }

        // 消费者快速读取
        let mut count = 0;
        while ring.pop().is_some() {
            count += 1;
        }
        assert_eq!(count, 10);
    }

    // ── 模拟日志模式（logger.rs 使用场景） ──────────────────────────────────

    #[test]
    fn simulate_log_writing() {
        let ring = Ring1K::<u8>::new();
        let message = b"RT-SAFETY: buffer underrun detected\n";

        // 实时线程写入日志
        let pushed = ring.push_slice(message);
        assert_eq!(pushed, message.len());

        // 非实时线程读取日志
        let mut output = vec![0u8; 256];
        let popped = ring.pop_slice(&mut output);
        assert_eq!(popped, message.len());
        assert_eq!(&output[..popped], message);
    }

    #[test]
    fn simulate_log_overflow() {
        let ring = Ring64::<u8>::new();

        // 写入超过容量的数据
        let big_message = vec![b'X'; 200];
        let pushed = ring.push_slice(&big_message);
        assert_eq!(pushed, 63); // 可用 63 个槽位

        // 读取所有可用数据
        let mut output = vec![0u8; 200];
        let popped = ring.pop_slice(&mut output);
        assert_eq!(popped, 63);
        assert!(output[..63].iter().all(|&b| b == b'X'));
    }

    // ── wrapping_add 不会 panic ─────────────────────────────────────────────

    #[test]
    fn usize_wraparound_safe() {
        let ring = RingBuffer::<u8, 4>::new();

        // 手动设置 write_pos/read_pos 接近 usize::MAX
        ring.write_pos.store(usize::MAX - 1, Ordering::Relaxed);
        ring.read_pos.store(usize::MAX - 1, Ordering::Relaxed);

        // 推入弹出应该正常工作（wrapping_add）
        ring.push(42).unwrap();
        assert_eq!(ring.pop(), Some(42));
    }
}