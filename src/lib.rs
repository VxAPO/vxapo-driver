//! vxapo-driver/src/lib.rs — 库根模块
//!
//! VxAPO：Windows 音频处理对象（APO）驱动，提供系统级音频 DSP 处理能力。
//!
//! 模块结构（内部实现，对外不可见）：
//!
//! - [`sys`]：FFI 层（COM/注册表/音频定义）
//! - [`pipeline`]：音频处理管道（context/buffer/interleave/chain/process/dsp）
//! - [`install`]：设备 APO 安装/卸载
//! - [`config`]：配置文件解析
//! - [`object`]：胶水层（ApoObject COM 对象）
//! - [`telemetry`]：无锁日志 + panic hook
//! - [`utils`]：工具层
//!
//! 对外只暴露下面 facade 里的条目（D3 pub 收窄）。新增对外 API 时必须同时
//! 在此登记，避免实现细节随模块路径泄漏出去。

// ======================== Crate 级配置 ========================

// 禁止不安全代码的文档缺失（强制要求 SAFETY 注释）
#![deny(clippy::undocumented_unsafe_blocks)]

// Windows COM 接口沿用 PascalCase / SCREAMING_SNAKE_CASE 命名
#![allow(non_camel_case_types, non_snake_case)]

// ======================== 模块声明（crate 内部） ========================

pub(crate) mod config;
pub(crate) mod install;
pub(crate) mod object;
pub(crate) mod pipeline;
pub(crate) mod sys;
pub(crate) mod telemetry;
pub(crate) mod utils;

// ======================== 对外 facade ========================
//
// 清单来源：cli 仓对 `vxapo_driver::` 的全部引用（含签名中出现的内嵌类型）。

// install/audiodg：音频服务与 audiodg 生命周期
pub use crate::install::audiodg::{
    ensure_audio_service_running, restart_audio_service_wait, start_audio_service_with_dependents,
    stop_audio_service, stop_audio_service_with_dependents, wait_for_audiodg_exit,
};
// install/device：设备枚举、端点定位、槽位与残留记录
pub use crate::install::device::endpoint::{EndpointInfo, EndpointState};
pub use crate::install::device::format::AudioFormat;
pub use crate::install::device::info::{
    detect_mode_for_guid, enumerate_devices, find_endpoint_path, DeviceInfo,
};
pub use crate::install::device::slots::{
    child_apo_key_exists, read_child_apo_guid, ChildApoKind, InstallMode, SlotValue,
};
pub use crate::install::device::stale::{
    cleanup_orphan, fix_config_acl, list_stale_installs, snapshot_dir, MigrationReport, StaleInstall,
};
// object：per-device 配置路径（cli 与 driver 共用同一路径布局）
pub use crate::object::apo::config::device_config_path;
// install/selector：安装/卸载/迁移编排
pub use crate::install::selector::operation::{
    install_endpoint, migrate_install, uninstall_endpoint, write_install_config, InstallConfig,
};
// object：COM 注册与 CLSID
pub use crate::object::dll_exports::register_apo_with_path;
pub use crate::object::vx_reg_props::{CLSID_VXAPO_POST_MIX, CLSID_VXAPO_PRE_MIX};
// sys：GUID 格式化与注册表访问
pub use crate::sys::com::prelude::guid_to_string;
pub use crate::sys::registry::{RegKey, RegValue};
// utils：统一错误类型（公开 API 的签名使用）
pub use crate::utils::vx_error::VxApoError;
