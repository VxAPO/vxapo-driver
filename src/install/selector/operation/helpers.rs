//! install/selector/operation/helpers.rs — 注册表写入内部辅助

//! 共享导入见父模块 install/selector/operation.rs。

use super::*;
use super::execute::*;

/// 确保 FxProperties 子键存在。
///
/// 返回 `(key, is_new)`。
///
/// **只读句柄 bug（实证）**：旧实现已存在时用 `RegKey::open`（SAM_READ）
/// 返回——后续 `write_child_apo_config` / `write_apo_slot` / `delete_value` 对只读句柄
/// 全部拒绝访问（0x80070005），即使进程是管理员。已存在时必须重新以 SAM_ALL 打开
/// （`RegKey::create`），is_new 判定仍是读 `version` 值。
pub(super) fn ensure_fx_properties(fx_path: &str, tx: &mut Transaction) -> Result<(RegKey, bool)> {
    // 已存在：读 version 判定 is_new，然后以 KEY_SET_VALUE 最小写权限打开。
    // （Windows 对 MMDevices 端点键只授予管理员 SetValue,ReadKey——请求
    //   KEY_ALL_ACCESS（含 CreateSubKey 位）会被拒绝 0x80070005。）
    if RegKey::open(HKEY_LOCAL_MACHINE, fx_path).is_ok() {
        let is_new = match RegKey::open(HKEY_LOCAL_MACHINE, fx_path) {
            Ok(k) => !k.value_exists("version").unwrap_or(false),
            Err(_) => true,
        };
        let key = RegKey::open_for_write(HKEY_LOCAL_MACHINE, fx_path)?;
        if is_new {
            tx.record(RollbackAction::DeleteKey {
                root: HKEY_LOCAL_MACHINE,
                path: fx_path.to_string(),
            });
        }
        return Ok((key, is_new));
    }

    // 不存在 → 创建（SAM_ALL）。
    let key = RegKey::create(HKEY_LOCAL_MACHINE, fx_path)?;
    tx.record(RollbackAction::DeleteKey {
        root: HKEY_LOCAL_MACHINE,
        path: fx_path.to_string(),
    });
    Ok((key, true))
}

/// 记录槽位值到事务（用于安装失败回滚）。
pub(super) fn record_slot_backups(
    fx_key: &RegKey,
    mode: InstallMode,
    fx_path: &str,
    tx: &mut Transaction,
) {
    for slot in [mode.premix_slot(), mode.postmix_slot()] {
        if let SlotValue::Guid(g) = read_slot_value(fx_key, slot) {
            tx.record(RollbackAction::RestoreValue {
                key_path: fx_path.to_string(),
                name: slot.value_name(),
                backup: guid_to_string(&g),
            });
        }
    }
}

/// 读取原始 APO GUID（用于子 APO 保留）。
///
/// **self-preserve 过滤（实证）**：重装时槽位可能已是 VxAPO 自己的
/// CLSID——必须视为「无原始 APO」，否则会把 VxAPO 自身保留为子 APO
/// （快照 diff 实测 childPreMix=41C34613 自占）。
pub(super) fn read_original_apo_guids(
    fx_key: &RegKey,
    config: &InstallConfig,
) -> (Option<GUID>, Option<GUID>) {
    let premix = if config.use_original_apo_premix {
        let g = read_slot_value(fx_key, config.install_mode.premix_slot()).as_guid();
        match g {
            Some(g) if g != CLSID_VXAPO_PRE_MIX && g != CLSID_VXAPO_POST_MIX => Some(g),
            _ => None,
        }
    } else {
        None
    };
    let postmix = if config.use_original_apo_postmix {
        let g = read_slot_value(fx_key, config.install_mode.postmix_slot()).as_guid();
        match g {
            Some(g) if g != CLSID_VXAPO_PRE_MIX && g != CLSID_VXAPO_POST_MIX => Some(g),
            _ => None,
        }
    } else {
        None
    };
    (premix, postmix)
}

/// 独立安装信息区中的「被覆盖槽位」备份值名。
///
/// install 覆盖 PreMix/PostMix 槽位前，把「原槽位值 + 原槽位名」备份到信息区
/// （EAPO 源码 DeviceAPOInfo.cpp C 节同款 per-device 备份行为）。uninstall 时
/// 按此恢复被覆盖的第三方 APO（EAPO 等），否则快照恢复后槽位变 NoValue。
pub(super) const BACKUP_PREMIX_SLOT: &str = "PreMixSlot";
pub(super) const BACKUP_POSTMIX_SLOT: &str = "PostMixSlot";
pub(super) const BACKUP_PREMIX_SLOT_VALUE: &str = "PreMixSlotValue";
pub(super) const BACKUP_POSTMIX_SLOT_VALUE: &str = "PostMixSlotValue";

/// 写入子 APO 配置（Step 1， 独立安装信息区）。
///
/// - 保留的原始 APO GUID → `HKLM\SOFTWARE\VxAPO\Child APOs\{deviceGuid}\
///   {PreMixChild|PostMixChild}`（与运行期 `object/child.rs` /
///   `slots::read_child_apo_guid` 读取路径一致）。
/// - 被覆盖前的槽位名 → `{PreMixSlot|PostMixSlot}`（uninstall 恢复槽位值用）。
/// - 设备稳定身份 → `{DeviceInstanceId|DeviceHardwareIds|DeviceProductName|
///   EndpointHistory}`（`identity.rs`；Windows 重排端点 GUID 后靠它把新 GUID
///   认回同一设备，见 `stale.rs` 分层匹配）。
/// - allowSilentBuffer / autoAdjust / version → FxProperties 值
///   （`info.rs::read_install_version` 依 version 判定安装状态）。
pub(super) fn write_child_apo_config(
    device_guid: &str,
    endpoint_path: &str,
    fx_key: &RegKey,
    config: &InstallConfig,
    original_premix: Option<GUID>,
    original_postmix: Option<GUID>,
    tx: &mut Transaction,
) -> Result<()> {
    // 独立安装信息区：HKLM\SOFTWARE\VxAPO\Child APOs\{device_guid}
    // （HKLM\SOFTWARE 管理员可建子键；require_admin 探测键同区已验证）。
    let info_key = format!("{}\\{}", CHILD_APO_PATH_ROOT, device_guid);
    let (root, sub_key) = split_hklm_path(&info_key)?;
    let info = RegKey::create(root, sub_key)?;
    // （审查 #8）：新建信息区必须登记回滚——安装中途失败时随事务一起删除，
    // 否则残留信息区会让下次安装误判为“非全量路径”。
    tx.record(RollbackAction::DeleteKey {
        root: HKEY_LOCAL_MACHINE,
        path: info_key,
    });

    // PreMixChild / PostMixChild — 保留的原始 APO GUID（有才写）。
    if let Some(g) = original_premix {
        info.write_sz(ChildApoKind::PreMix.value_name(), &guid_to_string(&g))?;
    }
    if let Some(g) = original_postmix {
        info.write_sz(ChildApoKind::PostMix.value_name(), &guid_to_string(&g))?;
    }

    // 被覆盖槽位名备份（无论是否有 child，都要记：uninstall 需要知道删哪个槽位、
    // 恢复时写回哪个槽位；写 VxAPO 前槽位还是原 APO，此刻读即原值）。
    info.write_sz(BACKUP_PREMIX_SLOT, &config.install_mode.premix_slot().value_name())?;
    info.write_sz(BACKUP_POSTMIX_SLOT, &config.install_mode.postmix_slot().value_name())?;

    // 被覆盖槽位原值备份（**无条件**——uninstall 恢复槽位必须用它把 EAPO 等
    // 第三方 APO 写回；child GUID 只在保留时写，原值备份始终写）。
    if let SlotValue::Guid(g) = read_slot_value(fx_key, config.install_mode.premix_slot()) {
        info.write_sz(BACKUP_PREMIX_SLOT_VALUE, &guid_to_string(&g))?;
    }
    if let SlotValue::Guid(g) = read_slot_value(fx_key, config.install_mode.postmix_slot()) {
        info.write_sz(BACKUP_POSTMIX_SLOT_VALUE, &guid_to_string(&g))?;
    }

    // 设备稳定身份（值，不是子键——迁移的 copy_values 只搬值；写成子键会丢）。
    let identity = read_endpoint_identity(endpoint_path);
    let history = merge_endpoint_history(&[
        identity.endpoint_history.clone(),
        vec![device_guid.to_string()],
    ]);
    write_identity_values(&info, &identity, &history)?;

    // 控制开关 → FxProperties（注意：本键由调用方以 KEY_SET_VALUE 打开，
    // 仅写值所需的最小权限）。
    fx_key.write_dword("allowSilentBuffer", config.allow_silent_buffer as u32)?;
    fx_key.write_dword("autoAdjust", config.auto_adjust as u32)?;
    fx_key.write_sz("version", INSTALL_VERSION)?;

    Ok(())
}

/// 拆分 `HKLM\...` 完整路径为(root HKEY, 子键路径)。
pub(super) fn split_hklm_path(path: &str) -> Result<(windows::Win32::System::Registry::HKEY, &str)> {
    let (root_str, rest) = path
        .split_once('\\')
        .ok_or_else(|| VxApoError::internal(&format!("路径无根键：{path}")))?;
    if !root_str.eq_ignore_ascii_case("HKLM") {
        return Err(VxApoError::internal(&format!("仅支持 HKLM 根：{path}")));
    }
    Ok((HKEY_LOCAL_MACHINE, rest))
}

/// 用已有 CLSID→DLL 绑定路径刷新全局 APO 注册。
///
/// 幂等：读 `HKCR\CLSID\{PreMix}\InprocServer32` 的 DLL 路径后调用
/// `register_apo_with_path`，补写/覆盖 `AudioEngine\AudioProcessingObjects` 完整字段。
/// 无绑定（从未 regsvr32）时跳过——CLI 安装前会自动注册，DllRegisterServer 也可单独做。
pub(super) fn refresh_global_registration() -> Result<()> {
    let clsid_str = guid_to_string(&CLSID_VXAPO_PRE_MIX);
    let binding = format!(r"CLSID\{}\InprocServer32", clsid_str);
    let dll_path = RegKey::open(HKEY_CLASSES_ROOT, &binding)
        .and_then(|k| k.read_sz_value(""))
        .ok()
        .filter(|p| !p.is_empty());
    if let Some(path) = dll_path {
        let hr = crate::object::dll_exports::register_apo_with_path(&path);
        if hr.0 != 0 {
            return Err(VxApoError::internal(&format!(
                "刷新全局 APO 注册失败：{hr:?}"
            )));
        }
    }
    Ok(())
}

/// 删除非当前模式的旧槽位（EAPO 互斥语义对齐，DeviceAPOInfo.cpp 578-640）。
///
/// EAPO 三模式互斥写槽位 + 不动保留槽位：
/// - LfxGfx: 删 SFX/MFX/EFX（Legacy 独占）
/// - SfxMfx: 删 LFX/GFX，**不动 EFX**（蓝牙组合设备 EFX 可能无效）
/// - SfxEfx: 删 LFX/GFX，**不动 MFX**
///
/// 旧 VxAPO 实现一律删「非当前模式所有槽位」→ SfxEfx 误删 MFX（蓝牙场景
/// 会造成 MFX 与 EFX 同时冲突）；本实现保留 EAPO 语义的「不动槽位」。
pub(super) fn delete_other_mode_slots(fx_key: &RegKey, mode: InstallMode) {
    let pre = mode.premix_slot();
    let post = mode.postmix_slot();
    for slot in ApoSlot::ALL {
        if slot == pre || slot == post {
            continue;
        }
        // EAPO 保留槽位：SfxMfx 不动 EFX、SfxEfx 不动 MFX。
        let keep = match mode {
            InstallMode::SfxMfx => slot == ApoSlot::Efx,
            InstallMode::SfxEfx => slot == ApoSlot::Mfx,
            InstallMode::LfxGfx => false,
        };
        if !keep {
            let _ = fx_key.delete_value(&slot.value_name());
        }
    }
}

/// 写入 APO CLSID 到指定槽位。
///
/// **必须写 REG_SZ（GUID 字符串）**——EAPO 生态（RegistryHelper.h）与 Windows
/// 音频枚举器读此槽位期望 REG_SZ；写 REG_BINARY 会报
/// 「Registry value ... has wrong type」导致 EAPO 无法枚举设备（实证）。
/// 与 `slots::read_slot_value` 的 REG_SZ 解析分支一致。
pub(super) fn write_apo_slot(fx_key: &RegKey, slot: ApoSlot, guid: GUID) -> Result<()> {
    fx_key.write_sz(&slot.value_name(), &guid_to_string(&guid))?;
    Ok(())
}

/// 写入默认处理模式（Step 6，**对齐 EAPO DeviceAPOInfo.cpp 74-77/603-638**）。
///
/// EAPO 写槽位 GUID 的**同时**写 `{d3993a3f-99c2-4402-b5ec-a92a0367664b},{PID}`
/// 的 REG_MULTI_SZ，值 = AUDIO_SIGNALPROCESSINGMODE_DEFAULT（{C18E2F7E-...}）。
/// Windows 音频引擎按此判「该槽位 APO 参与默认处理模式」——缺了它父槽位 APO 不加载
/// （实证：EAPO 当父时 VxAPO 子 APO 能加载；VxAPO 独立父槽位不加载）。
pub(super) fn write_default_processmode(
    fx_key: &RegKey,
    mode: InstallMode,
    install_premix: bool,
    install_postmix: bool,
) -> Result<()> {
    // ProcessingModes 值名（EAPO 源码常量）：{d3993a3f-99c2-4402-b5ec-a92a0367664b},{PID}。
    let default_str = guid_to_string(&AUDIO_SIGNALPROCESSINGMODE_DEFAULT);
    let values = vec![default_str];
    let write = |pid: u32| -> Result<()> {
        let name = format!("{{{}}},{}", "d3993a3f-99c2-4402-b5ec-a92a0367664b", pid);
        Ok(fx_key.write_multi_value(&name, &values)?)
    };
    // 对齐 EAPO DeviceAPOInfo.cpp：只为实际安装的槽位写对应 ProcessingModes；
    // LFX/GFX 分支不写（旧版始终写 SFX+EFX，SfxMfx 模式会漏 MFX → 父槽位不加载）。
    match mode {
        InstallMode::LfxGfx => Ok(()),
        InstallMode::SfxMfx => {
            if install_premix {
                write(5)?;
            }
            if install_postmix {
                write(6)?;
            }
            Ok(())
        }
        InstallMode::SfxEfx => {
            if install_premix {
                write(5)?;
            }
            if install_postmix {
                write(7)?;
            }
            Ok(())
        }
    }
}

/// .reg 备份（best-effort，经 sys::registry::save_to_file 导出 FxProperties）。
pub(super) fn backup_fx_properties_safe(
    device_name: &str,
    connection_name: &str,
    endpoint_path: &str,
) -> Result<String> {
    let _ = std::fs::create_dir_all(BACKUP_DIR);

    let file_name = format!("{}_{}.reg", device_name, connection_name)
        .replace(['\\', '/', ':', '*', '?', '"', '<', '>', '|'], "_");
    let path = format!("{}\\{}", BACKUP_DIR, file_name);

    let fx_path = format!("{}\\{}", endpoint_path, FX_PROPERTIES_KEY);
    crate::sys::registry::save_to_file(HKEY_LOCAL_MACHINE, &fx_path, &path)?;
    Ok(path)
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

