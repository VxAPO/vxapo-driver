//! config/commands/include.rs — Include: 命令（v6.3 规范 6.8）
//!
//! 语法：`Include: "presets/default.txt"`
//!
//! 递归加载子配置文件。路径相对于当前文件所在目录。
//! 最大递归深度 16 层。

use std::path::{Path, PathBuf};

use crate::config::error::ConfigError;
use crate::config::parser::{parse_lines_impl, read_config_file, ParseContext, MAX_CONFIG_FILE_SIZE};

/// 最大 Include 递归深度。
pub const MAX_INCLUDE_DEPTH: usize = 16;

/// 处理 Include: 命令。
///
/// 1. 检查递归深度限制
/// 2. 解析相对路径（相对于当前文件目录）
/// 3. 检查文件存在
/// 4. 读取文件内容
/// 5. 递归调用 parse_lines_impl（depth + 1）
pub fn handle(value: &str, ctx: &mut ParseContext) -> Result<(), ConfigError> {
    // Step 1: 深度限制
    if ctx.include_depth >= MAX_INCLUDE_DEPTH {
        return Err(ConfigError::SyntaxError {
            file: ctx.current_file.display().to_string(),
            line: ctx.line_number,
            message: format!(
                "Include: recursion depth exceeded (max {})",
                MAX_INCLUDE_DEPTH
            ),
        });
    }

    // Step 2: 解析路径（去引号）
    let path_str = unquote(value.trim());
    if path_str.is_empty() {
        return Err(ConfigError::SyntaxError {
            file: ctx.current_file.display().to_string(),
            line: ctx.line_number,
            message: "Include: requires a file path".to_owned(),
        });
    }

    // 相对路径基于当前文件目录
    let path = resolve_relative(&ctx.current_file, &path_str);

    // Step 3-3.5: 128KB 逐文件闸门（v7.9 P0-4）——子文件超限 → 整体解析失败
    //（include 失败 = 整体失败，杜绝"残缺 spec 污染基线"）。
    if std::fs::metadata(&path)
        .map(|m| m.len() > MAX_CONFIG_FILE_SIZE)
        .unwrap_or(false)
    {
        return Err(ConfigError::IoError {
            path: path.display().to_string(),
            message: format!("config exceeds {} bytes", MAX_CONFIG_FILE_SIZE),
        });
    }

    // Step 4: 读取文件内容（存在性检查由 read_config_file 隐式完成）
    let content = read_config_file(&path)?;

    // Step 5: 递归解析（子上下文覆盖 filters/specs/dsp_ctx 等借用字段，
    // 其余可变状态独立；current_file 为所有权 PathBuf，无 Box::leak）。
    // specs 共享同一 chain——子文件命令的 filter_spec 内联到主 spec chain
    //（v7.9：Include 自身不产出，子文件命令各自产出并内联）。
    let mut sub_ctx = ParseContext {
        filters: ctx.filters,
        specs: ctx.specs,
        registry: ctx.registry,
        dsp_ctx: ctx.dsp_ctx,
        stage: ctx.stage,
        is_capture: ctx.is_capture,
        current_file: path,
        line_number: 0,
        abort_file: false,
        cond_stack: Vec::new(),
        variables: ctx.variables.clone(),
        include_depth: ctx.include_depth + 1,
        current_channels: ctx.current_channels.clone(),
        all_channels: ctx.all_channels.clone(),
        current_device: ctx.current_device.clone(),
    };

    // 解析子文件（复用父 filters 缓冲，子上下文文件私有状态独立）。
    let result = parse_lines_impl(&content, &mut sub_ctx, ctx.include_depth + 1);

    // 同步回父上下文
    ctx.variables = sub_ctx.variables;
    ctx.include_depth = sub_ctx.include_depth;
    if sub_ctx.abort_file {
        ctx.abort_file = true;
    }

    result
}

/// 去除首尾引号。
fn unquote(s: &str) -> &str {
    let s = s.trim();
    if s.len() >= 2 {
        let b = s.as_bytes();
        if (b[0] == b'"' && b[s.len() - 1] == b'"')
            || (b[0] == b'\'' && b[s.len() - 1] == b'\'')
        {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// 解析相对路径（相对于当前文件所在目录）。
fn resolve_relative(current_file: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    // 非绝对路径：相对当前文件目录
    match current_file.parent() {
        Some(dir) => dir.join(p),
        None => p.to_path_buf(),
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unquote_double() {
        assert_eq!(unquote("\"presets/default.txt\""), "presets/default.txt");
    }

    #[test]
    fn unquote_single() {
        assert_eq!(unquote("'x.txt'"), "x.txt");
    }

    #[test]
    fn unquote_none() {
        assert_eq!(unquote("x.txt"), "x.txt");
    }

    #[test]
    fn resolve_relative_from_file() {
        let cur = Path::new(r"C:\config\main.txt");
        let resolved = resolve_relative(cur, "sub.txt");
        assert!(resolved.ends_with("sub.txt"));
        assert!(resolved.parent().unwrap().ends_with("config"));
    }

    #[test]
    fn resolve_absolute() {
        let cur = Path::new(r"C:\config\main.txt");
        let resolved = resolve_relative(cur, r"D:\abs\sub.txt");
        assert!(resolved.is_absolute());
    }
}