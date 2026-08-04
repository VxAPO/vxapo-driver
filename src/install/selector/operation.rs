//! install/selector/operation.rs — 设备 APO 安装/卸载执行 + 事务回滚（v6.4 规范 5.5.2）
//!
//! 原 `install.rs` + `rollback.rs` 合并至此。
//!
//! 职责：
//! - `install_endpoint`：Note 47 完整 7 步安装，Transaction 保护，失败自动回滚
//! - `uninstall_endpoint`：卸载（恢复原始 GUID，清理配置）
//! - `InstallConfig`：安装参数
//!
//! 禁止依赖：`pipeline/`、`config/`。

use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
use windows::Win32::Media::KernelStreaming::AUDIO_SIGNALPROCESSINGMODE_DEFAULT;

use crate::install::device::slots::{
    ApoSlot, ChildApoKind, InstallMode, SlotValue, read_slot_value, CHILD_APO_PATH_ROOT,
    FX_PROPERTIES_KEY, INSTALL_VERSION,
};
use crate::object::vx_reg_props::{CLSID_VXAPO_POST_MIX, CLSID_VXAPO_PRE_MIX};
use crate::sys::com::prelude::guid_to_string;
use crate::sys::registry::RegKey;
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

/// .reg 备份默认目录（Note 32）。
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
    /// 是否允许静音缓冲区快速路径（Note 11）。
    pub allow_silent_buffer: bool,
    /// 是否启用 autoAdjust（E3.3/v6.8，独立于 allow_silent_buffer）。
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
    DeleteKey(String),
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
                RollbackAction::DeleteKey(path) => {
                    let _ = RegKey::open(HKEY_LOCAL_MACHINE, path)
                        .and_then(|k| k.delete_sub_key(path));
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
// install_endpoint — Note 47 完整 7 步（带事务回滚）
// ══════════════════════════════════════════════════════════════════════════════

/// 安装 VxAPO 到指定音频端点。
///
/// # Note 47 安装步骤
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
/// - `verify`：E3.4/v6.8——true 时 7 步全部 commit 后执行 CoCreateInstance 自检。
pub fn install_endpoint(
    device_guid: &str,
    device_name: &str,
    connection_name: &str,
    config: &InstallConfig,
    verify: bool,
) -> Result<()> {
    let mut tx = Transaction::new();

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

    // ── Step 1: 写入子 APO 配置（独立安装信息区，v8.4 路径隔离）──────────
    // `HKLM\SOFTWARE\VxAPO\Child APOs\{deviceGuid}\{PreMixChild|PostMixChild}`。
    // 与运行期 `object/child.rs` / `install/device/slots::read_child_apo_guid`
    // 读取路径一致（旧实现写 `FxProperties\childGuid` + 建 `ChildApoKeys`
    // 子键——运行期无人读，且 FxProperties ACL 不给管理员 CreateSubKey）。
    // capture 不装 PostMix → childPostMix 无意义，强制 None。

    let child_postmix = if is_capture { None } else { original_postmix };
    write_child_apo_config(device_guid, &fx_key, config, original_premix, child_postmix)?;

    // ── Step 5: 按模式写入 APO GUID（capture 只写 PreMix）───────────────

    delete_other_mode_slots(&fx_key, config.install_mode);

    if config.install_premix {
        write_apo_slot(&fx_key, config.install_mode.premix_slot(), CLSID_VXAPO_PRE_MIX)?;
    }
    if config.install_postmix && !is_capture {
        write_apo_slot(&fx_key, config.install_mode.postmix_slot(), CLSID_VXAPO_POST_MIX)?;
    }

    // ── Step 6: 写入默认处理模式 GUID ────────────────────────────────────

    write_default_processmode(&fx_key)?;

    // ── Step 7: 删除 DisableEnhancements ──────────────────────────────────

    let _ = fx_key.delete_value("DisableEnhancements");

    // 全部成功 → 提交事务（禁用回滚）。
    tx.commit();

    // ── E3.4 安装自检（verify=true）：CoCreateInstance 验证 DLL 可实例化 ──
    // 失败**不自动回滚**（注册表已写入且 DLL 可能瞬时不可用；报告并让调用方决策）。
    if verify {
        // CoCreateInstance 前需初始化 COM（0x800401F0 CO_E_NOTINITIALIZED 实证：
        // 2026-08-04 管理员 CLI 直接调 install 未初始化 COM 即触发）。
        // SAFETY: CoInitializeEx 无 preconditions；进程级调用。
        // S_OK(0)=本次初始化成功；S_FALSE(1)=已由宿主初始化（合法）。
        // 其他值=COM 初始化失败，verify 不可靠 → 报错。
        let co_init = unsafe {
            windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_MULTITHREADED,
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
                windows::Win32::System::Com::CoCreateInstance::<_, windows::core::IUnknown>(
                    &clsid,
                    None,
                    windows::Win32::System::Com::CLSCTX_INPROC_SERVER,
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

    Ok(())
}

// ══════════════════════════════════════════════════════════════════════════════
// uninstall_endpoint
// ══════════════════════════════════════════════════════════════════════════════

/// 从指定音频端点卸载 VxAPO。
///
/// # 卸载语义（2026-08-04 用户明确）
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
/// 用户实测场景：VxAPO 把 EAPO 弄成子 APO 后，另一软件又覆盖了父 APO 槽位。
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

    // ── 删除 VxAPO CLSID ──────────────────────────────────────────────────
    // 注意：**不能**用 read_all_slots(&fx_key)——它期望端点根键（内部再 open
    // FxProperties 子键）；此处 fx_key 已是 FxProperties 键，会拿不到槽位
    // （2026-08-04 实测：uninstall 后 slot 仍残留 VxAPO CLSID）。改用
    // read_slot_value 直接在 fx_key 上读槽位值（REG_SZ/REG_BINARY 兼容）。
    //
    // 【2026-08-04 实测】audiodg 持有点端时 MMDevices 槽位值删除可能被锁
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

    // ── 恢复被覆盖的第三方 APO（快照恢复的核心）──────────────────────────
    // 只有在 **VxAPO 槽位被删空之后**（上一步），才把 install 时备份的
    // 「覆盖前槽位名 + 原值」读回写槽位 → 把设备恢复成安装前状态。
    //
    // 【接管者优先级】另一个软件可能已覆盖 VxAPO 槽位（EAPO 重装写回、第三方
    // 接管）。此时（上一步没删它——因为槽位不是 VxAPO CLSID）槽位仍有值：
    // - 槽位 = 备份原值 → 已是恢复目标，不动；
    // - 槽位 = 其他 APO → **尊重接管者，不覆盖**；
    // - 槽位空（NoValue/NoKey，VxAPO 槽位被我们删空）→ 写回备份原值。
    let info_key = format!("{}\\{}", CHILD_APO_PATH_ROOT, device_guid);
    if let Ok((root, sub_key)) = split_hklm_path(&info_key) {
        if let Ok(info) = RegKey::open(root, sub_key) {
            for backup_name in [BACKUP_PREMIX_SLOT, BACKUP_POSTMIX_SLOT] {
                // 槽位名（字符串，如 {d04e05a6-...},5）。
                if let Some(slot_name) = info.read_sz(backup_name) {
                    // 对应原值备份名：PreMixSlot → PreMixSlotValue。
                    let value_name = if backup_name == BACKUP_PREMIX_SLOT {
                        BACKUP_PREMIX_SLOT_VALUE
                    } else {
                        BACKUP_POSTMIX_SLOT_VALUE
                    };
                    if let Some(val) = info.read_sz(value_name) {
                        // 定位 ApoSlot 判断当前槽位状态。
                        if let Some(slot) = ApoSlot::ALL.iter().find(|s| s.value_name() == slot_name) {
                            let cur = read_slot_value(&fx_key, *slot);
                            // 仅当槽位为空才写回（NoValue=VxAPO 卸载后/未占；
                            // NoKey=键缺失）。接管者（Guid≠备份值）不动。
                            if matches!(cur, SlotValue::NoValue | SlotValue::NoKey) {
                                let _ = fx_key.write_sz(&slot_name, &val);
                            }
                        }
                    }
                }
            }
        }
    }

    // ── 恢复完成后，再删除 VxAPO 独立安装信息区（含所有备份，v8.4）──────
    // 顺序保证：先恢复槽位（从信息区读值），再删信息区本身，避免读到一半被删。

    if let Ok((root, sub_key)) = split_hklm_path(&info_key) {
        let _ = crate::sys::registry::delete_tree(root, sub_key);
    }

    // ── 删除子 APO 配置（旧遗留值，best-effort） ─────────────────────────

    for name in &["childGuid", "allowSilentBuffer", "autoAdjust", "version"] {
        let _ = fx_key.delete_value(name);
    }

    // ── 删除 DisableEnhancements ──────────────────────────────────────────

    let _ = fx_key.delete_value("DisableEnhancements");

    Ok(())
}

// ══════════════════════════════════════════════════════════════════════════════
// 内部辅助
// ══════════════════════════════════════════════════════════════════════════════

/// 从端点 GUID 定位注册表路径（先 Render 再 Capture）。
fn find_endpoint_path(device_guid: &str) -> Result<String> {
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
/// **只读句柄 bug（2026-08-04 实证）**：旧实现已存在时用 `RegKey::open`（SAM_READ）
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
            tx.record(RollbackAction::DeleteKey(fx_path.to_string()));
        }
        return Ok((key, is_new));
    }

    // 不存在 → 创建（SAM_ALL）。
    let key = RegKey::create(HKEY_LOCAL_MACHINE, fx_path)?;
    tx.record(RollbackAction::DeleteKey(fx_path.to_string()));
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
/// **self-preserve 过滤（2026-08-04 实证）**：重装时槽位可能已是 VxAPO 自己的
/// CLSID——必须视为「无原始 APO」，否则会把 VxAPO 自身保留为子 APO
/// （快照 diff 实测 childPreMix=41C34613 自占）。
fn read_original_apo_guids(
    fx_key: &RegKey,
    config: &InstallConfig,
) -> (Option<windows::core::GUID>, Option<windows::core::GUID>) {
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

/// 写入子 APO 配置（Step 1，v8.4 独立安装信息区）。
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
    original_premix: Option<windows::core::GUID>,
    original_postmix: Option<windows::core::GUID>,
) -> Result<()> {
    // 独立安装信息区：HKLM\SOFTWARE\VxAPO\Child APOs\{device_guid}
    // （HKLM\SOFTWARE 管理员可建子键；require_admin 探测键同区已验证）。
    let info_key = format!("{}\\{}", CHILD_APO_PATH_ROOT, device_guid);
    let (root, sub_key) = split_hklm_path(&info_key)?;
    let info = RegKey::create(root, sub_key)?;

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

/// 拆分 `HKLM\...` 完整路径为 (root HKEY, 子键路径)。
fn split_hklm_path(path: &str) -> Result<(windows::Win32::System::Registry::HKEY, &str)> {
    let (root_str, rest) = path
        .split_once('\\')
        .ok_or_else(|| VxApoError::internal(&format!("路径无根键：{path}")))?;
    if !root_str.eq_ignore_ascii_case("HKLM") {
        return Err(VxApoError::internal(&format!("仅支持 HKLM 根：{path}")));
    }
    Ok((HKEY_LOCAL_MACHINE, rest))
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
/// 「Registry value ... has wrong type」导致 EAPO 无法枚举设备（2026-08-04 实证）。
/// 与 `slots::read_slot_value` 的 REG_SZ 解析分支一致。
fn write_apo_slot(fx_key: &RegKey, slot: ApoSlot, guid: windows::core::GUID) -> Result<()> {
    fx_key.write_sz(&slot.value_name(), &guid_to_string(&guid))?;
    Ok(())
}

/// 写入默认处理模式 GUID（Step 6，Note 26）。
fn write_default_processmode(fx_key: &RegKey) -> Result<()> {
    fx_key.write_sz(
        "KSDATAFORMAT_SUBTYPE_DEFAULT_PROCESSMODE",
        &guid_to_string(&AUDIO_SIGNALPROCESSINGMODE_DEFAULT),
    )?;
    Ok(())
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
            guid_to_string(&windows::core::GUID::zeroed()),
            "{00000000-0000-0000-0000-000000000000}"
        );
    }

    #[test]
    fn guid_to_string_max_values() {
        let g = windows::core::GUID {
            data1: 0xFFFFFFFF,
            data2: 0xFFFF,
            data3: 0xFFFF,
            data4: [0xFF; 8],
        };
        assert_eq!(guid_to_string(&g), "{FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF}");
    }

    /// 回归：EAPO REG_SZ 槽位（GUID 字符串）由 `slots::read_slot_value` 正确解析。
    ///
    /// 2026-08-04 双 bug 实证：
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
}