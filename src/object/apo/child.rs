//! host/instance/apo_child.rs — 子 APO 管理
//!
//! 子 APO COM 生命周期管理：
//! - `CoCreateInstance` 创建子 APO 实例
//! - `QueryInterface` 获取三个接口（类型化接口引用，windows-rs cast）
//! - 延迟、重置、帧数计算、锁定/解锁委托给子 APO
//! - `Drop` 自动释放所有 COM 接口引用
//!
//! 子 APO 由 `init.rs` 在 `Initialize` 时创建（，失败降级为无子 APO），
//! 存储在 `ApoObject.child_apo` 中，供 apo_rt.rs（RT）和 apo_conf.rs（配置）委托调用。
//!
//! ：
//! - **类型化接口持有**（windows-rs `IAudioProcessingObject` 等）——替代早期裸 vtable 手动调用
//!   （原实现用 `vtbl_method` + `transmute` 手动索引 vtable；windows-rs 0.62.2 提供
//!   三个接口的 safe 调用方法，对齐类型化方案）
//! - **子 APO GUID 来源** = 端点 GUID → `HKLM\SOFTWARE\VxAPO\Child APOs\{deviceGuid}\{PreMixChild|PostMixChild}`
//! （独立安装信息区， 路径隔离；`install/device/slots` 提供读取）
//! - **格式协商参数（执行端建议采纳）**：`is_input/output_format_supported` 输入参数
//!   `Option<&IAudioMediaType>`（p_opposite 可 None=无对端；p_requested 由父转发非空）——
//!   可空借用语义用安全引用表达，与 windows-rs `#[interface]` 可空接口参数风格一致；
//!   输出 `pp_supported: *mut *mut` 为 COM 输出必须保留裸指针（方法仍 unsafe）
//! - **委托失败降级**：Initialize/LockForProcess/UnlockForProcess 失败不阻塞父
//! - **重置防御**：Unlock 失败后下次 Lock 前 child.reset()/重建


use crate::sys::com::apo_interfaces::{
    IAudioMediaType, IAudioProcessingObject, IAudioProcessingObjectConfiguration,
    IAudioProcessingObjectRT,
};
use crate::sys::com::apo_types::{
    APO_CONNECTION_DESCRIPTOR, APO_CONNECTION_PROPERTY, APO_REG_PROPERTIES, REFERENCE_TIME,
};
use crate::sys::com::prelude::{
    CLSCTX_ALL, CoCreateInstance, E_POINTER, GUID, HRESULT, Interface, IUnknown, S_OK,
};

// ══════════════════════════════════════════════════════════════════════════════
// ChildApo
// ══════════════════════════════════════════════════════════════════════════════

/// 子 APO COM 对象持有者。
///
/// 持有类型化的 COM 接口引用，通过 windows-rs 生成的调用方法委托（非手动 vtable 索引）。
/// Drop 时自动释放三个 COM 接口引用。
///
/// # COM 生命周期
///
/// - `create()`：`CoCreateInstance` → `IUnknown`（ref=1）→ cast×3（ref=4）→ drop `IUnknown`（ref=3）
/// - `Drop`：三个接口引用各自 Release（ref=0 → 对象销毁）
pub struct ChildApo {
    /// `IAudioProcessingObject` 接口（Reset/GetLatency/格式协商/Initialize 等）。
    iapo: IAudioProcessingObject,
    /// `IAudioProcessingObjectRT` 接口（APOProcess/CalcInputFrames/CalcOutputFrames）。
    iapo_rt: IAudioProcessingObjectRT,
    /// `IAudioProcessingObjectConfiguration` 接口（LockForProcess/UnlockForProcess）。
    iapo_cfg: IAudioProcessingObjectConfiguration,
}

// SAFETY: 接口引用在 ChildApo 生命周期内有效（cast 已 AddRef，Drop 自动 Release）。
// ApoObject 含 ChildApo 且 unsafe impl Send/Sync——COM 接口引用跨线程合法
// （Windows 音频引擎保证 APO 方法的线程亲和）。
unsafe impl Send for ChildApo {}
unsafe impl Sync for ChildApo {}

impl ChildApo {
    /// 创建子 APO 实例。
    ///
    /// `CoCreateInstance` → `IUnknown` → cast（自动 QI + AddRef）三个接口 → drop `IUnknown`。
    ///
    /// # Safety
    ///
    /// - COM 必须已初始化（`CoInitializeEx`，由宿主进程负责）
    /// - `clsid` 必须指向有效的 APO CLSID
    pub unsafe fn create(clsid: &GUID) -> Result<Self, HRESULT> {
        // Step 1: CoCreateInstance → IUnknown（ref=1）。
        // SAFETY: rclsid 非空 + CLSCTX_INPROC_SERVER；函数返回 Result<IUnknown>。
        let unknown: IUnknown = unsafe { CoCreateInstance(clsid, None, CLSCTX_ALL) }
            .map_err(|e| HRESULT::from(e))?;

        // Step 2: cast 三个接口（每次 cast 内部 QI + AddRef，ref 递增；cast 为 safe 方法）。
        // SAFETY: unknown 有效；目标接口为该 APO 真实实现的接口。
        let iapo: IAudioProcessingObject =
            unknown.cast().map_err(|e| HRESULT::from(e))?;
        let iapo_rt: IAudioProcessingObjectRT =
            unknown.cast().map_err(|e| HRESULT::from(e))?;
        let iapo_cfg: IAudioProcessingObjectConfiguration =
            unknown.cast().map_err(|e| HRESULT::from(e))?;

        // Step 3: drop 原始 IUnknown（三个 cast 引用保持对象存活）。
        drop(unknown);

        Ok(Self { iapo, iapo_rt, iapo_cfg })
    }

    /// 是否有效（所有接口引用非空）。
    ///
    /// 类型化接口的 null 语义：`cast` 成功即接口引用有效；此谓词保留
    /// 为防御性检查（与早期裸指针版本语义兼容）。
    pub fn is_valid(&self) -> bool {
        !self.iapo.as_raw().is_null()
            && !self.iapo_rt.as_raw().is_null()
            && !self.iapo_cfg.as_raw().is_null()
    }

    // ── IAudioProcessingObject 委托 ──────────────────────────────────────────

    /// 获取子 APO 延迟（`GetLatency`，windows-rs 调用方法）。
    ///
    /// 失败返回 0（保守——延迟未知按无延迟处理，不阻断父流程）。
    pub fn get_latency(&self) -> REFERENCE_TIME {
        // windows-rs: GetLatency() -> Result<i64>（i64 = REFERENCE_TIME）。
        unsafe { self.iapo.GetLatency() }.unwrap_or(0)
    }

    /// 重置子 APO（`Reset`）。
    ///
    /// 用于 Unlock 失败后的重置防御（下次 Lock 前调用）。
    pub fn reset(&self) -> HRESULT {
        // windows-rs: Reset() -> Result<()>。
        unsafe { self.iapo.Reset() }
            .map(|_| S_OK)
            .unwrap_or_else(|e| e.into())
    }

    /// 获取子 APO 注册属性（`GetRegistrationProperties`）。
    ///
    /// # Safety
    ///
    /// 调用方负责通过 `CoTaskMemFree` 释放 `*pp_props`。
    pub unsafe fn get_registration_properties(
        &self,
        pp_props: *mut *mut APO_REG_PROPERTIES,
    ) -> HRESULT {
        if pp_props.is_null() {
            return E_POINTER;
        }
        // windows-rs: GetRegistrationProperties() -> Result<*mut APO_REG_PROPERTIES>。
        match unsafe { self.iapo.GetRegistrationProperties() } {
            Ok(ptr) => {
                unsafe { *pp_props = ptr };
                S_OK
            }
            Err(e) => e.into(),
        }
    }

    /// 初始化子 APO（`Initialize`，同传父 APOInit 数据，EAPO 180-215 对齐）。
    ///
    /// # Safety
    ///
    /// `pby_data` 必须指向有效的 `cb_data_size` 字节缓冲区。
    pub unsafe fn initialize(&self, cb_data_size: u32, pby_data: *const u8) -> HRESULT {
        // windows-rs: Initialize(pbydata: &[u8]) — 由 slice 长度推导 cbdatasize。
        if pby_data.is_null() && cb_data_size > 0 {
            return E_POINTER;
        }
        // SAFETY: 调用方保证缓冲区有效；slice 生命周期仅覆盖此调用。
        let data = if cb_data_size == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(pby_data, cb_data_size as usize) }
        };
        unsafe { self.iapo.Initialize(data) }
            .map(|_| S_OK)
            .unwrap_or_else(|e| e.into())
    }

    /// 检查输入格式是否支持（`IsInputFormatSupported`， 参数采纳）。
    ///
    /// - `p_opposite`：对端格式，可能为 None（无对端）
    /// - `p_requested`：请求格式，由父接口转发（非空）
    /// - `pp_supported`：COM 输出——调用方分配、子 APO 写入支持格式接口指针（调用方负责 Release）
    ///
    /// # Safety
    ///
    /// `pp_supported` 必须有效（COM 输出指针）。
    pub unsafe fn is_input_format_supported(
        &self,
        p_opposite: Option<&IAudioMediaType>,
        p_requested: Option<&IAudioMediaType>,
        pp_supported: *mut *mut IAudioMediaType,
    ) -> HRESULT {
        self.resolve_supported(p_opposite, p_requested, pp_supported, |a, b| {
            unsafe { self.iapo.IsInputFormatSupported(a, b) }
        })
    }

    /// 检查输出格式是否支持（`IsOutputFormatSupported`， 参数采纳）。
    ///
    /// 同 `is_input_format_supported` 语义。
    ///
    /// # Safety
    ///
    /// `pp_supported` 必须有效（COM 输出指针）。
    pub unsafe fn is_output_format_supported(
        &self,
        p_opposite: Option<&IAudioMediaType>,
        p_requested: Option<&IAudioMediaType>,
        pp_supported: *mut *mut IAudioMediaType,
    ) -> HRESULT {
        self.resolve_supported(p_opposite, p_requested, pp_supported, |a, b| {
            unsafe { self.iapo.IsOutputFormatSupported(a, b) }
        })
    }

    /// 格式协商公共出口：调用具体接口方法，把返回的 `IAudioMediaType` 以 COM 输出指针移交。
    ///
    /// # Safety
    /// `pp_supported` 必须是有效的 COM 输出指针；`call` 由调用方保证只调用格式协商接口。
    unsafe fn resolve_supported<F>(
        &self,
        p_opposite: Option<&IAudioMediaType>,
        p_requested: Option<&IAudioMediaType>,
        pp_supported: *mut *mut IAudioMediaType,
        call: F,
    ) -> HRESULT
    where
        F: FnOnce(
            Option<&IAudioMediaType>,
            Option<&IAudioMediaType>,
        ) -> windows::core::Result<IAudioMediaType>,
    {
        if pp_supported.is_null() {
            return E_POINTER;
        }
        match call(p_opposite, p_requested) {
            Ok(supported) => {
                // 返回的接口引用 +1（from_abi）；ManuallyDrop 防泄漏，as_raw 取指针移交调用方
                // （调用方负责最终 Release）。
                let leaked = std::mem::ManuallyDrop::new(supported);
                unsafe { *pp_supported = Interface::as_raw(&*leaked) as *mut _ };
                S_OK
            }
            Err(e) => e.into(),
        }
    }

    /// 获取输入通道数（`GetInputChannelCount`）。
    pub fn get_input_channel_count(&self, p_count: *mut u32) -> HRESULT {
        if p_count.is_null() {
            return E_POINTER;
        }
        match unsafe { self.iapo.GetInputChannelCount() } {
            Ok(count) => {
                unsafe { *p_count = count };
                S_OK
            }
            Err(e) => e.into(),
        }
    }

    // ── IAudioProcessingObjectRT 委托 ────────────────────────────────────────

    /// 子 APO 计算输入帧数（`CalcInputFrames`）。
    pub fn calc_input_frames(&self, output_frames: u32) -> u32 {
        unsafe { self.iapo_rt.CalcInputFrames(output_frames) }
    }

    /// 子 APO 计算输出帧数（`CalcOutputFrames`）。
    pub fn calc_output_frames(&self, input_frames: u32) -> u32 {
        unsafe { self.iapo_rt.CalcOutputFrames(input_frames) }
    }

    /// 子 APO 实时处理（`APOProcess`，主规范 18.1 A3 前置每帧一次）。
    ///
    /// childRT->APOProcess **先跑**（作用于输入缓冲）→ 父 VxAPO 双链处理其输出；
    /// 无 child 时跳过（纯 VxAPO 处理）。
    ///
    /// # Safety
    ///
    /// `pp_inputs` / `pp_outputs` 指向引擎分配的有效 APO_CONNECTION_PROPERTY 指针数组。
    pub unsafe fn apo_process(
        &self,
        num_input: u32,
        pp_inputs: *const *const APO_CONNECTION_PROPERTY,
        num_output: u32,
        pp_outputs: *mut *mut APO_CONNECTION_PROPERTY,
    ) {
        unsafe { self.iapo_rt.APOProcess(num_input, pp_inputs, num_output, pp_outputs) }
    }

    // ── IAudioProcessingObjectConfiguration 委托 ─────────────────────────────

    /// 锁定子 APO（`LockForProcess`）。
    ///
    /// 失败**不阻塞父**锁定（降级立场；结果仅 Trace 不 return）。
    ///
    /// # Safety
    ///
    /// `pp_inputs` / `pp_outputs` 必须指向有效描述符指针数组，且描述符中的
    /// `pFormat`（`IAudioMediaType*`）和缓冲在调用期间保持有效。
    pub unsafe fn lock_for_process(
        &self,
        num_input: u32,
        pp_inputs: *mut *mut APO_CONNECTION_DESCRIPTOR,
        num_output: u32,
        pp_outputs: *mut *mut APO_CONNECTION_DESCRIPTOR,
    ) -> HRESULT {
        // windows-rs: LockForProcess(ppinputs: &[*const APO_CONNECTION_DESCRIPTOR], ...)。
        if num_input == 0 || pp_inputs.is_null() || num_output == 0 || pp_outputs.is_null() {
            return E_POINTER;
        }
        let inputs = unsafe {
            std::slice::from_raw_parts(pp_inputs as *const *const APO_CONNECTION_DESCRIPTOR, num_input as usize)
        };
        let outputs = unsafe {
            std::slice::from_raw_parts(pp_outputs as *const *const APO_CONNECTION_DESCRIPTOR, num_output as usize)
        };
        unsafe { self.iapo_cfg.LockForProcess(inputs, outputs) }
            .map(|_| S_OK)
            .unwrap_or_else(|e| e.into())
    }

    /// 解锁子 APO（`UnlockForProcess`）。
    ///
    /// 失败**不阻塞父**解锁（object 7.1.10 容错语义——UnlockForProcess 无重试语义，
    /// 子 APO 可能已部分解锁，父继续自身流程 + 日志；child 标记需重置，下次 Lock 前 reset）。
    pub fn unlock_for_process(&self) -> HRESULT {
        unsafe { self.iapo_cfg.UnlockForProcess() }
            .map(|_| S_OK)
            .unwrap_or_else(|e| e.into())
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试说明
// ══════════════════════════════════════════════════════════════════════════════

// ChildApo 类型化接口方案下**无法安全构造 null 接口做防御性测试**：
// windows-rs 接口 Drop 会对接口引用调用 IUnknown::Release——null 引用（裸指针时代的
// release_raw 有判空，类型化方案无）会解引用 vtable → STATUS_STACK_BUFFER_OVERRUN 崩溃。
// 故删除全部「null 接口防御性检查」测试（原 7 个），避免测试进程 abort。
//
// ChildApo::create / 委托链测试需真实 COM + 已注册 APO，无法单元测试——
// 留 CLI 端到端验证（与 听感验证同理）。
// 正确的纯逻辑测试见 install/device/slots.rs（childApo GUID 解析 + 全量判定）。
