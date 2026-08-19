//! object/apo/test_pipe.rs — 安装验证管道客户端（`install --verify` 专用）
//!
//! `Initialize` 时读取 `HKLM\SOFTWARE\VxAPO\DeviceTestPipeName`，存在则连接
//! `\\.\pipe\<name>` 发送一行 JSON 阶段消息：
//! `{"deviceGuid":"{...}","stage":"premix|postmix","phase":"initialize|child_apo"}`。
//!
//! 所有失败静默（不阻塞/不影响正常 Initialize）；无管道名时零开销返回。

use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, WriteFile, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
use windows::core::HSTRING;

use crate::sys::registry::RegKey;

/// 验证管道名所在全局键：HKLM\SOFTWARE\VxAPO。
const ROOT: &str = r"SOFTWARE\VxAPO";
/// 管道名值（CLI 写入、验证后删除）。
const VALUE_NAME: &str = "DeviceTestPipeName";

/// 上报阶段消息（best-effort，失败静默）。
pub(crate) fn notify(device_guid: &str, stage: &str, phase: &str) {
    let Some(pipe_name) = read_pipe_name() else {
        return;
    };
    let path = format!(r"\\.\pipe\{pipe_name}");
    let payload = format!(
        "{{\"deviceGuid\":\"{device_guid}\",\"stage\":\"{stage}\",\"phase\":\"{phase}\"}}\n"
    );

    // 服务重启后 audiodg 首次连接可能恰逢服务端 ConnectNamedPipe 尚未就绪，
    // 短重试 3 次（共约 600ms）。
    let mut handle = open_pipe(&path);
    for _ in 0..3 {
        if handle != INVALID_HANDLE_VALUE {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
        handle = open_pipe(&path);
    }
    if handle == INVALID_HANDLE_VALUE {
        crate::object::apo::config::diag_append(&format!(
            "TESTPIPE connect-fail stage={stage} phase={phase}"
        ));
        return;
    }

    let mut written = 0u32;
    // SAFETY: handle 有效（CreateFileW 成功）；payload 为有效字节切片。
    unsafe {
        let ok = WriteFile(handle, Some(payload.as_bytes()), Some(&mut written), None);
        let _ = CloseHandle(handle);
        crate::object::apo::config::diag_append(&format!(
            "TESTPIPE stage={stage} phase={phase} written={written} ok={}",
            ok.is_ok()
        ));
    }
}

/// 打开管道写端（失败返回 INVALID_HANDLE_VALUE）。
fn open_pipe(path: &str) -> HANDLE {
    // SAFETY: path 为有效管道路径字符串；其余参数为标准打开语义。
    unsafe {
        CreateFileW(
            &HSTRING::from(path),
            GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
        .unwrap_or(INVALID_HANDLE_VALUE)
    }
}

/// 读取验证管道名（值不存在 / 为空 → None）。
fn read_pipe_name() -> Option<String> {
    let key = RegKey::open(HKEY_LOCAL_MACHINE, ROOT).ok()?;
    key.read_sz_value(VALUE_NAME).ok().filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 无管道名时 notify 必须零副作用直接返回（不 panic、不连接）。
    #[test]
    fn notify_without_pipe_name_is_noop() {
        notify("{00000000-0000-0000-0000-000000000000}", "premix", "initialize");
        notify("{00000000-0000-0000-0000-000000000000}", "postmix", "child_apo");
    }
}
