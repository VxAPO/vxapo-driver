//! config/commands/stage.rs — Stage: 命令（v6.3 规范 6.9）
//!
//! 语法：`Stage: PreMix | PostMix | Capture`
//!
//! 设置当前处理阶段标志。支持多种别名：
//! - `PreMix` / `pre-mix` / `pre_mix`
//! - `PostMix` / `post-mix` / `post_mix`
//! - `Capture` / `capture`

use crate::config::error::ConfigError;
use crate::config::parser::{ParseContext, ParseStage};

/// 处理 Stage: 命令。
///
/// 将 `ctx.stage` 设置为对应阶段。
pub fn handle(value: &str, ctx: &mut ParseContext) -> Result<(), ConfigError> {
    let stage = parse_stage(value).ok_or_else(|| {
        ConfigError::SyntaxError {
            file: ctx.current_file.display().to_string(),
            line: ctx.line_number,
            message: format!("unknown stage '{}' (expected PreMix/PostMix/Capture)", value.trim()),
        }
    })?;

    ctx.stage = stage;
    Ok(())
}

/// 解析阶段字符串（支持别名）。
#[allow(dead_code)]
fn parse_stage(value: &str) -> Option<ParseStage> {
    match value.trim().to_ascii_lowercase().as_str() {
        "premix" | "pre-mix" | "pre_mix" => Some(ParseStage::PreMix),
        "postmix" | "post-mix" | "post_mix" => Some(ParseStage::PostMix),
        "capture" => Some(ParseStage::Capture),
        "none" => Some(ParseStage::None),
        _ => None,
    }
}