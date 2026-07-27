//! sys/com/base.rs — COM 基础 ABI 定义
//!
//! 提供 `sys/com/` 层共用的类型重导出、常量与辅助函数。
//! 所有 `sys/com/` 子模块统一从此文件导入，避免在各处重复书写完整 `windows::` 路径。
//!
//! 此模块为纯定义层，不引入任何运行时逻辑或状态。

use windows::core::HRESULT;

// ══════════════════════════════════════════════════════════════════════════════
// 类型重导出
// ══════════════════════════════════════════════════════════════════════════════

/// windows crate 的 IUnknown，所有 COM 接口的根基。
pub use windows::core::IUnknown;

/// COM 接口 trait，提供 `QueryInterface` / `AddRef` / `Release` 的安全包装。
pub use windows::core::Interface;

/// COM 接口标识符。
pub use windows::core::GUID;

/// COM 类工厂接口。
pub use windows::Win32::System::Com::IClassFactory;

/// `implement` 宏：为 Rust 结构体自动生成 COM vtable 和引用计数逻辑。
///
/// 用于 `instance/object.rs` 中实现 APO COM 对象（Note 3）。
pub use windows::core::implement;

// ══════════════════════════════════════════════════════════════════════════════
// HRESULT 常量
// ══════════════════════════════════════════════════════════════════════════════

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

/// 类工厂：请求的 CLSID 未注册（Note 4）。
pub const CLASS_E_CLASSNOTAVAILABLE: HRESULT = HRESULT(0x8004_0111u32 as i32);

/// 内存分配失败。
pub const E_OUTOFMEMORY: HRESULT = HRESULT(0x8007_000Eu32 as i32);

/// 类不支持聚合。
pub const CLASS_E_NOAGGREGATION: HRESULT = HRESULT(0x8004_0110u32 as i32);

// ══════════════════════════════════════════════════════════════════════════════
// 辅助函数
// ══════════════════════════════════════════════════════════════════════════════

/// 判断 HRESULT 是否表示成功（>= 0）。
pub fn succeeded(hr: HRESULT) -> bool {
    hr.0 >= 0
}

/// 判断 HRESULT 是否表示失败（< 0）。
pub fn failed(hr: HRESULT) -> bool {
    hr.0 < 0
}

/// 将 `HRESULT` 转为 `crate::utils::error::Result<()>`。
///
/// 成功（`S_OK` / `S_FALSE`）返回 `Ok(())`，失败返回 `Err(VxApoError::HResult)`。
///
/// 用于 COM 方法内部调用 windows API 后的错误传播：
/// ```ignore
/// check_hresult(unsafe { SomeComCall(...) })?;
/// ```
pub fn check_hresult(hr: HRESULT) -> crate::utils::error::Result<()> {
    if succeeded(hr) {
        Ok(())
    } else {
        Err(hr.into())
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn succeeded_on_ok() {
        assert!(succeeded(S_OK));
        assert!(succeeded(S_FALSE));
    }

    #[test]
    fn succeeded_on_fail() {
        assert!(!succeeded(E_NOINTERFACE));
        assert!(!succeeded(E_POINTER));
        assert!(!succeeded(E_FAIL));
    }

    #[test]
    fn failed_is_inverse() {
        assert!(!failed(S_OK));
        assert!(failed(E_FAIL));
    }

    #[test]
    fn check_hresult_ok() {
        assert!(check_hresult(S_OK).is_ok());
    }

    #[test]
    fn check_hresult_false() {
        // S_FALSE 也视为成功
        assert!(check_hresult(S_FALSE).is_ok());
    }

    #[test]
    fn check_hresult_fail() {
        let err = check_hresult(E_NOINTERFACE).unwrap_err();
        assert!(matches!(err, crate::utils::error::VxApoError::HResult(_)));
    }

    #[test]
    fn hresult_constants_values() {
        assert_eq!(S_OK.0, 0);
        assert_eq!(S_FALSE.0, 1);
        assert!(E_NOINTERFACE.0 < 0);
        assert!(E_POINTER.0 < 0);
        assert!(E_FAIL.0 < 0);
        assert!(CLASS_E_CLASSNOTAVAILABLE.0 < 0);
    }

    #[test]
    fn check_hresult_roundtrip() {
        // VxApoError::HResult → Display → 确认不 panic
        let err = check_hresult(CLASS_E_CLASSNOTAVAILABLE).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("COM error"));
    }
}