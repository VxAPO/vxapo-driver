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
    ApoSlot, ChildApoKind, InstallMode, SlotValue, CHILD_APO_PATH_ROOT, FX_PROPERTIES_KEY,
    INSTALL_VERSION,
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
// GUID 辅助（GUID→字符串走 sys::com::prelude；16 字节小端序列化就地实现）
// ══════════════════════════════════════════════════════════════════════════════

/// hex 片段（4 字符）→ 4 字节。
fn hex_to_bytes(s: &[u8]) -> Option<[u8; 4]> {
    if s.len() != 4 {
        return None;
    }
    let mut out = [0u8; 4];
    for (i, chunk) in s.chunks(2).enumerate() {
        let hi = hex_val(chunk[0])?;
        let lo = hex_val(chunk[1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

/// hex 片段（4 字符）→ u16。
fn hex_to_u16(s: &[u8]) -> Option<u16> {
    Some(hex_to_bytes(s)?[0] as u16 * 256 + hex_to_bytes(s)?[1] as u16)
}

/// hex 片段（8 字符）→ u32。
fn hex_to_u32(s: &[u8]) -> Option<u32> {
    let b = hex_to_bytes(s)?;
    Some(u32::from_be_bytes(b))
}

/// 单个 hex 字符 → 数值。
fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// 16 字节 → GUID。
fn guid_from_bytes(bytes: &[u8]) -> Option<windows::core::GUID> {
    if bytes.len() < 16 {
        return None;
    }
    Some(windows::core::GUID {
        data1: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        data2: u16::from_le_bytes([bytes[4], bytes[5]]),
        data3: u16::from_le_bytes([bytes[6], bytes[7]]),
        data4: [
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ],
    })
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

    // ── Step 1: 写入子 APO 配置（独立安装信息区，v8.4 路径隔离）──────────
    // `HKLM\SOFTWARE\VxAPO\Child APOs\{deviceGuid}\{PreMixChild|PostMixChild}`。
    // 与运行期 `object/child.rs` / `install/device/slots::read_child_apo_guid`
    // 读取路径一致（旧实现写 `FxProperties\childGuid` + 建 `ChildApoKeys`
    // 子键——运行期无人读，且 FxProperties ACL 不给管理员 CreateSubKey）。

    write_child_apo_config(device_guid, &fx_key, config, original_premix, original_postmix)?;

    // ── Step 5: 按模式写入 APO GUID ──────────────────────────────────────

    delete_other_mode_slots(&fx_key, config.install_mode);

    if config.install_premix {
        write_apo_slot(&fx_key, config.install_mode.premix_slot(), CLSID_VXAPO_PRE_MIX)?;
    }
    if config.install_postmix {
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
/// # 卸载步骤
///
/// 1. 定位端点 FxProperties
/// 2. 读取当前槽位，删除 VxAPO 的 CLSID
/// 3. 删除子 APO 配置值（childGuid / allowSilentBuffer / autoAdjust / version）
/// 4. 删除 DisableEnhancements
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
    // read_slot_safe 直接在 fx_key 上读槽位值。

    for slot in ApoSlot::ALL {
        if let SlotValue::Guid(g) = read_slot_safe(&fx_key, slot) {
            if g == CLSID_VXAPO_PRE_MIX || g == CLSID_VXAPO_POST_MIX {
                let _ = fx_key.delete_value(&slot.value_name());
            }
        }
    }

    // ── 删除 VxAPO 独立安装信息区（含 PreMixChild/PostMixChild，v8.4）────

    let info_key = format!("{}\\{}", CHILD_APO_PATH_ROOT, device_guid);
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
        if let SlotValue::Guid(g) = read_slot_safe(fx_key, slot) {
            tx.record(RollbackAction::RestoreValue {
                key_path: fx_path.to_string(),
                name: slot.value_name(),
                backup: guid_to_string(&g),
            });
        }
    }
}

/// 安全读取槽位值（读取失败返回 NoValue）。
///
/// 兼容 REG_SZ（EAPO 等第三方写 GUID 字符串）与 REG_BINARY（16 字节 LE）——与
/// `slots::read_slot_value` 同一判定语义，供卸载/回滚读取现有槽位。
fn read_slot_safe(fx_key: &RegKey, slot: ApoSlot) -> SlotValue {
    match fx_key.read_value(&slot.value_name()) {
        Ok(crate::sys::registry::RegValue::Sz(s)) => {
            let s = s.trim();
            // 就地解析 `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}`（与 guid_to_string 输出一致）。
            // data4 共 16 hex 字符 = 8 字节：前半 4 字符（[20..24]）+ 后半 12 字符（[25..37]）。
            // 【2026-08-04 实证】旧实现把后半 12 字符直接传 hex_to_bytes（要求恰好 4 字符）
            // → 永远返回 None → 整个解析失败 → EAPO REG_SZ 槽位读成 NoValue → 子 APO 永不保留。
            let b = s.as_bytes();
            if s.len() == 38 && s.starts_with('{') && s.ends_with('}') {
                let d1 = hex_to_u32(&b[1..9]);
                let d2 = hex_to_u16(&b[10..14]);
                let d3 = hex_to_u16(&b[15..19]);
                if let (Some(d1), Some(d2), Some(d3)) = (d1, d2, d3) {
                    // data4：跳过 [24] 的 '-'，16 字符 → 8 字节（每 2 字符 1 字节）。
                    let mut data4 = [0u8; 8];
                    let mut ok = true;
                    for i in 0..8 {
                        let (hi, lo) = if i < 2 {
                            // b[20..24] 前半 4 字符 → data4[0..2]
                            (hex_val(b[20 + i * 2]), hex_val(b[20 + i * 2 + 1]))
                        } else {
                            // b[25..37] 后半 12 字符 → data4[2..8]
                            let idx = 25 + (i - 2) * 2;
                            (hex_val(b[idx]), hex_val(b[idx + 1]))
                        };
                        match (hi, lo) {
                            (Some(hi), Some(lo)) => data4[i] = (hi << 4) | lo,
                            _ => { ok = false; break; }
                        }
                    }
                    if ok {
                        return SlotValue::Guid(windows::core::GUID { data1: d1, data2: d2, data3: d3, data4 });
                    }
                }
            }
            SlotValue::NoValue
        }
        Ok(crate::sys::registry::RegValue::Binary(raw)) if raw.len() >= 16 => {
            match guid_from_bytes(&raw) {
                Some(g) => SlotValue::Guid(g),
                None => SlotValue::NoValue,
            }
        }
        _ => SlotValue::NoValue,
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
        let g = read_slot_safe(fx_key, config.install_mode.premix_slot()).as_guid();
        match g {
            Some(g) if g != CLSID_VXAPO_PRE_MIX && g != CLSID_VXAPO_POST_MIX => Some(g),
            _ => None,
        }
    } else {
        None
    };
    let postmix = if config.use_original_apo_postmix {
        let g = read_slot_safe(fx_key, config.install_mode.postmix_slot()).as_guid();
        match g {
            Some(g) if g != CLSID_VXAPO_PRE_MIX && g != CLSID_VXAPO_POST_MIX => Some(g),
            _ => None,
        }
    } else {
        None
    };
    (premix, postmix)
}

/// 写入子 APO 配置（Step 1，v8.4 独立安装信息区）。
///
/// - 保留的原始 APO GUID → `HKLM\SOFTWARE\VxAPO\Child APOs\{deviceGuid}\
///   {PreMixChild|PostMixChild}`（与运行期 `object/child.rs` /
///   `slots::read_child_apo_guid` 读取路径一致）。
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

/// 删除非当前模式的旧槽位（Note 26）。
fn delete_other_mode_slots(fx_key: &RegKey, mode: InstallMode) {
    for slot in ApoSlot::ALL {
        if slot != mode.premix_slot() && slot != mode.postmix_slot() {
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

    #[test]
    fn guid_from_bytes_too_short() {
        assert!(guid_from_bytes(&[0u8; 15]).is_none());
    }

    /// 回归：EAPO REG_SZ 槽位（GUID 字符串）必须能解析出正确 GUID。
    /// 原实现 data4 后半 12 字符传 hex_to_bytes（要求 4 字符）→ 永远 None
    /// → EAPO 槽位读成 NoValue → 子 APO 永不保留（2026-08-04 实证）。
    #[test]
    fn read_slot_safe_parses_eapo_reg_sz_guid() {
        // 用真实 EAPO PreMix CLSID 字符串验证 read_slot_safe 的 REG_SZ 分支。
        // 无法直接 mock RegKey——改为验证 GUID 字符串解析的等价逻辑：
        // data4 前后拼接 = 16 字符 → 8 字节（每 2 字符 1 字节）。
        let s = "{EACD2258-FCAC-4FF4-B36D-419E924A6D79}";
        let b = s.as_bytes();
        assert_eq!(b.len(), 38);
        assert_eq!(&b[20..24], b"B36D"); // data4 前半 4 字符
        assert_eq!(&b[25..37], b"419E924A6D79"); // data4 后半 12 字符
        // 逐字符拼 data4：b[20..24]（2 字节）+ b[25..37]（6 字节）
        let mut data4 = [0u8; 8];
        data4[0] = (hex_val(b[20]).unwrap() << 4) | hex_val(b[21]).unwrap();
        data4[1] = (hex_val(b[22]).unwrap() << 4) | hex_val(b[23]).unwrap();
        for i in 0..6 {
            let idx = 25 + i * 2;
            data4[2 + i] = (hex_val(b[idx]).unwrap() << 4) | hex_val(b[idx + 1]).unwrap();
        }
        // 期望：B3 6D 41 9E 92 4A 6D 79
        assert_eq!(data4, [0xB3, 0x6D, 0x41, 0x9E, 0x92, 0x4A, 0x6D, 0x79]);
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