//! install/selector/operation/execute.rs — 安装/卸载/迁移执行与事务回滚

//! 共享导入与 InstallConfig 见父模块 install/selector/operation.rs。

use super::*;
use super::helpers::*;
use super::capx::*;

/// 回滚动作。
#[derive(Debug)]
pub(super) enum RollbackAction {
    /// 删除指定键路径（安装新建的键）。
    DeleteKey { root: windows::Win32::System::Registry::HKEY, path: String },
    /// 恢复指定值（原名 + 备份 GUID 字符串——槽位必须写 REG_SZ，
    /// 见 `write_apo_slot` 的 REG_SZ 实证说明）。
    RestoreValue { key_path: String, name: String, backup: String },
}

/// 简单事务：记录回滚动作，Drop 时逆序执行（未 commit 时）。
pub(super) struct Transaction {
    actions: Vec<RollbackAction>,
    committed: bool,
}

impl Transaction {
    pub(super) fn new() -> Self {
        Self { actions: Vec::new(), committed: false }
    }

    pub(super) fn record(&mut self, action: RollbackAction) {
        self.actions.push(action);
    }

    pub(super) fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        // 逆序回滚所有已记录动作。
        for action in self.actions.iter().rev() {
            match action {
                RollbackAction::DeleteKey { root, path } => {
                    // （审查 #7）：delete_sub_key 是相对句柄语义，此处持完整
                    // 路径必须走 delete_tree（幂等）；旧实现打开后传全路径 → 静默空操作。
                    let _ = crate::sys::registry::delete_tree(*root, path);
                }
                RollbackAction::RestoreValue { key_path, name, backup } => {
                    if let Ok(key) = RegKey::create(HKEY_LOCAL_MACHINE, key_path) {
                        let _ = key.write_sz(name, backup);
                    }
                }
            }
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// write_install_config / install_endpoint — 注册表写入（带事务回滚）+ 安装收尾
// ══════════════════════════════════════════════════════════════════════════════

/// 写入 VxAPO 到指定音频端点（纯注册表写入，含事务回滚）。
///
/// 与 `install_endpoint` 的分工：本函数只做 7 步注册表写入与清理
/// （DisableProtectedAudioDG、刷新全局 APO 注册、FxProperties、备份、
/// 子 APO 配置、槽位、默认 ProcessingModes、删 DisableEnhancements、
/// sysfx 接管），**不含**尾部端点重启、AudioSrv 确保与 CoCreateInstance 自检。
/// CLI `install --verify` 只调本函数，随后自行整服重启 + 管道验证
/// （避免 pnputil 端点重启与整服停启重复执行）。
pub fn write_install_config(
    device_guid: &str,
    device_name: &str,
    connection_name: &str,
    config: &InstallConfig,
) -> Result<()> {
    let mut tx = Transaction::new();

    // 全流程第一步：确保第三方 APO 可加载（DisableProtectedAudioDG=1）。
    crate::install::audiodg::disable()?;

    // 全流程第二步：刷新全局 APO 注册（旧注册可能缺 AudioEngine 键/字段）。
    refresh_global_registration()?;

    // ── 定位端点 ──────────────────────────────────────────────────────────

    let endpoint_path = find_endpoint_path(device_guid)?;
    let fx_path = format!("{}\\{}", endpoint_path, FX_PROPERTIES_KEY);

    // ── Step 2: 确保 FxProperties 存在（已存在用 open_for_write 最小写权限）──

    let (fx_key, fx_is_new) = ensure_fx_properties(&fx_path, &mut tx)?;

    // ── Step 3: 备份原始 GUID ─────────────────────────────────────────────

    if !fx_is_new {
        // .reg 备份（best-effort，经 sys::registry::save_to_file 导出）。
        match backup_fx_properties_safe(device_name, connection_name, &endpoint_path) {
            Ok(path) => {
                #[cfg(debug_assertions)]
                log::info!("Backup saved: {}", path);
                let _ = path;
            }
            Err(e) => {
                #[cfg(debug_assertions)]
                log::warn!("Backup failed (non-fatal): {}", e);
                let _ = e;
            }
        }
        record_slot_backups(&fx_key, config.install_mode, &fx_path, &mut tx);
    }

    // ── 读取当前槽位（用于原始 APO 保留） ────────────────────────────────

    let (original_premix, original_postmix) =
        read_original_apo_guids(&fx_key, config);

    // ── capture 特例（EAPO DeviceAPOInfo.cpp 583/607/632）───────────────
    // 采集端点（Capture）只装 PreMix，PostMix 不装（VxAPO 不做采集端增强）。
    // 由 find_endpoint_path 返回路径含 Capture 判定，写子 APO 和槽位时共用。
    let is_capture = endpoint_path.contains("Capture");

    // ── Step 1: 写入子 APO 配置（独立安装信息区， 路径隔离）──────────
    // `HKLM\SOFTWARE\VxAPO\Child APOs\{deviceGuid}\{PreMixChild|PostMixChild}`。
    // 与运行期 `object/child.rs` / `install/device/slots::read_child_apo_guid`
    // 读取路径一致（旧实现写 `FxProperties\childGuid` + 建 `ChildApoKeys`
    // 子键——运行期无人读，且 FxProperties ACL 不给管理员 CreateSubKey）。
    // capture 不装 PostMix → childPostMix 无意义，强制 None。

    let child_postmix = if is_capture { None } else { original_postmix };
    write_child_apo_config(
        device_guid,
        &endpoint_path,
        &fx_key,
        config,
        original_premix,
        child_postmix,
        &mut tx,
    )?;

    // ── Step 5: 按模式写入 APO GUID（capture 只写 PreMix）───────────────

    delete_other_mode_slots(&fx_key, config.install_mode);

    if config.install_premix {
        write_apo_slot(&fx_key, config.install_mode.premix_slot(), CLSID_VXAPO_PRE_MIX)?;
    }
    if config.install_postmix && !is_capture {
        write_apo_slot(&fx_key, config.install_mode.postmix_slot(), CLSID_VXAPO_POST_MIX)?;
    }

    // ── Step 6: 写入默认处理模式 GUID ────────────────────────────────────

    write_default_processmode(
        &fx_key,
        config.install_mode,
        config.install_premix,
        config.install_postmix && !is_capture,
    )?;

    // ── Step 7: 删除 DisableEnhancements ──────────────────────────────────

    let _ = fx_key.delete_value("DisableEnhancements");
    // EAPO DeviceAPOInfo.cpp 78/642-645：Windows 以
    // `{1da5d803-d492-4edd-8c23-e0c0ffee7f0e},5`（PKEY_AudioEndpoint_Disable_SysFx）
    // 禁用整条增强链；安装时删除以强制启用。
    let _ = fx_key.delete_value("{1da5d803-d492-4edd-8c23-e0c0ffee7f0e},5");

    // ── Step 8: 接管 Windows“设备默认效果”（CAPX MSFX 模板）───────────────
    // 仅改端点 FxProperties 会被 Windows 重启/重新枚举后从驱动模板恢复，
    // 导致微软 WMALFXGFX APO 与 VxAPO 同时加载（音频断断续续/慢放）。
    take_over_sysfx(device_guid, &endpoint_path, &fx_key, config, &mut tx)?;

    // 全部成功 → 提交事务（禁用回滚）。
    tx.commit();

    Ok(())
}

/// 安装 VxAPO 到指定音频端点。
///
/// `install_endpoint = write_install_config + 尾部（pnputil 端点重启 +
/// ensure AudioSrv 运行）+ 可选 CoCreateInstance 自检（verify=true）`。
/// `--verify` 流程请直接调 `write_install_config`。
pub fn install_endpoint(
    device_guid: &str,
    device_name: &str,
    connection_name: &str,
    config: &InstallConfig,
    verify: bool,
) -> Result<()> {
    write_install_config(device_guid, device_name, connection_name, config)?;

    // ── 安装自检（verify=true）：CoCreateInstance 验证 DLL 可实例化 ──
    // 失败**不自动回滚**（注册表已写入且 DLL 可能瞬时不可用；报告并让调用方决策）。
    if verify {
        // CoCreateInstance 前需初始化 COM（0x800401F0 CO_E_NOTINITIALIZED 实证：
        // 管理员 CLI 直接调 install 未初始化 COM 即触发）。
        // SAFETY: CoInitializeEx 无 preconditions；进程级调用。
        // S_OK(0)=本次初始化成功；S_FALSE(1)=已由宿主初始化（合法）。
        // 其他值=COM 初始化失败，verify 不可靠 → 报错。
        let co_init = unsafe {
            CoInitializeEx(
                None,
                COINIT_MULTITHREADED,
            )
        };
        let co_init_hr = co_init.0;
        if co_init_hr != 0 && co_init_hr != 1 {
            return Err(VxApoError::internal(&format!(
                "安装自检失败：CoInitializeEx err={}",
                co_init_hr
            )));
        }
        for clsid in [CLSID_VXAPO_PRE_MIX, CLSID_VXAPO_POST_MIX] {
            // 实例化验证：CoCreateInstance 成功即 DLL 可加载（不深究接口）。
            // SAFETY: windows-rs 3 参泛型（rclsid, punkouter, dwclscontext）返回 IUnknown。
            let hr = unsafe {
                CoCreateInstance::<_, IUnknown>(
                    &clsid,
                    None,
                    CLSCTX_INPROC_SERVER,
                )
            };
            if hr.is_err() {
                return Err(VxApoError::internal(&format!(
                    "安装自检失败：CoCreateInstance(CLSID) err={}",
                    hr.err().unwrap()
                )));
            }
        }
    }

    // 全流程收尾：定向重启端点设备，随后**无条件确保 AudioSrv 运行**
    // （否则“端点重启成功但服务仍停”会导致音频服务未启用）。best-effort。
    let endpoint_path = find_endpoint_path(device_guid)?;
    let is_capture = endpoint_path.contains("Capture");
    if let Err(e) = crate::install::audiodg::restart_endpoint_device(device_guid, is_capture) {
        log::warn!("install_endpoint: 端点设备重启失败：{e}");
    }
    if let Err(e) = crate::install::audiodg::ensure_audio_service_running() {
        log::warn!("install_endpoint: 音频服务未能确保运行（安装已生效，重启后生效）：{e}");
    }
    Ok(())
}

// ══════════════════════════════════════════════════════════════════════════════
// uninstall_endpoint
// ══════════════════════════════════════════════════════════════════════════════

/// 从指定音频端点卸载 VxAPO。
///
/// # 卸载语义（明确）
///
/// **只卸载「能确定属于 VxAPO 的部分」**，绝不碰其他 APO：
///
/// 1. **定位 FxProperties**（不存在 = 无安装，直接返回）。
/// 2. **删 VxAPO 的 CLSID**：遍历 5 槽位，仅当槽位值 == VxAPO PRE/POST CLSID
///    才删除该槽位值（别的 APO 的槽位不动——EAPO 重装占回时不受影响）。
/// 3. **看槽位是否为空**：VxAPO 的槽位被删后（NoValue/NoKey）才写回备份；
///    若该槽位已被其他 APO 接管（Guid 存在）→ **尊重接管者，不覆盖**。
///    （备份在 install 覆盖前写入信息区：槽位名 PreMixSlot/PostMixSlot + 原值。）
/// 4. **恢复完成后**，再删除 VxAPO 的信息区目录
///    `HKLM\SOFTWARE\VxAPO\Child APOs\{deviceGuid}`（含所有备份与 child 记录）。
/// 5. 清理 FxProperties 上的 VxAPO 配置值（childGuid/allowSilentBuffer/autoAdjust/
///    version——旧遗留 best-effort）+ DisableEnhancements。
///
/// # 为什么「槽位空才恢复」？
///
/// 实测场景：VxAPO 把 EAPO 弄成子 APO 后，另一软件又覆盖了父 APO 槽位。
/// 此时卸载：父槽位归接管软件（不删不覆盖），EAPO 作为旧子 APO 的恢复只发生在
/// 「VxAPO 槽位被我们删空了」之后——绝不覆盖任何现存 APO 的所有权。
pub fn uninstall_endpoint(device_guid: &str) -> Result<()> {
    let endpoint_path = find_endpoint_path(device_guid)?;
    let fx_path = format!("{}\\{}", endpoint_path, FX_PROPERTIES_KEY);

    // open_for_write（KEY_SET_VALUE）：删除值需写权限；SAM_ALL 超权限（含
    // CreateSubKey 位）在 MMDevices 端点键上会被拒（0x80070005）。
    let fx_key = match RegKey::open_for_write(HKEY_LOCAL_MACHINE, &fx_path) {
        Ok(k) => k,
        Err(_) => {
            // FxProperties 不存在 → 无安装，直接返回成功。
            return Ok(());
        }
    };

    // 全流程前置：确认已安装后才停音频服务。停服**不是删值的前提**（写/删
    // `FxProperties` 值只需 `KEY_SET_VALUE` 句柄，音频播放中、DLL 已被 audiodg
    // 加载、audiodg 持有点端时删槽位值同样成功）。真正的理由：
    // ① 释放 DLL 模块映像（audiodg 不退出则 vxapo_driver.dll 仍被占用，随后的
    //    重装/换 DLL 覆盖会失败；NSIS installer-hooks 亦为此停服务）；
    // ② 让本流程末尾的端点重启（pnputil /restart-device）立刻生效——引擎会缓存
    //    端点 APO 链。
    if let Err(e) = crate::install::audiodg::stop_audio_service() {
        log::warn!("uninstall: 停止音频服务失败（删槽位本身不受影响，仅影响变更生效时机）：{e}");
    }

    // ── 删除 VxAPO CLSID ──────────────────────────────────────────────────
    // 注意：**不能**用 read_all_slots(&fx_key)——它期望端点根键（内部再 open
    // FxProperties 子键）；此处 fx_key 已是 FxProperties 键，会拿不到槽位
    // （实测：uninstall 后 slot 仍残留 VxAPO CLSID）。改用
    // read_slot_value 直接在 fx_key 上读槽位值（REG_SZ/REG_BINARY 兼容）。
    //
    // 删值失败**不是**"端点被占用/被锁"：写/删 `FxProperties` 值只需要句柄具备
    // `KEY_SET_VALUE`（`open_for_write` 即是），在活动音频流上同样成功。
    // ACCESS_DENIED 的成因是句柄权限不足（`SAM_ALL` 含未授予的 CreateSubKey 位、
    // 或只读句柄打开）——「第一次删信息区成功但槽位值残留」即由此而来。
    // 仍不静默吞错：权限/句柄异常必须暴露给调用方，但失败语义是"写入被拒"，
    // 与音频服务是否运行无关。

    let mut any_failed = false;
    for slot in ApoSlot::ALL {
        if let SlotValue::Guid(g) = read_slot_value(&fx_key, slot) {
            if g == CLSID_VXAPO_PRE_MIX || g == CLSID_VXAPO_POST_MIX {
                let name = slot.value_name();
                if let Err(e) = fx_key.delete_value(&name) {
                    log::warn!("uninstall: delete slot {name} failed: {e} (write denied: handle rights or key ACL)");
                    any_failed = true;
                }
            }
        }
    }
    if any_failed {
        return Err(VxApoError::internal(
            "卸载槽位失败：写入被拒绝（句柄权限或键 ACL 异常）。请以管理员重试；若仍失败，重启音频服务（net stop audiosrv && net start audiosrv）后重试卸载。",
        ));
    }

    // ── 恢复 Windows“设备默认效果”（CAPX MSFX 模板）──────────────────────
    // 必须先于删除信息区执行：安装时保存的微软原始 APO 值在信息区里。
    restore_sysfx(device_guid, &endpoint_path)?;

    // ── 删除 VxAPO 独立安装信息区（含所有备份）────────────────────
    // **卸载 ≠ 快照恢复**（纠正）：卸载只删 VxAPO 自己的 CLSID，
    // **不**把 install 时备份的第三方 APO（EAPO）写回父槽位——那是快照 restore
    // （snapshot_restore）的职责。若卸载时恢复 EAPO，用户卸载 VxAPO 后 EAPO
    // 莫名回到父槽位（错误语义）。

    let info_key = format!("{}\\{}", CHILD_APO_PATH_ROOT, device_guid);
    let (root, sub_key) = split_hklm_path(&info_key)?;
    // （审查 #8 同族）：信息区删除失败必须返回 Err——残留会让下次安装
    // 误判为“非全量路径”；delete_tree 对“键不存在”幂等返回 Ok。
    crate::sys::registry::delete_tree(root, sub_key).map_err(|e| {
        VxApoError::internal(&format!("卸载失败：删除安装信息区 {info_key} 失败：{e}"))
    })?;

    // ── 删除子 APO 配置（旧遗留值，best-effort） ─────────────────────────

    for name in &["childGuid", "allowSilentBuffer", "autoAdjust", "version"] {
        let _ = fx_key.delete_value(name);
    }

    // ── 删除 DisableEnhancements ──────────────────────────────────────────

    let _ = fx_key.delete_value("DisableEnhancements");

    // 全流程收尾：定向重启端点设备，随后**无条件确保 AudioSrv 运行**
    // （否则“端点重启成功但服务仍停”会导致音频服务未启用）。best-effort。
    let is_capture = endpoint_path.contains("Capture");
    if let Err(e) = crate::install::audiodg::restart_endpoint_device(device_guid, is_capture) {
        log::warn!("uninstall: 端点设备重启失败：{e}");
    }
    if let Err(e) = crate::install::audiodg::ensure_audio_service_running() {
        log::warn!("uninstall: 音频服务未能确保运行（卸载已生效，重启后恢复输出）：{e}");
    }
    Ok(())
}

// ══════════════════════════════════════════════════════════════════════════════
// CAPX 设备默认效果接管
// ══════════════════════════════════════════════════════════════════════════════

/// 把旧 GUID 安装迁移到新 GUID（编排层入口）。
///
/// `device::stale::migrate_install` 不做安装配置写入（`InstallConfig` 属本层），
/// 由本函数构造配置并以回调传入，device 层因此不依赖 selector。
pub fn migrate_install(
    old_guid: &str,
    new_guid: &str,
    config_from: Option<&str>,
    snapshot_from: Option<&str>,
) -> Result<crate::install::device::stale::MigrationReport> {
    let repair = |guid: &str, name: &str, mode: InstallMode| -> Result<()> {
        let config = InstallConfig {
            install_premix: true,
            install_postmix: true,
            install_mode: mode,
            use_original_apo_premix: false,
            use_original_apo_postmix: false,
            allow_silent_buffer: true,
            auto_adjust: false,
        };
        write_install_config(guid, name, "", &config)
    };
    crate::install::device::stale::migrate_install(
        old_guid,
        new_guid,
        config_from,
        snapshot_from,
        &repair,
    )
}

// ══════════════════════════════════════════════════════════════════════════════
// 内部辅助
// ══════════════════════════════════════════════════════════════════════════════

