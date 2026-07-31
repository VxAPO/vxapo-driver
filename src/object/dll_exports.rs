//! host/installation/exports.rs — 四个 COM DLL 导出函数 + DllMain（Note 28/29/30/59）
//!
//! 导出函数：
//! - `DllRegisterServer`：注册 COM 类与 APO（Note 29）
//! - `DllUnregisterServer`：注销 COM 类与 APO（Note 30）
//! - `DllGetClassObject`：根据 CLSID 路由到对应 ClassFactory（Note 4）
//! - `DllCanUnloadNow`：检查 `INST_COUNT == 0 && LOCK_COUNT == 0`，判定可否卸载（Note 2）
//! - `DllMain`：DLL 入口，仅保存模块句柄到全局 `static`，始终返回 `TRUE`（Note 59）
//!
//! 全部函数标记 `#[no_mangle] pub extern "system"`（Note 28）。
//!
//! DllMain 约束（Note 59）：
//! 禁止在 `DLL_PROCESS_ATTACH` 中执行以下操作：
//! - 初始化 COM（`CoInitializeEx`）
//! - 创建线程（`std::thread::spawn`）
//! - 触发全局 `static` 的复杂初始化（`once_cell::Lazy::force()` 等）
//! - 使用 `#[ctor]` 宏标注的初始化函数
//! - 调用任何 `windows-rs` 的 COM 初始化宏
//!
//! 所有 COM 初始化与 APO 对象构造延迟到 `DllGetClassObject` 或
//! `CreateInstance` 被首次调用时执行。
//!
//! 注册表写入委托 `sys/registry/write.rs`，安装流程委托 `host/installation/install.rs`。

use std::ffi::c_void;
use std::sync::atomic::AtomicPtr;

use windows::core::{BOOL, GUID, HRESULT};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::Com::IClassFactory;
use windows::Win32::System::Registry::*;

use crate::sys::com::prelude::*;
use crate::object::factory;
use crate::object::ref_count as inst_count;

use crate::object::vx_reg_props;

/// 将字符串转换为注册表所需的 null-terminated UTF-16 字节数组。
fn to_registry_bytes(s: &str) -> Vec<u8> {
    let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2).to_vec()
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 全局状态
// ══════════════════════════════════════════════════════════════════════════════

/// DLL 模块句柄（DllMain 中保存，Note 59）。
/// 用于 `GetModuleFileNameW` 获取 DLL 路径，供注册/注销使用。
static MODULE_HANDLE: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

// ══════════════════════════════════════════════════════════════════════════════
// DllMain（Note 59）
//
// 极简实现——仅保存模块句柄，始终返回 TRUE。
//
// 禁止在 DllMain（DLL_PROCESS_ATTACH）中：
// - 初始化 COM（CoInitializeEx）
// - 创建线程（std::thread::spawn）
// - 触发全局 static 的复杂初始化（once_cell::Lazy::force、lazy_static 首次访问）
// - 使用 #[ctor] 宏标注的初始化函数
// - 调用任何 windows-rs 的 COM 初始化宏
//
// 所有 COM 初始化和 APO 对象构造必须延迟到 DllGetClassObject 或
// CreateInstance 被调用时进行（Windows Loader Lock 铁律）。
// ══════════════════════════════════════════════════════════════════════════════

/// DLL 入口点。
///
/// # Safety
///
/// 由 Windows 加载器在进程/线程 attach/detach 时调用。
/// 在 Loader Lock 持有期间执行，必须保持极简。
#[no_mangle]
#[allow(non_snake_case)]
pub unsafe extern "system" fn DllMain(
    h_module: HMODULE,
    ul_reason_for_call: u32,
    _lp_reserved: *mut c_void,
) -> BOOL {
    const DLL_PROCESS_ATTACH: u32 = 1;
    const DLL_PROCESS_DETACH: u32 = 0;
    const DLL_THREAD_ATTACH: u32 = 2;
    const DLL_THREAD_DETACH: u32 = 3;

    match ul_reason_for_call {
        DLL_PROCESS_ATTACH => {
            // Note 59：仅保存模块句柄，不做任何其他操作。
            // MODULE_HANDLE 是 AtomicPtr，store 只执行原子写入，无复杂初始化。
            MODULE_HANDLE.store(h_module.0, std::sync::atomic::Ordering::SeqCst);
        }
        DLL_PROCESS_DETACH => {
            // 清理：当前无需特殊清理。
            // 未来 telemetry/ 的日志刷新在此处执行。
        }
        DLL_THREAD_ATTACH | DLL_THREAD_DETACH => {
            // 忽略——APO 不关心线程创建/销毁。
        }
        _ => {}
    }

    BOOL::from(true)
}

// ══════════════════════════════════════════════════════════════════════════════
// DllGetClassObject（Note 4）
//
// 只接受 PreMix / PostMix 两个 CLSID，其余返回 CLASS_E_CLASSNOTAVAILABLE。
// 使用 `#[implement]` 自动管理 COM 生命周期，无需 `Box` 泄漏。
// ══════════════════════════════════════════════════════════════════════════════

/// 创建指定 CLSID 的 ClassFactory。
///
/// COM 运行时（`CoCreateInstance` 内部）调用此函数获取工厂。
/// Phase 4：使用 `#[implement]` COM 智能指针，自动管理引用计数。
#[no_mangle]
#[allow(non_snake_case)]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    // ── 参数校验 ──────────────────────────────────────────
    // SAFETY: COM 运行时保证传入有效指针或 null。

    if rclsid.is_null() || riid.is_null() || ppv.is_null() {
        return E_POINTER;
    }

    // 预置 null，调用方可以据此判断失败（Note 2）
    unsafe { *ppv = std::ptr::null_mut(); }

    // SAFETY: 由调用方（COM 运行时）保证 rclsid 有效。
    let clsid = unsafe { *rclsid };

    // ── CLSID 路由（Note 4） ────────────────────────────────

    // 创建工厂（#[implement] COM 智能指针，ref_count 初始 = 1）
    let factory: IClassFactory = match factory::create_factory(&clsid) {
        Some(f) => f,
        None => return CLASS_E_CLASSNOTAVAILABLE,
    };

    // ── QueryInterface 获取请求的接口 ──────────────────────
    // windows-interface 0.59.3 跨模块方法不可见，使用原始 vtable 调用 QI。
    // Phase 5: 升级 windows-rs 后可移除 vtable 直调，改用 factory.query(&riid, ppv)。
    let raw_ptr: *mut c_void = unsafe { std::mem::transmute_copy(&factory) };
    let vtbl = unsafe { *(raw_ptr as *const *const usize) };
    type QIFn = unsafe extern "system" fn(
        *mut c_void, *const GUID, *mut *mut c_void,
    ) -> HRESULT;
    // vtable[0] = QueryInterface
    let qi: QIFn = unsafe { std::mem::transmute(*vtbl.add(0)) };

    let hr = unsafe { qi(raw_ptr, &*riid, ppv as *mut *mut c_void) };

    // 释放工厂的临时引用（drop 触发 Release）
    drop(factory);
    // QI 成功时 ref_count 2 → 1，客户端持有 *ppv
    // QI 失败时 ref_count 1 → 0，对象自动释放

    if hr.is_err() {
        unsafe { *ppv = std::ptr::null_mut() };
    }

    hr
}

// ══════════════════════════════════════════════════════════════════════════════
// DllCanUnloadNow（Note 2）
//
// INST_COUNT（活跃实例）和 LOCK_COUNT（客户端锁定）均零才返回 S_OK。
// ══════════════════════════════════════════════════════════════════════════════

/// 检查 DLL 是否可以安全卸载。
///
/// COM 运行时定期调用此函数。INST_COUNT 与 LOCK_COUNT 均为零时可以卸载。
#[no_mangle]
#[allow(non_snake_case)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if inst_count::is_zero() && factory::lock_is_zero() {
        S_OK
    } else {
        S_FALSE
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// DllRegisterServer（Note 29）
//
// 注册顺序：PostMix → PreMix。
// 注册失败时回滚已注册的条目。
// ══════════════════════════════════════════════════════════════════════════════

/// 注册 COM 类。
///
/// 由 `regsvr32 vxapo.dll` 调用。
/// 注册顺序：PostMix → PreMix（Note 29）。
#[no_mangle]
#[allow(non_snake_case)]
pub extern "system" fn DllRegisterServer() -> HRESULT {
    // 获取 DLL 路径 — 需要 HMODULE（DllMain 中保存）。
    let dll_path = match get_dll_path() {
        Some(p) => p,
        None => return E_FAIL,
    };

    // 注册顺序（Note 29）：PostMix → PreMix
    let entries = vx_reg_props::registration_order();

    for (i, entry) in entries.iter().enumerate() {
        if let Err(hr) = register_com_class(entry, &dll_path) {
            // 注册失败，回滚已注册的条目
            for j in 0..i {
                let _ = unregister_com_class(&entries[j]);
            }
            return hr;
        }
    }

    S_OK
}

// ══════════════════════════════════════════════════════════════════════════════
// DllUnregisterServer（Note 30）
//
// 先删 InprocServer32 子键再删 CLSID 父键。
// 注销顺序：PreMix → PostMix（与注册相反）。
// ══════════════════════════════════════════════════════════════════════════════

/// 注销 COM 类。
///
/// 由 `regsvr32 /u vxapo.dll` 调用。
/// 注销顺序：PreMix → PostMix（与注册相反，Note 30）。
/// 尽力清理，即使某条目注销失败也继续。
#[no_mangle]
#[allow(non_snake_case)]
pub extern "system" fn DllUnregisterServer() -> HRESULT {
    // 注销顺序：PreMix → PostMix（与注册相反）
    let entries = vx_reg_props::unregistration_order();
    for entry in &entries {
        // 尽力清理，即使某条目注销失败也继续（Note 30）
        let _ = unregister_com_class(entry);
    }
    S_OK
}

// ══════════════════════════════════════════════════════════════════════════════
// 内部辅助
// ══════════════════════════════════════════════════════════════════════════════

/// 获取当前 DLL 的文件路径。
///
/// 使用 `GetModuleFileNameW` + `MODULE_HANDLE`。
fn get_dll_path() -> Option<String> {
    let ptr = MODULE_HANDLE.load(std::sync::atomic::Ordering::SeqCst);
    if ptr.is_null() {
        return None;
    }
    let h_module = HMODULE(ptr);

    let mut buf = vec![0u16; 1024];
    // SAFETY: h_module 由 DllMain 保存，buf 容量足够。
    let len = unsafe {
        windows::Win32::System::LibraryLoader::GetModuleFileNameW(Some(h_module), &mut buf)
    };
    if len == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

/// 注册单个 CLSID 的 COM 类。
///
/// 写入 `HKCR\CLSID\{GUID}\InprocServer32` 路径和 ThreadingModel。
fn register_com_class(
    entry: &vx_reg_props::ClsidEntry,
    dll_path: &str,
) -> Result<(), HRESULT> {
    let inproc_path = entry.inproc_server_path();

    // 创建 InprocServer32 键
    // SAFETY: 调用 RegCreateKeyExW 创建注册表键。
    let hkey = unsafe {
        let mut hkey = HKEY::default();
        let sub_key = windows::core::HSTRING::from(&inproc_path);
        let result = RegCreateKeyExW(
            HKEY_CLASSES_ROOT,
            &sub_key,
            Some(0),
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        );
        if result.is_err() {
            return Err(E_FAIL);
        }
        hkey
    };

    // 写入 Default = DLL 路径
    // SAFETY: dll_path 是合法 UTF-8 → HSTRING 转换保证 UTF-16 null 结尾。
    let write_result = unsafe {
        let value_name = windows::core::HSTRING::from("");
        RegSetValueExW(
            hkey,
            &value_name,
            Some(0),
            REG_SZ,
            Some(to_registry_bytes(dll_path).as_slice()),
        )
    };

    if write_result.is_err() {
        // SAFETY: hkey 由 RegCreateKeyExW 成功打开。
        unsafe { let _ = RegCloseKey(hkey); }
        return Err(E_FAIL);
    }

    // 写入 ThreadingModel = "Both"（Note 5）
    let tm_result = unsafe {
        let value_name = windows::core::HSTRING::from("ThreadingModel");
        RegSetValueExW(
            hkey,
            &value_name,
            Some(0),
            REG_SZ,
            Some(to_registry_bytes("Both").as_slice()),
        )
    };

    // 关闭键句柄
    // SAFETY: hkey 由 RegCreateKeyExW 成功打开。
    unsafe { let _ = RegCloseKey(hkey); }

    if tm_result.is_err() {
        return Err(E_FAIL);
    }

    Ok(())
}

/// 注销单个 CLSID 的 COM 类。
///
/// 先删 InprocServer32 子键，再删 CLSID 父键（Note 30）。
fn unregister_com_class(entry: &vx_reg_props::ClsidEntry) -> Result<(), HRESULT> {
    let inproc_path = entry.inproc_server_path();
    let clsid_path = entry.clsid_key_path();

    // 删除 InprocServer32 子键
    // SAFETY: 删除注册表键。路径由 ClsidEntry 生成，格式安全。
    unsafe {
        let sub_key = windows::core::HSTRING::from(&inproc_path);
        let _ = RegDeleteTreeW(HKEY_CLASSES_ROOT, &sub_key);
    }

    // 删除 CLSID 父键
    // SAFETY: 删除注册表键。路径由 ClsidEntry 生成，格式安全。
    unsafe {
        let sub_key = windows::core::HSTRING::from(&clsid_path);
        let _ = RegDeleteTreeW(HKEY_CLASSES_ROOT, &sub_key);
    }

    Ok(())
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use windows::core::{GUID, IUnknown, Interface};
    use crate::host::instance::reg_props::{CLSID_VXAPO_PRE_MIX, CLSID_VXAPO_POST_MIX};
    use super::*;

    /// 释放 COM 接口指针（通过 vtable 调用 Release）。
    ///
    /// Phase 4：使用 `#[implement]` COM 智能指针，通过 vtable 释放。
    /// Phase 5: windows-rs 方法可见后可改用 `.release()`。
    ///
    /// # Safety
    ///
    /// `ptr` 必须是有效的 COM 接口指针，或 null。
    unsafe fn release_com_ptr(ptr: *mut c_void) {
        if ptr.is_null() {
            return;
        }
        let vtbl = *(ptr as *const *const usize);
        type ReleaseFn = unsafe extern "system" fn(*mut c_void) -> u32;
        // vtable[2] = Release
        let release: ReleaseFn = std::mem::transmute(*vtbl.add(2));
        release(ptr);
    }

    // ── DllCanUnloadNow ─────────────────────────────────────────────────────

    #[test]
    fn can_unload_when_empty() {
        inst_count::reset_for_test();
        factory::lock_reset_for_test();
        assert_eq!(DllCanUnloadNow(), S_OK);
    }

    #[test]
    fn cannot_unload_with_instance() {
        inst_count::reset_for_test();
        factory::lock_reset_for_test();

        // 创建 APO 对象实例，INST_COUNT + 1
        let _apo = crate::host::instance::apo_interface::ApoObject::new(CLSID_VXAPO_PRE_MIX);
        assert_eq!(inst_count::get(), 1);
        assert_eq!(DllCanUnloadNow(), S_FALSE);

        // drop 时析构函数调用 Release，INST_COUNT - 1
        drop(_apo);
        assert_eq!(DllCanUnloadNow(), S_OK);
    }

    #[test]
    fn cannot_unload_with_lock() {
        inst_count::reset_for_test();
        factory::lock_reset_for_test();

        factory::lock_increment();
        assert_eq!(DllCanUnloadNow(), S_FALSE);

        factory::lock_decrement();
        assert_eq!(DllCanUnloadNow(), S_OK);
    }

    #[test]
    fn cannot_unload_with_both() {
        inst_count::reset_for_test();
        factory::lock_reset_for_test();

        inst_count::increment();
        factory::lock_increment();
        assert_eq!(DllCanUnloadNow(), S_FALSE);

        // 仅释放实例，仍有锁定
        inst_count::decrement();
        assert_eq!(DllCanUnloadNow(), S_FALSE);

        // 释放锁定后全部清零
        factory::lock_decrement();
        assert_eq!(DllCanUnloadNow(), S_OK);
    }

    // ── DllGetClassObject ───────────────────────────────────────────────────

    #[test]
    fn get_class_object_premix() {
        inst_count::reset_for_test();
        let clsid = CLSID_VXAPO_PRE_MIX;
        let iid = IUnknown::IID;
        let mut ppv: *mut c_void = std::ptr::null_mut();

        let hr = unsafe {
            DllGetClassObject(
                &clsid as *const GUID,
                &iid as *const GUID,
                &mut ppv as *mut *mut c_void,
            )
        };

        assert_eq!(hr, S_OK);
        assert!(!ppv.is_null());

        // Phase 4：通过 vtable 调用 Release 释放
        unsafe { release_com_ptr(ppv); }
    }

    #[test]
    fn get_class_object_postmix() {
        inst_count::reset_for_test();
        let clsid = CLSID_VXAPO_POST_MIX;
        let iid = IUnknown::IID;
        let mut ppv: *mut c_void = std::ptr::null_mut();

        let hr = unsafe {
            DllGetClassObject(
                &clsid as *const GUID,
                &iid as *const GUID,
                &mut ppv as *mut *mut c_void,
            )
        };

        assert_eq!(hr, S_OK);
        assert!(!ppv.is_null());

        // Phase 4：通过 vtable 调用 Release 释放
        unsafe { release_com_ptr(ppv); }
    }

    #[test]
    fn get_class_object_unknown_clsid() {
        let clsid = GUID::zeroed();
        let iid = IUnknown::IID;
        let mut ppv: *mut c_void = std::ptr::null_mut();

        let hr = unsafe {
            DllGetClassObject(
                &clsid as *const GUID,
                &iid as *const GUID,
                &mut ppv as *mut *mut c_void,
            )
        };

        assert_eq!(hr, CLASS_E_CLASSNOTAVAILABLE);
        assert!(ppv.is_null());
    }

    #[test]
    fn get_class_object_null_pointers() {
        let hr = unsafe {
            DllGetClassObject(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(hr, E_POINTER);
    }

    // ── DllMain ─────────────────────────────────────────────────────────────

    #[test]
    fn dll_main_attach_returns_true() {
        // DLL_PROCESS_ATTACH = 1
        let result = unsafe {
            DllMain(HMODULE::default(), 1, std::ptr::null_mut())
        };
        assert!(result.as_bool());
    }

    #[test]
    fn dll_main_detach_returns_true() {
        // DLL_PROCESS_DETACH = 0
        let result = unsafe {
            DllMain(HMODULE::default(), 0, std::ptr::null_mut())
        };
        assert!(result.as_bool());
    }

    #[test]
    fn dll_main_unknown_reason() {
        // 未定义的 reason 值也始终返回 TRUE
        let result = unsafe {
            DllMain(HMODULE::default(), 999, std::ptr::null_mut())
        };
        assert!(result.as_bool());
    }

    // ── 端到端：获取工厂 → 释放 → 确认可卸载 ────────────────

    #[test]
    fn full_lifecycle_simulation() {
        inst_count::reset_for_test();
        factory::lock_reset_for_test();

        // 1. 获取 ClassFactory（通过 DllGetClassObject → QI IUnknown）
        let clsid = CLSID_VXAPO_PRE_MIX;
        let iid = IUnknown::IID;
        let mut ppv: *mut c_void = std::ptr::null_mut();

        let hr = unsafe {
            DllGetClassObject(
                &clsid as *const GUID,
                &iid as *const GUID,
                &mut ppv as *mut *mut c_void,
            )
        };
        assert_eq!(hr, S_OK);
        assert!(!ppv.is_null());

        // 2. 释放工厂 — Phase 4 已用 #[implement] COM 智能指针
        unsafe { release_com_ptr(ppv); }

        // 3. 确认可卸载
        assert_eq!(DllCanUnloadNow(), S_OK);
    }
}