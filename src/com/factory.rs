//! com/factory.rs — COM ClassFactory 实现（Note 2/3/4/5）
//!
//! 职责：
//! 1. 管理 `LOCK_COUNT` 原子计数，跟踪客户端显式锁定（Note 2）
//! 2. 接收 `DllGetClassObject` 传入的 CLSID，路由到正确的 APO 对象创建（Note 4）
//! 3. 支持聚合模式，`pUnkOuter` 非空时仅暴露 `IUnknown`（Note 3）
//! 4. 注册表 `ThreadingModel = "Both"`，本模块不写注册表，仅声明常量（Note 5）
//!
//! `DllCanUnloadNow` 判定条件：`INST_COUNT == 0 && LOCK_COUNT == 0` 时返回 `S_OK`。
//!
//! 此模块持有 ClassFactory 的 vtable 与 `CreateInstance` 入口，
//! 实际对象创建委托 `instance/object.rs`。

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::{GUID, HRESULT, IUnknown, Interface, BOOL};
use windows::Win32::System::Com::IClassFactory;

use crate::com::abi;
use crate::com::reg_props::{is_vxapo_clsid, CLSID_VXAPO_PRE_MIX, CLSID_VXAPO_POST_MIX};
use crate::instance::object::ApoObjectState;
use crate::instance::ref_count as inst_count;

// ══════════════════════════════════════════════════════════════════════════════
// LOCK_COUNT（Note 2）
//
// 客户端调用 `IClassFactory::LockServer(TRUE)` 时 +1，
// 调用 `LockServer(FALSE)` 时 -1。
// 与 `INST_COUNT` 独立，`DllCanUnloadNow` 需要两者均零。
// ══════════════════════════════════════════════════════════════════════════════

static LOCK_COUNT: AtomicU32 = AtomicU32::new(0);

/// LockServer(TRUE) 时调用。
pub fn lock_increment() -> u32 {
    let prev = LOCK_COUNT.fetch_add(1, Ordering::SeqCst);
    debug_assert!(prev < u32::MAX, "LOCK_COUNT overflow");
    prev + 1
}

/// LockServer(FALSE) 时调用。
pub fn lock_decrement() -> u32 {
    let prev = LOCK_COUNT.fetch_sub(1, Ordering::SeqCst);
    debug_assert!(prev > 0, "LOCK_COUNT underflow");
    prev - 1
}

/// 读取当前锁定计数。
pub fn lock_count() -> u32 {
    LOCK_COUNT.load(Ordering::SeqCst)
}

/// 是否无客户端锁定。
pub fn lock_is_zero() -> bool {
    lock_count() == 0
}

#[cfg(test)]
/// 测试专用重置。
pub fn lock_reset_for_test() {
    LOCK_COUNT.store(0, Ordering::SeqCst);
}

// ══════════════════════════════════════════════════════════════════════════════
// ApoObject — Phase 2 最小 COM 对象
//
// Phase 4 补充 #[implement] 宏并实现三个 APO 接口。
// Phase 2 仅需 IUnknown 用于引用计数和 QI 测试。
// ══════════════════════════════════════════════════════════════════════════════

/// APO COM 对象（Phase 2 最小实现）。
///
/// Phase 4 中将添加 `#[implement(IAudioProcessingObject, ...)]`，
/// 当前仅用于验证 INST_COUNT 生命周期和工厂创建流程。
pub struct ApoObject {
    pub state: ApoObjectState,
    /// 自身引用计数（Phase 2 手动管理，Phase 4 由 #[implement] 宏接管）。
    ref_count: AtomicU32,
}

impl ApoObject {
    /// 创建新的 APO 对象。引用计数初始化为 1（Note 3）。
    ///
    /// 同时递增全局 `INST_COUNT`。
    pub fn new(clsid: GUID) -> Self {
        inst_count::increment();
        Self {
            state: ApoObjectState::new(clsid),
            ref_count: AtomicU32::new(1),
        }
    }

    /// 引用计数 +1。
    pub fn add_ref(&self) -> u32 {
        self.ref_count.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// 引用计数 -1。归零时返回 true，调用方负责释放。
    pub fn release(&self) -> (u32, bool) {
        let prev = self.ref_count.fetch_sub(1, Ordering::SeqCst);
        let new_count = prev - 1;
        let should_drop = new_count == 0;
        (new_count, should_drop)
    }

    /// 读取当前引用计数。
    pub fn ref_count(&self) -> u32 {
        self.ref_count.load(Ordering::SeqCst)
    }
}

impl Drop for ApoObject {
    fn drop(&mut self) {
        inst_count::decrement();
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// ClassFactory
// ══════════════════════════════════════════════════════════════════════════════

/// VxAPO COM ClassFactory。
///
/// 每个 CLSID（PreMix / PostMix）对应一个工厂实例。
/// `DllGetClassObject` 根据请求的 CLSID 选择对应的工厂（Note 4）。
pub struct VxApoClassFactory {
    /// 此工厂负责创建的 APO CLSID。
    target_clsid: GUID,
    /// 工厂自身的 COM 引用计数。
    ref_count: AtomicU32,
}

impl VxApoClassFactory {
    /// 创建新的 ClassFactory。
    pub fn new(clsid: GUID) -> Self {
        Self {
            target_clsid: clsid,
            ref_count: AtomicU32::new(1),
        }
    }

    /// 目标 CLSID。
    pub fn target_clsid(&self) -> GUID {
        self.target_clsid
    }

    // ── IUnknown ────────────────────────────────────────────────────────────

    /// `QueryInterface` 实现。
    ///
    /// 支持 `IUnknown` 和 `IClassFactory` 两个接口。
    pub fn query_interface(
        &self,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> HRESULT {
        // SAFETY: riid 由调用方保证有效，ppv 保证可写。
        let iid = unsafe { *riid };

        if iid == IUnknown::IID || iid == IClassFactory::IID {
            // SAFETY: ppv 输出参数，写入自身指针。
            unsafe {
                *ppv = self as *const Self as *mut c_void;
            }
            self.add_ref();
            abi::S_OK
        } else {
            // SAFETY: ppv 输出参数，置空表示不支持。
            unsafe {
                *ppv = std::ptr::null_mut();
            }
            abi::E_NOINTERFACE
        }
    }

    /// `AddRef` 实现。
    pub fn add_ref(&self) -> u32 {
        self.ref_count.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// `Release` 实现。
    ///
    /// 引用计数归零时不自行释放——由 `DllGetClassObject` 返回的
    /// `IClassFactory` 指针生命周期由调用方管理。
    pub fn release(&self) -> u32 {
        let prev = self.ref_count.fetch_sub(1, Ordering::SeqCst);
        prev - 1
    }

    // ── IClassFactory ───────────────────────────────────────────────────────

    /// 创建 APO COM 对象实例。
    ///
    /// # 聚合模式（Note 3）
    ///
    /// `pUnkOuter` 非空时：
    /// - 只能请求 `IUnknown`，否则返回 `E_NOINTERFACE`
    /// - 返回非委托 `IUnknown`，由外部对象管理生命周期
    ///
    /// # 非聚合模式
    ///
    /// 正常 `QueryInterface` 返回请求的接口。
    ///
    /// # 引用计数（Note 3）
    ///
    /// - 构造后 ref_count = 1
    /// - QueryInterface 成功后 +1
    /// - 工厂 Release 后最终归 1 交客户端
    pub fn create_instance(
        &self,
        p_unk_outer: Option<&IUnknown>,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> HRESULT {
        // ── 输入验证 ────────────────────────────────────────────────────────

        if ppv.is_null() {
            return abi::E_POINTER;
        }

        // SAFETY: ppv 保证可写，先置空防止悬挂指针。
        unsafe {
            *ppv = std::ptr::null_mut();
        }

        // ── 聚合检查（Note 3） ─────────────────────────────────────────────

        if let Some(_outer) = p_unk_outer {
            // 聚合模式：只允许请求 IUnknown
            // SAFETY: riid 由调用方保证有效。
            let iid = unsafe { *riid };
            if iid != IUnknown::IID {
                return abi::E_NOINTERFACE;
            }
        }

        // ── 创建对象 ───────────────────────────────────────────────────────

        // Note 3: 构造后 ref_count = 1（已在 ApoObject::new 中设置）
        // 先分配到堆上，确保 *ppv 和 Box::into_raw 指向同一块内存。
        // Phase 2 用 Box 泄漏（intentional leak）模拟 COM 生命周期。
        let boxed = Box::new(ApoObject::new(self.target_clsid));  // 去掉 mut

        // QueryInterface 成功后 +1
        let hr = query_apo_object(&*boxed, riid, ppv);

        if abi::failed(hr) {
            // QI 失败，boxed 被 drop，ref_count 归零时 INST_COUNT 递减。
            drop(boxed);
            return hr;
        }

        // QI 成功后 obj 的 ref_count = 2（构造 1 + QI +1）。
        // 工厂侧不再持有 obj，release 一次 → ref_count = 1 归客户端。
        let (count, _should_drop) = boxed.release();
        if count == 0 {
            // 不应走到这里（QI 刚成功，至少还有客户端持有）
            return abi::E_UNEXPECTED;
        }

        // Note 3: 最终 ref_count = 1，归客户端。
        // Box::into_raw 泄漏堆内存，保持 *ppv 指向的堆对象存活。
        // Phase 4 用 #[implement] 宏管理 COM 生命周期后可移除此泄漏。
        let _ = Box::into_raw(boxed);

        abi::S_OK
    }

    /// 锁定/解锁服务器（Note 2）。
    ///
    /// `fLock` 为 TRUE 时递增 `LOCK_COUNT`，FALSE 时递减。
    pub fn lock_server(&self, f_lock: BOOL) -> HRESULT {
        if f_lock.as_bool() {
            lock_increment();
        } else {
            lock_decrement();
        }
        abi::S_OK
    }
}

/// 对 ApoObject 执行 QueryInterface。
///
/// Phase 2 只支持 IUnknown。Phase 4 扩展支持 APO 接口。
fn query_apo_object(
    obj: &ApoObject,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    // SAFETY: riid 由调用方保证有效。
    let iid = unsafe { *riid };

    if iid == IUnknown::IID {
        // 返回对象自身作为 IUnknown
        // SAFETY: ppv 输出参数，写入对象指针。
        unsafe {
            *ppv = obj as *const ApoObject as *mut c_void;
        }
        obj.add_ref(); // Note 3: QI 成功 +1
        abi::S_OK
    } else {
        // Phase 4 将在此添加 IAudioProcessingObject / RT / Config 的匹配
        // SAFETY: ppv 输出参数，置空。
        unsafe {
            *ppv = std::ptr::null_mut();
        }
        abi::E_NOINTERFACE
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 工厂创建辅助
// ══════════════════════════════════════════════════════════════════════════════

/// 根据 CLSID 创建对应的 ClassFactory。
///
/// `DllGetClassObject` 调用此函数获取工厂实例。
/// 未知 CLSID 返回 `None`（Note 4：应返回 `CLASS_E_CLASSNOTAVAILABLE`）。
pub fn create_factory(clsid: &GUID) -> Option<VxApoClassFactory> {
    if is_vxapo_clsid(clsid) {
        Some(VxApoClassFactory::new(*clsid))
    } else {
        None
    }
}

/// 创建所有支持的工厂（PreMix + PostMix）。
///
/// 用于验证和测试。
pub fn create_all_factories() -> Vec<VxApoClassFactory> {
    vec![
        VxApoClassFactory::new(CLSID_VXAPO_PRE_MIX),
        VxApoClassFactory::new(CLSID_VXAPO_POST_MIX),
    ]
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::com::reg_props::{CLSID_VXAPO_PRE_MIX, CLSID_VXAPO_POST_MIX};
    use windows::core::{GUID, Interface};

    // ── LOCK_COUNT ──────────────────────────────────────────────────────────

    #[test]
    fn lock_count_initial() {
        lock_reset_for_test();
        assert_eq!(lock_count(), 0);
        assert!(lock_is_zero());
    }

    #[test]
    fn lock_count_cycle() {
        lock_reset_for_test();
        assert_eq!(lock_increment(), 1);
        assert_eq!(lock_increment(), 2);
        assert!(!lock_is_zero());

        assert_eq!(lock_decrement(), 1);
        assert_eq!(lock_decrement(), 0);
        assert!(lock_is_zero());
    }

    #[test]
    fn lock_server_true_increments() {
        lock_reset_for_test();
        let factory = VxApoClassFactory::new(CLSID_VXAPO_PRE_MIX);
        let hr = factory.lock_server(BOOL::from(true));
        assert_eq!(hr, abi::S_OK);
        assert_eq!(lock_count(), 1);
        lock_decrement(); // cleanup
    }

    #[test]
    fn lock_server_false_decrements() {
        lock_reset_for_test();
        lock_increment();
        let factory = VxApoClassFactory::new(CLSID_VXAPO_PRE_MIX);
        let hr = factory.lock_server(BOOL::from(false));
        assert_eq!(hr, abi::S_OK);
        assert_eq!(lock_count(), 0);
    }

    // ── Factory 创建 ────────────────────────────────────────────────────────

    #[test]
    fn create_factory_premix() {
        let factory = create_factory(&CLSID_VXAPO_PRE_MIX);
        assert!(factory.is_some());
        assert_eq!(factory.unwrap().target_clsid(), CLSID_VXAPO_PRE_MIX);
    }

    #[test]
    fn create_factory_postmix() {
        let factory = create_factory(&CLSID_VXAPO_POST_MIX);
        assert!(factory.is_some());
        assert_eq!(factory.unwrap().target_clsid(), CLSID_VXAPO_POST_MIX);
    }

    #[test]
    fn create_factory_unknown_clsid() {
        let factory = create_factory(&GUID::zeroed());
        assert!(factory.is_none());
    }

    #[test]
    fn create_all_factories_count() {
        let factories = create_all_factories();
        assert_eq!(factories.len(), 2);
    }

    // ── ClassFactory QI ─────────────────────────────────────────────────────

    #[test]
    fn factory_qi_iunknown() {
        let factory = VxApoClassFactory::new(CLSID_VXAPO_PRE_MIX);
        let mut ppv: *mut c_void = std::ptr::null_mut();
        let hr = factory.query_interface(&IUnknown::IID, &mut ppv);
        assert_eq!(hr, abi::S_OK);
        assert!(!ppv.is_null());
        // release the extra ref from QI
        factory.release();
    }

    #[test]
    fn factory_qi_iclassfactory() {
        let factory = VxApoClassFactory::new(CLSID_VXAPO_PRE_MIX);
        let mut ppv: *mut c_void = std::ptr::null_mut();
        let hr = factory.query_interface(&IClassFactory::IID, &mut ppv);
        assert_eq!(hr, abi::S_OK);
        assert!(!ppv.is_null());
        factory.release();
    }

    #[test]
    fn factory_qi_unsupported() {
        let factory = VxApoClassFactory::new(CLSID_VXAPO_PRE_MIX);
        let mut ppv: *mut c_void = std::ptr::null_mut();
        let unknown_guid = GUID::from_values(
            0xDEADBEEF,
            0x1234,
            0x5678,
            [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08],
        );
        let hr = factory.query_interface(&unknown_guid, &mut ppv);
        assert_eq!(hr, abi::E_NOINTERFACE);
        assert!(ppv.is_null());
    }

    // ── Factory 引用计数 ────────────────────────────────────────────────────

    #[test]
    fn factory_ref_count() {
        let factory = VxApoClassFactory::new(CLSID_VXAPO_PRE_MIX);
        assert_eq!(factory.ref_count.load(Ordering::SeqCst), 1);

        factory.add_ref();
        assert_eq!(factory.ref_count.load(Ordering::SeqCst), 2);

        factory.release();
        assert_eq!(factory.ref_count.load(Ordering::SeqCst), 1);

        factory.release();
        assert_eq!(factory.ref_count.load(Ordering::SeqCst), 0);
    }

    // ── CreateInstance ──────────────────────────────────────────────────────

    #[test]
    fn create_instance_basic() {
        inst_count::reset_for_test();
        let factory = VxApoClassFactory::new(CLSID_VXAPO_PRE_MIX);

        let mut ppv: *mut c_void = std::ptr::null_mut();
        let hr = factory.create_instance(None, &IUnknown::IID, &mut ppv);

        assert_eq!(hr, abi::S_OK);
        assert!(!ppv.is_null());
        // INST_COUNT 应该 +1
        assert_eq!(inst_count::get(), 1);

        // cleanup: release the created object
        // SAFETY: ppv 指向 ApoObject，create_instance 中 Box::into_raw 泄漏了内存。
        // 通过指针重建 Box 来释放。
        unsafe {
            let obj = Box::from_raw(ppv as *mut ApoObject);
            obj.release(); // release the QI ref
            // obj drop 时 INST_COUNT -1
        }
        assert_eq!(inst_count::get(), 0);
    }

    #[test]
    fn create_instance_null_ppv() {
        let factory = VxApoClassFactory::new(CLSID_VXAPO_PRE_MIX);
        let hr = factory.create_instance(None, &IUnknown::IID, std::ptr::null_mut());
        assert_eq!(hr, abi::E_POINTER);
    }

    #[test]
    fn create_instance_null_outer_allows_any_interface() {
        // 非聚合模式：请求 IUnknown 应成功
        inst_count::reset_for_test();
        let factory = VxApoClassFactory::new(CLSID_VXAPO_PRE_MIX);
        let mut ppv: *mut c_void = std::ptr::null_mut();
        let hr = factory.create_instance(None, &IUnknown::IID, &mut ppv);
        assert_eq!(hr, abi::S_OK);

        // cleanup
        unsafe {
            let obj = Box::from_raw(ppv as *mut ApoObject);
            obj.release();
        }
        assert_eq!(inst_count::get(), 0);
    }

    // ── ApoObject 引用计数 ──────────────────────────────────────────────────

    #[test]
    fn apo_object_ref_count_lifecycle() {
        inst_count::reset_for_test();

        let obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        assert_eq!(inst_count::get(), 1);
        assert_eq!(obj.ref_count(), 1);

        obj.add_ref();
        assert_eq!(obj.ref_count(), 2);

        let (count, _should_drop) = obj.release();
        assert_eq!(count, 1);

        // 不再 release 到 0——drop 时会 decrement inst_count
        drop(obj);
        assert_eq!(inst_count::get(), 0);
    }

    // ── INST_COUNT + LOCK_COUNT 联合 ────────────────────────────────────────

    #[test]
    fn can_unload_now_requires_both_zero() {
        inst_count::reset_for_test();
        lock_reset_for_test();

        assert!(inst_count::is_zero() && lock_is_zero());

        {
            let _obj = ApoObject::new(CLSID_VXAPO_PRE_MIX);
            assert!(!inst_count::is_zero());
        } // _obj drop here → decrement

        assert!(inst_count::is_zero());

        lock_increment();
        assert!(!lock_is_zero());
        lock_decrement();
        assert!(lock_is_zero());
    }

    #[test]
    fn both_zero_after_full_lifecycle() {
        inst_count::reset_for_test();
        lock_reset_for_test();

        let factory = VxApoClassFactory::new(CLSID_VXAPO_PRE_MIX);

        // LockServer TRUE
        let _ = factory.lock_server(BOOL::from(true));
        assert_eq!(lock_count(), 1);

        // CreateInstance
        let mut ppv: *mut c_void = std::ptr::null_mut();
        let _ = factory.create_instance(None, &IUnknown::IID, &mut ppv);
        assert_eq!(inst_count::get(), 1);

        // Release object — Phase 2 ApoObject，用 Box::from_raw 回收
        unsafe {
            let boxed = Box::from_raw(ppv as *mut ApoObject);
            let (_, should_drop) = boxed.release();
            if should_drop {
                drop(boxed);
            }
        }
        assert_eq!(inst_count::get(), 0);

        // LockServer FALSE
        let _ = factory.lock_server(BOOL::from(false));
        assert!(lock_is_zero());

        // 两者均零
        assert!(inst_count::is_zero() && lock_is_zero());
    }
}