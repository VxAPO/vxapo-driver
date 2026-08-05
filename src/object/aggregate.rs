//! object/aggregate.rs — COM 聚合委托外壳（P0-7 无声根因修复）
//!
//! Windows 音频引擎**强制用聚合模式（pUnkOuter 非空）创建 APO**（探针实证 punkouter_null=false）。
//! windows-rs 0.62 `#[implement]` 生成的 IUnknown 自包含、**不委托**——引擎聚合创建后
//! 经外层链 QI(IAudioProcessingObject) 走不到 inner → 弃用对象 → 完全无声。
//!
//! 本文件实现 EAPO 聚合语义 + 多接口 offset 布局（EqualizerAPO.cpp / .h）：
//! - EAPO 用 C++ 多重继承：NonDelegatingQI 对 IAPO/RT/Config/ASE 返回**各自不同 this 偏移**
//! - 我们的初版把所有接口返回同一 this（首字段=IAPO vtable）→ 引擎 QI(RT) 拿到的指针
//!   vtable 错位（RT.APOProcess 错到 IAPO.GetLatency）→ 引擎判 vtable 无效 → 弃用+零方法
//! - 修复：NApo 布局改为 **4 个独立 vtable 指针字段**，QI 返回对应字段地址（标准 COM
//!   多接口偏移），stub 方法用 offset 从接口指针还原 NApo 基址
//! - AddRef/Release **自维护**（NonDelegating 语义，EAPO:541-555）
//! - 接口方法：转发到内部 `ApoObject`（复用全部 DSP 逻辑）

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::{GUID, HRESULT, IUnknown, Interface};

use crate::object::apo::ApoObject;

// HRESULT 常量。
const S_OK: HRESULT = HRESULT(0);
const E_NOINTERFACE: HRESULT = HRESULT(0x8000_4002u32 as i32);
const E_POINTER: HRESULT = HRESULT(0x8000_4003u32 as i32);

// ── vtable 槽位类型 ─────────────────────────────────────────────
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

// ── NApo 聚合对象（多接口 offset 布局） ───────────────────────
// repr(C) 字段顺序固定：vtbl_apo(0) / vtbl_rt(8) / vtbl_cfg(16) / vtbl_ase(24)。
// 每个接口指针 = 对应 vtable 字段地址；stub 用偏移还原基址（标准 COM 多接口 offset）。
// 常量（x64 指针 8 字节）：
const OFF_APO: usize = 0;
const OFF_RT: usize = 8;
const OFF_CFG: usize = 16;
const OFF_ASE: usize = 24;

#[repr(C)]
pub struct NApo {
    /// IAudioProcessingObject vtable 指针（offset 0 = 对象基址首字段）。
    vtbl_apo: *const IapoVtbl,
    /// IAudioProcessingObjectRT vtable 指针（offset 8）。
    vtbl_rt: *const IapoRtVtbl,
    /// IAudioProcessingObjectConfiguration vtable 指针（offset 16）。
    vtbl_cfg: *const IapoCfgVtbl,
    /// IAudioSystemEffects vtable 指针（offset 24）。
    vtbl_ase: *const IapoAseVtbl,
    /// 聚合外壳 IUnknown（NULL = 非聚合）。
    p_unk_outer: *mut c_void,
    /// 引用计数（自维护，NonDelegating 语义）。
    cref: AtomicU32,
    /// 内部 4 接口（ApoObject 创建后 QI 缓存，NApo Release 归零时释放）。
    i_apo: *mut c_void,
    i_apo_rt: *mut c_void,
    i_cfg: *mut c_void,
    i_ase: *mut c_void,
}

/// 从接口指针还原 NApo 基址（offset 回退）。
fn base_from_iface(iface: *mut c_void, offset: usize) -> *mut NApo {
    // SAFETY: 调用方保证 iface 是指向 NApo 内某字段的地址，回退 offset 得基址。
    ((iface as usize) - offset) as *mut NApo
}

fn as_apo<'a>(base: *mut NApo) -> &'a NApo {
    // SAFETY: 调用方保证 base 指向有效 NApo。
    unsafe { &*base }
}

unsafe fn inner_addref(inner: *mut c_void) -> u32 {
    let vtbl = unsafe { *(inner as *const *const usize) };
    let addref: RefFn = unsafe { std::mem::transmute(*vtbl.add(1)) };
    unsafe { addref(inner) }
}

// ── NonDelegatingQI（EAPO:519-538）───────────────────────────
unsafe extern "system" fn na_qi(this: *mut c_void, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT {
    // 本函数作为 4 个 vtable 的共用 stub——this 可能是任意接口指针，需还原基址。
    // 从 vtable 首字段推断当前接口：比较 this 指向的 vtable 地址。
    if riid.is_null() || ppv.is_null() {
        return E_POINTER;
    }
    unsafe { *ppv = std::ptr::null_mut() };
    let iid = unsafe { *riid };

    // 先还原基址：根据 this 指向的 vtable 常量判断属于哪个接口。
    // 简化：QI 只被调用时 this 通常是对象基址（IAPO）。若引擎对 RT/Config 指针调 QI，
    // 由各自 stub 转发到带偏移的 na_qi_inner——见下方各 stub 的调用方式。
    let base = this as *mut NApo;
    let apo = as_apo(base);

    // QI(IUnknown) 恒返回对象基址（NonDelegatingUnknown 身份）。
    if iid == IUnknown::IID {
        unsafe { *ppv = base as *mut c_void };
        unsafe { na_addref(base as *mut c_void) };
        return S_OK;
    }

    // 其余接口 → 返回 NApo 对应 vtable 字段地址（多接口 offset）。
    let target: *mut c_void = if iid == windows::Win32::Media::Audio::Apo::IAudioProcessingObject::IID {
        &raw const apo.vtbl_apo as *const IapoVtbl as *mut c_void
    } else if iid == windows::Win32::Media::Audio::Apo::IAudioProcessingObjectRT::IID {
        &raw const apo.vtbl_rt as *const IapoRtVtbl as *mut c_void
    } else if iid == windows::Win32::Media::Audio::Apo::IAudioProcessingObjectConfiguration::IID {
        &raw const apo.vtbl_cfg as *const IapoCfgVtbl as *mut c_void
    } else if iid == windows::Win32::Media::Audio::Apo::IAudioSystemEffects::IID {
        &raw const apo.vtbl_ase as *const IapoAseVtbl as *mut c_void
    } else {
        return E_NOINTERFACE;
    };
    unsafe { *ppv = target };
    unsafe { na_addref(base as *mut c_void) };
    S_OK
}

// ── NonDelegatingAddRef/Release（EAPO:541-555，自维护）────────
unsafe extern "system" fn na_addref(this: *mut c_void) -> u32 {
    let apo = as_apo(this as *mut NApo);
    apo.cref.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn na_release(this: *mut c_void) -> u32 {
    let apo = as_apo(this as *mut NApo);
    let r = apo.cref.fetch_sub(1, Ordering::Release) - 1;
    if r == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        let apo = as_apo(this as *mut NApo);
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

// ── IAPO vtable stub（offset 0 = 基址） ─────────────────────────
unsafe extern "system" fn na_reset(this: *mut c_void) -> HRESULT {
    let base = as_apo(this as *mut NApo);
    let vtbl = unsafe { *(base.i_apo as *const *const usize) };
    let f: unsafe extern "system" fn(*mut c_void) -> HRESULT = unsafe { std::mem::transmute(*vtbl.add(3)) };
    unsafe { f(base.i_apo) }
}

unsafe extern "system" fn na_get_latency(this: *mut c_void, out: *mut i64) -> HRESULT {
    let base = as_apo(this as *mut NApo);
    let vtbl = unsafe { *(base.i_apo as *const *const usize) };
    let f: unsafe extern "system" fn(*mut c_void, *mut i64) -> HRESULT = unsafe { std::mem::transmute(*vtbl.add(4)) };
    unsafe { f(base.i_apo, out) }
}

unsafe extern "system" fn na_get_reg_props(this: *mut c_void, out: *mut *mut c_void) -> HRESULT {
    let base = as_apo(this as *mut NApo);
    let vtbl = unsafe { *(base.i_apo as *const *const usize) };
    let f: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT = unsafe { std::mem::transmute(*vtbl.add(5)) };
    unsafe { f(base.i_apo, out) }
}

unsafe extern "system" fn na_initialize(this: *mut c_void, cb: u32, data: *const u8) -> HRESULT {
    let base = as_apo(this as *mut NApo);
    let vtbl = unsafe { *(base.i_apo as *const *const usize) };
    let f: unsafe extern "system" fn(*mut c_void, u32, *const u8) -> HRESULT = unsafe { std::mem::transmute(*vtbl.add(6)) };
    unsafe { f(base.i_apo, cb, data) }
}

unsafe extern "system" fn na_is_input_fmt(this: *mut c_void, a: *mut c_void, b: *mut c_void, out: *mut *mut c_void) -> HRESULT {
    let base = as_apo(this as *mut NApo);
    let vtbl = unsafe { *(base.i_apo as *const *const usize) };
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, *mut *mut c_void) -> HRESULT = unsafe { std::mem::transmute(*vtbl.add(7)) };
    unsafe { f(base.i_apo, a, b, out) }
}

unsafe extern "system" fn na_is_output_fmt(this: *mut c_void, a: *mut c_void, b: *mut c_void, out: *mut *mut c_void) -> HRESULT {
    let base = as_apo(this as *mut NApo);
    let vtbl = unsafe { *(base.i_apo as *const *const usize) };
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, *mut *mut c_void) -> HRESULT = unsafe { std::mem::transmute(*vtbl.add(8)) };
    unsafe { f(base.i_apo, a, b, out) }
}

unsafe extern "system" fn na_get_input_channels(this: *mut c_void, out: *mut u32) -> HRESULT {
    let base = as_apo(this as *mut NApo);
    let vtbl = unsafe { *(base.i_apo as *const *const usize) };
    let f: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT = unsafe { std::mem::transmute(*vtbl.add(9)) };
    unsafe { f(base.i_apo, out) }
}

// ── IAPO_RT vtable stub（offset 8，需回退） ────────────────────
// RT stub 的 this 指向 vtbl_rt 字段地址；回退 OFF_RT 得基址。
unsafe extern "system" fn rt_qi(this: *mut c_void, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT {
    let base = base_from_iface(this, OFF_RT) as *mut c_void;
    na_qi(base, riid, ppv)
}

unsafe extern "system" fn rt_addref(this: *mut c_void) -> u32 {
    let base = base_from_iface(this, OFF_RT) as *mut c_void;
    na_addref(base)
}

unsafe extern "system" fn rt_release(this: *mut c_void) -> u32 {
    let base = base_from_iface(this, OFF_RT) as *mut c_void;
    na_release(base)
}

unsafe extern "system" fn rt_apo_process(this: *mut c_void, nin: u32, pin: *const *const c_void, nout: u32, pout: *mut *mut c_void) {
    let base = as_apo(base_from_iface(this, OFF_RT));
    let vtbl = unsafe { *(base.i_apo_rt as *const *const usize) };
    let f: unsafe extern "system" fn(*mut c_void, u32, *const *const c_void, u32, *mut *mut c_void) = unsafe { std::mem::transmute(*vtbl.add(3)) };
    unsafe { f(base.i_apo_rt, nin, pin, nout, pout) }
}

unsafe extern "system" fn rt_calc_input(this: *mut c_void, f: u32) -> u32 {
    let base = as_apo(base_from_iface(this, OFF_RT));
    let vtbl = unsafe { *(base.i_apo_rt as *const *const usize) };
    let fn_: unsafe extern "system" fn(*mut c_void, u32) -> u32 = unsafe { std::mem::transmute(*vtbl.add(4)) };
    unsafe { fn_(base.i_apo_rt, f) }
}

unsafe extern "system" fn rt_calc_output(this: *mut c_void, f: u32) -> u32 {
    let base = as_apo(base_from_iface(this, OFF_RT));
    let vtbl = unsafe { *(base.i_apo_rt as *const *const usize) };
    let fn_: unsafe extern "system" fn(*mut c_void, u32) -> u32 = unsafe { std::mem::transmute(*vtbl.add(5)) };
    unsafe { fn_(base.i_apo_rt, f) }
}

// ── IAPO_CFG vtable stub（offset 16） ───────────────────────────
unsafe extern "system" fn cfg_qi(this: *mut c_void, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT {
    let base = base_from_iface(this, OFF_CFG) as *mut c_void;
    na_qi(base, riid, ppv)
}

unsafe extern "system" fn cfg_addref(this: *mut c_void) -> u32 {
    let base = base_from_iface(this, OFF_CFG) as *mut c_void;
    na_addref(base)
}

unsafe extern "system" fn cfg_release(this: *mut c_void) -> u32 {
    let base = base_from_iface(this, OFF_CFG) as *mut c_void;
    na_release(base)
}

unsafe extern "system" fn cfg_lock(this: *mut c_void, nin: u32, pin: *const *const c_void, nout: u32, pout: *const *const c_void) -> HRESULT {
    let base = as_apo(base_from_iface(this, OFF_CFG));
    let vtbl = unsafe { *(base.i_cfg as *const *const usize) };
    let f: unsafe extern "system" fn(*mut c_void, u32, *const *const c_void, u32, *const *const c_void) -> HRESULT = unsafe { std::mem::transmute(*vtbl.add(3)) };
    unsafe { f(base.i_cfg, nin, pin, nout, pout) }
}

unsafe extern "system" fn cfg_unlock(this: *mut c_void) -> HRESULT {
    let base = as_apo(base_from_iface(this, OFF_CFG));
    let vtbl = unsafe { *(base.i_cfg as *const *const usize) };
    let f: unsafe extern "system" fn(*mut c_void) -> HRESULT = unsafe { std::mem::transmute(*vtbl.add(4)) };
    unsafe { f(base.i_cfg) }
}

// ── IAPO_ASE vtable stub（offset 24） ───────────────────────────
unsafe extern "system" fn ase_qi(this: *mut c_void, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT {
    let base = base_from_iface(this, OFF_ASE) as *mut c_void;
    na_qi(base, riid, ppv)
}

unsafe extern "system" fn ase_addref(this: *mut c_void) -> u32 {
    let base = base_from_iface(this, OFF_ASE) as *mut c_void;
    na_addref(base)
}

unsafe extern "system" fn ase_release(this: *mut c_void) -> u32 {
    let base = base_from_iface(this, OFF_ASE) as *mut c_void;
    na_release(base)
}

// ── 静态 vtable（每接口独立） ──────────────────────────────────
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
    qi: rt_qi,
    addref: rt_addref,
    release: rt_release,
    apo_process: rt_apo_process,
    calc_input: rt_calc_input,
    calc_output: rt_calc_output,
};

static IAPO_CFG_VTBL: IapoCfgVtbl = IapoCfgVtbl {
    qi: cfg_qi,
    addref: cfg_addref,
    release: cfg_release,
    lock_for_process: cfg_lock,
    unlock_for_process: cfg_unlock,
};

static IAPO_ASE_VTBL: IapoAseVtbl = IapoAseVtbl {
    qi: ase_qi,
    addref: ase_addref,
    release: ase_release,
};

// ── 创建入口 ────────────────────────────────────────────────────
///
/// 创建聚合 APO 对象（多接口 offset 布局）。
/// QI(接口) → 返回对应 vtable 字段地址；IUnknown → 对象基址；AddRef/Release 自维护。
///
/// # Safety
/// `clsid` 必须是 VxAPO PreMix/PostMix。
pub unsafe fn create_aggregate(p_unk_outer: *mut c_void, clsid: GUID) -> *mut c_void {
    // 1. 创建 ApoObject（复用全部 DSP 逻辑）。
    let apo = ApoObject::new(clsid);
    let unknown: IUnknown = apo.into();

    // 2. QI 出 4 个内部接口指针（缓存供转发）。
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

    // 3. 构造 NApo（多接口 offset 布局）。
    let apo_box = Box::new(NApo {
        vtbl_apo: &IAPO_VTBL,
        vtbl_rt: &IAPO_RT_VTBL,
        vtbl_cfg: &IAPO_CFG_VTBL,
        vtbl_ase: &IAPO_ASE_VTBL,
        p_unk_outer,
        cref: AtomicU32::new(1),
        i_apo,
        i_apo_rt,
        i_cfg,
        i_ase,
    });

    // 4. 释放 unknown 临时引用（内部接口由 NApo 持有引用）。
    drop(unknown);

    // 5. 返回对象基址（首字段 = IAPO vtable 指针）。
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

    #[test]
    fn qi_rt_returns_rt_interface() {
        let obj = unsafe { create_aggregate(std::ptr::null_mut(), CLSID_VXAPO_PRE_MIX) };
        assert!(!obj.is_null());

        let vtbl = unsafe { *(obj as *const *const usize) };
        let qi: QiFn = unsafe { std::mem::transmute(*vtbl) };
        let rtid = windows::Win32::Media::Audio::Apo::IAudioProcessingObjectRT::IID;
        let mut rt: *mut c_void = std::ptr::null_mut();
        let hr = unsafe { qi(obj, &rtid, &mut rt) };
        assert_eq!(hr.0, 0);
        assert!(!rt.is_null());
        // RT 接口指针应等于 NApo + OFF_RT（多接口偏移）。
        assert_eq!((rt as usize) - (obj as usize), OFF_RT);
        unsafe { na_release(obj) };
    }

    #[test]
    fn qi_cfg_returns_cfg_interface() {
        let obj = unsafe { create_aggregate(std::ptr::null_mut(), CLSID_VXAPO_PRE_MIX) };
        assert!(!obj.is_null());

        let vtbl = unsafe { *(obj as *const *const usize) };
        let qi: QiFn = unsafe { std::mem::transmute(*vtbl) };
        let cfgid = windows::Win32::Media::Audio::Apo::IAudioProcessingObjectConfiguration::IID;
        let mut cfg: *mut c_void = std::ptr::null_mut();
        let hr = unsafe { qi(obj, &cfgid, &mut cfg) };
        assert_eq!(hr.0, 0);
        assert!(!cfg.is_null());
        assert_eq!((cfg as usize) - (obj as usize), OFF_CFG);
        unsafe { na_release(obj) };
    }

    #[test]
    fn qi_ase_returns_ase_interface() {
        let obj = unsafe { create_aggregate(std::ptr::null_mut(), CLSID_VXAPO_PRE_MIX) };
        assert!(!obj.is_null());

        let vtbl = unsafe { *(obj as *const *const usize) };
        let qi: QiFn = unsafe { std::mem::transmute(*vtbl) };
        let aseid = windows::Win32::Media::Audio::Apo::IAudioSystemEffects::IID;
        let mut ase: *mut c_void = std::ptr::null_mut();
        let hr = unsafe { qi(obj, &aseid, &mut ase) };
        assert_eq!(hr.0, 0);
        assert!(!ase.is_null());
        assert_eq!((ase as usize) - (obj as usize), OFF_ASE);
        unsafe { na_release(obj) };
    }
}