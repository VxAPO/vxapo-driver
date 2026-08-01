//! install/audiodg.rs — DisableProtectedAudioDG 检查与修复（v6.2 规范 5.6）
//!
//! 保护模式阻止第三方 APO 加载。通过注册表
//! `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Audio` 下的
//! `DisableProtectedAudioDG`（REG_DWORD）控制。

use crate::sys::registry::RegKey;
use crate::utils::vx_error::Result;
use windows::Win32::System::Registry::{HKEY, HKEY_LOCAL_MACHINE};

/// 注册表路径：HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Audio。
const AUDIO_KEY_PATH: &str =
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\Audio";

/// 值名：DisableProtectedAudioDG。
const VALUE_NAME: &str = "DisableProtectedAudioDG";

// ══════════════════════════════════════════════════════════════════════════════
// 公开 API
// ══════════════════════════════════════════════════════════════════════════════

/// 检查保护是否已禁用。
///
/// 返回 `true`：DisableProtectedAudioDG 值存在且 == 1（允许第三方 APO 加载）。
/// 返回 `false`：值不存在或 != 1（Windows 阻止第三方 APO 加载）。
pub fn is_disabled() -> Result<bool> {
    let key = match RegKey::open(HKEY_LOCAL_MACHINE, AUDIO_KEY_PATH) {
        Ok(k) => k,
        Err(_) => {
            // 键不存在 → 未禁用。
            return Ok(false);
        }
    };

    match key.read_dword_value(VALUE_NAME) {
        Ok(v) => Ok(v == 1),
        Err(_) => Ok(false),
    }
}

/// 检查是否允许第三方 APO 加载。
///
/// `is_disabled()` 的语义别名——返回 `true` 表示可以加载。
pub fn is_third_party_allowed() -> Result<bool> {
    is_disabled()
}

/// 设置 DisableProtectedAudioDG = 1（禁用保护，允许第三方加载）。
///
/// 需要管理员权限（写入 HKLM）。
pub fn disable() -> Result<()> {
    let key = RegKey::create(HKEY_LOCAL_MACHINE, AUDIO_KEY_PATH)?;
    key.write_dword(VALUE_NAME, 1)?;
    Ok(())
}

/// 删除 DisableProtectedAudioDG 值（恢复 Windows 默认保护行为）。
///
/// 值不存在不算错误。
pub fn restore() -> Result<()> {
    let key = match RegKey::open(HKEY_LOCAL_MACHINE, AUDIO_KEY_PATH) {
        Ok(k) => k,
        Err(_) => {
            // 键不存在 → 无需恢复。
            return Ok(());
        }
    };
    key.delete_value(VALUE_NAME)?;
    Ok(())
}

/// 检查并确保允许加载，不允许时尝试修复。
///
/// 由 object/apo.rs LockForProcess 调用。
pub fn ensure_can_load() -> Result<()> {
    if is_disabled()? {
        return Ok(());
    }
    disable()
}

// ══════════════════════════════════════════════════════════════════════════════
// 内部辅助（测试用）
// ══════════════════════════════════════════════════════════════════════════════

/// 使用显式路径查询（测试可注入 HKCU 路径验证逻辑）。
#[allow(dead_code)]
fn is_disabled_at(root: HKEY, path: &str) -> Result<bool> {
    let key = match RegKey::open(root, path) {
        Ok(k) => k,
        Err(_) => return Ok(false),
    };
    match key.read_dword_value(VALUE_NAME) {
        Ok(v) => Ok(v == 1),
        Err(_) => Ok(false),
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Registry::HKEY_CURRENT_USER;

    const TEST_PREFIX: &str = r"SOFTWARE\VxAPO_Test_Audiodg";

    /// 每个测试使用独立子键，避免并行测试互相干扰。
    fn test_path(name: &str) -> String {
        format!("{}\\{}", TEST_PREFIX, name)
    }

    fn cleanup(path: &str) {
        if let Ok(key) = RegKey::open(HKEY_CURRENT_USER, path) {
            let _ = key.delete_value(VALUE_NAME);
        }
    }

    #[test]
    fn missing_key_means_not_disabled() {
        let result = is_disabled_at(HKEY_CURRENT_USER, r"SOFTWARE\VxAPO_Test_Nonexistent_Audiodg");
        assert_eq!(result.unwrap(), false);
    }

    #[test]
    fn write_1_then_disabled() {
        let path = test_path("write_1");
        cleanup(&path);
        let key = RegKey::create(HKEY_CURRENT_USER, &path).unwrap();
        key.write_dword(VALUE_NAME, 1).unwrap();
        assert_eq!(is_disabled_at(HKEY_CURRENT_USER, &path).unwrap(), true);
        cleanup(&path);
    }

    #[test]
    fn write_0_means_not_disabled() {
        let path = test_path("write_0");
        cleanup(&path);
        let key = RegKey::create(HKEY_CURRENT_USER, &path).unwrap();
        key.write_dword(VALUE_NAME, 0).unwrap();
        assert_eq!(is_disabled_at(HKEY_CURRENT_USER, &path).unwrap(), false);
        cleanup(&path);
    }

    #[test]
    fn delete_restores_default() {
        let path = test_path("delete_restore");
        cleanup(&path);
        let key = RegKey::create(HKEY_CURRENT_USER, &path).unwrap();
        key.write_dword(VALUE_NAME, 1).unwrap();
        assert_eq!(is_disabled_at(HKEY_CURRENT_USER, &path).unwrap(), true);

        // 模拟 delete_value（通过 key）
        key.delete_value(VALUE_NAME).unwrap();
        assert_eq!(is_disabled_at(HKEY_CURRENT_USER, &path).unwrap(), false);
        cleanup(&path);
    }
}