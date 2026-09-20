//! install/selector/operation.rs — 设备 APO 安装/卸载执行 + 事务回滚（规范 5.5.2）
//!
//! 职责：
//! - `install_endpoint`： 完整 7 步安装，Transaction 保护，失败自动回滚
//! - `uninstall_endpoint`：卸载（恢复原始 GUID，清理配置）
//! - `InstallConfig`：安装参数
//!
//! 禁止依赖：`pipeline/`、`config/`。

use windows::Win32::System::Registry::{HKEY_CLASSES_ROOT, HKEY_LOCAL_MACHINE};
use windows::Win32::Media::KernelStreaming::AUDIO_SIGNALPROCESSINGMODE_DEFAULT;

use crate::install::device::slots::{
    ApoSlot, ChildApoKind, InstallMode, SlotValue, read_slot_value, CHILD_APO_PATH_ROOT,
    FX_PROPERTIES_KEY, INSTALL_VERSION,
};
use crate::install::device::identity::{
    merge_endpoint_history, read_endpoint_identity, write_identity_values,
};
use crate::install::device::info::find_endpoint_path;
use crate::install::device::sysfx;
use crate::object::vx_reg_props::{CLSID_VXAPO_POST_MIX, CLSID_VXAPO_PRE_MIX};
use crate::sys::com::prelude::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, GUID, IUnknown,
    guid_to_string,
};
use crate::sys::registry::{RegKey, RegValue};
use crate::utils::vx_error::{Result, VxApoError};

// ══════════════════════════════════════════════════════════════════════════════
// 路径常量
// ══════════════════════════════════════════════════════════════════════════════

/// .reg 备份默认目录。
const BACKUP_DIR: &str = r"C:\ProgramData\VxAPO\backups";

// ══════════════════════════════════════════════════════════════════════════════
// InstallConfig
// ══════════════════════════════════════════════════════════════════════════════

/// 安装参数。
#[derive(Debug, Clone)]
pub struct InstallConfig {
    /// 是否安装 PreMix APO。
    pub install_premix: bool,
    /// 是否安装 PostMix APO。
    pub install_postmix: bool,
    /// 安装模式（决定使用哪两个槽位）。
    pub install_mode: InstallMode,
    /// 是否保留原有 PreMix APO 作为子 APO。
    pub use_original_apo_premix: bool,
    /// 是否保留原有 PostMix APO 作为子 APO。
    pub use_original_apo_postmix: bool,
    /// 是否允许静音缓冲区快速路径。
    pub allow_silent_buffer: bool,
    /// 是否启用 autoAdjust（独立于 allow_silent_buffer）。
    ///
    /// 默认 false（VxAPO 无自动校正实现，保守默认关）。
    pub auto_adjust: bool,
}

impl InstallConfig {
    /// 默认配置：SfxEfx 模式，安装 PreMix + PostMix，允许静音缓冲区。
    pub fn default_config() -> Self {
        Self {
            install_premix: true,
            install_postmix: true,
            install_mode: InstallMode::default_mode(),
            use_original_apo_premix: false,
            use_original_apo_postmix: false,
            allow_silent_buffer: true,
            auto_adjust: false,
        }
    }
}

// ── 子模块（执行 / CAPX 接管 / 内部辅助）─────────────────────────────────

mod capx;
mod execute;
mod helpers;

pub use execute::{install_endpoint, migrate_install, uninstall_endpoint, write_install_config};

#[cfg(test)]
mod tests;
