//! com/non_delegating.rs — INonDelegatingUnknown，COM 聚合支持（Note 3）
//!
//! COM 聚合模型中，内部对象持有外部对象的 `IUnknown` 指针（`pUnkOuter`），
//! 并通过非委托版本的 `IUnknown` 方法维护自身的引用计数与接口查询。
//!
//! 方法职责：
//! - `NonDelegatingQueryInterface`：不转发到外部 `IUnknown`，直接查询本对象的接口
//! - `NonDelegatingAddRef` / `NonDelegatingRelease`：仅操作本对象的引用计数
//!
//! 当 `pUnkOuter` 为 null 时，非委托 `IUnknown` 退化为标准 `IUnknown` 行为。
//!
//! 此模块仅提供 trait 与方法签名定义，不包含引用计数的具体实现。

use windows_core::{GUID, IUnknown, IUnknown_Vtbl, Interface, HRESULT};

// 获取 outer 的 vtable
unsafe fn outer_query_interface(outer: &IUnknown, riid: *const GUID, ppv: *mut *mut core::ffi::c_void) -> HRESULT {
    let raw = outer.as_raw();
    let vtbl = *(raw as *const *const IUnknown_Vtbl);
    ((*vtbl).QueryInterface)(raw, riid, ppv)
}

unsafe fn outer_add_ref(outer: &IUnknown) -> u32 {
    let raw = outer.as_raw();
    let vtbl = *(raw as *const *const IUnknown_Vtbl);
    ((*vtbl).AddRef)(raw)
}

unsafe fn outer_release(outer: &IUnknown) -> u32 {
    let raw = outer.as_raw();
    let vtbl = *(raw as *const *const IUnknown_Vtbl);
    ((*vtbl).Release)(raw)
}

/// COM 聚合的非委托 `IUnknown` trait。
///
/// 用于 `ClassFactory::CreateInstance` 中 `pUnkOuter` 非空的场景（Note 3）。
///
/// windows-rs 的 `implement` 宏可自动生成此 trait 的实现，
/// 手写时必须在每步操作旁标注引用计数值变化。
pub trait INonDelegatingUnknown {
    /// 非委托版 `QueryInterface`——不转发到外部 `pUnkOuter`。
    ///
    /// `pUnkOuter` 非空时，只能请求 `IID_IUnknown`，返回非委托 `IUnknown`。
    fn non_delegating_query_interface(
        &self,
        riid: *const windows::core::GUID,
        ppv: *mut *mut std::ffi::c_void,
    ) -> HRESULT;

    /// 非委托版 `AddRef`——操作本对象自身的引用计数。
    fn non_delegating_add_ref(&self) -> u32;

    /// 非委托版 `Release`——操作本对象自身的引用计数。
    ///
    /// 引用计数归零时释放对象内存。
    fn non_delegating_release(&self) -> u32;
}

/// 聚合对象的外部 `IUnknown` 指针持有器。
///
/// 内部 APO 对象在构造时接收 `pUnkOuter`，保存在此结构中。
/// 标准委托版 `QueryInterface`（由 `implement` 宏生成）会转发到此指针。
///
/// 当 `pUnkOuter` 为 null（非聚合模式）时，`inner` 字段为 null，
/// 委托版 `QueryInterface` / `AddRef` / `Release` 直接使用非委托实现。
#[derive(Debug)]
pub struct AggregationController {
    /// 外部对象的 `IUnknown`，聚合时非 null，独立时为 null。
    outer: Option<IUnknown>,
}

impl AggregationController {
    /// 创建新的聚合控制器。
    ///
    /// - `pUnkOuter` 为 `Some(unk)`：聚合模式，委托到外部
    /// - `pUnkOuter` 为 `None`：独立模式，不委托
    pub fn new(outer: Option<IUnknown>) -> Self {
        Self { outer }
    }

    /// 返回是否处于聚合模式。
    pub fn is_aggregated(&self) -> bool {
        self.outer.is_some()
    }

    /// 委托版 `QueryInterface`。
    ///
    /// 聚合模式下，转发到外部 `IUnknown`。
    /// 独立模式下，返回 `E_NOINTERFACE`（调用方应使用非委托版）。
    pub fn delegate_query_interface(
        &self,
        riid: *const windows::core::GUID,
        ppv: *mut *mut std::ffi::c_void,
    ) -> HRESULT {
        match &self.outer {
            Some(outer) => {
                // SAFETY: riid 和 ppv 由调用方保证有效。
                unsafe { outer.query(&*riid, ppv.cast()) }
            }
            None => {
                // 独立模式不应调用委托版——这是编程错误。
                // 返回 E_NOINTERFACE 让调用方知道出问题了。
                super::abi::E_NOINTERFACE
            }
        }
    }

    /// 委托版 `AddRef`。
    pub fn delegate_add_ref(&self) -> u32 {
        match &self.outer {
            Some(outer) => {
                // SAFETY: AddRef 不会 panic，不访问无效内存。
                unsafe {
                    let raw: *mut core::ffi::c_void = outer.as_raw();
                    let vtbl = *(raw as *const *const IUnknown_Vtbl);
                    ((*vtbl).AddRef)(raw)
                }
            }
            None => 0, // 不应被调用
        }
    }

    /// 委托版 `Release`。
    pub fn delegate_release(&self) -> u32 {
        match &self.outer {
            Some(outer) => {
                // SAFETY: Release 不会 panic，不访问无效内存。
                unsafe {
                    let raw: *mut core::ffi::c_void = outer.as_raw();
                    let vtbl = *(raw as *const *const IUnknown_Vtbl);
                    ((*vtbl).Release)(raw)
                }
            }
            None => 0, // 不应被调用
        }
    }

    /// 仅在聚合模式下查询外部对象的接口。
    ///
    /// 用于聚合对象内部需要访问外部对象能力的场景。
    pub fn query_outer(&self, riid: &windows::core::GUID) -> Option<IUnknown> {
        let outer = self.outer.as_ref()?;
        // SAFETY: riid 是有效引用，outer 是有效 COM 对象。
        unsafe {
            let mut ptr: *mut core::ffi::c_void = core::ptr::null_mut();
            let hr = outer.query(riid, &mut ptr);
            if hr.is_ok() && !ptr.is_null() {
                Some(std::mem::transmute(ptr))
            } else {
                None
            }
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregation_controller_independent_mode() {
        let ctrl = AggregationController::new(None);
        assert!(!ctrl.is_aggregated());
    }

    #[test]
    fn delegate_qi_returns_enointerface_in_independent_mode() {
        let ctrl = AggregationController::new(None);
        let hr = ctrl.delegate_query_interface(
            {
                let iid = crate::com::iid::IID_IAPO;
                std::ptr::addr_of!(iid)
            },
            {
                let mut ptr = std::ptr::null_mut();
                &mut ptr
            },
        );
        assert_eq!(hr, super::super::abi::E_NOINTERFACE);
    }

    #[test]
    fn delegate_add_ref_returns_zero_in_independent_mode() {
        let ctrl = AggregationController::new(None);
        assert_eq!(ctrl.delegate_add_ref(), 0);
    }

    #[test]
    fn delegate_release_returns_zero_in_independent_mode() {
        let ctrl = AggregationController::new(None);
        assert_eq!(ctrl.delegate_release(), 0);
    }

    #[test]
    fn query_outer_returns_none_in_independent_mode() {
        let ctrl = AggregationController::new(None);
        let result = ctrl.query_outer(&crate::com::iid::IID_IAPO);
        assert!(result.is_none());
    }
}