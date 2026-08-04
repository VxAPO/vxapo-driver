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
        // 聚合语义：riid 必须是 IUnknown，返回 inner IUnknown；其余接口由外层层路由。
        // 简化实现（对齐 EAPO 实际行为）：接受聚合，返回 inner 并对目标接口直接 QI。
        if !punkouter.is_null() {
            // 聚合时 riid 必须为 IUnknown（COM 规范），否则 E_NOINTERFACE。
            let iid_unknown = IUnknown::IID;
            if unsafe { *riid } != iid_unknown {
                return Err(Error::from(windows::core::HRESULT(0x8000_4002u32 as i32))); // E_NOINTERFACE
            }
        }

        // ── Step 4: 创建 ApoObject ─────────────────────────
        let apo = ApoObject::new(self.target_clsid);

        // ── Step 5: 转为 IUnknown 并 QI ────────────────────
        let unknown: IUnknown = apo.into();

        // 通过 vtable 调用 QueryInterface（index 0）
        // windows-interface 0.59.3 生成的方法跨模块不可见，使用原始 vtable
        let raw_ptr: *mut c_void = unsafe { std::mem::transmute_copy(&unknown) };
        let vtbl = unsafe { *(raw_ptr as *const *const usize) };
        type QIFn = unsafe extern "system" fn(
            *mut c_void, *const GUID, *mut *mut c_void,
        ) -> HRESULT;
        let qi: QIFn = unsafe { std::mem::transmute(*vtbl.add(0)) };

        let hr = unsafe { qi(raw_ptr, &*riid, ppvobject as *mut *mut c_void) };
        if hr.is_err() {
            // ---- 探针 4b：QI 失败记录（2026-08-04，debug 门控，排查完删除）----
            #[cfg(debug_assertions)]
            {
                let _ = std::fs::write(
                    r"C:\ProgramData\VxAPO\createinstance_probe.txt",
                    format!("CreateInstance FAILED QI hr={:08X} clsid={:?} riid={:?}\n", hr.0 as u32, self.target_clsid, unsafe { *riid }),
                );
            }
            drop(unknown);
            return Err(Error::from(hr));
        }

        // ---- 探针 4b：成功记录 + 自检 QI IAudioProcessingObject（2026-08-04，debug 门控，删）----
        #[cfg(debug_assertions)]
        {
            // 自检：inner 对象能否 QI 到 IAudioProcessingObject（IID fd7f2b29...）
            let iapoid = windows::core::GUID::from_values(
                0xfd7f2b29, 0x24d0, 0x4b5c, [0xb1, 0x77, 0x59, 0x2c, 0x39, 0xf9, 0xca, 0x10],
            );
            let mut iapopt: *mut c_void = std::ptr::null_mut();
            let hr_self = unsafe { qi(raw_ptr, &iapoid, &mut iapopt as *mut *mut c_void) };
            // 释放自检引用（若成功）
            if hr_self.is_ok() {
                let vtbl_self = unsafe { *(iapopt as *const *const usize) };
                type ReleaseFn = unsafe extern "system" fn(*mut c_void) -> u32;
                let release: ReleaseFn = unsafe { std::mem::transmute(*vtbl_self.add(2)) };
                unsafe { release(iapopt) };
            }
            let _ = std::fs::write(
                r"C:\ProgramData\VxAPO\createinstance_probe.txt",
                format!(
                    "CreateInstance SUCCESS clsid={:?} selfQI_IAPO_hr={:08X} returned_riid={:?}\n",
                    self.target_clsid, hr_self.0 as u32, unsafe { *riid }
                ),
            );
        }

        // ── Step 6: 释放临时引用 ───────────────────────────
        drop(unknown);

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