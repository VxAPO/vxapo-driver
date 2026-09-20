//! object/apo.rs — ApoObject 核心（规范 7.1，按 windows-rs 0.62.2 _Impl trait 实现）
//! 模块入口：COM 接口薄转发，纯逻辑子模块见 `object/apo/`。

pub mod aggregate;
pub mod child;
pub mod config;
pub mod init;
pub mod inner;
pub mod lock_key;
pub mod negotiate;
pub mod process;
pub mod reload;
pub mod rtdump;
pub mod state;
pub mod test_pipe;

use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex};

use windows::core::Result;

use crate::object::apo::child::ChildApo;
use crate::object::apo::config::{DEFAULT_CONFIG_PATH, WatcherState, hot_reload_impl};
use crate::object::apo::inner::ApoObjectInner;
use crate::object::apo::state::StateCell;
use crate::object::ref_count;
use crate::pipeline::process::ProcessStatistics;
use crate::sys::com::apo_interfaces::{
    IAudioMediaType, IAudioProcessingObject, IAudioProcessingObjectConfiguration,
    IAudioProcessingObjectRT, IAudioProcessingObject_Impl, IAudioProcessingObjectRT_Impl,
    IAudioProcessingObjectConfiguration_Impl, IAudioSystemEffects, IAudioSystemEffects_Impl,
};
use crate::sys::com::apo_types::{
    APO_CONNECTION_DESCRIPTOR, APO_CONNECTION_PROPERTY, APO_REG_PROPERTIES,
};
use crate::sys::com::prelude::{E_FAIL, GUID, implement};

// ═══ ApoObject ═══
#[implement(
    IAudioProcessingObject,
    IAudioProcessingObjectRT,
    IAudioProcessingObjectConfiguration,
    IAudioSystemEffects
)]
#[allow(dead_code)]
pub struct ApoObject {
    pub(crate) clsid: GUID,
    pub(crate) state_cell: StateCell,
    /// 内部状态（双链过渡）。Arc<Mutex>：spawn 线程可 clone（hot_reload 独立访问）。
    pub(crate) mutex: Arc<Mutex<ApoObjectInner>>,
    pub(crate) latency_samples: AtomicU32,
    pub(crate) latency_frames_atomic: AtomicU32,
    pub(crate) process_stats: ProcessStatistics,
    /// 配置文件路径（Initialize 确定，per-device `C:\ProgramData\VxAPO\{GUID}\config.toml`）。
    /// Arc<Mutex>：spawn 线程可 clone（hot_reload 独立访问）。
    pub(crate) config_path: Arc<Mutex<String>>,
    /// watcher 运行时状态（外部驱动模型）：Lock 末尾启动 / Unlock 停止。
    /// Arc<Mutex>：&self 可写（#[implement] 无 &mut Foo）；spawn 可 clone 移入线程。
    watcher_state: Arc<Mutex<WatcherState>>,
    /// 子 APO（object 7.1.3）：Initialize 创建，失败降级 None。
    /// Arc<Mutex>：&self 可写 + 控制线程（Initialize/Lock/Unlock）持有；
    /// RT 路径 APOProcess 锁 inner 前短锁读取（引擎保证不重叠，无实际阻塞）。
    /// 语义等价规范 7.1.3 的字段（Arc Mutex WatcherState 先例）。
    pub(crate) child_apo: Arc<Mutex<Option<ChildApo>>>,
}

impl ApoObject {
    pub fn new(clsid: GUID) -> Self {
        ref_count::increment();
        Self {
            clsid,
            state_cell: StateCell::new(),
            mutex: Arc::new(Mutex::new(ApoObjectInner::new())),
            latency_samples: AtomicU32::new(0),
            latency_frames_atomic: AtomicU32::new(0),
            process_stats: ProcessStatistics::new(),
            config_path: Arc::new(Mutex::new(DEFAULT_CONFIG_PATH.to_owned())),
            watcher_state: Arc::new(Mutex::new(WatcherState::default())),
            child_apo: Arc::new(Mutex::new(None)),
        }
    }

    /// 配置热重载（watcher 回调）。委托模块级 `hot_reload_impl`（共享 Arc 字段）。
    pub fn hot_reload(&self) {
        hot_reload_impl(
            &self.config_path,
            &self.mutex,
            self.clsid,
            // 仅诊断日志用（不 deref）：用 mutex Arc 的稳定堆地址，避免 &ApoObject 生命周期耦合。
            Arc::as_ptr(&self.mutex) as usize,
        );
    }
}

impl Drop for ApoObject {
    fn drop(&mut self) {
        ref_count::decrement();
    }
}

// ═══ IAudioProcessingObject 实现（windows-rs _Impl trait 签名 → 子模块转发） ═══
impl IAudioProcessingObject_Impl for ApoObject_Impl {
    fn Reset(&self) -> Result<()> {
        // 控制型 COM 入口统一 catch_unwind——内部 mutex 中毒/format!
        // 等 panic 不得跨 extern "system" 边界 unwind（release panic=abort 时为空操作）。
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| process::reset(self)))
            .unwrap_or_else(|_| Err(windows::core::Error::from(E_FAIL)))
    }

    fn GetLatency(&self) -> Result<i64> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| process::get_latency(self)))
            .unwrap_or_else(|_| Err(windows::core::Error::from(E_FAIL)))
    }

    fn GetRegistrationProperties(&self) -> Result<*mut APO_REG_PROPERTIES> {
        init::get_registration_properties(self)
    }

    fn Initialize(&self, cb_data_size: u32, pby_data: *const u8) -> Result<()> {
        // （P2）：Initialize 内部含 mutex 锁与自愈 I/O，同样必须 catch_unwind。
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            init::initialize(self, cb_data_size, pby_data)
        }))
        .unwrap_or_else(|_| Err(windows::core::Error::from(E_FAIL)))
    }

    fn IsInputFormatSupported(
        &self,
        p_opposite_format: windows::core::Ref<IAudioMediaType>,
        p_requested: windows::core::Ref<IAudioMediaType>,
    ) -> Result<IAudioMediaType> {
        negotiate::is_input_format_supported(&p_opposite_format, &p_requested)
    }

    fn IsOutputFormatSupported(
        &self,
        p_opposite_format: windows::core::Ref<IAudioMediaType>,
        p_requested: windows::core::Ref<IAudioMediaType>,
    ) -> Result<IAudioMediaType> {
        negotiate::is_output_format_supported(&p_opposite_format, &p_requested)
    }

    fn GetInputChannelCount(&self) -> Result<u32> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            process::get_input_channel_count(self)
        }))
        .unwrap_or_else(|_| Err(windows::core::Error::from(E_FAIL)))
    }
}

// ═══ IAudioProcessingObjectRT 实现 ═══
impl IAudioProcessingObjectRT_Impl for ApoObject_Impl {
    fn APOProcess(
        &self,
        num_input: u32,
        pp_inputs: *const *const APO_CONNECTION_PROPERTY,
        num_output: u32,
        pp_outputs: *mut *mut APO_CONNECTION_PROPERTY,
    ) {
        process::apo_process(self, num_input, pp_inputs, num_output, pp_outputs);
    }

    fn CalcInputFrames(&self, output_frames: u32) -> u32 {
        process::calc_input_frames(self, output_frames)
    }

    fn CalcOutputFrames(&self, input_frames: u32) -> u32 {
        process::calc_output_frames(self, input_frames)
    }
}

// ═══ IAudioSystemEffects 实现（EAPO 对齐，marker 接口） ═══
impl IAudioSystemEffects_Impl for ApoObject_Impl {}

// ═══ IAudioProcessingObjectConfiguration 实现 ═══
impl IAudioProcessingObjectConfiguration_Impl for ApoObject_Impl {
    fn LockForProcess(
        &self,
        num_input: u32,
        pp_inputs: *const *const APO_CONNECTION_DESCRIPTOR,
        num_output: u32,
        pp_outputs: *const *const APO_CONNECTION_DESCRIPTOR,
    ) -> Result<()> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            process::lock_for_process(self, num_input, pp_inputs, num_output, pp_outputs)
        }))
        .unwrap_or_else(|_| Err(windows::core::Error::from(E_FAIL)))
    }

    fn UnlockForProcess(&self) -> Result<()> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            process::unlock_for_process(self)
        }))
        .unwrap_or_else(|_| Err(windows::core::Error::from(E_FAIL)))
    }
}

// SAFETY: 引擎保证同一 ApoObject 的 COM 方法不重叠调用；跨线程访问仅经
// `Arc<Mutex<...>>` / `AtomicU32` / `StateCell`（内部同步），`#[implement]`
// 对象生命周期由 COM 引用计数管理（规范 7.1 原文语义）。
unsafe impl Send for ApoObject {}
unsafe impl Sync for ApoObject {}

// ═══ 测试 ═══
#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::object::apo::config::resolve_config_path_from;
    use crate::sys::com::apo_types::APOInitSystemEffects;

    /// 构造「无 IPropertyStore」的最低有效 APOInitSystemEffects（zeroed 后仅设置 APOInit.cbSize）。
    /// 提取端点 GUID 会因属性存储缺失返回 None → 走 `_default` 兜底。
    fn empty_init() -> APOInitSystemEffects {
        let mut init: APOInitSystemEffects = unsafe { std::mem::zeroed() };
        init.APOInit.cbSize = std::mem::size_of::<APOInitSystemEffects>() as u32;
        init
    }

    #[test]
    fn config_path_default_device_dir_when_no_guid() {
        // 无端点 GUID（属性存储缺失）→ `{config_root}\_default\config.toml`。
        let root = std::env::temp_dir().join("vxapo_apo_test").join("cfg");
        let root_str = root.display().to_string();
        let init = empty_init();
        let path = resolve_config_path_from(&root_str, Some(&init));
        let p = Path::new(&path);
        assert!(p.starts_with(&root));
        assert!(p.ends_with("config.toml"));
        // 目录应包含 `_default`。
        assert!(path.contains("_default"));
        // 目录已创建 + 默认 passthrough 文件已写入。
        assert!(p.parent().unwrap().is_dir());
        assert!(p.exists());
        let content = std::fs::read_to_string(p).unwrap();
        assert!(content.contains("passthrough"));
        // 清理（避免污染 temp）。
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn config_path_custom_guid_dir() {
        // 有明确端点 GUID（用模拟 IPropertyStore 成本高，此处用 default GUID 走不到
        // Real IPropertyStore——改为验证：手动构造 pAPOSystemEffectsProperties 为 None
        // 时仍 `_default`。GUID 路径分支由 extract_endpoint_guid（真实环境）覆盖。
        // 这里验证 `_default` 兜底 + 目录创建 + 文件写入的完整链路。
        let root = std::env::temp_dir().join("vxapo_apo_test2").join("cfg");
        let root_str = root.display().to_string();
        let init = empty_init();
        let path = resolve_config_path_from(&root_str, Some(&init));
        assert!(path.contains("_default"));
        // 幂等：再次调用不应报错（目录已存在）。
        let _ = resolve_config_path_from(&root_str, Some(&init));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn config_path_none_init_falls_back_default() {
        // init=None（Initialize 数据非法降级）→ `_default` 兜底。
        let root = std::env::temp_dir().join("vxapo_apo_test3").join("cfg");
        let root_str = root.display().to_string();
        let path = resolve_config_path_from(&root_str, None);
        assert!(path.contains("_default"));
        assert!(Path::new(&path).parent().unwrap().is_dir());
        let _ = std::fs::remove_dir_all(&root);
    }
}
