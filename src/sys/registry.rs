//! sys/registry — 注册表底层操作
//!
//! 提供注册表的读、写、删除操作。
//! 所有辅助函数集中在此模块，供子模块共享。

pub mod read;
pub mod write;
pub mod delete;

// ── 共享辅助函数 ──────────────────────────────────────────────────────────

use windows::Win32::Foundation::WIN32_ERROR;
use windows::core::{Result, HRESULT};

/// 将 `WIN32_ERROR` 转为 `Result<()>`
pub(crate) fn win32_ok(err: WIN32_ERROR) -> Result<()> {
    if err.0 == 0 {
        Ok(())
    } else {
        Err(windows::core::Error::from_hresult(HRESULT(
            (0x8007_0000u32 | (err.0 & 0xFFFF)) as i32,
        )))
    }
}