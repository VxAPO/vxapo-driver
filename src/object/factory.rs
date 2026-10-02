//! object/factory.rs — COM ClassFactory 实现
//!
//! 职责：
//! 1. 管理 `LOCK_COUNT` 原子计数，跟踪客户端显式锁定
//! 2. 接收 `DllGetClassObject` 传入的 CLSID，路由到正确的 APO 对象创建
//! 3. 支持聚合模式，`pUnkOuter` 非空时仅暴露 `IUnknown`
//! 4. 注册表 `ThreadingModel = "Both"`，本模块不写注册表
//!
//! `DllCanUnloadNow` 判定条件：`INST_COUNT == 0 && LOCK_COUNT == 0` 时返回 `S_OK`。
//!
//! 使用 `#[implement]` 宏自动生成 vtable 与 COM 引用计数。
//! 实际对象创建委托 `object/apo/aggregate.rs`（NApo 聚合外壳）。

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::{BOOL, Error, Ref};

use crate::object::vx_reg_props::is_vxapo_clsid;
use crate::sys::com::prelude::*;

// ══════════════════════════════════════════════════════════════════════════════
// LOCK_COUNT
// ══════════════════════════════════════════════════════════════════════════════

static LOCK_COUNT: AtomicU32 = AtomicU32::new(0);

pub fn lock_increment() -> u32 {
    LOCK_COUNT.fetch_add(1, Ordering::SeqCst) + 1
}

pub fn lock_decrement() -> u32 {
    // 零值保护（对齐 ref_count.rs CAS 模式）：LockServer(false) 的多余调用
    // 不得把计数下溢成 u32::MAX——否则 DllCanUnloadNow 永久返回 S_FALSE。
    let mut prev = LOCK_COUNT.load(Ordering::SeqCst);
    loop {
        if prev == 0 {
            return 0;
        }
        match LOCK_COUNT.compare_exchange(prev, prev - 1, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return prev - 1,
            Err(actual) => prev = actual,
        }
    }
}

pub fn lock_count() -> u32 {
    LOCK_COUNT.load(Ordering::SeqCst)
}

pub fn lock_is_zero() -> bool {
    lock_count() == 0
}

#[cfg(test)]
pub fn lock_reset_for_test() {
    LOCK_COUNT.store(0, Ordering::SeqCst);
}

// ══════════════════════════════════════════════════════════════════════════════
// ClassFactory — #[implement(IClassFactory)]
// ══════════════════════════════════════════════════════════════════════════════

#[implement(IClassFactory)]
pub struct ClassFactory {
    target_clsid: GUID,
}

impl IClassFactory_Impl for ClassFactory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Ref<'_, IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut c_void,
    ) -> windows_core::Result<()> {
        // ── Step 1: 参数校验（先校验后写入，：空指针不得先解引用）──
        if riid.is_null() || ppvobject.is_null() {
            return Err(Error::from(E_INVALIDARG));
        }

        // ── Step 2: 输出指针初始化 ─────────────────────────
        // SAFETY: DllGetClassObject 的 COM 契约保证 ppvobject 非空且可写；
        // 失败前先写 null 是 COM 惯例（调用方据此判断未返回接口）。
        unsafe { *ppvobject = std::ptr::null_mut() };

        // ── Step 3: 聚合支持 ────────────────────────
        // EAPO `EqualizerAPO(IUnknown* pUnkOuter)` 明确支持聚合（引擎以 pUnkOuter 非空
        // 创建 APO）；拒绝聚合 → 引擎静默放弃 → 无声。
        //
        // COM 聚合规范：CreateInstance 聚合时必须返回 **inner 的 IUnknown**（引擎外壳
        // 通过它 QI 非委托接口），并非返回 outer 指针（返回 outer/不建 inner = 空壳）。
        // 引擎外壳 QI(IAPO) 需要 inner 提供 NonDelegatingQI——windows-rs #[implement]
        // 的自包含 IUnknown 没有该机制（探针 selfQI_IAPO_hr=0 只证明 inner 自 QI 可行，
        // 不代表引擎经外壳链能拿到）。
        //
        // 聚合语义（delegating QI→outer / NonDelegating QI→inner）由
        // object/apo/aggregate.rs 的 NApo 完整实现（已落地）。
        if !punkouter.is_null() {
            let iid_unknown = IUnknown::IID;
            // SAFETY: 同一契约——riid 由调用方保证指向有效 GUID。
            if unsafe { *riid } != iid_unknown {
                return Err(Error::from(E_NOINTERFACE));
            }
        }

        // ── Step 4: 创建聚合外壳（NApo， 手写 vtable）──
        // 聚合与非聚合统一走 create_aggregate——NApo 动态转发到内部 ApoObject，
        // 且实现 EAPO 聚合语义（QI(IUnknown)→outer、QI(接口）→inner、AddRef/Release→outer)。
        // windows-rs #[implement] 无 NonDelegating 分离，引擎聚合 QI(IAPO) 走不到 inner
        // → 弃用对象 → 无声；NApo 手写 vtable 补上该委托（08--audiodg-analysis §6）。
        // SAFETY: pUnkOuter 从 COM Ref 转裸指针（非空时引擎外壳有效）。
        // Ref<IUnknown> Deref 到接口，.as_ref() 得 Option<&IUnknown>，接口 .abi() 取裸指针。
        let outer_raw: *mut c_void = punkouter
            .as_ref()
            .map(|u| Interface::as_raw(u) as *mut c_void)
            .unwrap_or(std::ptr::null_mut());
        // SAFETY: self.target_clsid 是 VxAPO CLSID（create_factory 已校验）。
        let na = unsafe { crate::object::apo::aggregate::create_aggregate(outer_raw, self.target_clsid) };
        if na.is_null() {
            return Err(Error::from(E_OUTOFMEMORY));
        }

        // ── Step 5: 对 NApo QI 请求接口并返回 ─────────────
        // 聚合时 riid==IUnknown → NApo 返回 outer 身份；非聚合 → NApo 返回自身。
        // 其余接口（IAPO/RT/Config/IAudioSystemEffects）→ NApo NonDelegating 返回 inner 指针。
        // SAFETY: na 是 create_aggregate 刚返回的有效 COM 对象指针；COM 布局保证
        // 对象首字段是 vtable 指针（*const usize 指向函数指针数组）。
        let vtbl = unsafe { *(na as *const *const usize) };
        type QIFn2 = unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT;
        // SAFETY: vtable 第 0 项按 COM 契约即 QueryInterface；函数签名与 IUnknown 一致，
        // transmute 到该签名后由下一条 SAFETY 说明的调用点使用。
        let qi2: QIFn2 = unsafe { std::mem::transmute(*vtbl.add(0)) };
        // SAFETY: riid/ppvobject 由 COM 契约保证有效；na 是刚创建的有效 COM 对象。
        let hr = unsafe { qi2(na, &*riid, ppvobject) };
        if hr.is_err() {
            // QI 失败 → 释放 NApo（其 Release 会释放内部 ApoObject/接口）。
            // SAFETY: na 仍持有 create_aggregate 的初始引用（QI 失败未增加引用），
            // 由本处按 COM 引用计数规则释放；不会二次释放。
            unsafe { crate::object::apo::aggregate::release_aggregate(na) };
            return Err(Error::from(hr));
        }

        // EAPO ClassFactory.cpp:73 对齐：创建后立即 NonDelegatingRelease 工厂临时引用。
        // create_aggregate 初始 cref=1；QI 成功后 +1，这里释放工厂临时引用，
        // 最终由调用方持有的那一个引用负责销毁（不释放会永久泄漏 NApo）。
        // SAFETY: 此处释放的是工厂持有的**临时**引用（create_aggregate 的初始引用），
        // QI 成功已使引用计数 +1，故释放后仍有调用方持有的引用，对象不被销毁。
        unsafe { crate::object::apo::aggregate::release_aggregate(na) };

        Ok(())
    }

    fn LockServer(&self, flock: BOOL) -> windows_core::Result<()> {
        if flock.as_bool() {
            lock_increment();
        } else {
            lock_decrement();
        }
        Ok(())
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 工厂创建辅助
// ══════════════════════════════════════════════════════════════════════════════

pub fn create_factory(clsid: &GUID) -> Option<IClassFactory> {
    if is_vxapo_clsid(clsid) {
        Some(ClassFactory { target_clsid: *clsid }.into())
    } else {
        None
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::apo::ApoObject;
    use crate::object::ref_count as inst_count;
    use crate::object::vx_reg_props::{CLSID_VXAPO_PRE_MIX, CLSID_VXAPO_POST_MIX};

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
    fn lock_decrement_zero_is_noop() {
        // 回归：LockServer(false) 在计数已为 0 时不得下溢为 u32::MAX
        // （否则 DllCanUnloadNow 永久 S_FALSE）。
        lock_reset_for_test();
        assert_eq!(lock_decrement(), 0);
        assert_eq!(lock_decrement(), 0);
        assert!(lock_is_zero());
    }

    // ── create_factory ─────────────────────────────────────────────────────

    #[test]
    fn create_factory_premix() {
        let factory = create_factory(&CLSID_VXAPO_PRE_MIX);
        assert!(factory.is_some());
    }

    #[test]
    fn create_factory_postmix() {
        let factory = create_factory(&CLSID_VXAPO_POST_MIX);
        assert!(factory.is_some());
    }

    #[test]
    fn create_factory_unknown() {
        let factory = create_factory(&GUID::zeroed());
        assert!(factory.is_none());
    }

    // ── inst_count 生命周期 ─────────────────────────────────────────────────

    #[test]
    fn create_instance_inst_count() {
        inst_count::reset_for_test();
        lock_reset_for_test();

        let factory = create_factory(&CLSID_VXAPO_PRE_MIX).unwrap();
        let _factory_iunknown: IUnknown = factory.into();

        let apo = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        assert_eq!(inst_count::get(), 1);

        let unknown: IUnknown = apo.into();
        drop(unknown);
        assert_eq!(inst_count::get(), 0);

        drop(_factory_iunknown);
    }

    // ── full_lifecycle ──────────────────────────────────────────────────────

    #[test]
    fn full_lifecycle() {
        inst_count::reset_for_test();
        lock_reset_for_test();

        lock_increment();
        assert_eq!(lock_count(), 1);

        let apo = ApoObject::new(CLSID_VXAPO_PRE_MIX);
        assert_eq!(inst_count::get(), 1);

        let unknown: IUnknown = apo.into();
        drop(unknown);
        assert_eq!(inst_count::get(), 0);

        lock_decrement();
        assert!(lock_is_zero());
        assert!(inst_count::is_zero() && lock_is_zero());
    }

    #[test]
    fn create_instance_aggregated_matches_eapo_ref_semantics() {
        use std::ffi::c_void;
        use std::sync::atomic::{AtomicU32, Ordering};
        use crate::sys::com::prelude::Interface;

        type QiFn = unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT;
        type RefFn = unsafe extern "system" fn(*mut c_void) -> u32;

        #[repr(C)]
        struct OuterVtbl {
            qi: QiFn,
            addref: RefFn,
            release: RefFn,
        }
        #[repr(C)]
        struct Outer {
            vtbl: *const OuterVtbl,
            refs: AtomicU32,
            inner: *mut c_void,
        }

        unsafe extern "system" fn outer_qi(
            this: *mut c_void,
            riid: *const GUID,
            ppv: *mut *mut c_void,
        ) -> HRESULT {
            let outer = &mut *(this as *mut Outer);
            // SAFETY: 本函数是 COM Outer::QueryInterface 实现，COM 契约保证 riid 指向
            // 有效 GUID（只读一次）。
            let iid = unsafe { *riid };
            if iid == IUnknown::IID {
                // SAFETY: 同一契约保证 ppv 可写；this 是合法的 Outer* 且引用计数在此 +1。
                unsafe { *ppv = this };
                outer_addref(this);
                S_OK
            } else if outer.inner.is_null() {
                E_NOINTERFACE
            } else {
                // SAFETY: outer.inner 非空（上方分支已排除 null），是有效 COM 对象指针；
                // COM 布局保证首字段为 vtable。
                let vtbl = unsafe { *(outer.inner as *const *const usize) };
                // SAFETY: vtable 第 0 项即 QueryInterface，签名与 QiFn 一致。
                let qi: QiFn = unsafe { std::mem::transmute(*vtbl) };
                // SAFETY: outer.inner 有效，riid/ppv 由 COM 契约保证有效；转调非委托 QI。
                unsafe { qi(outer.inner, riid, ppv) }
            }
        }

        unsafe extern "system" fn outer_addref(this: *mut c_void) -> u32 {
            let outer = &*(this as *mut Outer);
            outer.refs.fetch_add(1, Ordering::SeqCst) + 1
        }

        unsafe extern "system" fn outer_release(this: *mut c_void) -> u32 {
            let outer = &*(this as *mut Outer);
            outer.refs.fetch_sub(1, Ordering::SeqCst) - 1
        }

        static OUTER_VTBL: OuterVtbl = OuterVtbl {
            qi: outer_qi,
            addref: outer_addref,
            release: outer_release,
        };

        let mut outer = Outer {
            vtbl: &OUTER_VTBL,
            refs: AtomicU32::new(1),
            inner: std::ptr::null_mut(),
        };
        let outer_ptr = (&mut outer as *mut Outer) as *mut c_void;
        // SAFETY: outer_ptr 指向本函数栈上存活、已按 COM 布局初始化的 Outer；
        // from_raw 仅借用指针构造接口视图（测试内不转移所有权，不用来释放）。
        let outer_unknown: IUnknown = unsafe { IUnknown::from_raw(outer_ptr) };

        let factory = create_factory(&CLSID_VXAPO_PRE_MIX).unwrap();
        // 聚合创建：返回类型为 IUnknown，方法内部用 T::IID（IUnknown）调用工厂。
        // SAFETY: outer_unknown 指向栈上有效的 Outer 视图，调用期间存活；
        // CreateInstance 为 COM 委派调用，参数由 COM 契约校验。
        let inner: IUnknown = unsafe {
            factory.CreateInstance(Some(&outer_unknown))
        }
        .expect("aggregated CreateInstance failed");
        outer.inner = Interface::as_raw(&inner) as *mut c_void;

        // 引擎拿到返回的非委托 IUnknown 视图后 QI(IAPO)：
        // QI 成功应让 outer 引用 +1（接口视图 AddRef 委托 outer）。
        let before = outer.refs.load(Ordering::SeqCst);
        // SAFETY: outer.inner 在 CreateInstance 成功后非空且为有效 COM 对象指针；
        // COM 布局保证首字段为 vtable。
        let nd_vtbl = unsafe { *(outer.inner as *const *const usize) };
        // SAFETY: 非委托 vtable 第 0 项即 QueryInterface，签名与 QiFn 一致。
        let nd_qi: QiFn = unsafe { std::mem::transmute(*nd_vtbl) };
        let iapoid = crate::sys::com::apo_interfaces::IID_IAPO;
        let mut iao: *mut c_void = std::ptr::null_mut();
        // SAFETY: outer.inner 有效；&iapoid 为本地 GUID，&mut iao 为本地出参，均存活。
        let hr2 = unsafe { nd_qi(outer.inner, &iapoid, &mut iao) };
        assert_eq!(hr2.0, 0);
        assert!(!iao.is_null());
        assert_eq!(outer.refs.load(Ordering::SeqCst), before + 1);

        // 释放 IAPO 视图：委托 outer->Release，回到 QI 前计数。
        // SAFETY: iao 是上一步 QI 成功返回的接口视图（非空），其 vtable 有效；
        // COM 布局保证第 2 项为 Release。
        let iao_vtbl = unsafe { *(iao as *const *const usize) };
        // SAFETY: 同上——将 vtable 第 2 项转为 Release 签名。
        let iao_release: RefFn = unsafe { std::mem::transmute(*iao_vtbl.add(2)) };
        // SAFETY: iao 有效且持有 +1 引用，此处按 COM 规则释放该视图（委托 outer->Release）。
        unsafe { iao_release(iao) };
        assert_eq!(outer.refs.load(Ordering::SeqCst), before);

        // inner 是引擎持有的非委托视图；Drop 时调用 Release（工厂临时引用已释放）。
        drop(inner);
        drop(outer_unknown);
    }
}
