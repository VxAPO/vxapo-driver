//! config/error.rs — 配置解析专用错误类型（v6.2 规范 6.0）

/// 配置解析错误类型。
///
/// `ConditionFalse` 不作为错误返回——条件不满足时跳过行即可，不影响解析流程。
#[derive(Debug, Clone)]
pub enum ConfigError {
    /// 文件 I/O 错误。
    IoError { path: String, message: String },
    /// 命令语法错误（含文件名和行号）。
    SyntaxError { file: String, line: usize, message: String },
    /// AbortFile 终止（Device: 命令设置）。
    AbortFile,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IoError { path, message } => write!(f, "I/O error reading {path}: {message}"),
            Self::SyntaxError { file, line, message } => {
                write!(f, "Syntax error in {file}:{line}: {message}")
            }
            Self::AbortFile => write!(f, "Parse aborted by Device: AbortFile"),
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_io_error() {
        let err = ConfigError::IoError {
            path: "config.txt".into(),
            message: "not found".into(),
        };
        let msg = format!("{err}");
        assert!(msg.contains("config.txt"));
        assert!(msg.contains("not found"));
    }

    #[test]
    fn display_syntax_error() {
        let err = ConfigError::SyntaxError {
            file: "config.txt".into(),
            line: 42,
            message: "unknown command".into(),
        };
        let msg = format!("{err}");
        assert!(msg.contains("config.txt:42"));
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
}