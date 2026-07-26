//! host/parse/commands/cmd_include.rs — Include: 命令
//!
//! 语法：`Include: path/to/config.txt`
//!
//! 递归加载并解析指定的配置文件。
//! 路径相对于当前文件所在目录。
//!
//! 最大递归深度：16 层（防止无限循环）。

use std::path::{Path, PathBuf};

use crate::host::parse::parser::{
    self, ConfigError, ParseContext,
};

/// 最大 Include 递归深度。
pub const MAX_INCLUDE_DEPTH: usize = 16;

/// 处理 `Include:` 命令。
///
/// 解析路径（相对于当前文件），调用 `parser::parse_config_file_content` 解析。
pub fn handle(
    value: &str,
    ctx: &mut ParseContext,
    depth: usize,
) -> Result<(), ConfigError> {
    if value.is_empty() {
        return Err(ConfigError::SyntaxError {
            file: ctx.current_file.display().to_string(),
            line: ctx.line_number,
            message: "Include: requires a file path".into(),
        });
    }

    if depth >= MAX_INCLUDE_DEPTH {
        return Err(ConfigError::SyntaxError {
            file: ctx.current_file.display().to_string(),
            line: ctx.line_number,
            message: format!(
                "include depth exceeded maximum ({})",
                MAX_INCLUDE_DEPTH
            ),
        });
    }

    // 解析相对路径
    let include_path = resolve_include_path(ctx.current_file, value);

    log::debug!(
        "{}:{}: including '{}'",
        ctx.current_file.display(),
        ctx.line_number,
        include_path.display()
    );

    // 检查文件存在
    if !include_path.exists() {
        return Err(ConfigError::IoError {
            path: include_path.display().to_string(),
            message: "included file not found".into(),
        });
    }

    // 读取并解析
    let content = parser::read_config_file(&include_path)?;
    parser::parse_content(
        &content,
        ctx.chain,
        ctx.is_capture,
        &include_path,
        depth + 1,
    )?;

    Ok(())
}

/// 解析 Include 路径（相对于当前文件目录）。
fn resolve_include_path(current_file: &Path, relative: &str) -> PathBuf {
    let parent = current_file
        .parent()
        .unwrap_or_else(|| Path::new("."));
    parent.join(relative.trim())
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::stream::chain::Chain;
    use crate::host::parse::parser::ProcessingStage;

    fn make_ctx<'a>(
        chain: &'a mut Chain,
        file: &'a Path,
    ) -> ParseContext<'a> {
        ParseContext {
            chain: chain,
            is_capture: false,
            stage: ProcessingStage::None,
            current_file: file,
            line_number: 1,
            abort_file: false,
            cond_stack: Vec::new(),
            variables: std::collections::HashMap::new(),
            include_depth: 0,
        }
    }

    #[test]
    fn include_empty_path() {
        let mut chain = Chain::new(2, 480, vec!["L".into(), "R".into()]);
        let file = Path::new("test.txt");
        let mut ctx = make_ctx(&mut chain, file);
        let result = handle("", &mut ctx, 0);
        assert!(result.is_err());
    }

    #[test]
    fn include_nonexistent_file() {
        let mut chain = Chain::new(2, 480, vec!["L".into(), "R".into()]);
        let file = Path::new("test.txt");
        let mut ctx = make_ctx(&mut chain, file);
        let result = handle("nonexistent_12345.txt", &mut ctx, 0);
        assert!(result.is_err());
    }

    #[test]
    fn include_max_depth() {
        let mut chain = Chain::new(2, 480, vec!["L".into(), "R".into()]);
        let file = Path::new("test.txt");
        let mut ctx = make_ctx(&mut chain, file);
        let result = handle("some.txt", &mut ctx, MAX_INCLUDE_DEPTH);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_relative_path() {
        let current = Path::new("/etc/audio/config.txt");
        let resolved = resolve_include_path(current, "sub/other.txt");
        assert_eq!(resolved, PathBuf::from("/etc/audio/sub/other.txt"));
    }

    #[test]
    fn resolve_same_dir() {
        let current = Path::new("/etc/audio/config.txt");
        let resolved = resolve_include_path(current, "other.txt");
        assert_eq!(resolved, PathBuf::from("/etc/audio/other.txt"));
    }

    #[test]
    fn resolve_no_parent() {
        let current = Path::new("config.txt");
        let resolved = resolve_include_path(current, "other.txt");
        assert_eq!(resolved, PathBuf::from("other.txt"));
    }
}