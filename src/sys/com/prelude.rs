//! sys/com/prelude.rs — COM 基础类型重导出 + HRESULT 常量（v6.2 规范 3.1）
//!
//! 职责：重导出 `windows-rs` 的 COM 基础类型与 HRESULT 常量。
//!
//! 引用来源：
//! - `windows::core::{IUnknown, Interface, GUID, HRESULT, implement}`
//! - `windows::Win32::System::Com::IClassFactory`
//!
//! 导出给：`sys/com/` 下所有子模块。
//!
//! 禁止：不包含任何自定义类型、函数或逻辑。

// ── 类型重导出 ──

/// windows crate 的 IUnknown，所有 COM 接口的根基。
pub use windows::core::IUnknown;

/// IUnknown vtable（`#[interface]` 宏展开需要）。
pub use windows::core::IUnknown_Vtbl;

/// COM 接口 trait，提供 `QueryInterface` / `AddRef` / `Release` 的安全包装。
pub use windows::core::Interface;

/// COM 接口标识符。
pub use windows::core::GUID;

/// COM 错误码。
pub use windows::core::HRESULT;

/// `implement` 宏：为 Rust 结构体自动生成 COM vtable 和引用计数逻辑。
pub use windows::core::implement;

/// `interface` 宏：为 Rust trait 自动生成 COM vtable。
pub use windows::core::interface;

/// COM 类工厂接口。
pub use windows::Win32::System::Com::IClassFactory;

// ── HRESULT 常量（v6.2 规范 3.1） ──

/// 操作成功。
pub const S_OK: HRESULT = HRESULT(0);

/// 操作成功，但返回非标准结果（如 `IsInputFormatSupported` 返回替代格式）。
pub const S_FALSE: HRESULT = HRESULT(1);

/// `QueryInterface`：请求的接口不可用。
pub const E_NOINTERFACE: HRESULT = HRESULT(0x8000_4002u32 as i32);

/// 传入了空指针参数。
pub const E_POINTER: HRESULT = HRESULT(0x8000_4003u32 as i32);

/// 一般性失败。
pub const E_FAIL: HRESULT = HRESULT(0x8000_4005u32 as i32);

/// 未预期的错误。
pub const E_UNEXPECTED: HRESULT = HRESULT(0x8000_FFFF_u32 as i32);

/// 无效参数。
pub const E_INVALIDARG: HRESULT = HRESULT(0x8007_0057u32 as i32);

/// 类工厂：请求的 CLSID 未注册。
pub const CLASS_E_CLASSNOTAVAILABLE: HRESULT = HRESULT(0x8004_0111u32 as i32);

/// 内存分配失败。
pub const E_OUTOFMEMORY: HRESULT = HRESULT(0x8007_000Eu32 as i32);

/// 类不支持聚合。
pub const CLASS_E_NOAGGREGATION: HRESULT = HRESULT(0x8004_0110u32 as i32);

// ── 测试 ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hresult_constants_values() {
        assert_eq!(S_OK.0, 0);
        assert_eq!(S_FALSE.0, 1);
        assert!(E_NOINTERFACE.0 < 0);
        assert!(E_POINTER.0 < 0);
        assert!(E_FAIL.0 < 0);
        assert!(E_UNEXPECTED.0 < 0);
        assert!(E_INVALIDARG.0 < 0);
        assert!(CLASS_E_CLASSNOTAVAILABLE.0 < 0);
        assert!(E_OUTOFMEMORY.0 < 0);
        assert!(CLASS_E_NOAGGREGATION.0 < 0);
    }

    #[test]
    fn hresult_constants_unique() {
        let values = [
            S_OK.0,
            S_FALSE.0,
            E_NOINTERFACE.0,
            E_POINTER.0,
            E_FAIL.0,
            E_UNEXPECTED.0,
            E_INVALIDARG.0,
            CLASS_E_CLASSNOTAVAILABLE.0,
            E_OUTOFMEMORY.0,
            CLASS_E_NOAGGREGATION.0,
        ];
        for i in 0..values.len() {
            for j in (i + 1)..values.len() {
                assert_ne!(values[i], values[j], "HRESULT at {i} and {j} are equal");
            }
        }
    }
}