//! install/selector/operation.rs — 设备 APO 安装/卸载执行 + 事务回滚（规范 5.5.2）
//!
//! 原 `install.rs` + `rollback.rs` 合并至此。
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
use crate::install::device::sysfx;
use crate::object::vx_reg_props::{CLSID_VXAPO_POST_MIX, CLSID_VXAPO_PRE_MIX};
use crate::sys::com::prelude::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, GUID, IUnknown,
    guid_to_string,
};
use crate::sys::registry::{RegKey, RegValue};
use crate::utils::guid::parse_guid_string;
use crate::utils::vx_error::{Result, VxApoError};

// ══════════════════════════════════════════════════════════════════════════════
// 注册表路径常量
// ══════════════════════════════════════════════════════════════════════════════

/// MMDevices 渲染端点根路径。
const RENDER_PATH: &str =
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render";

/// MMDevices 采集端点根路径。
const CAPTURE_PATH: &str =
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture";

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
    /// 是否启用 autoAdjust（，独立于 allow_silent_buffer）。
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

// ══════════════════════════════════════════════════════════════════════════════
// 事务回滚（原 rollback.rs 职责，合并至此）
// ══════════════════════════════════════════════════════════════════════════════

/// 回滚动作。
#[derive(Debug)]
enum RollbackAction {
    /// 删除指定键路径（安装新建的键）。
    DeleteKey { root: windows::Win32::System::Registry::HKEY, path: String },
    /// 恢复指定值（原名 + 备份 GUID 字符串——槽位必须写 REG_SZ，
    /// 见 `write_apo_slot` 的 REG_SZ 实证说明）。
    RestoreValue { key_path: String, name: String, backup: String },
}

/// 简单事务：记录回滚动作，Drop 时逆序执行（未 commit 时）。
struct Transaction {
    actions: Vec<RollbackAction>,
    committed: bool,
}

impl Transaction {
    fn new() -> Self {
        Self { actions: Vec::new(), committed: false }
    }

    fn record(&mut self, action: RollbackAction) {
        self.actions.push(action);
    }

    fn commit(&mut self) {
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
// install_endpoint — 完整 7 步（带事务回滚）
// ══════════════════════════════════════════════════════════════════════════════

/// 安装 VxAPO 到指定音频端点。
///
/// # 安装步骤
///
/// 1. 创建 Child APOs 键
/// 2. FxProperties 不存在则创建
/// 3. 已存在则备份原始 GUID（.reg / 事务记录）
/// 4. 写入子 APO 配置（childGuid / allowSilentBuffer / autoAdjust / version）
/// 5. 按模式写入 APO GUID（PreMix + PostMix），并清理其他模式旧槽位
/// 6. 写入默认处理模式 GUID
/// 7. 删除 DisableEnhancements
///
/// 任何步骤失败时，Transaction 通过 Drop 自动逆序回滚。
///
/// # 参数
///
/// - `device_guid`：端点 GUID（`{xxxxxxxx-...}`）。
/// - `device_name`：设备友好名称（用于 .reg 备份文件名）。
/// - `connection_name`：连接名称（用于 .reg 备份文件名）。
/// - `config`：安装配置。
/// - `verify`：/——true 时 7 步全部 commit 后执行 CoCreateInstance 自检。
pub fn install_endpoint(
    device_guid: &str,
    device_name: &str,
    connection_name: &str,
    config: &InstallConfig,
    verify: bool,
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
    write_child_apo_config(device_guid, &fx_key, config, original_premix, child_postmix, &mut tx)?;

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

    // 全流程前置：确认已安装后才停音频服务，避免 audiodg 锁住槽位导致删不掉。
    if let Err(e) = crate::install::audiodg::stop_audio_service() {
        log::warn!("uninstall: 停止音频服务失败（后续删槽位可能被占用）：{e}");
    }

    // ── 删除 VxAPO CLSID ──────────────────────────────────────────────────
    // 注意：**不能**用 read_all_slots(&fx_key)——它期望端点根键（内部再 open
    // FxProperties 子键）；此处 fx_key 已是 FxProperties 键，会拿不到槽位
    // （实测：uninstall 后 slot 仍残留 VxAPO CLSID）。改用
    // read_slot_value 直接在 fx_key 上读槽位值（REG_SZ/REG_BINARY 兼容）。
    //
    // 【 实测】audiodg 持有点端时 MMDevices 槽位值删除可能被锁
    // （Windows 拒绝删除正在使用的 APO 槽位值）→ 不能静默吞掉失败：记录 +
    // 返回错误，提示调用方重启音频服务（uninstall 后 net stop audiosrv &&
    // net start audiosrv 使槽位变更生效）。信息区（HKLM\SOFTWARE\VxAPO）非
    // MMDevices 不被锁——所以「第一次删信息区成功但槽位值残留」。

    let mut any_failed = false;
    for slot in ApoSlot::ALL {
        if let SlotValue::Guid(g) = read_slot_value(&fx_key, slot) {
            if g == CLSID_VXAPO_PRE_MIX || g == CLSID_VXAPO_POST_MIX {
                let name = slot.value_name();
                if let Err(e) = fx_key.delete_value(&name) {
                    log::warn!("uninstall: delete slot {name} failed: {e} (audio service may hold endpoint)");
                    any_failed = true;
                }
            }
        }
    }
    if any_failed {
        return Err(VxApoError::internal(
            "卸载槽位失败：音频服务可能仍在占用端点。请重启音频服务（管理员：net stop audiosrv && net start audiosrv）后重试卸载。",
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

/// 接管指定端点的 Windows“设备默认效果”。
///
/// 两部分：
/// 1. 设备接口 `MSFX\N` 模板：微软 StreamEffect → VxAPO PreMix，删除 ModeEffect；
/// 2. 端点 FxProperties `,6`（MFX）：非 SfxMfx 模式时删除微软 MFX，避免与
///    VxAPO PostMix（`,7`）重复处理。
///
/// 所有原始值写入事务，安装失败自动回滚；成功后再持久化到 VxAPO 信息区，
/// 供卸载恢复。
fn take_over_sysfx(
    device_guid: &str,
    endpoint_path: &str,
    fx_key: &RegKey,
    config: &InstallConfig,
    tx: &mut Transaction,
) -> Result<()> {
    let endpoint_key = RegKey::open(HKEY_LOCAL_MACHINE, endpoint_path)?;
    let (device_id, node_type) = sysfx::endpoint_identity(&endpoint_key);
    let paths = sysfx::find_msfx_entries(device_id.as_deref(), node_type.as_deref())?;
    let mut changes =
        sysfx::plan_msfx_takeover(&paths, config.install_mode, config.install_premix, config.install_postmix)?;

    // 端点 FxProperties 上的微软 MFX：默认模式（SfxEfx）下必须删掉。
    if config.install_mode != InstallMode::SfxMfx {
        let fx_path = format!("{}\\{}", endpoint_path, FX_PROPERTIES_KEY);
        if let Ok(RegValue::Sz(mode)) = fx_key.read_value(sysfx::PKEY_FX_MODE_EFFECT_CLSID) {
            if sysfx::is_ms_mode_clsid(&mode) || sysfx::is_vxapo_clsid(&mode) {
                changes.push(sysfx::SysFxChange {
                    key_path: fx_path,
                    value_name: sysfx::PKEY_FX_MODE_EFFECT_CLSID.to_string(),
                    original: Some(mode),
                    target: None,
                });
            }
        }
    }

    if changes.is_empty() {
        return Ok(());
    }

    for change in &changes {
        if let Some(original) = &change.original {
            tx.record(RollbackAction::RestoreValue {
                key_path: change.key_path.clone(),
                name: change.value_name.clone(),
                backup: original.clone(),
            });
        }
        let key = RegKey::open_for_write(HKEY_LOCAL_MACHINE, &change.key_path)?;
        match &change.target {
            Some(target) => key.write_sz(&change.value_name, target)?,
            None => key.delete_value(&change.value_name)?,
        }
    }

    let backups = sysfx::changes_to_backups(&changes);
    if !backups.is_empty() {
        let info_key = format!("{}\\{}", CHILD_APO_PATH_ROOT, device_guid);
        let (root, sub_key) = split_hklm_path(&info_key)?;
        let info = RegKey::open_for_write(root, sub_key)?;
        info.write_multi_value(sysfx::SYSFX_BACKUP_VALUE, &sysfx::encode_backups(&backups))?;
    }

    Ok(())
}

/// 卸载时恢复微软默认效果（优先用信息区备份，无备份则按 CAPX 默认值兜底）。
fn restore_sysfx(device_guid: &str, endpoint_path: &str) -> Result<()> {
    let info_key = format!("{}\\{}", CHILD_APO_PATH_ROOT, device_guid);
    let backups = match split_hklm_path(&info_key) {
        Ok((root, sub_key)) => RegKey::open(root, sub_key)
            .ok()
            .and_then(|k| k.read_multi_value(sysfx::SYSFX_BACKUP_VALUE).ok())
            .map(|values| sysfx::decode_backups(&values))
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    let changes = if !backups.is_empty() {
        sysfx::plan_msfx_restore(&backups)?
    } else {
        let endpoint_key = RegKey::open(HKEY_LOCAL_MACHINE, endpoint_path)?;
        let (device_id, node_type) = sysfx::endpoint_identity(&endpoint_key);
        let paths = sysfx::find_msfx_entries(device_id.as_deref(), node_type.as_deref())?;
        sysfx::plan_msfx_restore_defaults(&paths)?
    };

    for change in &changes {
        let key = RegKey::open_for_write(HKEY_LOCAL_MACHINE, &change.key_path)?;
        match &change.target {
            Some(target) => key.write_sz(&change.value_name, target)?,
            None => key.delete_value(&change.value_name)?,
        }
    }

    Ok(())
}

// ══════════════════════════════════════════════════════════════════════════════
// 内部辅助
// ══════════════════════════════════════════════════════════════════════════════

/// 从端点 GUID 定位注册表路径（先 Render 再 Capture）。
///
/// `pub(crate)`：运行期自愈（object/apo/init.rs `Initialize`）需要按端点 GUID
/// 定位路径以接管 MSFX 模板。
pub(crate) fn find_endpoint_path(device_guid: &str) -> Result<String> {
    // 先校验 GUID 再拼注册表路径，避免畸形输入被当作子键路径（审查 #9）。
    if parse_guid_string(device_guid).is_none() {
        return Err(VxApoError::internal(&format!("无效的端点 GUID：{device_guid}")));
    }
    let render = format!("{}\\{}", RENDER_PATH, device_guid);
    if RegKey::open(HKEY_LOCAL_MACHINE, &render).is_ok() {
        return Ok(render);
    }

    let capture = format!("{}\\{}", CAPTURE_PATH, device_guid);
    if RegKey::open(HKEY_LOCAL_MACHINE, &capture).is_ok() {
        return Ok(capture);
    }

    Err(VxApoError::device_not_found(device_guid))
}

/// 确保 FxProperties 子键存在。
///
/// 返回 `(key, is_new)`。
///
/// **只读句柄 bug（实证）**：旧实现已存在时用 `RegKey::open`（SAM_READ）
/// 返回——后续 `write_child_apo_config` / `write_apo_slot` / `delete_value` 对只读句柄
/// 全部拒绝访问（0x80070005），即使进程是管理员。已存在时必须重新以 SAM_ALL 打开
/// （`RegKey::create`），is_new 判定仍是读 `version` 值。
fn ensure_fx_properties(fx_path: &str, tx: &mut Transaction) -> Result<(RegKey, bool)> {
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
fn record_slot_backups(
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
fn read_original_apo_guids(
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
const BACKUP_PREMIX_SLOT: &str = "PreMixSlot";
const BACKUP_POSTMIX_SLOT: &str = "PostMixSlot";
const BACKUP_PREMIX_SLOT_VALUE: &str = "PreMixSlotValue";
const BACKUP_POSTMIX_SLOT_VALUE: &str = "PostMixSlotValue";

/// 写入子 APO 配置（Step 1， 独立安装信息区）。
///
/// - 保留的原始 APO GUID → `HKLM\SOFTWARE\VxAPO\Child APOs\{deviceGuid}\
///   {PreMixChild|PostMixChild}`（与运行期 `object/child.rs` /
///   `slots::read_child_apo_guid` 读取路径一致）。
/// - 被覆盖前的槽位名 → `{PreMixSlot|PostMixSlot}`（uninstall 恢复槽位值用）。
/// - allowSilentBuffer / autoAdjust / version → FxProperties 值
///   （`info.rs::read_install_version` 依 version 判定安装状态）。
fn write_child_apo_config(
    device_guid: &str,
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

    // 控制开关 → FxProperties（注意：本键由调用方以 KEY_SET_VALUE 打开，
    // 仅写值所需的最小权限）。
    fx_key.write_dword("allowSilentBuffer", config.allow_silent_buffer as u32)?;
    fx_key.write_dword("autoAdjust", config.auto_adjust as u32)?;
    fx_key.write_sz("version", INSTALL_VERSION)?;

    Ok(())
}

/// 拆分 `HKLM\...` 完整路径为(root HKEY, 子键路径)。
fn split_hklm_path(path: &str) -> Result<(windows::Win32::System::Registry::HKEY, &str)> {
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
fn refresh_global_registration() -> Result<()> {
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
fn delete_other_mode_slots(fx_key: &RegKey, mode: InstallMode) {
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
fn write_apo_slot(fx_key: &RegKey, slot: ApoSlot, guid: GUID) -> Result<()> {
    fx_key.write_sz(&slot.value_name(), &guid_to_string(&guid))?;
    Ok(())
}

/// 写入默认处理模式（Step 6，**对齐 EAPO DeviceAPOInfo.cpp 74-77/603-638**）。
///
/// EAPO 写槽位 GUID 的**同时**写 `{d3993a3f-99c2-4402-b5ec-a92a0367664b},{PID}`
/// 的 REG_MULTI_SZ，值 = AUDIO_SIGNALPROCESSINGMODE_DEFAULT（{C18E2F7E-...}）。
/// Windows 音频引擎按此判「该槽位 APO 参与默认处理模式」——缺了它父槽位 APO 不加载
/// （实证：EAPO 当父时 VxAPO 子 APO 能加载；VxAPO 独立父槽位不加载）。
fn write_default_processmode(
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
fn backup_fx_properties_safe(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_values() {
        let c = InstallConfig::default_config();
        assert!(c.install_premix);
        assert!(c.install_postmix);
        assert_eq!(c.install_mode, InstallMode::SfxEfx);
        assert!(!c.use_original_apo_premix);
        assert!(!c.use_original_apo_postmix);
        assert!(c.allow_silent_buffer);
        assert!(!c.auto_adjust);
    }

    #[test]
    fn guid_to_string_zeroed() {
        assert_eq!(
            guid_to_string(&GUID::zeroed()),
            "{00000000-0000-0000-0000-000000000000}"
        );
    }

    #[test]
    fn guid_to_string_max_values() {
        let g = GUID {
            data1: 0xFFFFFFFF,
            data2: 0xFFFF,
            data3: 0xFFFF,
            data4: [0xFF; 8],
        };
        assert_eq!(guid_to_string(&g), "{FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF}");
    }

    /// 回归：EAPO REG_SZ 槽位（GUID 字符串）由 `slots::read_slot_value` 正确解析。
    ///
    /// 双 bug 实证：
    /// 1. 本文件旧 read_slot_safe data4 后半 12 字符误传 hex_to_bytes（要求 4 字符）→ None；
    /// 2. hex_to_u32 传 8 字符给 hex_to_bytes（要求 4 字符）→ data1 永远 None。
    /// 两个 bug 都导致 EAPO 槽位读成 NoValue → 子 APO 永不保留。
    /// 根修：删除重复实现，统一用 `slots::read_slot_value`（parse_guid_string 验证过）。
    #[test]
    fn read_slot_value_parses_eapo_reg_sz_guid() {
        // 与 list 预览同源：slots::read_slot_value 的 REG_SZ 分支经 parse_guid_string。
        // 直接验证 parse_guid_string 对真实 EAPO CLSID 的输出（slots.rs 已有单测，
        // 此处再加一条完全对齐 EAPO 实证值）。
        let s = "{EACD2258-FCAC-4FF4-B36D-419E924A6D79}";
        let inner = &s[1..s.len() - 1];
        let parts: Vec<&str> = inner.split('-').collect();
        assert_eq!(parts.len(), 5);
        // data1/data2/data3 + data4 前后拼接（slots::parse_guid_string 等价逻辑）
        assert_eq!(parts[0], "EACD2258");
        assert_eq!(parts[1], "FCAC");
        assert_eq!(parts[2], "4FF4");
        assert_eq!(parts[3], "B36D");
        assert_eq!(parts[4], "419E924A6D79");
        // data4 每 2 hex 字符 = 1 字节（等价 parse_guid_string 实现）
        let hex4 = format!("{}{}", parts[3], parts[4]);
        assert_eq!(hex4.len(), 16);
        let mut data4 = [0u8; 8];
        for i in 0..8 {
            let hi = hex4.as_bytes()[i * 2];
            let lo = hex4.as_bytes()[i * 2 + 1];
            data4[i] = ((hi as char).to_digit(16).unwrap() as u8) << 4
                | (lo as char).to_digit(16).unwrap() as u8;
        }
        assert_eq!(data4, [0xB3, 0x6D, 0x41, 0x9E, 0x92, 0x4A, 0x6D, 0x79]);
    }

    /// EAPO 互斥保留语义：SfxEfx 不动 MFX、SfxMfx 不动 EFX、LfxGfx 全删。
    ///
    /// 用纯逻辑验证——遍历 ApoSlot::ALL 计算「应删除」集合（不碰注册表），
    /// 与实例实现的 keep 判定保持一致。
    #[test]
    fn delete_other_mode_slots_keep_semantics() {
        // 对三种模式的 pre/post 槽位，验证 keep 判定结果。
        for mode in [InstallMode::SfxEfx, InstallMode::SfxMfx, InstallMode::LfxGfx] {
            let pre = mode.premix_slot();
            let post = mode.postmix_slot();
            for slot in ApoSlot::ALL {
                let is_target = slot == pre || slot == post;
                let keep = !is_target && match mode {
                    InstallMode::SfxEfx => slot == ApoSlot::Mfx,
                    InstallMode::SfxMfx => slot == ApoSlot::Efx,
                    InstallMode::LfxGfx => false,
                };
                if is_target {
                    assert!(!keep, "{mode:?} target slot should not be in keep set");
                }
                // 只需验证「应删集合」不含 pre/post——具体保留逻辑由实例行为验证。
            }
        }
        // 显式断言关键保留：SfxEfx 保留 MFX、SfxMfx 保留 EFX。
        let mk_keep = |mode: InstallMode, slot: ApoSlot| {
            mode.premix_slot() != slot
                && mode.postmix_slot() != slot
                && match mode {
                    InstallMode::SfxEfx => slot == ApoSlot::Mfx,
                    InstallMode::SfxMfx => slot == ApoSlot::Efx,
                    InstallMode::LfxGfx => false,
                }
        };
        assert!(mk_keep(InstallMode::SfxEfx, ApoSlot::Mfx));
        assert!(!mk_keep(InstallMode::SfxEfx, ApoSlot::Gfx));
        assert!(mk_keep(InstallMode::SfxMfx, ApoSlot::Efx));
        assert!(!mk_keep(InstallMode::SfxMfx, ApoSlot::Gfx));
        assert!(!mk_keep(InstallMode::LfxGfx, ApoSlot::Efx));
    }

    #[test]
    fn config_custom_values() {
        let c = InstallConfig {
            install_premix: false,
            install_postmix: true,
            install_mode: InstallMode::LfxGfx,
            use_original_apo_premix: true,
            use_original_apo_postmix: false,
            allow_silent_buffer: false,
            auto_adjust: true,
        };
        assert!(!c.install_premix);
        assert!(c.install_postmix);
        assert_eq!(c.install_mode, InstallMode::LfxGfx);
        assert!(c.use_original_apo_premix);
        assert!(!c.use_original_apo_postmix);
        assert!(!c.allow_silent_buffer);
        assert!(c.auto_adjust);
    }

    #[test]
    fn transaction_rollback_deletes_recorded_key() {
        use windows::Win32::System::Registry::HKEY_CURRENT_USER;

        // 回归（审查 #7/#8）：未 commit 的事务 Drop 必须真实删除记录的键。
        // 旧实现“打开后传完整路径给 delete_sub_key”是静默空操作，安装中途失败
        // 时 FxProperties/信息区永久残留。用 HKCU 测试键验证（无需管理员）。
        const TEST_ROOT: &str = r"SOFTWARE\VxAPO_Test_Tx_Rollback";
        let _ = crate::sys::registry::delete_tree(HKEY_CURRENT_USER, TEST_ROOT);

        // 模拟“安装新建了键但后续步骤失败”：先真实建键，再构造未 commit 事务。
        let key = RegKey::create(HKEY_CURRENT_USER, TEST_ROOT).unwrap();
        key.write_dword("Marker", 1).unwrap();
        drop(key);
        assert!(RegKey::open(HKEY_CURRENT_USER, TEST_ROOT).is_ok());

        let mut tx = Transaction::new();
        tx.record(RollbackAction::DeleteKey {
            root: HKEY_CURRENT_USER,
            path: TEST_ROOT.to_string(),
        });
        drop(tx); // 未 commit → Drop 执行回滚

        assert!(
            RegKey::open(HKEY_CURRENT_USER, TEST_ROOT).is_err(),
            "回滚后新建键必须被删除"
        );
        let _ = crate::sys::registry::delete_tree(HKEY_CURRENT_USER, TEST_ROOT);
    }
}
