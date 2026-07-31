//! host/installation/audiodg.rs — DisableProtectedAudioDG 检查与修复（Note 33）
//!
//! `DisableProtectedAudioDG` 注册表值不存在或不为 1 时，
//! Windows 阻止第三方 APO 加载到 audiodg.exe 进程中。
//!
//! 检查逻辑：读取该值，不存在或 ≠1 则判定为阻止加载。
//! 修复逻辑：写入 1，允许第三方 APO 加载。
//!
//! 此模块涉及注册表写入，实际操作委托 `sys/registry/write.rs`。

use windows::Win32::System::Registry::{HKEY, HKEY_LOCAL_MACHINE};

use crate::utils::error::{Result};
use crate::sys::registry::read::{RegKey, RegValue};
use crate::sys::registry::write::{self, close_key, create_key, write_dword};

// ══════════════════════════════════════════════════════════════════════════════
// 常量
// ══════════════════════════════════════════════════════════════════════════════

/// 注册表路径。
const AUDIODG_KEY_PATH: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Audio";

/// 值名。
const VALUE_NAME: &str = "DisableProtectedAudioDG";

// ══════════════════════════════════════════════════════════════════════════════
// 检查
// ══════════════════════════════════════════════════════════════════════════════

/// 检查 `DisableProtectedAudioDG` 是否已设置。
///
/// 返回 `true` 如果值存在且为 1（允许第三方加载）。
/// 返回 `false` 如果值不存在或不为 1（阻止第三方加载）。
pub fn is_disabled() -> Result<bool> {
    let key = match RegKey::open(HKEY_LOCAL_MACHINE, AUDIODG_KEY_PATH) {
        Ok(k) => k,
        Err(_) => return Ok(false), // 键不存在 → 阻止
    };

    match key.read_value(VALUE_NAME) {
        Ok(RegValue::Dword(v)) => Ok(v == 1),
        Ok(_) => Ok(false), // 非 DWORD → 阻止
        Err(_) => Ok(false), // 值不存在 → 阻止
    }
}

/// 检查是否允许第三方 APO 加载。
///
/// `is_disabled()` 的语义反转——返回 `true` 表示可以加载。
pub fn is_third_party_allowed() -> Result<bool> {
    Ok(is_disabled()?)
}

// ══════════════════════════════════════════════════════════════════════════════
// 修复
// ══════════════════════════════════════════════════════════════════════════════

/// 设置 `DisableProtectedAudioDG = 1`。
///
/// 在 `install()` 中调用（Note 33 fix）。
/// 需要管理员权限（写入 HKLM）。
pub fn disable() -> Result<()> {
    let handle = create_key(HKEY_LOCAL_MACHINE, AUDIODG_KEY_PATH)?;
    write_dword(handle, VALUE_NAME, 1)?;
    // SAFETY: handle 由 create_key 成功打开。
    close_key(handle);
    Ok(())
}

/// 删除 `DisableProtectedAudioDG` 值（恢复默认行为）。
pub fn restore() -> Result<()> {
    let key = RegKey::open(HKEY_LOCAL_MACHINE, AUDIODG_KEY_PATH)?;
    match write::delete_value(key_handle_from_regkey(&key), VALUE_NAME) {
        Ok(()) => Ok(()),
        Err(_) => Ok(()), // 值不存在不算错误
    }
}

/// 临时从 RegKey 获取底层 HKEY（测试辅助）。
///
/// Phase 5 初期用 unsafe 转换。后续在 RegKey 中添加 `handle()` 方法。
fn key_handle_from_regkey(key: &RegKey) -> HKEY {
    // SAFETY: RegKey 内部持有 HKEY，这里临时借用。
    // 实际实现中应让 RegKey 暴露 handle() 方法。
    unsafe { std::ptr::read(key as *const RegKey as *const HKEY) }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_runs_without_panic() {
        // 只验证不 panic，不验证具体值（取决于系统状态）
        let _ = is_disabled();
        let _ = is_third_party_allowed();
    }

    #[test]
    fn disable_and_check() {
        // 需要管理员权限才能写入 HKLM
        // 普通用户测试中可能失败，但不应 panic
        if disable().is_ok() {
            assert!(is_disabled().unwrap());
        }
    }
}