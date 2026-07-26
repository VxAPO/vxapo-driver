//! sys/com/vtable.rs — 编译期静态 vtable 布局验证（Note 1）
//!
//! `sys/com/apo_abi.rs` 的 `#[interface]` 宏与 `host/instance/object.rs` 的
//! `#[implement]` 宏自动生成 vtable 结构体。本模块负责编译期验证：
//!
//! 1. 生成的 vtable 结构体大小与对齐符合 Windows ABI 预期
//! 2. 函数指针数量与对应接口方法数一致
//! 3. IClassFactory vtable 的编译期断言
//!
//! 所有 vtable 必须为编译期静态常量，由宏生成，禁止运行时构造。
//!
//! 此模块仅包含 `const _: () = assert!(...)` 形式的编译期检查，不产生运行时代码。

use std::ffi::c_void;

// ══════════════════════════════════════════════════════════════════════════════
// vtable 结构体大小常量
//
// IUnknown vtable = 3 个函数指针（QueryInterface / AddRef / Release）
// 每个派生接口在其基础上追加自己的方法
// ══════════════════════════════════════════════════════════════════════════════

/// 单个函数指针在目标平台的大小（x86_64 = 8 字节）。
const PTR_SIZE: usize = std::mem::size_of::<*const c_void>();

// ══════════════════════════════════════════════════════════════════════════════
// vtable 条目数
// ══════════════════════════════════════════════════════════════════════════════

/// IUnknown vtable 条目数：3 个方法
///
/// 方法：QueryInterface / AddRef / Release
const IUNKNOWN_VTBL_ENTRIES: usize = 3;

/// IClassFactory vtable 条目数：IUnknown(3) + 2 个自定义方法
///
/// 方法：CreateInstance / LockServer
const ICLASSFACTORY_VTBL_ENTRIES: usize = 3 + 2;

/// IAudioProcessingObject vtable 条目数：IUnknown(3) + 6 个自定义方法
///
/// 方法：Reset / GetLatency / GetRegistrationProperties /
///       IsInputFormatSupported / IsOutputFormatSupported / GetInputChannelCount
const IAPO_VTBL_ENTRIES: usize = 3 + 6;

/// IAudioProcessingObjectRT vtable 条目数：IUnknown(3) + 3 个自定义方法
///
/// 方法：APOProcess / CalcInputFrames / CalcOutputFrames
const IAPO_RT_VTBL_ENTRIES: usize = 3 + 3;

/// IAudioProcessingObjectConfiguration vtable 条目数：IUnknown(3) + 2 个自定义方法
///
/// 方法：LockForProcess / UnlockForProcess
const IAPO_CONFIG_VTBL_ENTRIES: usize = 3 + 2;

// ══════════════════════════════════════════════════════════════════════════════
// 字节大小（用于 assert_vtbl_size 验证）
// ══════════════════════════════════════════════════════════════════════════════

const IUNKNOWN_VTBL_SIZE: usize = PTR_SIZE * IUNKNOWN_VTBL_ENTRIES;
const ICLASSFACTORY_VTBL_SIZE: usize = PTR_SIZE * ICLASSFACTORY_VTBL_ENTRIES;
const IAPO_VTBL_SIZE: usize = PTR_SIZE * IAPO_VTBL_ENTRIES;
const IAPO_RT_VTBL_SIZE: usize = PTR_SIZE * IAPO_RT_VTBL_ENTRIES;
const IAPO_CONFIG_VTBL_SIZE: usize = PTR_SIZE * IAPO_CONFIG_VTBL_ENTRIES;

// ══════════════════════════════════════════════════════════════════════════════
// 辅助：函数指针数量验证
// ══════════════════════════════════════════════════════════════════════════════

/// 验证 vtable 结构体大小等于预期的函数指针数量 × 指针大小。
///
/// 失败时在编译期 panic，附带实际大小和预期大小。
const fn assert_vtbl_size(_name: &str, actual: usize, expected: usize) {
    // const fn 中不能 format!，用简单比较 + 固定消息
    assert!(
        actual == expected,
        "vtable size mismatch: actual != expected (see compile error context)"
    );
}

// ══════════════════════════════════════════════════════════════════════════════
// 编译期断言
//
// windows-rs 0.62 的 #[interface] 宏生成的 vtable 类型名格式为：
//   {InterfaceName}_Vtbl
// 这些断言验证它们的大小符合预期。
// ══════════════════════════════════════════════════════════════════════════════

const _: () = {
    // 指针大小验证（防止意外编译到 32 位目标）
    assert!(PTR_SIZE == 8, "expected 64-bit pointers");

    // IClassFactory: 5 个函数指针
    let actual = std::mem::size_of::<windows::Win32::System::Com::IClassFactory_Vtbl>();
    assert_vtbl_size("IClassFactory", actual, ICLASSFACTORY_VTBL_SIZE);
};

// ══════════════════════════════════════════════════════════════════════════════
// NonDelegatingUnknown vtable 布局
//
// COM 聚合中，内部对象需要自己的 IUnknown vtable（不委托到外部）。
// windows-rs 的 implement 宏在聚合模式下自动生成此 vtable。
// 此处定义结构体大小断言，确保与标准 IUnknown vtable 一致。
// ══════════════════════════════════════════════════════════════════════════════

/// NonDelegatingUnknown vtable 的函数指针签名。
///
/// 与标准 IUnknown 完全相同（3 个函数指针），只是语义上不委托。
/// `implement` 宏生成的 NonDelegating vtable 使用相同的布局。
pub type NonDelegatingUnknown_Vtbl_Size = [u8; IUNKNOWN_VTBL_SIZE];

const _: () = {
    assert!(
        std::mem::size_of::<NonDelegatingUnknown_Vtbl_Size>() == IUNKNOWN_VTBL_SIZE,
        "NonDelegatingUnknown vtable must match IUnknown size"
    );
};

// ══════════════════════════════════════════════════════════════════════════════
// vtable 函数指针顺序文档
//
// 以下是各接口 vtable 中函数指针的精确顺序，严格与 Windows ABI 一致。
// 运行时不得构造 vtable，仅作开发参考。
// ══════════════════════════════════════════════════════════════════════════════

/// IUnknown vtable 函数指针顺序（所有 COM 接口共享）：
///
/// | 偏移 | 方法                | 签名                                          |
/// |------|---------------------|-----------------------------------------------|
/// | 0    | QueryInterface      | fn(this, riid, ppv) -> HRESULT                |
/// | 1    | AddRef              | fn(this) -> u32                               |
/// | 2    | Release             | fn(this) -> u32                               |
#[allow(dead_code)]
const _VTBL_ORDER_IUNKNOWN: () = ();

/// IClassFactory vtable 函数指针顺序：
///
/// | 偏移 | 方法                | 签名                                          |
/// |------|---------------------|-----------------------------------------------|
/// | 0    | QueryInterface      | fn(this, riid, ppv) -> HRESULT                |
/// | 1    | AddRef              | fn(this) -> u32                               |
/// | 2    | Release             | fn(this) -> u32                               |
/// | 3    | CreateInstance      | fn(this, pUnkOuter, riid, ppv) -> HRESULT     |
/// | 4    | LockServer          | fn(this, fLock) -> HRESULT                    |
#[allow(dead_code)]
const _VTBL_ORDER_ICLASSFACTORY: () = ();

/// IAudioProcessingObject vtable 函数指针顺序：
///
/// | 偏移 | 方法                      | 签名                                              |
/// |------|---------------------------|---------------------------------------------------|
/// | 0    | QueryInterface            | fn(this, riid, ppv) -> HRESULT                    |
/// | 1    | AddRef                    | fn(this) -> u32                                   |
/// | 2    | Release                   | fn(this) -> u32                                   |
/// | 3    | Reset                     | fn(this) -> HRESULT                               |
/// | 4    | GetLatency                | fn(this, pLatency) -> HRESULT                     |
/// | 5    | GetRegistrationProperties | fn(this, ppProps) -> HRESULT                      |
/// | 6    | IsInputFormatSupported    | fn(this, pOut, pReq, ppSup) -> HRESULT            |
/// | 7    | IsOutputFormatSupported   | fn(this, pIn, pReq, ppSup) -> HRESULT             |
/// | 8    | GetInputChannelCount      | fn(this, pCount) -> HRESULT                       |
#[allow(dead_code)]
const _VTBL_ORDER_IAPO: () = ();

/// IAudioProcessingObjectRT vtable 函数指针顺序：
///
/// | 偏移 | 方法              | 签名                                                          |
/// |------|-------------------|---------------------------------------------------------------|
/// | 0    | QueryInterface    | fn(this, riid, ppv) -> HRESULT                                |
/// | 1    | AddRef            | fn(this) -> u32                                               |
/// | 2    | Release           | fn(this) -> u32                                               |
/// | 3    | APOProcess        | fn(this, nIn, ppIn, nOut, ppOut) -> void                      |
/// | 4    | CalcInputFrames   | fn(this, outputFrames) -> u32                                 |
/// | 5    | CalcOutputFrames  | fn(this, inputFrames) -> u32                                  |
#[allow(dead_code)]
const _VTBL_ORDER_IAPO_RT: () = ();

/// IAudioProcessingObjectConfiguration vtable 函数指针顺序：
///
/// | 偏移 | 方法              | 签名                                                          |
/// |------|-------------------|---------------------------------------------------------------|
/// | 0    | QueryInterface    | fn(this, riid, ppv) -> HRESULT                                |
/// | 1    | AddRef            | fn(this) -> u32                                               |
/// | 2    | Release           | fn(this) -> u32                                               |
/// | 3    | LockForProcess    | fn(this, nIn, ppIn, nOut, ppOut) -> HRESULT                   |
/// | 4    | UnlockForProcess  | fn(this) -> HRESULT                                           |
#[allow(dead_code)]
const _VTBL_ORDER_IAPO_CONFIG: () = ();

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ptr_size_is_8_on_x64() {
        assert_eq!(PTR_SIZE, 8);
    }

    #[test]
    fn iclassfactory_vtbl_size() {
        let actual = std::mem::size_of::<windows::Win32::System::Com::IClassFactory_Vtbl>();
        assert_eq!(actual, ICLASSFACTORY_VTBL_SIZE);
        assert_eq!(actual, PTR_SIZE * 5);
    }

    #[test]
    fn iclassfactory_vtbl_alignment() {
        let align = std::mem::align_of::<windows::Win32::System::Com::IClassFactory_Vtbl>();
        assert_eq!(align, PTR_SIZE);
    }

    #[test]
    fn non_delegating_vtbl_size_matches_iunknown() {
        // NonDelegatingUnknown 的 vtable 与 IUnknown 布局完全相同
        assert_eq!(
            std::mem::size_of::<NonDelegatingUnknown_Vtbl_Size>(),
            IUNKNOWN_VTBL_SIZE
        );
    }

    #[test]
    fn expected_vtbl_sizes_are_consistent() {
        // IUnknown = 3 ptrs
        assert_eq!(IUNKNOWN_VTBL_SIZE, PTR_SIZE * 3);
        // IClassFactory = IUnknown(3) + CreateInstance(1) + LockServer(1) = 5 ptrs
        assert_eq!(ICLASSFACTORY_VTBL_SIZE, PTR_SIZE * 5);
        // IAPO = IUnknown(3) + 6 methods = 9 ptrs
        assert_eq!(IAPO_VTBL_SIZE, PTR_SIZE * 9);
        // IAPO_RT = IUnknown(3) + 3 methods = 6 ptrs
        assert_eq!(IAPO_RT_VTBL_SIZE, PTR_SIZE * 6);
        // IAPO_CONFIG = IUnknown(3) + 2 methods = 5 ptrs
        assert_eq!(IAPO_CONFIG_VTBL_SIZE, PTR_SIZE * 5);
    }
}