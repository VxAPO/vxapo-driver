//! config/error.rs — 配置解析专用错误类型（规范 6.0）

/// 配置解析错误类型。
///
/// `ConditionFalse` 不作为错误返回——条件不满足时跳过行即可，不影响解析流程。
/// 变体名与 Display 文案为对外错误信息契约，调整需同步 CLI/App 文案。
#[derive(Debug, Clone, thiserror::Error)]
pub enum ConfigError {
    /// 文件 I/O 错误。
    #[error("I/O error reading {path}: {message}")]
    IoError { path: String, message: String },
    /// 命令语法错误（含文件名和行号）。
    #[error("Syntax error in {file}:{line}: {message}")]
    SyntaxError { file: String, line: usize, message: String },
    /// TOML 反序列化失败。
    #[error("TOML error in {file}: {message}")]
    TomlError { file: String, message: String },
    /// FileModel → ChainModel 转换/校验失败。
    #[error("Config model error in {file}: {message}")]
    ModelError { file: String, message: String },
    /// AbortFile 终止（Device: 命令设置）。
    #[error("Parse aborted by Device: AbortFile")]
    AbortFile,
}

impl From<ConfigError> for crate::utils::vx_error::VxApoError {
    fn from(err: ConfigError) -> Self {
        crate::utils::vx_error::VxApoError::config(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_io_error() {
        let err = ConfigError::IoError {
            path: "config.toml".into(),
            message: "not found".into(),
        };
        let msg = format!("{err}");
        assert!(msg.contains("config.toml"));
        assert!(msg.contains("not found"));
    }

    #[test]
    fn display_syntax_error() {
        let err = ConfigError::SyntaxError {
            file: "config.toml".into(),
            line: 42,
            message: "unknown command".into(),
        };
        let msg = format!("{err}");
        assert!(msg.contains("config.toml:42"));
        assert!(msg.contains("unknown command"));
    }

    #[test]
    fn display_abort_file() {
        let msg = format!("{}", ConfigError::AbortFile);
        assert!(msg.contains("AbortFile"));
    }

    #[test]
    fn error_trait_impl() {
        let err: Box<dyn std::error::Error> = Box::new(ConfigError::AbortFile);
        assert!(err.to_string().contains("AbortFile"));
    }

    #[test]
    fn converts_to_vxapo_error_as_config() {
        let err = ConfigError::ModelError {
            file: "config.toml".into(),
            message: "bad value".into(),
        };
        let vx: crate::utils::vx_error::VxApoError = err.into();
        assert!(matches!(vx, crate::utils::vx_error::VxApoError::Config(_)));
        assert!(vx.to_string().contains("bad value"));
    }
}
