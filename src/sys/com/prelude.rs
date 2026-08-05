//! sys/com/prelude.rs — COM 基础类型重导出 + HRESULT 常量（v6.3 规范 3.1）
//!
//! 职责：重导出 `windows-rs` 的 COM 基础类型与 HRESULT 常量。
//!
//! 引用来源：
//! - `windows::core::{IUnknown, Interface, GUID, HRESULT, implement}`
//! - `windows::Win32::System::Com::IClassFactory`
//!
//! 导出给：`sys/com/` 下所有子模块。
//!
//! 禁止：不包含任何自定义类型、函数或逻辑（重导出除外）。
//!
//! # GUID 字符串转换的安全边界
//!
//! `windows::core::GUID` 本身未实现 `Display`/`ToString`，若直接使用 `Debug` 输出（`{:?}`）
//! 依赖不稳定格式，且无法保证可读性。官方 FFI 绑定 `StringFromGUID2` 需要 `unsafe` 调用，
//! 但该 `unsafe` 已由 `guid_to_string` 函数收窄为单一安全边界。
//!
//! 调用方应使用重导出的 `guid_to_string(&GUID) -> String`，而非自行调用 `StringFromGUID2`。

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

/// GUID → 字符串（标准 `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}` 格式，带花括号）。
pub use windows::Win32::System::Com::StringFromGUID2;

// ── HRESULT 常量（v6.3 规范 3.1） ──

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

/// `DllRegisterServer` 自注册失败（windows crate 未导出，本地补充）。
pub const SELFREG_E_CLASS: HRESULT = HRESULT(0x8004_0201u32 as i32);

// ── APO 专用 HRESULT 错误码（v6.3 规范 3.3.7）──
pub const APOERR_ALREADY_INITIALIZED:          HRESULT = HRESULT(0x887D_0001u32 as i32);
pub const APOERR_NOT_INITIALIZED:              HRESULT = HRESULT(0x887D_0002u32 as i32);
pub const APOERR_FORMAT_NOT_SUPPORTED:         HRESULT = HRESULT(0x887D_0003u32 as i32);
pub const APOERR_INVALID_APO_CLSID:            HRESULT = HRESULT(0x887D_0004u32 as i32);
pub const APOERR_BUFFERS_OVERLAP:              HRESULT = HRESULT(0x887D_0005u32 as i32);
pub const APOERR_ALREADY_UNLOCKED:             HRESULT = HRESULT(0x887D_0006u32 as i32);
pub const APOERR_NUM_CONNECTIONS_INVALID:      HRESULT = HRESULT(0x887D_0007u32 as i32);
pub const APOERR_INVALID_OUTPUT_MAXFRAMECOUNT: HRESULT = HRESULT(0x887D_0008u32 as i32);
pub const APOERR_INVALID_CONNECTION_FORMAT:    HRESULT = HRESULT(0x887D_0009u32 as i32);
pub const APOERR_APO_LOCKED:                   HRESULT = HRESULT(0x887D_000Au32 as i32);
pub const APOERR_INVALID_COEFFCOUNT:           HRESULT = HRESULT(0x887D_000Bu32 as i32);
pub const APOERR_INVALID_COEFFICIENT:          HRESULT = HRESULT(0x887D_000Cu32 as i32);
pub const APOERR_INVALID_CURVE_PARAM:          HRESULT = HRESULT(0x887D_000Du32 as i32);
pub const APOERR_INVALID_INPUTID:              HRESULT = HRESULT(0x887D_000Eu32 as i32);

// ── GUID 格式化辅助 ──

/// 将 GUID 格式化为标准字符串 `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`。
///
/// 底层使用 `StringFromGUID2`（windows-rs 0.62.2 签名：`(rguid, lpsz: &mut [u16]) -> i32`），
/// 返回**带花括号**的格式；与 `GUID::Debug`（不带花括号）不同。
/// `StringFromGUID2` 需要至少 39 个 WCHAR（38 字符 + null 终止符）。
pub fn guid_to_string(g: &GUID) -> String {
    let mut buf = [0u16; 40];
    // FFI：buf 长度足够，StringFromGUID2 保证 null 终止并返回写入字符数（含 null）。
    let len = unsafe { StringFromGUID2(g, &mut buf) };
    if len <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..(len as usize - 1)])
}

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
    fn guid_to_string_zeroed() {
        assert_eq!(
            guid_to_string(&GUID::zeroed()),
            "{00000000-0000-0000-0000-000000000000}"
        );
    }

    #[test]
    fn guid_to_string_standard_format() {
        let guid = GUID::from_values(
            0xA1B2C3D4,
            0x1234,
            0x5678,
            [0x9A, 0xBC, 0xDE, 0xF0, 0x12, 0x34, 0x56, 0x78],
        );
        assert_eq!(
            guid_to_string(&guid),
            "{A1B2C3D4-1234-5678-9ABC-DEF012345678}"
        );
    }

    #[test]
    fn guid_to_string_has_braces() {
        let s = guid_to_string(&GUID::zeroed());
        assert!(s.starts_with('{'));
        assert!(s.ends_with('}'));
        assert_eq!(s.len(), 38);
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
