//! object/apo/init.rs — Initialize / 注册属性逻辑
//!
//! 职责：从 APOInitSystemEffects 提取端点 GUID、创建子 APO、解析 per-device 配置路径，
//! 以及按 CLSID 返回注册属性（GetRegistrationProperties）。
//! 不包含 COM 接口方法本体，由 `apo.rs` 的 trait 实现转发调用。
use windows::core::Result;

use super::ApoObject_Impl;
use super::child::ChildApo;
use super::config::{extract_endpoint_guid, resolve_config_path};
use super::state::ApoState;
use crate::install::device::slots::{ChildApoKind, read_child_apo_guid};
use crate::object::vx_reg_props::{
    CLSID_VXAPO_PRE_MIX, REG_PROPS_POST_MIX, REG_PROPS_PRE_MIX,
};
use crate::sys::com::apo_types::{APOInitSystemEffects, APO_REG_PROPERTIES};
use crate::sys::com::prelude::{CoTaskMemAlloc, E_OUTOFMEMORY, HRESULT, guid_to_string};

/// `Initialize`：解析端点 GUID、创建子 APO、确定 per-device 配置路径。
///
/// 数据非法时仍初始化成功并降级默认配置，不阻断 APO 加载（object 7.1.8）。
pub(crate) fn initialize(apo: &ApoObject_Impl, cb_data_size: u32, pby_data: *const u8) -> Result<()> {
    // ---- P0-7 无声诊断探针 5（2026-08-04，debug 门控，排查完删除）----
    // Initialize 被调与否：引擎拿到 IAudioProcessingObject 后先调 Initialize。
    // 之前只有 lock/apoprocess 探针，Initialize 失败被拒时 lock 自然不触发——补上。
    #[cfg(debug_assertions)]
    {
        let _ = std::fs::write(
            r"C:\ProgramData\VxAPO\initialize_probe.txt",
            format!("Initialize clsid={:?} cb={}\n", apo.clsid, cb_data_size),
        );
    }

    // 1. 参数校验：pby_data 非空、cb_data_size 足以容纳 APOInitSystemEffects
    //    （SDK 约定：Initialize 的 pby_data 指向完整的 APOInitSystemEffects）。
    let valid_init_data = !pby_data.is_null()
        && cb_data_size >= std::mem::size_of::<APOInitSystemEffects>() as u32;

    // 2. 状态转换 Created → Initialized，失败 → 对应 HRESULT。
    apo.state_cell
        .transition(ApoState::Created, ApoState::Initialized)
        .map_err(|e| windows::core::Error::from(HRESULT::from(e)))?;

    // 3. 解析 APOInitSystemEffects → 端点 GUID + 子 APO（object 7.1.8 v8.4）。
    //    Safety: pby_data 已验证非空 + 尺寸足够；APOInitSystemEffects 为 repr(C) 结构。
    let endpoint_guid = if valid_init_data {
        let init = unsafe { &*(pby_data as *const APOInitSystemEffects) };
        extract_endpoint_guid(init)
    } else {
        None
    };

    // 4. 子 APO 创建（P0-6 v8.4：vendor 安装信息区读取，失败降级为无子 APO，Note 57）。
    //    - GUID 来源：端点 GUID + 安装信息区 PreMixChild/PostMixChild 值
    //    - 空/特殊 GUID、create 失败 → None（不阻塞 Initialize）
    let child = match endpoint_guid {
        Some(eg) => {
            let eg_str = guid_to_string(&eg);
            let premix = read_child_apo_guid(&eg_str, ChildApoKind::PreMix);
            let postmix = read_child_apo_guid(&eg_str, ChildApoKind::PostMix);
            premix.or(postmix).and_then(|c| {
                // SAFETY: COM 已初始化（宿主进程 audiodg）；c 为有效 APO CLSID。
                unsafe { ChildApo::create(&c) }.ok()
            })
        }
        None => None,
    };
    *apo.child_apo.lock().unwrap() = child;

    // 5. per-device 配置路径（object 7.1.8）。
    let path = if valid_init_data {
        let init = unsafe { &*(pby_data as *const APOInitSystemEffects) };
        resolve_config_path(Some(init))
    } else {
        log::warn!(
            "Initialize: invalid init data (ptr null = {}, size {} < {}) — using default config",
            pby_data.is_null(),
            cb_data_size,
            std::mem::size_of::<APOInitSystemEffects>()
        );
        resolve_config_path(None)
    };
    *apo.config_path.lock().unwrap() = path;

    Ok(())
}

/// `GetRegistrationProperties`：按 CLSID 选择注册属性，CoTaskMemAlloc 拷贝返回。
///
/// 调用方负责最终 `CoTaskMemFree`。
pub(crate) fn get_registration_properties(apo: &ApoObject_Impl) -> Result<*mut APO_REG_PROPERTIES> {
    let prop = if apo.clsid == CLSID_VXAPO_PRE_MIX {
        &REG_PROPS_PRE_MIX
    } else {
        &REG_PROPS_POST_MIX
    };
    let size = std::mem::size_of::<APO_REG_PROPERTIES>();
    // 分配并对齐（alignment_of<APO_REG_PROPERTIES>）。
    let alloc = unsafe { CoTaskMemAlloc(size) };
    if alloc.is_null() {
        return Err(windows::core::Error::from(E_OUTOFMEMORY));
    }
    // Safety: alloc 由 CoTaskMemAlloc 分配且尺寸/对齐满足 APO_REG_PROPERTIES。
    unsafe {
        std::ptr::write(alloc as *mut APO_REG_PROPERTIES, *prop);
    }
    Ok(alloc as *mut APO_REG_PROPERTIES)
}
