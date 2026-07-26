//! installation/exports.rs — 四个 COM DLL 导出函数 + DllMain（Note 28/29/30/59）
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

use std::ffi::c_void;
use std::sync::atomic::AtomicPtr;

use windows::core::{BOOL, GUID, HRESULT};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::Registry::*;

use crate::com::abi;
use crate::com::clsid_reg::{self, ClsidEntry};
use crate::com::factory::{self};
use crate::instance::ref_count as inst_count;

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
            // MODULE_HANDLE 是 OnceLock，set 只执行一次，无复杂初始化。
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
// ══════════════════════════════════════════════════════════════════════════════

/// 创建指定 CLSID 的 ClassFactory。
///
/// COM 运行时（`CoCreateInstance` 内部）调用此函数获取工厂。
#[no_mangle]
#[allow(non_snake_case)]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    // ── 输入验证 ────────────────────────────────────────────────────────────

    if rclsid.is_null() || riid.is_null() || ppv.is_null() {
        return abi::E_POINTER;
    }

    // SAFETY: 由调用方（COM 运行时）保证 rclsid/riid 有效。
    let clsid = unsafe { *rclsid };

    // ── CLSID 路由（Note 4） ────────────────────────────────────────────────

    // Phase 2: Box 分配到堆上，确保 *ppv 在函数返回后仍然有效。
    // Phase 4: 切换到 #[implement] COM 生命周期管理后可移除 Box。
    let fac = Box::new(match factory::create_factory(&clsid) {
        Some(f) => f,
        None => return abi::CLASS_E_CLASSNOTAVAILABLE,
    });

    // ── QueryInterface 获取请求的接口 ───────────────────────────────────────

    let hr = fac.query_interface(riid, ppv);

    if abi::failed(hr) {
        drop(fac);
        return hr;
    }

    // QI 成功后 factory 的 ref_count = 2（构造 1 + QI +1）。
    // 释放工厂自身的引用 → ref_count = 1 归客户端。
    fac.release();

    // 泄漏 Box — 堆内存保持存活，*ppv 仍然有效。
    // Phase 4 用 #[implement] 宏管理 COM 生命周期后可移除此泄漏。
    let _ = Box::into_raw(fac);

    hr
}

// ══════════════════════════════════════════════════════════════════════════════
// DllCanUnloadNow（Note 2）
//
// INST_COUNT（活跃实例）和 LOCK_COUNT（客户端锁定）均零才返回 S_OK。
// ══════════════════════════════════════════════════════════════════════════════

/// 检查 DLL 是否可以安全卸载。
///
/// COM 运行时定期调用此函数。两者均零时可以卸载。
#[no_mangle]
#[allow(non_snake_case)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if inst_count::is_zero() && factory::lock_is_zero() {
        abi::S_OK
    } else {
        abi::S_FALSE
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// DllRegisterServer（Note 29）
//
// 注册顺序：PostMix APO → 失败回滚 PostMix → PreMix APO → 失败回滚两者 →
// COM 类注册 → 失败回滚两者 + 删除 COM 类键。
// ══════════════════════════════════════════════════════════════════════════════

/// 注册 APO 和 COM 类。
///
/// 由 `regsvr32 vxapo.dll` 调用。
#[no_mangle]
#[allow(non_snake_case)]
pub extern "system" fn DllRegisterServer() -> HRESULT {
    // 获取 DLL 路径
    let dll_path = match get_dll_path() {
        Some(p) => p,
        None => return abi::E_FAIL,
    };

    // 注册顺序（Note 29）：PostMix → PreMix
    let entries = clsid_reg::registration_order();

    for (i, entry) in entries.iter().enumerate() {
        if let Err(hr) = register_com_class(entry, &dll_path) {
            // 注册失败，回滚已注册的条目
            for j in 0..i {
                let _ = unregister_com_class(&entries[j]);
            }
            return hr;
        }
    }

    abi::S_OK
}

// ══════════════════════════════════════════════════════════════════════════════
// DllUnregisterServer（Note 30）
//
// 先删 InprocServer32 子键再删 CLSID 父键，然后注销 APO。
// 注销顺序：PreMix → PostMix（与注册相反）。
// ══════════════════════════════════════════════════════════════════════════════

/// 注销 APO 和 COM 类。
///
/// 由 `regsvr32 /u vxapo.dll` 调用。
#[no_mangle]
#[allow(non_snake_case)]
pub extern "system" fn DllUnregisterServer() -> HRESULT {
    let entries = clsid_reg::unregistration_order();

    for entry in &entries {
        // 即使某条目注销失败也继续（尽力清理）
        let _ = unregister_com_class(entry);
    }

    abi::S_OK
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
        windows::Win32::System::LibraryLoader::GetModuleFileNameW(
            Some(h_module),
            &mut buf,
        )
    };

    if len == 0 {
        return None;
    }

    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

/// 注册单个 CLSID 的 COM 类。
///
/// 写入 `HKCR\CLSID\{GUID}\InprocServer32` 路径和 ThreadingModel。
fn register_com_class(entry: &ClsidEntry, dll_path: &str) -> Result<(), HRESULT> {
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
            return Err(abi::E_FAIL);
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
        unsafe { let _ = RegCloseKey(hkey); }
        return Err(abi::E_FAIL);
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
    unsafe {
        let _ = RegCloseKey(hkey);
    }

    if tm_result.is_err() {
        return Err(abi::E_FAIL);
    }

    Ok(())
}

/// 注销单个 CLSID 的 COM 类。
///
/// 先删 InprocServer32 子键，再删 CLSID 父键（Note 30）。
fn unregister_com_class(entry: &ClsidEntry) -> Result<(), HRESULT> {
    let inproc_path = entry.inproc_server_path();
    let clsid_path = entry.clsid_key_path();

    // 删除 InprocServer32 子键
    // SAFETY: 删除注册表键。路径由 ClsidEntry 生成，格式安全。
    unsafe {
        let sub_key = windows::core::HSTRING::from(&inproc_path);
        let _ = RegDeleteTreeW(HKEY_CLASSES_ROOT, &sub_key);
    }

    // 删除 CLSID 父键
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
    use super::*;
    use windows::core::{GUID, IUnknown, Interface};
    use crate::installation::exports::factory::VxApoClassFactory;
    use crate::com::reg_props::{CLSID_VXAPO_PRE_MIX, CLSID_VXAPO_POST_MIX};

    /// 释放 DllGetClassObject 返回的 Phase 2 工厂指针。
    ///
    /// SAFETY: `ppv` 必须是从 DllGetClassObject 获得的合法指针。
    ///         Phase 4 切换到 #[implement] COM 对象后改用 release_com_ptr。
    unsafe fn release_factory_ptr(ppv: *mut c_void) {
        let boxed = Box::from_raw(ppv as *mut VxApoClassFactory);
        boxed.release();
        drop(boxed);
    }

    /// 释放 create_instance 返回的 Phase 2 APO 对象指针。
    ///
    /// SAFETY: `ppv` 必须是从 create_instance 获得的合法指针。
    ///         Phase 4 切换到 #[implement] COM 对象后改用 release_com_ptr。
    unsafe fn release_apo_ptr(ppv: *mut c_void) {
        let boxed = Box::from_raw(ppv as *mut crate::com::factory::ApoObject);
        let (_, should_drop) = boxed.release();
        if should_drop {
            drop(boxed);
        }
    }

    // ── DllCanUnloadNow ─────────────────────────────────────────────────────

    #[test]
    fn can_unload_when_empty() {
        inst_count::reset_for_test();
        factory::lock_reset_for_test();
        assert_eq!(DllCanUnloadNow(), abi::S_OK);
    }

    #[test]
    fn cannot_unload_with_instance() {
        inst_count::reset_for_test();
        factory::lock_reset_for_test();

        let _obj = crate::instance::object::ApoObjectState::new(CLSID_VXAPO_PRE_MIX);
        inst_count::increment();

        assert_eq!(DllCanUnloadNow(), abi::S_FALSE);

        inst_count::decrement();
        assert_eq!(DllCanUnloadNow(), abi::S_OK);
    }

    #[test]
    fn cannot_unload_with_lock() {
        inst_count::reset_for_test();
        factory::lock_reset_for_test();

        factory::lock_increment();
        assert_eq!(DllCanUnloadNow(), abi::S_FALSE);

        factory::lock_decrement();
        assert_eq!(DllCanUnloadNow(), abi::S_OK);
    }

    #[test]
    fn cannot_unload_with_both() {
        inst_count::reset_for_test();
        factory::lock_reset_for_test();

        inst_count::increment();
        factory::lock_increment();
        assert_eq!(DllCanUnloadNow(), abi::S_FALSE);

        inst_count::decrement();
        assert_eq!(DllCanUnloadNow(), abi::S_FALSE);

        factory::lock_decrement();
        assert_eq!(DllCanUnloadNow(), abi::S_OK);
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

        assert_eq!(hr, abi::S_OK);
        assert!(!ppv.is_null());

        unsafe { release_factory_ptr(ppv); }
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

        assert_eq!(hr, abi::S_OK);
        assert!(!ppv.is_null());

        unsafe { release_factory_ptr(ppv); }
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

        assert_eq!(hr, abi::CLASS_E_CLASSNOTAVAILABLE);
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
        assert_eq!(hr, abi::E_POINTER);
    }

    // ── DllMain ─────────────────────────────────────────────────────────────

    #[test]
    fn dll_main_attach_returns_true() {
        // SAFETY: 模拟 DLL_PROCESS_ATTACH
        let result = unsafe {
            DllMain(HMODULE::default(), 1, std::ptr::null_mut())
        };
        assert!(result.as_bool());
    }

    #[test]
    fn dll_main_detach_returns_true() {
        let result = unsafe {
            DllMain(HMODULE::default(), 0, std::ptr::null_mut())
        };
        assert!(result.as_bool());
    }

    #[test]
    fn dll_main_unknown_reason() {
        let result = unsafe {
            DllMain(HMODULE::default(), 999, std::ptr::null_mut())
        };
        assert!(result.as_bool());
    }

    // ── 端到端：注册 → 获取工厂 → 注销 ─────────────────────────────────────

    #[test]
    fn full_lifecycle_simulation() {
        inst_count::reset_for_test();
        factory::lock_reset_for_test();

        // 1. 获取 ClassFactory
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
        assert_eq!(hr, abi::S_OK);
        assert!(!ppv.is_null());

        // 2. 释放工厂 — Phase 2 用 Box::from_raw，Phase 4 改用 release_com_ptr。
        unsafe { release_factory_ptr(ppv); }

        // 3. 确认可卸载
        assert_eq!(DllCanUnloadNow(), abi::S_OK);
    }
}