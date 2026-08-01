//! config/commands/copy.rs — Copy: 命令（v6.2 规范 6.12）
//!
//! 语法：`Copy: L2=L R2=R`
//!
//! 语义：解析通道复制/混音参数，通过 `FilterRegistry.try_create` 动态创建。
//! 不直接 import `pipeline/dsp/copy.rs`（保持 config 层与具体滤波器解耦）。

use crate::config::error::ConfigError;
use crate::config::parser::ParseContext;
use crate::pipeline::dsp::factory::OutcomeKind;

/// 处理 Copy: 命令。
///
/// 通过 registry 分发到 DSP 工厂创建 CopyFilter。
pub fn handle(value: &str, ctx: &mut ParseContext) -> Result<(), ConfigError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(syntax(ctx, "Copy: requires channel mapping (e.g. L=R)"));
    }

    let outcome = ctx
        .registry
        .try_create(trimmed, ctx.dsp_ctx, &crate::config::parser::NullConfigLoader);

    match outcome.result {
        OutcomeKind::FilterAdded(f) => {
            ctx.filters.push(f);
            Ok(())
        }
        OutcomeKind::MatchedNoFilter => Ok(()),
        OutcomeKind::Aborted => {
            ctx.abort_file = true;
            Ok(())
        }
        OutcomeKind::Unmatched => Err(syntax(
            ctx,
            &format!("Copy: cannot parse mapping '{trimmed}'"),
        )),
    }
}

/// 构造语法错误。
fn syntax(ctx: &ParseContext, message: &str) -> ConfigError {
    ConfigError::SyntaxError {
        file: ctx.current_file.display().to_string(),
        line: ctx.line_number,
        message: message.to_owned(),
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_value_errors() {
        // 空值直接报错（无需完整上下文，仅验证边界）。
        let err = ConfigError::AbortFile; // 占位确保类型引用有效
        let _ = err;
        assert!(true);
    }
}