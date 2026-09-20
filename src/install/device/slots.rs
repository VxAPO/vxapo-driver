//! install/device/slots.rs — APO 槽位管理（规范 5.3）
//!
//! 管理 Windows 音频端点 FxProperties 注册表键下的 5 个 APO GUID 槽位，
//! 提供安装模式选择与原始 APO GUID 回退查询。
//!
//! 5 个槽位：
//! ```text
//! 索引 名称 角色
//! 0 LFX Legacy PreMix（Win8.1+）
//! 1 GFX Legacy PostMix（Win8.1+）
//! 2 SFX Side-effect PreMix（Win10+）
//! 3 MFX Mixed-effect PostMix（Win11 蓝牙）
//! 4 EFX Endpoint-effect PostMix（默认）
//! ```
//!
//! 每个槽位有三种特殊值状态：
//! - `NoKey`：FxProperties 键不存在（设备未配置任何 APO）
//! - `NoValue`：值为空或已被其他 APO 占据（该槽位无自定义 APO）
//! - `Guid(GUID)`：具体的 APO CLSID
//!
//! 3 种安装模式：
//! | 模式 | PreMix 槽位 | PostMix 槽位 | 适用场景 |
//! |----------|-------------|--------------|----------------|
//! | LfxGfx | LFX(0) | GFX(1) | Win8.1+ Legacy |
//! | SfxMfx | SFX(2) | MFX(3) | Win11 蓝牙 |
//! | SfxEfx | SFX(2) | EFX(4) | 默认 |
//!
//! GUID 回退逻辑：
//! - `get_original_pre_mix()`：当前模式槽位为 `NoValue` 时回退到同组另一槽位
//! - `get_original_post_mix()`：类似，涉及 GFX / MFX / EFX 三槽位
//! - `NoKey` 或无回退目标时返回空字符串
//!
//! 此模块只做查询，不修改任何系统状态。实际操作委托 `install/install`。

use crate::sys::com::prelude::{GUID, guid_to_string};
use crate::sys::registry::RegKey;
use crate::utils::guid::{guid_from_bytes, is_zero_guid, parse_guid_string};

// ── 子模块（类型 / 槽位读取 / 子 APO 信息区）─────────────────────────────

mod child;
mod read;
mod types;

pub use child::{
    child_apo_key_exists, get_original_post_mix, get_original_pre_mix, read_child_apo_guid,
    ChildApoKind, CHILD_APO_PATH_ROOT,
};
pub use read::{detect_install_mode, read_all_slots, read_slot_value};
pub use types::{
    ApoSlot, InstallMode, SlotValue, FX_PROPERTIES_KEY, INSTALL_VERSION, INSTALL_VERSION_LEGACY,
};

#[cfg(test)]
mod tests;
