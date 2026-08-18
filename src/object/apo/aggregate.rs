//! object/apo/aggregate.rs — COM 聚合委托外壳（无声根因修复）
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

use crate::object::apo::ApoObject;
use crate::sys::com::apo_interfaces::{
    IID_IAPO, IID_IAPO_CONFIG, IID_IAPO_RT, IID_IAUDIO_SYSTEM_EFFECTS,
};
use crate::sys::com::prelude::{E_NOINTERFACE, E_POINTER, GUID, HRESULT, Interface, IUnknown, S_OK};

/// 聚合实例生命周期计数（诊断）：`create_aggregate` 成功 +1，
/// `na_release` 归零析构 -1。用于验证“audiodg 实例是否泄漏”
/// （实测：设置页卡顿在重启后消失，怀疑实例未释放累积）。
pub(crate) static AGG_CREATED: AtomicU32 = AtomicU32::new(0);
pub(crate) static AGG_DESTROYED: AtomicU32 = AtomicU32::new(0);

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
#[repr(C)]
struct IUnknownVtbl {
    qi: QiFn,
    addref: RefFn,
    release: RefFn,
}

// 常量（x64 指针 8 字节）：
const OFF_RT: usize = 8;
const OFF_CFG: usize = 16;
const OFF_ASE: usize = 24;
/// 非委托 IUnknown 视图（EAPO INonDelegatingUnknown 子对象，offset 32）。
/// CreateInstance 返回此视图——引擎 QI(IAPO) 走此视图的 NonDelegatingQI。
const OFF_ND_UNKNOWN: usize = 32;

// 编译期护栏（，审查 #11）：偏移常量按 x64（8 字节指针）硬编码，
// 32 位构建直接编译失败，不得带错误布局进入链接/运行。
const _: () = assert!(core::mem::size_of::<*const ()>() == 8);

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
    /// 非委托 IUnknown 视图（offset 32，EAPO INonDelegatingUnknown）。
    /// 槽 0-2 = NonDelegatingQI/AddRef/Release（自维护 + 直接暴露 inner 接口）。
    /// **CreateInstance 聚合/非聚合统一返回此视图**（EAPO ClassFactory.cpp:73 语义）。
    vtbl_nondeg_unknown: *const IUnknownVtbl,
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

/// 从任意接口视图还原 NApo 基址。
unsafe fn base_from_this(this: *mut c_void) -> *mut NApo {
    // SAFETY: 调用方保证 this 指向 NApo 内某个视图字段地址。
    //
    // 不变式（文档化）：RT/CFG/ASE 三个接口视图的 stub 一律先经
    // `base_from_iface` 回退到基址，再进 `na_*`；IAPO 视图 offset 为 0，
    // `this == base`。因此运行时唯一可能走到本函数的“非基址视图”只有
    // `vtbl_nondeg_unknown`（offset 32）——用 vtable 静态地址唯一性区分。
    // 新增直接以 RT/CFG/ASE 视图调用 `na_qi` 的路径前，必须重新论证此不变式。
    let this_vtbl = unsafe { *(this as *const *const usize) };
    if this_vtbl as usize == &ND_UNKNOWN_VTBL as *const _ as usize {
        base_from_iface(this, OFF_ND_UNKNOWN)
    } else {
        this as *mut NApo
    }
}

// ── Delegating IUnknown（聚合语义核心）───────────────────────
// 引擎通过 IAPO/RT/CFG/ASE 接口指针调 QI/AddRef/Release 时，走的是 **delegating 版本**
// （委托 pUnkOuter）——引擎 IAPO->QI(IUnknown) 必须返回 outer IUnknown（聚合身份），
// 身份检查才通过（否则判接口来自不同对象 → 弃用 → 方法零调用）。
// NonDelegating（na_qi/na_addref/na_release）保留为 inner 真实实现，供 outer 经特殊路径调用。

/// delegating QI 核心：非聚合 → na_qi；聚合 → 委托 outer->QI。
unsafe fn delegate_qi_at(base: *mut NApo, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT {
    let apo = as_apo(base);
    if apo.p_unk_outer.is_null() {
        return na_qi(base as *mut c_void, riid, ppv);
    }
    // ★ 委托 outer——引擎身份检查通过（IAPO->QI(IUnknown） = outer IUnknown)
    // SAFETY: COM 聚合契约保证 p_unk_outer 是有效 IUnknown 实现（引擎外壳，
    // 生命周期由引擎管理）；vtable 槽 0 为该对象的 QI。
    let outer_vtbl = unsafe { *(apo.p_unk_outer as *const *const usize) };
    let qi: QiFn = unsafe { std::mem::transmute(*outer_vtbl) };
    unsafe { qi(apo.p_unk_outer, riid, ppv) }
}

/// delegating AddRef：非聚合 → na_addref；聚合 → 委托 outer->AddRef。
unsafe fn delegate_addref_at(base: *mut NApo) -> u32 {
    let apo = as_apo(base);
    if apo.p_unk_outer.is_null() {
        return na_addref(base as *mut c_void);
    }
    // SAFETY: 同 delegate_qi_at——p_unk_outer 为有效聚合外壳，槽 1 为 AddRef。
    let outer_vtbl = unsafe { *(apo.p_unk_outer as *const *const usize) };
    let addref: RefFn = unsafe { std::mem::transmute(*outer_vtbl.add(1)) };
    unsafe { addref(apo.p_unk_outer) }
}

/// delegating Release：非聚合 → na_release；聚合 → 委托 outer->Release。
unsafe fn delegate_release_at(base: *mut NApo) -> u32 {
    let apo = as_apo(base);
    if apo.p_unk_outer.is_null() {
        return na_release(base as *mut c_void);
    }
    // SAFETY: 同 delegate_qi_at——p_unk_outer 为有效聚合外壳，槽 2 为 Release。
    let outer_vtbl = unsafe { *(apo.p_unk_outer as *const *const usize) };
    let release: RefFn = unsafe { std::mem::transmute(*outer_vtbl.add(2)) };
    unsafe { release(apo.p_unk_outer) }
}

// IAPO 接口 ptr = base+0（offset 0，this 即 base）。
unsafe extern "system" fn apo_dl_qi(this: *mut c_void, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT {
    delegate_qi_at(this as *mut NApo, riid, ppv)
}
unsafe extern "system" fn apo_dl_addref(this: *mut c_void) -> u32 {
    delegate_addref_at(this as *mut NApo)
}
unsafe extern "system" fn apo_dl_release(this: *mut c_void) -> u32 {
    delegate_release_at(this as *mut NApo)
}

// IAPO_RT 接口 ptr = base+8，需回退。
unsafe extern "system" fn rt_dl_qi(this: *mut c_void, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT {
    delegate_qi_at(base_from_iface(this, OFF_RT), riid, ppv)
}
unsafe extern "system" fn rt_dl_addref(this: *mut c_void) -> u32 {
    delegate_addref_at(base_from_iface(this, OFF_RT))
}
unsafe extern "system" fn rt_dl_release(this: *mut c_void) -> u32 {
    delegate_release_at(base_from_iface(this, OFF_RT))
}

// IAPO_CFG 接口 ptr = base+16，需回退。
unsafe extern "system" fn cfg_dl_qi(this: *mut c_void, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT {
    delegate_qi_at(base_from_iface(this, OFF_CFG), riid, ppv)
}
unsafe extern "system" fn cfg_dl_addref(this: *mut c_void) -> u32 {
    delegate_addref_at(base_from_iface(this, OFF_CFG))
}
unsafe extern "system" fn cfg_dl_release(this: *mut c_void) -> u32 {
    delegate_release_at(base_from_iface(this, OFF_CFG))
}

// IAPO_ASE 接口 ptr = base+24，需回退。
unsafe extern "system" fn ase_dl_qi(this: *mut c_void, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT {
    delegate_qi_at(base_from_iface(this, OFF_ASE), riid, ppv)
}
unsafe extern "system" fn ase_dl_addref(this: *mut c_void) -> u32 {
    delegate_addref_at(base_from_iface(this, OFF_ASE))
}
unsafe extern "system" fn ase_dl_release(this: *mut c_void) -> u32 {
    delegate_release_at(base_from_iface(this, OFF_ASE))
}

// ── NonDelegatingQI（EAPO:519-538）───────────────────────────
// this 可能是任意接口视图（非委托 IUnknown 视图 base+32 / IAPO base / RT base+8 …），
// 统一按指向的 vtable 判断当前这是哪个视图，还原 base 再暴露接口。
unsafe extern "system" fn na_qi(this: *mut c_void, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT {
    if riid.is_null() || ppv.is_null() {
        return E_POINTER;
    }
    unsafe { *ppv = std::ptr::null_mut() };
    let iid = unsafe { *riid };

    // 判断当前视图：this 指向的 vtable 地址 = ND_UNKNOWN_VTBL → 非委托 IUnknown 视图（offset 32）。
    // 否则视为对象基址（offset 0，非聚合直接对对象调 QI 的路径）。
    // SAFETY: this 必须是 NApo 内某视图字段地址（create_aggregate 返回的偏移指针）。
    let base = base_from_this(this);
    let apo = as_apo(base);

    // QI(IUnknown) → 返回非委托 IUnknown 视图（EAPO:521-522，NonDelegatingUnknown 身份）。
    // ★ 返回 `&vtbl_nondeg_unknown`（base+32），不是对象基址（base）——后者是 IAPO 委托视图，
    // 引擎对它的 QI 走委托 outer → 外壳不认 → 弃用零方法（实测根因）。
    if iid == IUnknown::IID {
        let nd_view = &raw const apo.vtbl_nondeg_unknown as *const IUnknownVtbl as *mut c_void;
        unsafe { *ppv = nd_view };
        unsafe { na_addref(nd_view) }; // NonDAddRef（自维护，base_from_this 自动回退）
        return S_OK;
    }

    // 其余接口 → 返回 NApo 对应 vtable 字段地址（多接口 offset）。
    let target: *mut c_void = if iid == IID_IAPO {
        &raw const apo.vtbl_apo as *const IapoVtbl as *mut c_void
    } else if iid == IID_IAPO_RT {
        &raw const apo.vtbl_rt as *const IapoRtVtbl as *mut c_void
    } else if iid == IID_IAPO_CONFIG {
        &raw const apo.vtbl_cfg as *const IapoCfgVtbl as *mut c_void
    } else if iid == IID_IAUDIO_SYSTEM_EFFECTS {
        &raw const apo.vtbl_ase as *const IapoAseVtbl as *mut c_void
    } else {
        return E_NOINTERFACE;
    };
    unsafe { *ppv = target };
    // EAPO:519-538 对齐——QI 成功后调用「返回视图」的 AddRef：
    // IUnknown 视图 = NonDAddRef；IAPO/RT/CFG/ASE 视图 = 该视图自己的 AddRef
    // （聚合时委托 outer->AddRef，非聚合时 na_addref）。
    unsafe {
        let vtbl = *(target as *const *const usize);
        let addref: RefFn = std::mem::transmute(*vtbl.add(1));
        addref(target);
    }
    S_OK
}

// ── NonDelegatingAddRef/Release（EAPO:541-555，自维护）────────
unsafe extern "system" fn na_addref(this: *mut c_void) -> u32 {
    let apo = as_apo(base_from_this(this));
    apo.cref.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn na_release(this: *mut c_void) -> u32 {
    let base = base_from_this(this);
    let apo = as_apo(base);
    let r = apo.cref.fetch_sub(1, Ordering::Release) - 1;
    if r == 0 {
        AGG_DESTROYED.fetch_add(1, Ordering::Relaxed);
        std::sync::atomic::fence(Ordering::Acquire);
        let apo = as_apo(base);
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
        drop(Box::from_raw(base));
    }
    r
}

// ── vtable 转发宏（零成本：仅展开为既有的直接 vtable 调用） ──────
macro_rules! forward_method {
    ($name:ident, $base:expr, $inner:ident, $slot:expr, $ret:ty,
     ($($param:ident: $pty:ty),*), ($($arg:ident),*)) => {
        unsafe extern "system" fn $name(this: *mut c_void $(, $param: $pty)*) -> $ret {
            let base = $base(this);
            let vtbl = unsafe { *(base.$inner as *const *const usize) };
            let f: unsafe extern "system" fn(*mut c_void $(, $pty)*) -> $ret =
                unsafe { std::mem::transmute(*vtbl.add($slot)) };
            unsafe { f(base.$inner $(, $arg)*) }
        }
    };
}

// ── IAPO vtable stub（offset 0 = 基址） ─────────────────────────
forward_method!(na_reset, |this| as_apo(this as *mut NApo), i_apo, 3, HRESULT, (), ());
forward_method!(na_get_latency, |this| as_apo(this as *mut NApo), i_apo, 4, HRESULT, (out: *mut i64), (out));
forward_method!(na_get_reg_props, |this| as_apo(this as *mut NApo), i_apo, 5, HRESULT, (out: *mut *mut c_void), (out));
forward_method!(na_initialize, |this| as_apo(this as *mut NApo), i_apo, 6, HRESULT, (cb: u32, data: *const u8), (cb, data));
forward_method!(na_is_input_fmt, |this| as_apo(this as *mut NApo), i_apo, 7, HRESULT, (a: *mut c_void, b: *mut c_void, out: *mut *mut c_void), (a, b, out));
forward_method!(na_is_output_fmt, |this| as_apo(this as *mut NApo), i_apo, 8, HRESULT, (a: *mut c_void, b: *mut c_void, out: *mut *mut c_void), (a, b, out));
forward_method!(na_get_input_channels, |this| as_apo(this as *mut NApo), i_apo, 9, HRESULT, (out: *mut u32), (out));

// ── IAPO_RT vtable stub（offset 8，需回退） ────────────────────
forward_method!(rt_apo_process, |this| as_apo(base_from_iface(this, OFF_RT)), i_apo_rt, 3, (), (nin: u32, pin: *const *const c_void, nout: u32, pout: *mut *mut c_void), (nin, pin, nout, pout));
forward_method!(rt_calc_input, |this| as_apo(base_from_iface(this, OFF_RT)), i_apo_rt, 4, u32, (f: u32), (f));
forward_method!(rt_calc_output, |this| as_apo(base_from_iface(this, OFF_RT)), i_apo_rt, 5, u32, (f: u32), (f));

// ── IAPO_CFG vtable stub（offset 16） ───────────────────────────
forward_method!(cfg_lock, |this| as_apo(base_from_iface(this, OFF_CFG)), i_cfg, 3, HRESULT, (nin: u32, pin: *const *const c_void, nout: u32, pout: *const *const c_void), (nin, pin, nout, pout));
forward_method!(cfg_unlock, |this| as_apo(base_from_iface(this, OFF_CFG)), i_cfg, 4, HRESULT, (), ());

// ── IAPO_ASE vtable stub（offset 24） ───────────────────────────
// IUnknown 槽 0-2 = delegating。

/// 非委托 IUnknown vtable（EAPO INonDelegatingUnknown）：槽 0-2 = na_qi/na_addref/na_release
/// （自维护 + 直接暴露 inner 接口）。CreateInstance 返回此视图，引擎对它的 QI 走 NonDQI。
static ND_UNKNOWN_VTBL: IUnknownVtbl = IUnknownVtbl { qi: na_qi, addref: na_addref, release: na_release };
static IAPO_VTBL: IapoVtbl = IapoVtbl {
    qi: apo_dl_qi,
    addref: apo_dl_addref,
    release: apo_dl_release,
    reset: na_reset,
    get_latency: na_get_latency,
    get_reg_props: na_get_reg_props,
    initialize: na_initialize,
    is_input_fmt: na_is_input_fmt,
    is_output_fmt: na_is_output_fmt,
    get_input_channels: na_get_input_channels,
};

static IAPO_RT_VTBL: IapoRtVtbl = IapoRtVtbl {
    qi: rt_dl_qi,
    addref: rt_dl_addref,
    release: rt_dl_release,
    apo_process: rt_apo_process,
    calc_input: rt_calc_input,
    calc_output: rt_calc_output,
};

static IAPO_CFG_VTBL: IapoCfgVtbl = IapoCfgVtbl {
    qi: cfg_dl_qi,
    addref: cfg_dl_addref,
    release: cfg_dl_release,
    lock_for_process: cfg_lock,
    unlock_for_process: cfg_unlock,
};

static IAPO_ASE_VTBL: IapoAseVtbl = IapoAseVtbl {
    qi: ase_dl_qi,
    addref: ase_dl_addref,
    release: ase_dl_release,
};

// ── 创建入口 ────────────────────────────────────────────────────
///
/// 创建聚合 APO 对象（多接口 offset 布局）。
/// QI(接口) → 返回对应 vtable 字段地址；IUnknown → 对象基址；AddRef/Release 自维护。
///
/// # Safety
/// - `clsid` 必须是 VxAPO PreMix/PostMix。
/// - `p_unk_outer` 必须为 null 或指向有效 IUnknown 聚合外壳（COM 聚合契约，
///   生命周期由调用方/引擎管理；NApo 仅借用指针，不 AddRef/Release outer）。
pub unsafe fn create_aggregate(p_unk_outer: *mut c_void, clsid: GUID) -> *mut c_void {
    // 1. 创建 ApoObject（复用全部 DSP 逻辑）。
    let apo = ApoObject::new(clsid);
    let unknown: IUnknown = apo.into();

    // 2. QI 出 4 个内部接口指针（缓存供转发）。
    let raw = std::mem::transmute_copy::<IUnknown, *mut c_void>(&unknown);
    let vtbl = *(raw as *const *const usize);
    let qi: QiFn = std::mem::transmute(*vtbl);

    let iapoid = IID_IAPO;
    let rtid = IID_IAPO_RT;
    let cfgid = IID_IAPO_CONFIG;
    let aseid = IID_IAUDIO_SYSTEM_EFFECTS;

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
        vtbl_nondeg_unknown: &ND_UNKNOWN_VTBL,
        p_unk_outer,
        cref: AtomicU32::new(1),
        i_apo,
        i_apo_rt,
        i_cfg,
        i_ase,
    });

    // 4. 释放 unknown 临时引用（内部接口由 NApo 持有引用）。
    drop(unknown);

    // 5. ★ 返回**非委托 IUnknown 视图**（offset 32，EAPO ClassFactory.cpp:73 语义）——
    //    引擎对返回值调 QI(IAPO) → na_qi（NonDQI，不委托）→ 直接返回 NApo 的 IAPO 视图。
    //    （旧实现返回基址 = IAPO 视图（委托 QI）→ 引擎 QI 走 outer→ 外壳不认 → 弃用零方法）
    let base = Box::into_raw(apo_box) as *mut NApo;
    AGG_CREATED.fetch_add(1, Ordering::Relaxed);
    (base as usize + OFF_ND_UNKNOWN) as *mut c_void
}

/// 释放聚合对象（CreateInstance QI 失败时清理用）。
///
/// # Safety
/// `obj` 必须是 create_aggregate 返回的有效指针或 null。
pub unsafe fn release_aggregate(obj: *mut c_void) {
    if !obj.is_null() {
        // na_release 现在能自动识别非委托 IUnknown 视图并回退基址。
        na_release(obj);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::vx_reg_props::CLSID_VXAPO_PRE_MIX;

    #[test]
    fn non_aggregated_qi_unknown_succeeds() {
        // obj 现在是「非委托 IUnknown 视图」（base+OFF_ND_UNKNOWN），与 EAPO 一致。
        let obj = unsafe { create_aggregate(std::ptr::null_mut(), CLSID_VXAPO_PRE_MIX) };
        assert!(!obj.is_null());

        let vtbl = unsafe { *(obj as *const *const usize) };
        let qi: QiFn = unsafe { std::mem::transmute(*vtbl) };
        let mut ppv: *mut c_void = std::ptr::null_mut();
        let hr = unsafe { qi(obj, &IUnknown::IID as *const GUID, &mut ppv) };
        assert_eq!(hr.0, 0);
        // QI(IUnknown) 应返回非委托视图自身（base+OFF_ND_UNKNOWN），且等于 obj。
        assert_eq!(ppv as usize, obj as usize);
        unsafe { release_aggregate(obj) };
    }

    #[test]
    fn qi_rt_returns_rt_interface() {
        let obj = unsafe { create_aggregate(std::ptr::null_mut(), CLSID_VXAPO_PRE_MIX) };
        assert!(!obj.is_null());

        let vtbl = unsafe { *(obj as *const *const usize) };
        let qi: QiFn = unsafe { std::mem::transmute(*vtbl) };
        let rtid = IID_IAPO_RT;
        let mut rt: *mut c_void = std::ptr::null_mut();
        let hr = unsafe { qi(obj, &rtid, &mut rt) };
        assert_eq!(hr.0, 0);
        assert!(!rt.is_null());
        // RT 接口指针应等于 NApo + OFF_RT（多接口偏移）；obj = base+OFF_ND_UNKNOWN。
        let base = base_from_iface(obj, OFF_ND_UNKNOWN);
        assert_eq!((rt as usize) - (base as usize), OFF_RT);
        unsafe { release_aggregate(obj) };
    }

    #[test]
    fn qi_cfg_returns_cfg_interface() {
        let obj = unsafe { create_aggregate(std::ptr::null_mut(), CLSID_VXAPO_PRE_MIX) };
        assert!(!obj.is_null());

        let vtbl = unsafe { *(obj as *const *const usize) };
        let qi: QiFn = unsafe { std::mem::transmute(*vtbl) };
        let cfgid = IID_IAPO_CONFIG;
        let mut cfg: *mut c_void = std::ptr::null_mut();
        let hr = unsafe { qi(obj, &cfgid, &mut cfg) };
        assert_eq!(hr.0, 0);
        assert!(!cfg.is_null());
        let base = base_from_iface(obj, OFF_ND_UNKNOWN);
        assert_eq!((cfg as usize) - (base as usize), OFF_CFG);
        unsafe { release_aggregate(obj) };
    }

    #[test]
    fn qi_ase_returns_ase_interface() {
        let obj = unsafe { create_aggregate(std::ptr::null_mut(), CLSID_VXAPO_PRE_MIX) };
        assert!(!obj.is_null());

        let vtbl = unsafe { *(obj as *const *const usize) };
        let qi: QiFn = unsafe { std::mem::transmute(*vtbl) };
        let aseid = IID_IAUDIO_SYSTEM_EFFECTS;
        let mut ase: *mut c_void = std::ptr::null_mut();
        let hr = unsafe { qi(obj, &aseid, &mut ase) };
        assert_eq!(hr.0, 0);
        assert!(!ase.is_null());
        let base = base_from_iface(obj, OFF_ND_UNKNOWN);
        assert_eq!((ase as usize) - (base as usize), OFF_ASE);
        unsafe { release_aggregate(obj) };
    }

    #[test]
    fn nondelegating_addref_release_handles_offset_view() {
        let obj = unsafe { create_aggregate(std::ptr::null_mut(), CLSID_VXAPO_PRE_MIX) };
        assert!(!obj.is_null());

        let vtbl = unsafe { *(obj as *const *const usize) };
        let addref: RefFn = unsafe { std::mem::transmute(*vtbl.add(1)) };
        let release: RefFn = unsafe { std::mem::transmute(*vtbl.add(2)) };

        // obj 是非委托 IUnknown 视图（base+OFF_ND_UNKNOWN）；AddRef/Release 必须
        // 先回退到 NApo 基址再改 cref，否则会在错误偏移上读写。
        assert_eq!(unsafe { addref(obj) }, 2);
        assert_eq!(unsafe { release(obj) }, 1);

        unsafe { release_aggregate(obj) };
    }

    #[test]
    fn aggregated_qi_adds_outer_ref() {
        use std::sync::atomic::{AtomicU32, Ordering};

        #[repr(C)]
        struct Outer {
            vtbl: *const OuterVtbl,
            refs: AtomicU32,
            inner: *mut c_void,
        }
        #[repr(C)]
        struct OuterVtbl {
            qi: QiFn,
            addref: RefFn,
            release: RefFn,
        }

        unsafe extern "system" fn outer_qi(
            this: *mut c_void,
            riid: *const GUID,
            ppv: *mut *mut c_void,
        ) -> HRESULT {
            let outer = &mut *(this as *mut Outer);
            let iid = unsafe { *riid };
            if iid == IUnknown::IID {
                unsafe { *ppv = this };
                outer_addref(this);
                S_OK
            } else if outer.inner.is_null() {
                E_NOINTERFACE
            } else {
                let vtbl = unsafe { *(outer.inner as *const *const usize) };
                let qi: QiFn = unsafe { std::mem::transmute(*vtbl) };
                unsafe { qi(outer.inner, riid, ppv) }
            }
        }

        unsafe extern "system" fn outer_addref(this: *mut c_void) -> u32 {
            let outer = &*(this as *mut Outer);
            outer.refs.fetch_add(1, Ordering::SeqCst) + 1
        }

        unsafe extern "system" fn outer_release(this: *mut c_void) -> u32 {
            let outer = &*(this as *mut Outer);
            outer.refs.fetch_sub(1, Ordering::SeqCst) - 1
        }

        static OUTER_VTBL: OuterVtbl = OuterVtbl {
            qi: outer_qi,
            addref: outer_addref,
            release: outer_release,
        };

        let mut outer = Outer {
            vtbl: &OUTER_VTBL,
            refs: AtomicU32::new(1),
            inner: std::ptr::null_mut(),
        };
        let outer_ptr = (&mut outer as *mut Outer) as *mut c_void;

        // 模拟 CoCreateInstance(pUnkOuter)：inner 返回非委托 IUnknown 视图。
        let obj = unsafe { create_aggregate(outer_ptr, CLSID_VXAPO_PRE_MIX) };
        assert!(!obj.is_null());
        outer.inner = obj;

        let before = outer.refs.load(Ordering::SeqCst);
        let nd_vtbl = unsafe { *(obj as *const *const usize) };
        let nd_qi: QiFn = unsafe { std::mem::transmute(*nd_vtbl) };

        // 引擎对返回的 inner 调 QI(IAPO)：NonDQI 返回 IAPO 视图，AddRef 应委托 outer。
        let iapoid = IID_IAPO;
        let mut iao: *mut c_void = std::ptr::null_mut();
        let hr = unsafe { nd_qi(obj, &iapoid, &mut iao) };
        assert_eq!(hr.0, 0);
        assert!(!iao.is_null());
        assert_eq!(outer.refs.load(Ordering::SeqCst), before + 1);

        // 身份检查：IAPO->QI(IUnknown) 必须返回 outer（聚合身份）。
        let iao_vtbl = unsafe { *(iao as *const *const usize) };
        let iao_qi: QiFn = unsafe { std::mem::transmute(*iao_vtbl) };
        let mut unk: *mut c_void = std::ptr::null_mut();
        let hr2 = unsafe { iao_qi(iao, &IUnknown::IID as *const GUID, &mut unk) };
        assert_eq!(hr2.0, 0);
        assert_eq!(unk as usize, outer_ptr as usize);
        // QI(IUnknown) 的返回值也要 Release，否则 outer 计数会多 1。
        let unk_vtbl = unsafe { *(unk as *const *const usize) };
        let unk_release: RefFn = unsafe { std::mem::transmute(*unk_vtbl.add(2)) };
        unsafe { unk_release(unk) };

        // 释放 IAPO 视图：委托 outer->Release，计数回到 QI 前。
        let iao_release: RefFn = unsafe { std::mem::transmute(*iao_vtbl.add(2)) };
        unsafe { iao_release(iao) };
        assert_eq!(outer.refs.load(Ordering::SeqCst), before);

        unsafe { release_aggregate(obj) };
    }
}
