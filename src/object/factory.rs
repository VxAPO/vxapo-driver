//! host/instance/factory.rs — COM ClassFactory 实现（Note 2/3/4/5）
//!
//! 职责：
//! 1. 管理 `LOCK_COUNT` 原子计数，跟踪客户端显式锁定（Note 2）
//! 2. 接收 `DllGetClassObject` 传入的 CLSID，路由到正确的 APO 对象创建（Note 4）
//! 3. 支持聚合模式，`pUnkOuter` 非空时仅暴露 `IUnknown`（Note 3）
//! 4. 注册表 `ThreadingModel = "Both"`，本模块不写注册表（Note 5）
//!
//! `DllCanUnloadNow` 判定条件：`INST_COUNT == 0 && LOCK_COUNT == 0` 时返回 `S_OK`。
//!
//! 使用 `#[implement]` 宏自动生成 vtable 与 COM 引用计数。
//! 实际对象创建委托 `host/instance/apo_interface.rs`。

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::{GUID, HRESULT, IUnknown, Ref, BOOL, implement, Error};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};

#[allow(unused_imports)]
use crate::object::apo::ApoObject;
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
    let prev = LOCK_COUNT.fetch_sub(1, Ordering::SeqCst);
    prev.saturating_sub(1)
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
        // ---- P0-7 无声诊断探针 4（2026-08-04，debug 门控，排查完删除）----
        // 移到函数最顶部：即使聚合检查（Step 3 CLASS_E_NOAGGREGATION）提前 return，
        // 也留下「CreateInstance 被调 + pUnkOuter 是否非空」记录——区分「未被调」vs「被聚合拒绝」。
        #[cfg(debug_assertions)]
        {
            let _ = std::fs::write(
                r"C:\ProgramData\VxAPO\createinstance_probe.txt",
                format!("CreateInstance clsid={:?} riid={:?} punkouter_null={}\n", self.target_clsid, unsafe { *riid }, punkouter.is_null()),
            );
        }

        // ── Step 1: 输出指针初始化 ─────────────────────────
        unsafe { *ppvobject = std::ptr::null_mut() };

        // ── Step 2: 参数校验 ───────────────────────────────
        if riid.is_null() || ppvobject.is_null() {
            return Err(Error::from(E_INVALIDARG));
        }

        // ── Step 3: 聚合支持（P0-7 无声根因修复）────────────
        // EAPO `EqualizerAPO(IUnknown* pUnkOuter)` 明确支持聚合（引擎以 pUnkOuter 非空
        // 创建 APO）——之前拒绝聚合 → 引擎静默放弃 → 无声（探针实证 punkouter_null=false）。
        //
        // COM 聚合规范：CreateInstance 聚合时必须返回 **inner 的 IUnknown**（引擎外壳
        // 通过它 QI 非委托接口），并非返回 outer 指针（返回 outer/不建 inner = 空壳）。
        // 引擎外壳 QI(IAPO) 需要 inner 提供 NonDelegatingQI——windows-rs #[implement]
        // 的自包含 IUnknown 没有该机制（探针 selfQI_IAPO_hr=0 只证明 inner 自 QI 可行，
        // 不代表引擎经外壳链能拿到）。
        //
        // 本分支保持「接受聚合 + 创建 inner 返回」（探针可观察引擎下一步动作）；
        // 完整 NonDelegating 委托需手写 vtable（08 文档 §6），本阶段先锁定行为。
        if !punkouter.is_null() {
            let iid_unknown = IUnknown::IID;
            if unsafe { *riid } != iid_unknown {
                return Err(Error::from(windows::core::HRESULT(0x8000_4002u32 as i32))); // E_NOINTERFACE
            }
            // 探针记录聚合被接受（不拦，走下去创建 inner）。
        }

        // ── Step 4: 创建聚合外壳（NApo，P0-7 手写 vtable）──
        // 聚合与非聚合统一走 create_aggregate——NApo 动态转发到内部 ApoObject，
        // 且实现 EAPO 聚合语义（QI(IUnknown)→outer、QI(接口)→inner、AddRef/Release→outer）。
        // windows-rs #[implement] 无 NonDelegating 分离，引擎聚合 QI(IAPO) 走不到 inner
        // → 弃用对象 → 无声；NApo 手写 vtable 补上该委托（08-P0-7-audiodg-analysis §6）。
        // SAFETY: pUnkOuter 从 COM Ref 转裸指针（非空时引擎外壳有效）。
        // Ref<IUnknown> Deref 到接口，.as_ref() 得 Option<&IUnknown>，接口 .abi() 取裸指针。
        let outer_raw: *mut c_void = punkouter
            .as_ref()
            .map(|u| windows::core::Interface::as_raw(u) as *mut c_void)
            .unwrap_or(std::ptr::null_mut());
        // SAFETY: self.target_clsid 是 VxAPO CLSID（create_factory 已校验）。
        let na = unsafe { crate::object::aggregate::create_aggregate(outer_raw, self.target_clsid) };
        if na.is_null() {
            return Err(Error::from(windows::core::HRESULT(0x8007_000Eu32 as i32))); // ERROR_OUTOFMEMORY
        }

        // ── Step 5: 对 NApo QI 请求接口并返回 ─────────────
        // 聚合时 riid==IUnknown → NApo 返回 outer 身份；非聚合 → NApo 返回自身。
        // 其余接口（IAPO/RT/Config/IAudioSystemEffects）→ NApo NonDelegating 返回 inner 指针。
        let vtbl = unsafe { *(na as *const *const usize) };
        type QIFn2 = unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT;
        let qi2: QIFn2 = unsafe { std::mem::transmute(*vtbl.add(0)) };
        // SAFETY: riid/ppvobject 由 COM 契约保证有效；na 是刚创建的有效 COM 对象。
        let hr = unsafe { qi2(na, &*riid, ppvobject) };
        if hr.is_err() {
            // QI 失败 → 释放 NApo（其 Release 会释放内部 ApoObject/接口）。
            unsafe { crate::object::aggregate::release_aggregate(na) };
            return Err(Error::from(hr));
        }

        // ---- 探针 4b：CreateInstance 结果（2026-08-04，debug 门控，排查完删除）----
        #[cfg(debug_assertions)]
        {
            let _ = std::fs::write(
                r"C:\ProgramData\VxAPO\createinstance_probe.txt",
                format!(
                    "CreateInstance SUCCESS clsid={:?} aggregate_na=1 returned_riid={:?}\n",
                    self.target_clsid, unsafe { *riid }
                ),
            );
        }

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
}