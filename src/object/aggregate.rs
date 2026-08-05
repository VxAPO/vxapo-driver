//! object/aggregate.rs — COM 聚合委托外壳（P0-7 无声根因修复）
//!
//! Windows 音频引擎**强制用聚合模式（pUnkOuter 非空）创建 APO**（探针实证 punkouter_null=false）。
//! windows-rs 0.62 `#[implement]` 生成的 IUnknown 自包含、**不委托**——引擎聚合创建后
//! 经外层链 QI(IAudioProcessingObject) 走不到 inner → 弃用对象 → 完全无声。
//!
//! 本文件实现 EAPO 聚合语义（EqualizerAPO.cpp 67-80 / 519-539）：
//! - NApo 是 **NonDelegating 内层**：QI(IUnknown) 恒返回自身（NonDelegatingUnknown 身份）；
//!   QI(接口) 返回内部 `ApoObject` 接口（NonDelegatingQI）
//! - NApo AddRef/Release **自维护**（NonDelegating 引用计数）——引擎对返回的接口指针调
//!   AddRef/Release 都落在 NApo/内部接口上；委托外壳会导致 NApo 永不到零泄漏
//! - 引擎外层经聚合外壳 QI → 外壳转发回 inner 的 NonDelegatingQI（本对象）
//! - 接口方法：转发到内部 `ApoObject`（复用全部 DSP 逻辑，零迁移）
//!
//! RT/Config/ASE vtable 当前仅预置（引擎主走 IAPO 路径，QI 拿到其余接口后转发用）；
//! 保留声明避免未来扩展时重写。dead_code 因未直接引用而告警——压制。
#![allow(dead_code)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::{GUID, HRESULT, IUnknown, Interface};

use crate::object::apo::ApoObject;

// HRESULT 常量（windows-core 0.62 不导出 E_/S_ 系列，手写）。
const S_OK: HRESULT = HRESULT(0);
const E_NOINTERFACE: HRESULT = HRESULT(0x8000_4002u32 as i32);
const E_POINTER: HRESULT = HRESULT(0x8000_4003u32 as i32);

// ── vtable 槽位类型（真实 fn 指针，非 usize——const eval 允许）────────
type QiFn = unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT;
type RefFn = unsafe extern "system" fn(*mut c_void) -> u32;

#[repr(C)]
struct IapoVtbl {
    qi: QiFn,
    addref: RefFn,
    release: RefFn,
    reset: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    get_latency: unsafe extern "system" fn(*mut c_void, *mut i64) -> HRESULT,
    get_reg_props: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    initialize: unsafe extern "system" fn(*mut c_void, u32, *const u8) -> HRESULT,
    is_input_fmt: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, *mut *mut c_void) -> HRESULT,
    is_output_fmt: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, *mut *mut c_void) -> HRESULT,
    get_input_channels: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
}

#[repr(C)]
struct IapoRtVtbl {
    qi: QiFn,
    addref: RefFn,
    release: RefFn,
    apo_process: unsafe extern "system" fn(*mut c_void, u32, *const *const c_void, u32, *mut *mut c_void),
    calc_input: unsafe extern "system" fn(*mut c_void, u32) -> u32,
    calc_output: unsafe extern "system" fn(*mut c_void, u32) -> u32,
}

#[repr(C)]
struct IapoCfgVtbl {
    qi: QiFn,
    addref: RefFn,
    release: RefFn,
    lock_for_process: unsafe extern "system" fn(*mut c_void, u32, *const *const c_void, u32, *const *const c_void) -> HRESULT,
    unlock_for_process: unsafe extern "system" fn(*mut c_void) -> HRESULT,
}

#[repr(C)]
struct IapoAseVtbl {
    qi: QiFn,
    addref: RefFn,
    release: RefFn,
}

// ── NApo 聚合对象（NonDelegating 内层） ──────────────────────
#[repr(C)]
pub struct NApo {
    /// IAudioProcessingObject vtable（对象首字段 = COM 要求）。
    iapovtbl: *const IapoVtbl,
    /// 聚合外壳 IUnknown（NULL = 非聚合）。
    p_unk_outer: *mut c_void,
    /// NonDelegating 引用计数（自维护）。
    cref: AtomicU32,
    /// 内部 4 接口（ApoObject 创建后 QI 缓存，NApo Release 归零时释放）。
    i_apo: *mut c_void,
    i_apo_rt: *mut c_void,
    i_cfg: *mut c_void,
    i_ase: *mut c_void,
}

fn as_apo<'a>(this: *mut c_void) -> &'a NApo {
    // SAFETY: 调用方保证 this 指向有效 NApo（Box 分配后移交 COM）。
    unsafe { &*(this as *const NApo) }
}

unsafe fn inner_addref(inner: *mut c_void) -> u32 {
    let vtbl = unsafe { *(inner as *const *const usize) };
    let addref: RefFn = unsafe { std::mem::transmute(*vtbl.add(1)) };
    unsafe { addref(inner) }
}

// ── NonDelegatingQI（EAPO:519-538）───────────────────────────
unsafe extern "system" fn na_qi(this: *mut c_void, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT {
    // ---- 探针 7：记录引擎对 NApo 的每个 QI（2026-08-05，debug 门控，排查完删除）----
    #[cfg(debug_assertions)]
    {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(
            r"C:\ProgramData\VxAPO\qi_probe.txt",
        ) {
            let _ = writeln!(f, "NApo QI riid={:?} this={:p}", unsafe { *riid }, this);
        }
    }
    if riid.is_null() || ppv.is_null() {
        return E_POINTER;
    }
    unsafe { *ppv = std::ptr::null_mut() };
    let apo = as_apo(this);
    let iid = unsafe { *riid };

    // QI(IUnknown) 恒返回自身（NonDelegatingUnknown 身份）——即使聚合也如此。
    if iid == IUnknown::IID {
        unsafe { *ppv = this as *mut c_void };
        unsafe { na_addref(this) };
        return S_OK;
    }

    // 其余接口 → 返回 **NApo 自身的接口指针**（EAPO NonDelegatingQI 返回 *this 的语义，
    // EqualizerAPO.cpp:523-530）——引擎对返回指针调方法必须落回 NApo vtable（转发器），
    // 而非内部 ApoObject 的独立接口（身份不一致 + 绕过聚合层）。AddRef 自维护。
    let target = if iid == windows::Win32::Media::Audio::Apo::IAudioProcessingObject::IID {
        // IAPO = NApo 对象首字段（vtable 指针地址）。
        this
    } else if iid == windows::Win32::Media::Audio::Apo::IAudioProcessingObjectRT::IID {
        // 后续接口指针：NApo 对象尾部追加的 vtable 槽位（仍以 this 为基址，见 NApo 布局）。
        this
    } else if iid == windows::Win32::Media::Audio::Apo::IAudioProcessingObjectConfiguration::IID {
        this
    } else if iid == windows::Win32::Media::Audio::Apo::IAudioSystemEffects::IID {
        this
    } else {
        return E_NOINTERFACE;
    };
    unsafe { *ppv = target };
    unsafe { na_addref(this) };
    S_OK
}

// ── NonDelegatingAddRef/Release（EAPO:541-555，自维护）────────
unsafe extern "system" fn na_addref(this: *mut c_void) -> u32 {
    let apo = as_apo(this);
    apo.cref.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn na_release(this: *mut c_void) -> u32 {
    let apo = as_apo(this);
    let r = apo.cref.fetch_sub(1, Ordering::Release) - 1;
    if r == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        let apo = as_apo(this);
        let release_inner = |p: *mut c_void| {
            if !p.is_null() {
                let vtbl = unsafe { *(p as *const *const usize) };
                let release: RefFn = unsafe { std::mem::transmute(*vtbl.add(2)) };
                unsafe { release(p) };
            }
        };
        release_inner(apo.i_apo);
        release_inner(apo.i_apo_rt);
        release_inner(apo.i_cfg);
        release_inner(apo.i_ase);
        drop(Box::from_raw(this as *mut NApo));
    }
    r
}

// IAudioProcessingObject 方法转发（全部手工——避免 vtable 槽位错位风险）。
unsafe extern "system" fn na_reset(this: *mut c_void) -> HRESULT {
    // ---- 探针 9：Reset（2026-08-05）----
    #[cfg(debug_assertions)]
    {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(
            r"C:\ProgramData\VxAPO\method_probe.txt",
        ) {
            let _ = writeln!(f, "Reset called this={:p}", this);
        }
    }
    let apo = as_apo(this);
    let vtbl = unsafe { *(apo.i_apo as *const *const usize) };
    let f: unsafe extern "system" fn(*mut c_void) -> HRESULT = unsafe { std::mem::transmute(*vtbl.add(3)) };
    unsafe { f(apo.i_apo) }
}

unsafe extern "system" fn na_get_latency(this: *mut c_void, out: *mut i64) -> HRESULT {
    // ---- 探针 9：GetLatency（2026-08-05）----
    #[cfg(debug_assertions)]
    {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(
            r"C:\ProgramData\VxAPO\method_probe.txt",
        ) {
            let _ = writeln!(f, "GetLatency called this={:p}", this);
        }
    }
    let apo = as_apo(this);
    let vtbl = unsafe { *(apo.i_apo as *const *const usize) };
    let f: unsafe extern "system" fn(*mut c_void, *mut i64) -> HRESULT = unsafe { std::mem::transmute(*vtbl.add(4)) };
    unsafe { f(apo.i_apo, out) }
}

unsafe extern "system" fn na_get_reg_props(this: *mut c_void, out: *mut *mut c_void) -> HRESULT {
    // ---- 探针 9：GetRegistrationProperties（引擎 QI 后第一步验证，2026-08-05）----
    #[cfg(debug_assertions)]
    {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(
            r"C:\ProgramData\VxAPO\method_probe.txt",
        ) {
            let _ = writeln!(f, "GetRegistrationProperties called this={:p}", this);
        }
    }
    let apo = as_apo(this);
    let vtbl = unsafe { *(apo.i_apo as *const *const usize) };
    let f: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT = unsafe { std::mem::transmute(*vtbl.add(5)) };
    unsafe { f(apo.i_apo, out) }
}

unsafe extern "system" fn na_initialize(this: *mut c_void, cb: u32, data: *const u8) -> HRESULT {
    let apo = as_apo(this);
    let vtbl = unsafe { *(apo.i_apo as *const *const usize) };
    let f: unsafe extern "system" fn(*mut c_void, u32, *const u8) -> HRESULT = unsafe { std::mem::transmute(*vtbl.add(6)) };
    unsafe { f(apo.i_apo, cb, data) }
}

unsafe extern "system" fn na_is_input_fmt(this: *mut c_void, a: *mut c_void, b: *mut c_void, out: *mut *mut c_void) -> HRESULT {
    // ---- 探针 8：引擎格式协商（2026-08-05，debug 门控，排查完删除）----
    #[cfg(debug_assertions)]
    {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(
            r"C:\ProgramData\VxAPO\fmt_probe.txt",
        ) {
            let _ = writeln!(f, "IsInputFormatSupported called this={:p}", this);
        }
    }
    let apo = as_apo(this);
    let vtbl = unsafe { *(apo.i_apo as *const *const usize) };
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, *mut *mut c_void) -> HRESULT = unsafe { std::mem::transmute(*vtbl.add(7)) };
    unsafe { f(apo.i_apo, a, b, out) }
}

unsafe extern "system" fn na_is_output_fmt(this: *mut c_void, a: *mut c_void, b: *mut c_void, out: *mut *mut c_void) -> HRESULT {
    // ---- 探针 8：引擎输出格式协商（2026-08-05，debug 门控，排查完删除）----
    #[cfg(debug_assertions)]
    {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(
            r"C:\ProgramData\VxAPO\fmt_probe.txt",
        ) {
            let _ = writeln!(f, "IsOutputFormatSupported called this={:p}", this);
        }
    }
    let apo = as_apo(this);
    let vtbl = unsafe { *(apo.i_apo as *const *const usize) };
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, *mut *mut c_void) -> HRESULT = unsafe { std::mem::transmute(*vtbl.add(8)) };
    unsafe { f(apo.i_apo, a, b, out) }
}

unsafe extern "system" fn na_get_input_channels(this: *mut c_void, out: *mut u32) -> HRESULT {
    let apo = as_apo(this);
    let vtbl = unsafe { *(apo.i_apo as *const *const usize) };
    let f: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT = unsafe { std::mem::transmute(*vtbl.add(9)) };
    unsafe { f(apo.i_apo, out) }
}

// IAudioProcessingObjectRT 转发。
unsafe extern "system" fn na_apo_process(this: *mut c_void, nin: u32, pin: *const *const c_void, nout: u32, pout: *mut *mut c_void) {
    let apo = as_apo(this);
    let vtbl = unsafe { *(apo.i_apo_rt as *const *const usize) };
    let f: unsafe extern "system" fn(*mut c_void, u32, *const *const c_void, u32, *mut *mut c_void) = unsafe { std::mem::transmute(*vtbl.add(3)) };
    unsafe { f(apo.i_apo_rt, nin, pin, nout, pout) }
}

unsafe extern "system" fn na_calc_input(this: *mut c_void, f: u32) -> u32 {
    let apo = as_apo(this);
    let vtbl = unsafe { *(apo.i_apo_rt as *const *const usize) };
    let fn_: unsafe extern "system" fn(*mut c_void, u32) -> u32 = unsafe { std::mem::transmute(*vtbl.add(4)) };
    unsafe { fn_(apo.i_apo_rt, f) }
}

unsafe extern "system" fn na_calc_output(this: *mut c_void, f: u32) -> u32 {
    let apo = as_apo(this);
    let vtbl = unsafe { *(apo.i_apo_rt as *const *const usize) };
    let fn_: unsafe extern "system" fn(*mut c_void, u32) -> u32 = unsafe { std::mem::transmute(*vtbl.add(5)) };
    unsafe { fn_(apo.i_apo_rt, f) }
}

// IAudioProcessingObjectConfiguration 转发。
unsafe extern "system" fn na_lock(this: *mut c_void, nin: u32, pin: *const *const c_void, nout: u32, pout: *const *const c_void) -> HRESULT {
    let apo = as_apo(this);
    let vtbl = unsafe { *(apo.i_cfg as *const *const usize) };
    let f: unsafe extern "system" fn(*mut c_void, u32, *const *const c_void, u32, *const *const c_void) -> HRESULT = unsafe { std::mem::transmute(*vtbl.add(3)) };
    unsafe { f(apo.i_cfg, nin, pin, nout, pout) }
}

unsafe extern "system" fn na_unlock(this: *mut c_void) -> HRESULT {
    let apo = as_apo(this);
    let vtbl = unsafe { *(apo.i_cfg as *const *const usize) };
    let f: unsafe extern "system" fn(*mut c_void) -> HRESULT = unsafe { std::mem::transmute(*vtbl.add(4)) };
    unsafe { f(apo.i_cfg) }
}

// ── 静态 vtable ─────────────────────────────────────────────────
static IAPO_VTBL: IapoVtbl = IapoVtbl {
    qi: na_qi,
    addref: na_addref,
    release: na_release,
    reset: na_reset,
    get_latency: na_get_latency,
    get_reg_props: na_get_reg_props,
    initialize: na_initialize,
    is_input_fmt: na_is_input_fmt,
    is_output_fmt: na_is_output_fmt,
    get_input_channels: na_get_input_channels,
};

static IAPO_RT_VTBL: IapoRtVtbl = IapoRtVtbl {
    qi: na_qi,
    addref: na_addref,
    release: na_release,
    apo_process: na_apo_process,
    calc_input: na_calc_input,
    calc_output: na_calc_output,
};

static IAPO_CFG_VTBL: IapoCfgVtbl = IapoCfgVtbl {
    qi: na_qi,
    addref: na_addref,
    release: na_release,
    lock_for_process: na_lock,
    unlock_for_process: na_unlock,
};

static IAPO_ASE_VTBL: IapoAseVtbl = IapoAseVtbl {
    qi: na_qi,
    addref: na_addref,
    release: na_release,
};

// ── 创建入口（供 ClassFactory 调用）───────────────────────────
///
/// 创建聚合 APO 对象（手写 vtable NApo，NonDelegating 内层）。
/// QI(IUnknown)→自身；QI(接口)→内部 ApoObject；AddRef/Release→自维护。
///
/// # Safety
/// `clsid` 必须是 VxAPO PreMix/PostMix；`p_unk_outer` 合法或 null（当前仅记录不使用）。
pub unsafe fn create_aggregate(p_unk_outer: *mut c_void, clsid: GUID) -> *mut c_void {
    // 1. 创建 ApoObject（复用全部 DSP 逻辑）。
    let apo = ApoObject::new(clsid);
    let unknown: IUnknown = apo.into();

    // 2. QI 出 4 个接口指针（缓存供 NApo 转发）。
    let raw = std::mem::transmute_copy::<IUnknown, *mut c_void>(&unknown);
    let vtbl = *(raw as *const *const usize);
    let qi: QiFn = std::mem::transmute(*vtbl);

    let iapoid = windows::Win32::Media::Audio::Apo::IAudioProcessingObject::IID;
    let rtid = windows::Win32::Media::Audio::Apo::IAudioProcessingObjectRT::IID;
    let cfgid = windows::Win32::Media::Audio::Apo::IAudioProcessingObjectConfiguration::IID;
    let aseid = windows::Win32::Media::Audio::Apo::IAudioSystemEffects::IID;

    let mut i_apo: *mut c_void = std::ptr::null_mut();
    let hr_apo = qi(raw, &iapoid, &mut i_apo);
    let mut i_apo_rt: *mut c_void = std::ptr::null_mut();
    let hr_rt = qi(raw, &rtid, &mut i_apo_rt);
    let mut i_cfg: *mut c_void = std::ptr::null_mut();
    let hr_cfg = qi(raw, &cfgid, &mut i_cfg);
    let mut i_ase: *mut c_void = std::ptr::null_mut();
    let hr_ase = qi(raw, &aseid, &mut i_ase);

    if hr_apo.0 != 0 || hr_rt.0 != 0 || hr_cfg.0 != 0 || hr_ase.0 != 0 {
        let rel = |p: *mut c_void| {
            if !p.is_null() {
                let v = *(p as *const *const usize);
                let r: RefFn = std::mem::transmute(*v.add(2));
                r(p);
            }
        };
        rel(i_apo);
        rel(i_apo_rt);
        rel(i_cfg);
        rel(i_ase);
        drop(unknown);
        return std::ptr::null_mut();
    }

    // 3. 构造 NApo（内部接口引用已 +1，转 NApo 管理）。
    let apo_box = Box::new(NApo {
        iapovtbl: &IAPO_VTBL,
        p_unk_outer,
        cref: AtomicU32::new(1),
        i_apo,
        i_apo_rt,
        i_cfg,
        i_ase,
    });

    // 4. 释放 unknown 的临时引用（内部接口由 NApo 持有引用）。
    drop(unknown);

    // 5. 返回指向对象首字段（vtable 指针）的地址 = COM 接口指针。
    Box::into_raw(apo_box) as *mut NApo as *mut c_void
}

/// 释放聚合对象（CreateInstance QI 失败时清理用）。
///
/// # Safety
/// `obj` 必须是 create_aggregate 返回的有效指针或 null。
pub unsafe fn release_aggregate(obj: *mut c_void) {
    if !obj.is_null() {
        na_release(obj);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::vx_reg_props::CLSID_VXAPO_PRE_MIX;

    #[test]
    fn non_aggregated_qi_unknown_succeeds() {
        let obj = unsafe { create_aggregate(std::ptr::null_mut(), CLSID_VXAPO_PRE_MIX) };
        assert!(!obj.is_null());

        let vtbl = unsafe { *(obj as *const *const usize) };
        let qi: QiFn = unsafe { std::mem::transmute(*vtbl) };
        let mut ppv: *mut c_void = std::ptr::null_mut();
        let hr = unsafe { qi(obj, &IUnknown::IID as *const GUID, &mut ppv) };
        assert_eq!(hr.0, 0);
        assert!(!ppv.is_null());
        unsafe { na_release(obj) };
    }
}