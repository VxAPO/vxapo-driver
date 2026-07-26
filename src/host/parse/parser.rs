//! host/parse/parser.rs — 配置文件解析器（Note 16/51）
//!
//! 解析 `config.txt` 配置文件，逐行分发到命令处理器。
//!
//! 解析流程：
//! 1. 读取文件（UTF-8 优先，ANSI 降级，Note 16）
//! 2. 逐行解析：跳过空行/注释 → 识别命令前缀 → 分发到处理器
//! 3. 通道选择恢复（Note 51）：解析完成后恢复解析前的通道名称快照
//!
//! 命令格式：`CommandName: value`
//!
//! 支持的命令：
//! - `Device:` 设备路径绑定 + AbortFile（`cmd_device.rs`）
//! - `Stage:` 处理阶段标志（`cmd_stage.rs`）
//! - `Channel:` 通道选择（`cmd_channel.rs`）
//! - `If:` 条件分支（`cmd_cond.rs`）
//! - `Eval:` 数学表达式（`cmd_expr.rs`）
//! - `Include:` 递归加载（`cmd_include.rs`）
//! - 其他以 `#` 开头的行为注释

use std::path::Path;

use crate::pipeline::stream::chain::Chain;

use super::commands::cmd_device;
use super::commands::cmd_stage;
use super::commands::cmd_channel;
use super::commands::cmd_cond;
use super::commands::cmd_expr;
use super::commands::cmd_include;

// ══════════════════════════════════════════════════════════════════════════════
// 解析上下文
// ══════════════════════════════════════════════════════════════════════════════

/// 配置解析上下文。
///
/// 在解析一个配置文件期间维护状态。
pub struct ParseContext<'a> {
    /// 当前正在构建的过滤器链。
    pub chain: &'a mut Chain,
    /// 当前设备类型（render / capture）。
    pub is_capture: bool,
    /// 当前处理阶段。
    pub stage: ProcessingStage,
    /// 当前文件路径（用于错误报告和 Include 相对路径）。
    pub current_file: &'a Path,
    /// 当前行号（用于错误报告）。
    pub line_number: usize,
    /// AbortFile 标志（Device: 命令设置）。
    pub abort_file: bool,
    /// 条件栈（If/ElseIf/Else/EndIf 嵌套）。
    pub cond_stack: cmd_cond::CondStack,
    /// 变量存储（Eval: 命令写入，If: 条件引用）。
    pub variables: cmd_expr::Variables,
    /// Include 递归深度。
    pub include_depth: usize,
}

/// 处理阶段标志。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessingStage {
    None,
    PreMix,
    PostMix,
    Capture,
}

impl Default for ProcessingStage {
    fn default() -> Self {
        Self::None
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 读取文件（Note 16）
// ══════════════════════════════════════════════════════════════════════════════

/// 读取配置文件内容。
///
/// Note 16：UTF-8 优先，检测到 BOM 则跳过，
/// 非 UTF-8 内容使用 Windows ANSI 代码页降级。
pub fn read_config_file(path: &Path) -> Result<String, ConfigError> {
    let bytes = std::fs::read(path).map_err(|e| ConfigError::IoError {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;

    // 检测 UTF-8 BOM (EF BB BF)
    let bytes = if bytes.len() >= 3 && bytes[0..3] == [0xEF, 0xBB, 0xBF] {
        &bytes[3..]
    } else {
        &bytes
    };

    // 尝试 UTF-8
    match std::str::from_utf8(bytes) {
        Ok(s) => Ok(s.to_owned()),
        Err(_) => {
            // ANSI 降级：使用 Windows 代码页 1252（西欧）或 936（简体中文）
            // Phase 7: 简单替换非 ASCII 字节为 '?'
            // Phase 8+: 使用 encoding_rs 做完整转码
            let decoded: String = bytes
                .iter()
                .map(|&b| if b < 128 { b as char } else { '?' })
                .collect();
            Ok(decoded)
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 行解析
// ══════════════════════════════════════════════════════════════════════════════

/// 解析一行配置。
///
/// 返回 `Ok(())` 正常继续，`Err` 表示解析错误。
pub fn parse_line(line: &str, ctx: &mut ParseContext) -> Result<(), ConfigError> {
    let trimmed = line.trim();

    // 跳过空行和注释
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return Ok(());
    }

    // 分割命令和值：`Command: value`
    let (command, value) = split_command_value(trimmed);

    match command {
        "Device" => cmd_device::handle(value, ctx),
        "Stage" => cmd_stage::handle(value, ctx),
        // Phase 7 后续批次：
        // "Channel" => cmd_channel::handle(value, ctx),
        // "If" => cmd_cond::handle(value, ctx),
        // "Eval" => cmd_expr::handle(value, ctx),
        // "Include" => cmd_include::handle(value, ctx),
        _ => {
            // 未知命令：跳过但记录警告
            log::warn!(
                "{}:{}: unknown command '{}'",
                ctx.current_file.display(),
                ctx.line_number,
                command
            );
            Ok(())
        }
    }
}

/// 分割 `Command: value` 格式。
///
/// 返回 (command, value)。没有冒号时 value 为空字符串。
fn split_command_value(line: &str) -> (&str, &str) {
    if let Some(pos) = line.find(':') {
        let command = line[..pos].trim();
        let value = line[pos + 1..].trim();
        (command, value)
    } else {
        (line.trim(), "")
    }
}

/// 解析整个配置文件。
///
/// Note 51：解析前保存通道名称快照，解析后恢复。
pub fn parse_config_file(
    path: &Path,
    chain: &mut Chain,
    is_capture: bool,
) -> Result<(), ConfigError> {
    let content = read_config_file(path)?;

    // Note 51: 保存通道快照
    let snapshot = chain.save_channel_snapshot();

    parse_content(&content, chain, is_capture, path, 0)?;

    // Note 51: 恢复通道快照
    chain.restore_channel_snapshot(snapshot);

    Ok(())
}

/// 解析配置文件内容字符串。
///
/// 供 `Include:` 命令递归调用。
/// `parse_config_file` 负责读取文件和 Note 51 快照，
/// 本函数只做纯解析，不保存/恢复快照（由调用方控制）。
pub fn parse_content(
    content: &str,
    chain: &mut Chain,
    is_capture: bool,
    file_path: &Path,
    depth: usize,
) -> Result<(), ConfigError> {
    let mut ctx = ParseContext {
        chain,
        is_capture,
        stage: ProcessingStage::None,
        current_file: file_path,
        line_number: 0,
        abort_file: false,
        cond_stack: Vec::new(),
        variables: std::collections::HashMap::new(),
        include_depth: depth,
    };
    let depth = ctx.include_depth;

    for (i, line) in content.lines().enumerate() {
        ctx.line_number = i + 1;
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let (command, value) = split_command_value(trimmed);

        // 条件分支：If/ElseIf/Else/EndIf 始终处理（管理栈状态）
        match command {
            "If" => {
                cmd_cond::handle_if(
                    value, &ctx.variables, ctx.stage, ctx.is_capture,
                    &mut ctx.cond_stack,
                    &file_path.display().to_string(), ctx.line_number,
                )?;
                continue;
            }
            "ElseIf" => {
                cmd_cond::handle_elseif(
                    value, &ctx.variables, ctx.stage, ctx.is_capture,
                    &mut ctx.cond_stack,
                    &file_path.display().to_string(), ctx.line_number,
                )?;
                continue;
            }
            "Else" => {
                cmd_cond::handle_else(
                    &mut ctx.cond_stack,
                    &file_path.display().to_string(), ctx.line_number,
                )?;
                continue;
            }
            "EndIf" => {
                cmd_cond::handle_endif(
                    &mut ctx.cond_stack,
                    &file_path.display().to_string(), ctx.line_number,
                )?;
                continue;
            }
            _ => {}
        }

        // 条件跳过：当前处于 false 分支时跳过其他命令
        if cmd_cond::is_skipping(&ctx.cond_stack) {
            continue;
        }

        match command {
            "Device" => cmd_device::handle(value, &mut ctx)?,
            "Stage" => cmd_stage::handle(value, &mut ctx)?,
            "Channel" => cmd_channel::handle(value, &mut ctx)?,
            "Eval" => cmd_expr::handle(
                value, &mut ctx.variables,
                &file_path.display().to_string(), ctx.line_number,
            )?,
            "Include" => cmd_include::handle(value, &mut ctx, depth)?,
            _ => {
                log::warn!(
                    "{}:{}: unknown command '{}'",
                    file_path.display(), ctx.line_number, command
                );
            }
        }

        if ctx.abort_file {
            log::debug!("{}: aborted by Device: AbortFile", file_path.display());
            return Ok(());
        }
    }

    // 检查条件栈是否平衡
    if !ctx.cond_stack.is_empty() {
        return Err(ConfigError::SyntaxError {
            file: file_path.display().to_string(),
            line: ctx.line_number,
            message: format!(
                "unterminated If: block ({} levels open)",
                ctx.cond_stack.len()
            ),
        });
    }

    Ok(())
}

// ══════════════════════════════════════════════════════════════════════════════
// 错误类型
// ══════════════════════════════════════════════════════════════════════════════

/// 配置解析错误。
#[derive(Debug, Clone)]
pub enum ConfigError {
    /// 文件 I/O 错误。
    IoError { path: String, message: String },
    /// 命令语法错误。
    SyntaxError {
        file: String,
        line: usize,
        message: String,
    },
    /// 条件不满足（If: 命令）。
    ConditionFalse,
    /// AbortFile 终止。
    AbortFile,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::IoError { path, message } => {
                write!(f, "IO error reading '{}': {}", path, message)
            }
            Self::SyntaxError { file, line, message } => {
                write!(f, "{}:{}: {}", file, line, message)
            }
            Self::ConditionFalse => write!(f, "condition not met"),
            Self::AbortFile => write!(f, "aborted by Device: command"),
        }
    }
}

impl std::error::Error for ConfigError {}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn test_chain() -> Chain {
        Chain::new(2, 480, vec!["L".into(), "R".into()])
    }

    fn test_path() -> std::path::PathBuf {
        std::path::PathBuf::from("test_config.txt")
    }

    // ── split_command_value ──────────────────────────────────────────────────

    #[test]
    fn split_standard() {
        let (cmd, val) = split_command_value("Stage: PreMix");
        assert_eq!(cmd, "Stage");
        assert_eq!(val, "PreMix");
    }

    #[test]
    fn split_no_value() {
        let (cmd, val) = split_command_value("Device");
        assert_eq!(cmd, "Device");
        assert_eq!(val, "");
    }

    #[test]
    fn split_extra_colons() {
        let (cmd, val) = split_command_value("Device: C:\\Audio\\config.txt");
        assert_eq!(cmd, "Device");
        assert_eq!(val, "C:\\Audio\\config.txt");
    }

    #[test]
    fn split_whitespace() {
        let (cmd, val) = split_command_value("  Stage :  PostMix  ");
        assert_eq!(cmd, "Stage");
        assert_eq!(val, "PostMix");
    }

    // ── parse_line ──────────────────────────────────────────────────────────

    #[test]
    fn parse_empty_line() {
        let mut chain = test_chain();
        let mut ctx = ParseContext {
            chain: &mut chain,
            is_capture: false,
            stage: ProcessingStage::None,
            current_file: std::path::Path::new("test.txt"),  // &'static Path
            line_number: 0,
            abort_file: false,
            cond_stack: Vec::new(),
            variables: std::collections::HashMap::new(),
            include_depth: 0,
        };
        assert!(parse_line("", &mut ctx).is_ok());
        assert!(parse_line("   ", &mut ctx).is_ok());
    }

    #[test]
    fn parse_comment() {
        let mut chain = test_chain();
        let mut ctx = ParseContext {
            chain: &mut chain,
            is_capture: false,
            stage: ProcessingStage::None,
            current_file: std::path::Path::new("test.txt"),  // &'static Path
            line_number: 0,
            abort_file: false,
            cond_stack: Vec::new(),
            variables: std::collections::HashMap::new(),
            include_depth: 0,
        };
        assert!(parse_line("# this is a comment", &mut ctx).is_ok());
    }

    #[test]
    fn parse_unknown_command() {
        let mut chain = test_chain();
        let mut ctx = ParseContext {
            chain: &mut chain,
            is_capture: false,
            stage: ProcessingStage::None,
            current_file: std::path::Path::new("test.txt"),  // &'static Path
            line_number: 0,
            abort_file: false,
            cond_stack: Vec::new(),
            variables: std::collections::HashMap::new(),
            include_depth: 0,
        };
        // 未知命令不返回错误，只记录警告
        assert!(parse_line("Foobar: baz", &mut ctx).is_ok());
    }

    // ── read_config_file ────────────────────────────────────────────────────

    #[test]
    fn read_nonexistent_file() {
        let result = read_config_file(Path::new("nonexistent_config_12345.txt"));
        assert!(result.is_err());
    }

    // ── ConfigError Display ─────────────────────────────────────────────────

    #[test]
    fn config_error_display() {
        let err = ConfigError::SyntaxError {
            file: "test.txt".into(),
            line: 5,
            message: "bad value".into(),
        };
        assert_eq!(err.to_string(), "test.txt:5: bad value");
    }

    #[test]
    fn config_error_abort() {
        let err = ConfigError::AbortFile;
        assert!(err.to_string().contains("aborted"));
    }
}