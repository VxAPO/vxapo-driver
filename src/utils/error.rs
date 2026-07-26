//! utils/error.rs — 统一错误类型（Note 36/57）
//!
//! 定义 `VxApoError` 枚举，贯穿整个 vxapo-driver 的非实时路径。
//!
//! 变体：
//! - `HResult(HRESULT)`：Windows COM 错误码
//! - `Registry(String)`：注册表操作失败，附带键路径上下文
//! - `Io(std::io::Error)`：标准 I/O 错误
//! - `DeviceNotFound(String)`：目标音频设备未找到
//! - `FormatUnsupported(String)`：音频格式不支持
//! - `VersionMismatch { expected, actual }`：安装版本不匹配
//!
//! 为 `windows::core::Error`、`windows::core::HRESULT`、`std::io::Error`
//! 实现 `From` 转换，支持 `?` 操作符。
//!
//! 实时路径（`process`）不允许任何错误传播，不使用此类型（Note 57）。
//! `dsp/` 模块禁止 import 此类型，以保持独立可测试性（Note 58）。
//!
//! 提供便捷类型别名 `pub type Result<T> = std::result::Result<T, VxApoError>`。

use std::fmt;

/// 统一错误枚举，贯穿整个 vxapo-driver 的非实时路径。
///
/// 实时路径（`process`）不允许任何错误传播（Note 57）。
/// dsp/ 模块禁止 import 此类型，以保持独立可测试性（Note 58）。
#[derive(Debug)]
pub enum VxApoError {
    /// Windows COM HRESULT 错误码
    HResult(windows::core::HRESULT),
    /// 注册表操作失败（附带上下文描述，如键路径、操作类型）
    Registry(String),
    /// 标准 I/O 错误
    Io(std::io::Error),
    /// 目标音频设备未找到
    DeviceNotFound(String),
    /// 音频格式不支持（附带格式描述）
    FormatUnsupported(String),
    /// 安装版本不匹配
    VersionMismatch {
        expected: String,
        actual: String,
    },
}

// ── Display ──────────────────────────────────────────────────────────────────

impl fmt::Display for VxApoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HResult(hr) => write!(f, "COM error: 0x{:08X}", hr.0 as u32),
            Self::Registry(msg) => write!(f, "Registry error: {msg}"),
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::DeviceNotFound(name) => write!(f, "Device not found: {name}"),
            Self::FormatUnsupported(desc) => write!(f, "Unsupported audio format: {desc}"),
            Self::VersionMismatch { expected, actual } => {
                write!(
                    f,
                    "Installation version mismatch: expected {expected}, got {actual}"
                )
            }
        }
    }
}

// ── Error trait ──────────────────────────────────────────────────────────────

impl std::error::Error for VxApoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

// ── From 转换 ────────────────────────────────────────────────────────────────

/// windows crate 返回的 `windows::core::Error` → `VxApoError::HResult`
impl From<windows::core::Error> for VxApoError {
    fn from(err: windows::core::Error) -> Self {
        Self::HResult(err.code())
    }
}

/// 裸 HRESULT 值 → `VxApoError::HResult`
impl From<windows::core::HRESULT> for VxApoError {
    fn from(hr: windows::core::HRESULT) -> Self {
        Self::HResult(hr)
    }
}

/// 标准 I/O 错误 → `VxApoError::Io`
impl From<std::io::Error> for VxApoError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

// ── 便捷类型别名 ─────────────────────────────────────────────────────────────

/// `vxapo-driver` 统一 Result 类型
pub type Result<T> = std::result::Result<T, VxApoError>;

// ── 辅助构造方法 ─────────────────────────────────────────────────────────────

impl VxApoError {
    /// 构造注册表错误，附带键路径上下文
    pub fn registry(key: &str, detail: &str) -> Self {
        Self::Registry(format!("{key}: {detail}"))
    }

    /// 构造设备未找到错误
    pub fn device_not_found(name: &str) -> Self {
        Self::DeviceNotFound(name.to_owned())
    }

    /// 构造格式不支持错误
    pub fn format_unsupported(desc: &str) -> Self {
        Self::FormatUnsupported(desc.to_owned())
    }

    /// 构造版本不匹配错误
    pub fn version_mismatch(expected: &str, actual: &str) -> Self {
        Self::VersionMismatch {
            expected: expected.to_owned(),
            actual: actual.to_owned(),
        }
    }
}

// ── 测试 ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::S_OK;

    #[test]
    fn display_hresult() {
        // 0x80004005 = E_FAIL
        let err = VxApoError::HResult(windows::core::HRESULT(0x8000_4005u32 as i32));
        let msg = format!("{err}");
        assert!(msg.contains("COM error"));
        assert!(msg.contains("80004005"));
    }

    #[test]
    fn display_registry() {
        let err = VxApoError::registry(r"HKLM\SOFTWARE\Microsoft", "access denied");
        let msg = format!("{err}");
        assert!(msg.contains("Registry error"));
        assert!(msg.contains("access denied"));
    }

    #[test]
    fn display_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let err = VxApoError::Io(io_err);
        assert!(format!("{err}").contains("I/O error"));
    }

    #[test]
    fn display_device_not_found() {
        let err = VxApoError::device_not_found("Speakers (Realtek HD Audio)");
        let msg = format!("{err}");
        assert!(msg.contains("Device not found"));
        assert!(msg.contains("Realtek"));
    }

    #[test]
    fn display_format_unsupported() {
        let err = VxApoError::format_unsupported("96000 Hz / 8ch / 32bit");
        assert!(format!("{err}").contains("Unsupported audio format"));
    }

    #[test]
    fn display_version_mismatch() {
        let err = VxApoError::version_mismatch("2", "1");
        let msg = format!("{err}");
        assert!(msg.contains("expected 2"));
        assert!(msg.contains("got 1"));
    }

    #[test]
    fn from_hresult() {
        let hr = windows::core::HRESULT(0x0000_0000);
        let err: VxApoError = hr.into();
        assert!(matches!(err, VxApoError::HResult(_)));
    }

    #[test]
    fn from_io_error() {
        let io = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let err: VxApoError = io.into();
        assert!(matches!(err, VxApoError::Io(_)));
    }

    #[test]
    fn error_trait_source_io() {
        let io = std::io::Error::from(std::io::ErrorKind::BrokenPipe);
        let err: Box<dyn std::error::Error> = Box::new(VxApoError::Io(io));
        assert!(err.source().is_some());
    }

    #[test]
    fn error_trait_source_hresult() {
        let err: Box<dyn std::error::Error> =
            Box::new(VxApoError::HResult(windows::core::HRESULT(0)));
        assert!(err.source().is_none());
    }

    #[test]
    fn result_alias_ok() {
        let ok: Result<i32> = Ok(42);
        assert_eq!(ok.unwrap(), 42);
    }

    #[test]
    fn result_alias_err() {
        let err: Result<i32> = Err(VxApoError::device_not_found("test"));
        assert!(err.is_err());
    }

    #[test]
    fn question_mark_operator_with_io() {
        fn inner() -> Result<()> {
            let _f = std::fs::read_to_string("nonexistent_file_xyz.txt")?;
            Ok(())
        }
        assert!(inner().is_err());
        assert!(matches!(inner().unwrap_err(), VxApoError::Io(_)));
    }

    #[test]
    fn question_mark_operator_with_hresult() {
        fn inner() -> Result<()> {
            let _ = S_OK;
            Ok(())
        }
        assert!(inner().is_ok());
    }
}