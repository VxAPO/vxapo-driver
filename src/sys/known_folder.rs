//! sys/known_folder.rs — 已知文件夹路径解析（v6.3 规范 3.6）
//!
//! 职责：封装 `SHGetKnownFolderPath(FOLDERID_Documents)` 为安全函数 `documents_folder()`。
//! 只做"已知文件夹 → 字符串路径"的 FFI 收窄。不知道 VxAPO / config.txt / 设备 GUID 拼接。
//!
//! 引用来源：
//! - `windows::Win32::UI::Shell::{SHGetKnownFolderPath, FOLDERID_Documents}`
//! - `windows::Win32::System::Com::CoTaskMemFree`（释放 SHGetKnownFolderPath 分配的 PWSTR）
//!
//! 导出给：`object/apo.rs`（Initialize per-device 路径解析）。

use windows::core::{Error, PWSTR};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_Documents, SHGetKnownFolderPath};

/// RAII 守卫：释放 SHGetKnownFolderPath 返回的 CoTaskMemAlloc 内存。
///
/// 所有路径（正常 + 错误）均释放——即使转换 String 失败也先记录指针再释放，
/// 杜绝泄漏（规范 3.6 强制要求）。
struct CoTaskMemGuard(PWSTR);

impl Drop for CoTaskMemGuard {
    fn drop(&mut self) {
        // Safety: SHGetKnownFolderPath 分配的 PWSTR 由 CoTaskMemFree 释放（Windows API 契约）。
        unsafe {
            CoTaskMemFree(Some(self.0 .0 as *const _));
        }
    }
}

/// 返回文档（Documents）文件夹的绝对路径（不含结尾反斜杠）。
///
/// 封装 `SHGetKnownFolderPath(FOLDERID_Documents, KF_FLAG_DEFAULT, ...)`：
/// 1. 调用返回 `PWSTR`（CoTaskMemAlloc 分配）
/// 2. 转 UTF-16 → String
/// 3. `CoTaskMemFree` 释放（RAII 守卫，所有路径包括错误路径均释放）
///
/// # Errors
/// - `Error`：SHGetKnownFolderPath 返回非 S_OK（windows-rs 自动转 `Result<PWSTR>`）
/// - `FromUtf16Error`：返回路径含非法 UTF-16（极不可能，防御性映射为 E_UNEXPECTED）
pub fn documents_folder() -> Result<String, Error> {
    // Safety: FOLDERID_Documents 为静态 GUID；KF_FLAG_DEFAULT 无特殊标志；
    // htoken 传 None（当前用户）。返回的 PWSTR 由 CoTaskMemAlloc 分配，调用方负责释放。
    let path_ptr = unsafe { SHGetKnownFolderPath(&FOLDERID_Documents, Default::default(), None)? };

    // RAII 守卫持有 PWSTR，作用域结束（含 FromUtf16Error 提前 return 路径）即释放。
    let guard = CoTaskMemGuard(path_ptr);

    // Safety: SHGetKnownFolderPath 成功返回的 PWSTR 指向以 null 结尾的 UTF-16 字符串。
    let s = unsafe { path_ptr.to_string() }
        .map_err(|_| Error::from(windows::Win32::Foundation::E_UNEXPECTED))?;
    let _ = guard; // 显式绑定避免未使用（Drop 仍在作用域末执行）。
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_folder_returns_path() {
        // 真实环境：Documents 文件夹应存在且以盘符开头。
        // 测试机可能无 Documents（CI/精简环境），失败也接受（不 panic）。
        if let Ok(path) = documents_folder() {
            assert!(!path.is_empty());
            let bytes = path.as_bytes();
            assert!(bytes[0].is_ascii_alphabetic() && bytes[1] == b':');
        }
    }
}