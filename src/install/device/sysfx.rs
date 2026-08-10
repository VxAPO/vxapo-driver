//! install/device/sysfx.rs — Windows CAPX “设备默认效果”接管（v9.0）
//!
//! 背景（2026-08-10 实证）：
//! - 通用 USB 音频设备（wdma_usb.inf）会在设备接口注册表
//!   `HKLM\SYSTEM\CurrentControlSet\Control\DeviceClasses\...\Device Parameters\MSFX\N`
//!   下写入“Microsoft Audio Home Theater Effects”（CAPX 系统效果模板）。
//! - 该模板包含两个微软 APO：StreamEffectClsid（`,5`）与 ModeEffectClsid（`,6`），
//!   并带 `{B13412EE-...}` 设置上下文。
//! - 只修改端点 `FxProperties` 时，Windows 重启/重新枚举端点后可能从该模板
//!   重新灌入微软 APO，导致“设备默认效果”开启时微软 APO 与 VxAPO 同时工作。
//!
//! 本模块负责：
//! 1. 按端点设备实例 + 节点类型定位对应的 `MSFX\N` 模板；
//! 2. 把微软 StreamEffectClsid 替换为 VxAPO PreMix，并删除 ModeEffectClsid
//!   （避免 VxAPO PostMix 与微软 MFX/EFX 重复处理）；
//! 3. 在 VxAPO 安装信息区保存原始值，卸载时恢复微软默认效果。

use crate::install::device::slots::InstallMode;
use crate::install::device::slots::ApoSlot;
use crate::object::vx_reg_props::{CLSID_VXAPO_POST_MIX, CLSID_VXAPO_PRE_MIX};
use crate::sys::com::prelude::guid_to_string;
use crate::sys::registry::RegKey;
use crate::utils::vx_error::Result;
use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use once_cell::sync::Lazy;

// ── DeviceClasses 路径与 KS 分类 GUID ──────────────────────────────────────

const DEVICE_CLASSES_ROOT: &str = r"SYSTEM\CurrentControlSet\Control\DeviceClasses";
/// KSCATEGORY_RENDER（65E8773E-...）。
const KS_RENDER_CLASS: &str = "{65E8773E-8F56-11D0-A3B9-00A0C9223196}";
/// KSCATEGORY_AUDIO（6994AD04-...）。
const KS_AUDIO_CLASS: &str = "{6994AD04-93EF-11D0-A3CC-00A0C9223196}";

// ── FxProperties / CAPX 模板值名（audioenginebaseapo.h 实证）────────────────

/// `PKEY_FX_Association`（`,0`）：节点类型 GUID，用于把端点映射到 `MSFX\N`。
const PKEY_FX_ASSOCIATION: &str = "{D04E05A6-594B-4FB6-A80D-01AF5EED7D1D},0";
/// `PKEY_FX_StreamEffectClsid`（`,5`）：SFX / StreamEffect APO。
pub const PKEY_FX_STREAM_EFFECT_CLSID: &str =
    "{D04E05A6-594B-4FB6-A80D-01AF5EED7D1D},5";
/// `PKEY_FX_ModeEffectClsid`（`,6`）：MFX / ModeEffect APO。
pub const PKEY_FX_MODE_EFFECT_CLSID: &str =
    "{D04E05A6-594B-4FB6-A80D-01AF5EED7D1D},6";
/// 微软 WMALFXGFX APO 的设置上下文子键。
const WMALFX_CONTEXT: &str = "{B13412EE-07AF-4C57-B08B-E327F8DB085B}";

// ── 微软 CAPX APO CLSID（wdmaudio.inf FXCapX.AddReg 实证）──────────────────

/// WM LFX APO（StreamEffect）。
const MS_CAPX_STREAM_CLSID: &str = "{C9453E73-8C5C-4463-9984-AF8BAB2F5447}";
/// WM GFX APO（ModeEffect）。
const MS_CAPX_MODE_CLSID: &str = "{13AB3EBD-137E-4903-9D89-60BE8277FD17}";

// ── 端点 Properties 值名（Windows 11 实证）─────────────────────────────────

/// `PKEY_DeviceInstanceId`。
const PKEY_DEVICE_INSTANCE_ID: &str = "{B3F8FA53-0004-438E-9003-51A46E139BFC},2";
/// `PKEY_AudioEndpoint_JackSubType`（`,8`）＝端点 KS 节点类型 GUID，
/// 与 `MSFX\N` 的 `PKEY_FX_Association`（`,0`）对应。
const PKEY_AUDIO_ENDPOINT_ASSOCIATION: &str =
    "{1DA5D803-D492-4EDD-8C23-E0C0FFEE7F0E},8";

/// VxAPO 安装信息区中保存的 MSFX 原始值（REG_MULTI_SZ）。
pub const SYSFX_BACKUP_VALUE: &str = "SysFxBackups";

// ── 变更描述 ───────────────────────────────────────────────────────────────

/// 对某个注册表值的一次接管/恢复变更。
///
/// - `original`：变更前的 REG_SZ 值（None 表示原本不存在）。
/// - `target`：变更后的值（None 表示删除该值）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SysFxChange {
    pub key_path: String,
    pub value_name: String,
    pub original: Option<String>,
    pub target: Option<String>,
}

/// 单个 `MSFX\N` 键的原始值备份（供卸载恢复）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SysFxBackup {
    pub key_path: String,
    /// `PKEY_FX_StreamEffectClsid` 原始值；None = 原本不存在。
    pub stream: Option<String>,
    /// `PKEY_FX_ModeEffectClsid` 原始值；None = 原本不存在。
    pub mode: Option<String>,
}

// ── 公开 API ───────────────────────────────────────────────────────────────

/// 从端点根键读取设备实例 ID 与节点类型。
///
/// 返回 `(device_instance_id, node_type_guid)`，缺失时为 `None`。
pub fn endpoint_identity(endpoint_key: &RegKey) -> (Option<String>, Option<String>) {
    let props = match endpoint_key.open_sub_key("Properties") {
        Ok(k) => k,
        Err(_) => return (None, None),
    };
    (
        props.read_sz(PKEY_DEVICE_INSTANCE_ID),
        props.read_sz(PKEY_AUDIO_ENDPOINT_ASSOCIATION),
    )
}

/// 查找与指定端点对应的 `MSFX\N` 模板键（HKLM 相对路径列表）。
///
/// 匹配规则：
/// - 设备实例 ID 归一化后出现在 DeviceClasses 实例键名中；
/// - `MSFX\N` 的 `PKEY_FX_Association` 与端点节点类型一致；
/// - 键内存在微软 CAPX APO 或 `{B13412EE-...}` 上下文，才会纳入接管范围。
pub fn find_msfx_entries(
    device_id: Option<&str>,
    node_type: Option<&str>,
) -> Result<Vec<String>> {
    let mut result = Vec::new();
    let Some(device_id) = device_id else {
        return Ok(result);
    };
    let normalized = normalize_device_id(device_id);
    if normalized.is_empty() {
        return Ok(result);
    }

    // v9.6 快速路径：DeviceClasses 实例键名 = `##?#{归一化设备ID}#{KS类GUID}`
    // （USB 等标准设备实证，大小写不敏感）。直接构造候选路径，把「首次切换到
    // 新端点时 Initialize 的数百次注册表打开」降为几次——修复切换设备后
    // 首秒音频断续慢速（自愈全树扫描阻塞音频服务控制线程）。
    for class_guid in [KS_RENDER_CLASS, KS_AUDIO_CLASS] {
        let candidates = [
            format!("{DEVICE_CLASSES_ROOT}\\{class_guid}\\##?#{normalized}#{class_guid}"),
            format!("{DEVICE_CLASSES_ROOT}\\{class_guid}\\{normalized}#{class_guid}"),
        ];
        for instance_path in candidates {
            if RegKey::open(HKEY_LOCAL_MACHINE, &instance_path).is_err() {
                continue;
            }
            collect_msfx_from_instance(&instance_path, node_type, &mut result)?;
        }
    }
    if !result.is_empty() {
        return Ok(result);
    }

    // 回退：完整树枚举（非标准实例名变体，如虚拟设备）。
    for class_guid in [KS_RENDER_CLASS, KS_AUDIO_CLASS] {
        let class_path = format!("{DEVICE_CLASSES_ROOT}\\{class_guid}");
        let class_key = match RegKey::open(HKEY_LOCAL_MACHINE, &class_path) {
            Ok(k) => k,
            Err(_) => continue,
        };

        let instances = match class_key.enum_sub_keys() {
            Ok(v) => v,
            Err(_) => continue,
        };
        for instance in instances {
            if !instance.to_lowercase().contains(&normalized) {
                continue;
            }
            let instance_path = format!("{}\\{}", class_path, instance);
            collect_msfx_from_instance(&instance_path, node_type, &mut result)?;
        }
    }

    Ok(result)
}

/// 枚举单个 DeviceClasses 实例下所有引用（如 `#GLOBAL`）的 `MSFX\N` 条目，
/// 命中端点节点类型的条目写入 `result`（快速路径与全树回退共用，v9.6）。
fn collect_msfx_from_instance(
    instance_path: &str,
    node_type: Option<&str>,
    result: &mut Vec<String>,
) -> Result<()> {
    let instance_key = match RegKey::open(HKEY_LOCAL_MACHINE, instance_path) {
        Ok(k) => k,
        Err(_) => return Ok(()),
    };
    let references = match instance_key.enum_sub_keys() {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    for reference in references {
        let msfx_root = format!(
            "{}\\{}\\Device Parameters\\MSFX",
            instance_path, reference
        );
        let msfx_key = match RegKey::open(HKEY_LOCAL_MACHINE, &msfx_root) {
            Ok(k) => k,
            Err(_) => continue,
        };
        let indexes = match msfx_key.enum_sub_keys() {
            Ok(v) => v,
            Err(_) => continue,
        };
        for index in indexes {
            let entry_path = format!("{}\\{}", msfx_root, index);
            if matches_endpoint(&entry_path, node_type)? {
                result.push(entry_path);
            }
        }
    }
    Ok(())
}

/// 计算接管 `MSFX\N` 模板所需的注册表变更。
///
/// - 默认/EFX 模式：把 `,5` 换成 VxAPO PreMix，删除 `,6`（微软 MFX 不再加载）；
/// - SfxMfx 模式：把 `,5` 换成 VxAPO PreMix，`,6` 换成 VxAPO PostMix。
pub fn plan_msfx_takeover(
    paths: &[String],
    mode: InstallMode,
    install_premix: bool,
    install_postmix: bool,
) -> Result<Vec<SysFxChange>> {
    let mut changes = Vec::new();
    for path in paths {
        let key = RegKey::open(HKEY_LOCAL_MACHINE, path)?;
        let stream = key.read_sz(PKEY_FX_STREAM_EFFECT_CLSID);
        let mode_effect = key.read_sz(PKEY_FX_MODE_EFFECT_CLSID);
        let has_context = key
            .key_exists_child(WMALFX_CONTEXT)
            .unwrap_or(false);

        let stream_is_ms = stream
            .as_deref()
            .is_some_and(is_ms_stream_clsid);
        let stream_is_vxapo = stream
            .as_deref()
            .is_some_and(is_vxapo_clsid);
        if !(stream_is_ms || stream_is_vxapo || has_context) {
            continue;
        }

        // StreamEffect 槽位：仅当存在微软/旧 VxAPO 值时才替换。
        if install_premix && (stream_is_ms || stream_is_vxapo) && !stream_is_vxapo {
            changes.push(SysFxChange {
                key_path: path.clone(),
                value_name: PKEY_FX_STREAM_EFFECT_CLSID.to_string(),
                original: stream,
                target: Some(guid_to_string(&CLSID_VXAPO_PRE_MIX)),
            });
        }

        // ModeEffect 槽位：SfxMfx 用 VxAPO PostMix，其余模式删除微软 MFX。
        let mode_is_ms = mode_effect
            .as_deref()
            .is_some_and(is_ms_mode_clsid);
        let mode_is_vxapo = mode_effect
            .as_deref()
            .is_some_and(is_vxapo_clsid);
        if mode == InstallMode::SfxMfx && install_postmix {
            if mode_is_ms && !mode_is_vxapo {
                changes.push(SysFxChange {
                    key_path: path.clone(),
                    value_name: PKEY_FX_MODE_EFFECT_CLSID.to_string(),
                    original: mode_effect,
                    target: Some(guid_to_string(&CLSID_VXAPO_POST_MIX)),
                });
            }
        } else if mode_is_ms || mode_is_vxapo {
            changes.push(SysFxChange {
                key_path: path.clone(),
                value_name: PKEY_FX_MODE_EFFECT_CLSID.to_string(),
                original: mode_effect,
                target: None,
            });
        }
    }
    Ok(changes)
}

/// 根据安装时保存的备份生成恢复微软默认效果的变更。
pub fn plan_msfx_restore(backups: &[SysFxBackup]) -> Result<Vec<SysFxChange>> {
    let mut changes = Vec::new();
    for backup in backups {
        let key = RegKey::open(HKEY_LOCAL_MACHINE, &backup.key_path)?;
        let current_stream = key.read_sz(PKEY_FX_STREAM_EFFECT_CLSID);
        let current_mode = key.read_sz(PKEY_FX_MODE_EFFECT_CLSID);

        // StreamEffect：只有当前仍是 VxAPO 时才恢复/删除，绝不覆盖第三方 APO。
        match &backup.stream {
            Some(original) => {
                let needs_restore = current_stream.is_none()
                    || current_stream
                        .as_deref()
                        .is_some_and(is_vxapo_clsid);
                if needs_restore {
                    changes.push(SysFxChange {
                        key_path: backup.key_path.clone(),
                        value_name: PKEY_FX_STREAM_EFFECT_CLSID.to_string(),
                        original: current_stream,
                        target: Some(original.clone()),
                    });
                }
            }
            None => {
                if current_stream
                    .as_deref()
                    .is_some_and(is_vxapo_clsid)
                {
                    changes.push(SysFxChange {
                        key_path: backup.key_path.clone(),
                        value_name: PKEY_FX_STREAM_EFFECT_CLSID.to_string(),
                        original: current_stream,
                        target: None,
                    });
                }
            }
        }

        // ModeEffect：当前是 VxAPO 或已被我们删除（None）时恢复原始值。
        match &backup.mode {
            Some(original) => {
                let needs_restore = current_mode
                    .as_deref()
                    .is_some_and(is_vxapo_clsid)
                    || current_mode.is_none();
                if needs_restore {
                    changes.push(SysFxChange {
                        key_path: backup.key_path.clone(),
                        value_name: PKEY_FX_MODE_EFFECT_CLSID.to_string(),
                        original: current_mode,
                        target: Some(original.clone()),
                    });
                }
            }
            None => {
                if current_mode
                    .as_deref()
                    .is_some_and(is_vxapo_clsid)
                {
                    changes.push(SysFxChange {
                        key_path: backup.key_path.clone(),
                        value_name: PKEY_FX_MODE_EFFECT_CLSID.to_string(),
                        original: current_mode,
                        target: None,
                    });
                }
            }
        }
    }
    Ok(changes)
}

/// 无备份时的兜底恢复：把仍是 VxAPO 的 `MSFX\N` 恢复为微软 CAPX 默认值。
pub fn plan_msfx_restore_defaults(paths: &[String]) -> Result<Vec<SysFxChange>> {
    let mut changes = Vec::new();
    for path in paths {
        let key = RegKey::open(HKEY_LOCAL_MACHINE, path)?;
        let current_stream = key.read_sz(PKEY_FX_STREAM_EFFECT_CLSID);
        let current_mode = key.read_sz(PKEY_FX_MODE_EFFECT_CLSID);

        if current_stream
            .as_deref()
            .is_some_and(is_vxapo_clsid)
        {
            changes.push(SysFxChange {
                key_path: path.clone(),
                value_name: PKEY_FX_STREAM_EFFECT_CLSID.to_string(),
                original: current_stream,
                target: Some(MS_CAPX_STREAM_CLSID.to_string()),
            });
            if current_mode.is_none() {
                // 安装时删除了微软 MFX；无备份也按 CAPX 默认值恢复。
                changes.push(SysFxChange {
                    key_path: path.clone(),
                    value_name: PKEY_FX_MODE_EFFECT_CLSID.to_string(),
                    original: None,
                    target: Some(MS_CAPX_MODE_CLSID.to_string()),
                });
            }
        }
        if current_mode
            .as_deref()
            .is_some_and(is_vxapo_clsid)
        {
            changes.push(SysFxChange {
                key_path: path.clone(),
                value_name: PKEY_FX_MODE_EFFECT_CLSID.to_string(),
                original: current_mode,
                target: Some(MS_CAPX_MODE_CLSID.to_string()),
            });
        }
    }
    Ok(changes)
}

/// 把变更列表压缩成按 `MSFX\N` 键分组的原始值备份。
pub fn changes_to_backups(changes: &[SysFxChange]) -> Vec<SysFxBackup> {
    let mut backups: Vec<SysFxBackup> = Vec::new();
    for change in changes {
        let entry = backups
            .iter_mut()
            .find(|b| b.key_path == change.key_path);
        let entry = match entry {
            Some(e) => e,
            None => {
                backups.push(SysFxBackup {
                    key_path: change.key_path.clone(),
                    stream: None,
                    mode: None,
                });
                backups.last_mut().expect("just pushed")
            }
        };
        if change.value_name == PKEY_FX_STREAM_EFFECT_CLSID {
            entry.stream = change.original.clone();
        } else if change.value_name == PKEY_FX_MODE_EFFECT_CLSID {
            entry.mode = change.original.clone();
        }
    }
    backups
}

/// 编码备份为 REG_MULTI_SZ 行（`key_path|stream|mode`，None 用 `-`）。
pub fn encode_backups(backups: &[SysFxBackup]) -> Vec<String> {
    backups
        .iter()
        .map(|b| {
            format!(
                "{}|{}|{}",
                b.key_path,
                b.stream.as_deref().unwrap_or("-"),
                b.mode.as_deref().unwrap_or("-")
            )
        })
        .collect()
}

/// 解码 [`encode_backups`] 生成的 REG_MULTI_SZ。
pub fn decode_backups(values: &[String]) -> Vec<SysFxBackup> {
    values
        .iter()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '|');
            let key_path = parts.next()?;
            let stream = parts.next()?;
            let mode = parts.next()?;
            Some(SysFxBackup {
                key_path: key_path.to_string(),
                stream: (stream != "-").then(|| stream.to_string()),
                mode: (mode != "-").then(|| mode.to_string()),
            })
        })
        .collect()
}

// ── 内部辅助 ───────────────────────────────────────────────────────────────

/// 设备实例 ID（如 `{1}.USB\VID_...\6&...`）→ DeviceClasses 实例键的匹配片段
/// （如 `USB#VID_...#6&...`，小写）。
fn normalize_device_id(device_id: &str) -> String {
    let trimmed = device_id.trim();
    // 去掉 Windows 实例 ID 的 `{N}.` 前缀；没有前缀时原样保留。
    // 例如 `{1}.USB\VID_...` → `USB\VID_...`；
    // `{2}.\\?\usb#...` → `\\?\usb#...`（随后 `\` 归一为 `#`）。
    let after_prefix = trimmed
        .find("}.")
        .map(|i| &trimmed[i + 2..])
        .unwrap_or(trimmed);
    after_prefix.replace('\\', "#").to_lowercase()
}

/// 判断 `MSFX\N` 是否属于指定端点的节点类型，且是微软 CAPX 效果。
fn matches_endpoint(entry_path: &str, node_type: Option<&str>) -> Result<bool> {
    let key = match RegKey::open(HKEY_LOCAL_MACHINE, entry_path) {
        Ok(k) => k,
        Err(_) => return Ok(false),
    };

    if let Some(expected) = node_type {
        let actual = key.read_sz(PKEY_FX_ASSOCIATION);
        match actual {
            Some(a) if a.eq_ignore_ascii_case(expected) => {}
            _ => return Ok(false),
        }
    }

    let has_ms_clsid = key
        .read_sz(PKEY_FX_STREAM_EFFECT_CLSID)
        .as_deref()
        .is_some_and(is_ms_stream_clsid)
        || key
            .read_sz(PKEY_FX_MODE_EFFECT_CLSID)
            .as_deref()
            .is_some_and(is_ms_mode_clsid);
    let has_context = key.key_exists_child(WMALFX_CONTEXT).unwrap_or(false);
    Ok(has_ms_clsid || has_context)
}

pub fn is_ms_stream_clsid(v: &str) -> bool {
    v.eq_ignore_ascii_case(MS_CAPX_STREAM_CLSID)
}

pub fn is_ms_mode_clsid(v: &str) -> bool {
    v.eq_ignore_ascii_case(MS_CAPX_MODE_CLSID)
}

pub fn is_vxapo_clsid(v: &str) -> bool {
    let pre = guid_to_string(&CLSID_VXAPO_PRE_MIX);
    let post = guid_to_string(&CLSID_VXAPO_POST_MIX);
    v.eq_ignore_ascii_case(&pre) || v.eq_ignore_ascii_case(&post)
}

/// 单个 `MSFX\N` 条目的运行期自愈动作（纯决策，可单测）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealAction {
    /// 无需变更。
    Noop,
    /// 把微软 StreamEffect 替换为 VxAPO PreMix，并删除微软 ModeEffect。
    ReplaceStreamDeleteMode,
    /// 只删除微软 ModeEffect（StreamEffect 已是 VxAPO 或第三方）。
    DeleteModeOnly,
}

/// 判断单个 `MSFX\N` 条目是否需要运行期接管（v9.4）。
///
/// 铁律：**只动微软 CAPX**——stream 是微软 CAPX → 替换为 VxAPO PreMix；
/// mode 是微软 CAPX → 删除；含 WMALFX 上下文但值缺失也视为微软条目。
/// 第三方 APO 一律不动。
pub fn msfx_heal_action(
    stream: Option<&str>,
    mode: Option<&str>,
    has_context: bool,
) -> HealAction {
    let stream_is_ms = stream.is_some_and(is_ms_stream_clsid);
    let mode_is_ms = mode.is_some_and(is_ms_mode_clsid);
    let stream_is_vx = stream.is_some_and(is_vxapo_clsid);

    if stream_is_ms {
        return HealAction::ReplaceStreamDeleteMode;
    }
    if mode_is_ms || (has_context && stream_is_vx) {
        return HealAction::DeleteModeOnly;
    }
    HealAction::Noop
}

/// 运行期自愈：Windows 重新枚举/重启后可能从驱动模板把微软 CAPX 重新灌回
/// `MSFX\N`（与 VxAPO 同时加载 → 断断续续/慢放，v9.0 仅安装时接管不够）。
///
/// 本函数在本 DLL 被加载（`Initialize`，控制线程）时调用：
/// - 仅当端点 FxProperties 已装 VxAPO（本 DLL 管理该端点）才动作；
/// - 只替换/删除微软 CAPX CLSID，绝不覆盖第三方 APO；
/// - 与安装 Step 7 对齐，同时删除禁用增强链的值，保证接管生效；
/// - 幂等：无微软条目时零写入；失败仅返回 Err，调用方降级日志。
pub fn ensure_takeover_for_endpoint(endpoint_path: &str) -> Result<()> {
    // 1. 端点必须由 VxAPO 管理（任一槽位含 VxAPO CLSID）。
    let fx_path = format!("{}\\{}", endpoint_path, "FxProperties");
    let fx = match RegKey::open(HKEY_LOCAL_MACHINE, &fx_path) {
        Ok(k) => k,
        Err(_) => return Ok(()),
    };
    let managed = ApoSlot::ALL.iter().any(|slot| {
        fx.read_sz(&ApoSlot::value_name(*slot))
            .as_deref()
            .is_some_and(is_vxapo_clsid)
    });
    if !managed {
        return Ok(());
    }

    // 1b. 强制启用增强链（与 install Step 7 对齐：删除 DisableEnhancements /
    //     PKEY_AudioEndpoint_Disable_SysFx），否则接管了模板也可能整链被禁用。
    //     该动作廉价且必须，不参与扫描缓存。
    if let Ok(fx_write) = RegKey::open_for_write(HKEY_LOCAL_MACHINE, &fx_path) {
        let _ = fx_write.delete_value("DisableEnhancements");
        let _ = fx_write.delete_value("{1da5d803-d492-4edd-8c23-e0c0ffee7f0e},5");
    }

    // 1c. MSFX 模板扫描缓存（v9.5）：`find_msfx_entries` 要遍历 DeviceClasses
    //     两个 KS 类下全部实例，成本不低；而设置页/多流启动可能高频实例化本 APO
    //     （每次 Initialize 都会走到这里）。自愈是“设备重新枚举后兜底”，30 秒
    //     粒度完全足够——命中缓存直接跳过扫描，避免拖慢音频服务/设置页。
    {
        static LAST_SCAN: Lazy<Mutex<HashMap<String, Instant>>> =
            Lazy::new(|| Mutex::new(HashMap::new()));
        let mut cache = LAST_SCAN.lock().unwrap_or_else(|e| e.into_inner());
        if cache
            .get(endpoint_path)
            .is_some_and(|last| last.elapsed() < Duration::from_secs(30))
        {
            return Ok(());
        }
        cache.insert(endpoint_path.to_owned(), Instant::now());
    }

    // 2. 定位 MSFX 模板（失败视为无模板，不阻塞）。
    let endpoint_key = RegKey::open(HKEY_LOCAL_MACHINE, endpoint_path)?;
    let (device_id, node_type) = endpoint_identity(&endpoint_key);
    let paths = find_msfx_entries(device_id.as_deref(), node_type.as_deref())?;
    if paths.is_empty() {
        return Ok(());
    }

    // 3. 逐条目按纯决策接管。
    for path in &paths {
        let key = match RegKey::open(HKEY_LOCAL_MACHINE, path) {
            Ok(k) => k,
            Err(_) => continue,
        };
        let stream = key.read_sz(PKEY_FX_STREAM_EFFECT_CLSID);
        let mode = key.read_sz(PKEY_FX_MODE_EFFECT_CLSID);
        let has_context = key.key_exists_child(WMALFX_CONTEXT).unwrap_or(false);
        match msfx_heal_action(stream.as_deref(), mode.as_deref(), has_context) {
            HealAction::Noop => {}
            HealAction::ReplaceStreamDeleteMode => {
                let write_key = RegKey::open_for_write(HKEY_LOCAL_MACHINE, path)?;
                write_key.write_sz(
                    PKEY_FX_STREAM_EFFECT_CLSID,
                    &guid_to_string(&CLSID_VXAPO_PRE_MIX),
                )?;
                let _ = write_key.delete_value(PKEY_FX_MODE_EFFECT_CLSID);
            }
            HealAction::DeleteModeOnly => {
                let write_key = RegKey::open_for_write(HKEY_LOCAL_MACHINE, path)?;
                let _ = write_key.delete_value(PKEY_FX_MODE_EFFECT_CLSID);
            }
        }
    }

    Ok(())
}

// ── 测试 ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_device_id_drops_prefix_and_converts_separators() {
        assert_eq!(
            normalize_device_id("{1}.USB\\VID_2D99&PID_A037&MI_00\\6&20BE7186&2&0000"),
            "usb#vid_2d99&pid_a037&mi_00#6&20be7186&2&0000"
        );
    }

    #[test]
    fn normalize_device_id_without_prefix() {
        assert_eq!(
            normalize_device_id("USB\\VID_1234&PID_5678\\1&0"),
            "usb#vid_1234&pid_5678#1&0"
        );
    }

    #[test]
    fn fast_path_candidate_matches_known_instance_name() {
        // v9.6：快速路径构造的实例键名应与安装时 SysFxBackups 实证路径一致。
        let normalized =
            normalize_device_id("{1}.USB\\VID_2D99&PID_A037&MI_00\\6&20BE7186&2&0000");
        let candidate = format!(
            "{DEVICE_CLASSES_ROOT}\\{KS_RENDER_CLASS}\\##?#{normalized}#{KS_RENDER_CLASS}"
        );
        let known = r"SYSTEM\CurrentControlSet\Control\DeviceClasses\{65E8773E-8F56-11D0-A3B9-00A0C9223196}\##?#USB#VID_2D99&PID_A037&MI_00#6&20BE7186&2&0000#{65e8773e-8f56-11d0-a3b9-00a0c9223196}";
        assert!(
            candidate.eq_ignore_ascii_case(known),
            "candidate={candidate}\nknown={known}"
        );
    }

    #[test]
    fn ms_clsid_matchers_case_insensitive() {
        assert!(is_ms_stream_clsid("{c9453e73-8c5c-4463-9984-af8bab2f5447}"));
        assert!(is_ms_mode_clsid("{13AB3EBD-137E-4903-9D89-60BE8277FD17}"));
        assert!(!is_ms_stream_clsid("{00000000-0000-0000-0000-000000000000}"));
    }

    #[test]
    fn vxapo_clsid_matcher_accepts_both_clsids() {
        assert!(is_vxapo_clsid(&guid_to_string(&CLSID_VXAPO_PRE_MIX)));
        assert!(is_vxapo_clsid(&guid_to_string(&CLSID_VXAPO_POST_MIX)));
        assert!(!is_vxapo_clsid("{C9453E73-8C5C-4463-9984-AF8BAB2F5447}"));
    }

    #[test]
    fn msfx_heal_action_replaces_only_ms_capx() {
        let ms_stream = Some(MS_CAPX_STREAM_CLSID);
        let ms_mode = Some(MS_CAPX_MODE_CLSID);
        let vx_str = guid_to_string(&CLSID_VXAPO_PRE_MIX);
        let vx = Some(vx_str.as_str());
        let third_party = Some("{6861CFDC-0461-49D5-A8DF-BE5ACD02692F}");

        // 微软 StreamEffect → 替换 + 删微软 ModeEffect。
        assert_eq!(
            msfx_heal_action(ms_stream, ms_mode, true),
            HealAction::ReplaceStreamDeleteMode
        );
        assert_eq!(
            msfx_heal_action(ms_stream, None, false),
            HealAction::ReplaceStreamDeleteMode
        );
        // StreamEffect 已是 VxAPO + 微软 ModeEffect → 只删 ModeEffect。
        assert_eq!(
            msfx_heal_action(vx, ms_mode, true),
            HealAction::DeleteModeOnly
        );
        // 只有微软 ModeEffect → 只删。
        assert_eq!(
            msfx_heal_action(None, ms_mode, false),
            HealAction::DeleteModeOnly
        );
        // 第三方一律不动（即使有 WMALFX 上下文残留，只要不是微软值也不动）。
        assert_eq!(
            msfx_heal_action(third_party, third_party, true),
            HealAction::Noop
        );
        assert_eq!(msfx_heal_action(None, None, true), HealAction::Noop);
    }

    #[test]
    fn changes_to_backups_groups_by_path() {
        let path = r"SYSTEM\X\MSFX\0".to_string();
        let changes = vec![
            SysFxChange {
                key_path: path.clone(),
                value_name: PKEY_FX_STREAM_EFFECT_CLSID.to_string(),
                original: Some(MS_CAPX_STREAM_CLSID.to_string()),
                target: Some(guid_to_string(&CLSID_VXAPO_PRE_MIX)),
            },
            SysFxChange {
                key_path: path.clone(),
                value_name: PKEY_FX_MODE_EFFECT_CLSID.to_string(),
                original: Some(MS_CAPX_MODE_CLSID.to_string()),
                target: None,
            },
        ];
        let backups = changes_to_backups(&changes);
        assert_eq!(backups.len(), 1);
        assert_eq!(backups[0].stream.as_deref(), Some(MS_CAPX_STREAM_CLSID));
        assert_eq!(backups[0].mode.as_deref(), Some(MS_CAPX_MODE_CLSID));
    }

    #[test]
    fn backup_roundtrip() {
        let backups = vec![SysFxBackup {
            key_path: r"SYSTEM\A\MSFX\0".to_string(),
            stream: Some(MS_CAPX_STREAM_CLSID.to_string()),
            mode: None,
        }];
        let encoded = encode_backups(&backups);
        let decoded = decode_backups(&encoded);
        assert_eq!(decoded, backups);
    }

    #[test]
    fn backup_decode_handles_missing_mode() {
        let decoded = decode_backups(&[
            r"SYSTEM\A\MSFX\1|{C9453E73-8C5C-4463-9984-AF8BAB2F5447}|-".to_string(),
        ]);
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].stream.as_deref(), Some(MS_CAPX_STREAM_CLSID));
        assert_eq!(decoded[0].mode, None);
    }

    #[test]
    fn encode_uses_dash_for_none() {
        let backups = vec![SysFxBackup {
            key_path: "K".to_string(),
            stream: None,
            mode: None,
        }];
        assert_eq!(encode_backups(&backups), vec!["K|-|-"]);
    }
}
