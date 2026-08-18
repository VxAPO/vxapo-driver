//! utils/vx_error.rs — VxApoError 业务错误（规范）
//!
//! 边界：不依赖任何其他模块（仅 `windows-core` 的 `HRESULT`）。
//!
//! 定义 `VxApoError` 枚举，贯穿整个 vxapo-driver 的非实时路径。
//! 实时路径（`process`）不允许任何错误传播，不使用此类型。

use core::result;

/// 统一业务错误枚举（规范，8 变体）。
#[derive(Debug, thiserror::Error)]
pub enum VxApoError {
    #[error("注册表错误: {0}")]
    Registry(String),

    #[error("配置解析错误: {0}")]
    Config(String),

    #[error("格式不支持: {0}")]
    Format(String),

    #[error("I/O 错误: {0}")]
    Io(String),

    #[error("内部错误: {0}")]
    Internal(String),

    #[error("实时安全违规: {0}")]
    RtSafety(String),

    #[error("状态转换错误: {0}")]
    State(String),

    #[error("设备未找到: {0}")]
    DeviceNotFound(String),
}

/// `vxapo-driver` 统一 Result 类型。
pub type Result<T> = result::Result<T, VxApoError>;

/// 判断 HRESULT 是否成功（>= 0）。
pub fn succeeded(hr: windows_core::HRESULT) -> bool {
    hr.0 >= 0
}

/// 判断 HRESULT 是否失败（< 0）。
pub fn failed(hr: windows_core::HRESULT) -> bool {
    hr.0 < 0
}

/// 将 HRESULT 转换为 `Result<()>`。
pub fn check_hresult(hr: windows_core::HRESULT) -> Result<()> {
    if succeeded(hr) {
        Ok(())
    } else {
        Err(VxApoError::Internal(format!(
            "HRESULT 错误: 0x{:08X}",
            hr.0
        )))
    }
}

// ── APO 专用 HRESULT 错误码（规范 3.3.7）──────────────────────────────
// 注：这些常量在规范中定义于 sys/com/apo_types.rs，但 utils/ 层禁止依赖 sys/。
// 因此此处内联常量值，保持 utils 独立性（值同规范，两处保持一致）。

/// 0x887D_0001
pub(crate) const APOERR_ALREADY_INITIALIZED: u32 = 0x887D_0001;
/// 0x887D_0003
pub(crate) const APOERR_FORMAT_NOT_SUPPORTED: u32 = 0x887D_0003;

const E_FAIL: windows_core::HRESULT = windows_core::HRESULT(0x8000_4005u32 as i32);
const E_UNEXPECTED: windows_core::HRESULT = windows_core::HRESULT(0x8000_FFFFu32 as i32);

// ── VxApoError → HRESULT 映射（供 COM 方法返回）────────────────────────────

impl From<windows_core::Error> for VxApoError {
    fn from(err: windows_core::Error) -> Self {
        VxApoError::Registry(err.to_string())
    }
}

impl From<VxApoError> for windows_core::HRESULT {
    fn from(err: VxApoError) -> Self {
        match err {
            VxApoError::Registry(_) => E_FAIL,
            VxApoError::Config(_) => E_FAIL,
            VxApoError::Format(_) => {
                windows_core::HRESULT(APOERR_FORMAT_NOT_SUPPORTED as i32)
            }
            VxApoError::Io(_) => E_FAIL,
            VxApoError::Internal(_) => E_UNEXPECTED,
            VxApoError::RtSafety(_) => E_FAIL,
            VxApoError::State(_) => {
                windows_core::HRESULT(APOERR_ALREADY_INITIALIZED as i32)
            }
            VxApoError::DeviceNotFound(_) => E_FAIL,
        }
    }
}

// ── 辅助构造方法 ─────────────────────────────────────────────────────────────

impl VxApoError {
    /// 构造注册表错误。
    pub fn registry(msg: impl Into<String>) -> Self {
        Self::Registry(msg.into())
    }

    /// 构造配置解析错误。
    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }

    /// 构造格式不支持错误。
    pub fn format(msg: impl Into<String>) -> Self {
        Self::Format(msg.into())
    }

    /// 构造 I/O 错误。
    pub fn io(msg: impl Into<String>) -> Self {
        Self::Io(msg.into())
    }

    /// 构造内部错误。
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }

    /// 构造实时安全违规错误。
    pub fn rt_safety(msg: impl Into<String>) -> Self {
        Self::RtSafety(msg.into())
    }

    /// 构造状态转换错误。
    pub fn state(msg: impl Into<String>) -> Self {
        Self::State(msg.into())
    }

    /// 构造设备未找到错误。
    pub fn device_not_found(name: impl Into<String>) -> Self {
        Self::DeviceNotFound(name.into())
    }
}

// ── 测试 ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use windows_core::HRESULT;

    #[test]
    fn succeeded_ok() {
        assert!(succeeded(HRESULT(0)));
        assert!(succeeded(HRESULT(1)));
    }

    #[test]
    fn failed_ok() {
        let hr = HRESULT(0x8000_4005u32 as i32);
        assert!(failed(hr));
        assert!(!succeeded(hr));
    }

    #[test]
    fn check_hresult_ok() {
        assert!(check_hresult(HRESULT(0)).is_ok());
    }

    #[test]
    fn check_hresult_err() {
        let hr = HRESULT(0x8000_4005u32 as i32);
        let err = check_hresult(hr).unwrap_err();
        assert!(matches!(err, VxApoError::Internal(_)));
    }

    #[test]
    fn from_format_to_hresult() {
        let hr: HRESULT = VxApoError::format("96000 Hz").into();
        assert_eq!(hr.0, 0x887D_0003u32 as i32);
    }

    #[test]
    fn from_state_to_hresult() {
        let hr: HRESULT = VxApoError::state("非法状态转换").into();
        assert_eq!(hr.0, 0x887D_0001u32 as i32);
    }

    #[test]
    fn from_internal_to_hresult() {
        let hr: HRESULT = VxApoError::internal("boom").into();
        assert_eq!(hr.0, 0x8000_FFFFu32 as i32); // E_UNEXPECTED
    }

    #[test]
    fn helpers_build_variants() {
        assert!(matches!(VxApoError::registry("x"), VxApoError::Registry(_)));
        assert!(matches!(VxApoError::config("x"), VxApoError::Config(_)));
        assert!(matches!(VxApoError::format("x"), VxApoError::Format(_)));
        assert!(matches!(VxApoError::io("x"), VxApoError::Io(_)));
        assert!(matches!(VxApoError::internal("x"), VxApoError::Internal(_)));
        assert!(matches!(VxApoError::rt_safety("x"), VxApoError::RtSafety(_)));
        assert!(matches!(VxApoError::state("x"), VxApoError::State(_)));
        assert!(matches!(
            VxApoError::device_not_found("x"),
            VxApoError::DeviceNotFound(_)
        ));
    }
}
