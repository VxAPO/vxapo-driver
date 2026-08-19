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
    // 1. 参数校验：pby_data 非空、cb_data_size 足以容纳 APOInitSystemEffects
    //    （SDK 约定：Initialize 的 pby_data 指向完整的 APOInitSystemEffects）。
    let valid_init_data = !pby_data.is_null()
        && cb_data_size >= std::mem::size_of::<APOInitSystemEffects>() as u32;

    // 2. 状态转换 Created → Initialized，失败 → 对应 HRESULT。
    apo.state_cell
        .transition(ApoState::Created, ApoState::Initialized)
        .map_err(|e| windows::core::Error::from(HRESULT::from(e)))?;

    // 3. 解析 APOInitSystemEffects → 端点 GUID + 子 APO（object 7.1.8）。
    //    Safety: pby_data 已验证非空 + 尺寸足够；APOInitSystemEffects 为 repr(C) 结构。
    let endpoint_guid = if valid_init_data {
        let init = unsafe { &*(pby_data as *const APOInitSystemEffects) };
        extract_endpoint_guid(init)
    } else {
        None
    };

    // 4. 子 APO 创建（vendor 安装信息区读取，失败降级为无子 APO）。
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
    let child_created = child.is_some();
    *apo.child_apo.lock().unwrap_or_else(|e| e.into_inner()) = child;

    // 安装验证管道上报（install --verify）：先报 initialize，子 APO 创建成功
    // 再报 child_apo。无 DeviceTestPipeName 时 test_pipe 内部直接 no-op。
    if let Some(eg) = endpoint_guid {
        let stage = if apo.clsid == CLSID_VXAPO_PRE_MIX {
            "premix"
        } else {
            "postmix"
        };
        let eg_str = guid_to_string(&eg);
        crate::object::apo::test_pipe::notify(&eg_str, stage, "initialize");
        if child_created {
            crate::object::apo::test_pipe::notify(&eg_str, stage, "child_apo");
        }
    }

    // 5b. 运行期自愈：Windows 重新枚举/重启后可能从驱动模板把微软 CAPX
    //     重新灌回 `MSFX\N`，与 VxAPO 同时加载导致断断续续/慢放。本 DLL 在
    //     每次加载（Initialize，控制线程）时按端点自愈接管——仅动微软 CAPX，
    //     仅限已装 VxAPO 的端点；失败仅降级日志，不阻塞初始化。
    if let Some(eg) = endpoint_guid {
        let eg_str = guid_to_string(&eg);
        let selfheal_start = std::time::Instant::now();
        match crate::install::selector::operation::find_endpoint_path(&eg_str) {
            Ok(endpoint_path) => {
                if let Err(e) =
                    crate::install::device::sysfx::ensure_takeover_for_endpoint(&endpoint_path)
                {
                    log::warn!("Initialize: MSFX self-heal failed for {eg_str}: {e}");
                }
                crate::object::apo::config::diag_append(&format!(
                    "INIT clsid={:?} pid={} endpoint={eg_str} selfheal_ms={} agg_created={} agg_destroyed={}",
                    apo.clsid,
                    std::process::id(),
                    selfheal_start.elapsed().as_millis(),
                    crate::object::apo::aggregate::AGG_CREATED.load(std::sync::atomic::Ordering::Relaxed),
                    crate::object::apo::aggregate::AGG_DESTROYED.load(std::sync::atomic::Ordering::Relaxed)
                ));
            }
            Err(_) => {
                // 端点路径找不到（虚拟设备/已拔出）→ 无需接管。
            }
        }
    }

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
    *apo.config_path
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = path;

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
