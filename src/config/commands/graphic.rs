//! config/commands/graphic.rs — GraphicEQ: 命令（v6.3 规范 6.10）
//!
//! 语法：`GraphicEQ: 20 -3.1; 25 -3.1; ...`
//!
//! 语义：解析频段参数，通过 `FilterRegistry.try_create` 动态创建。
//! 不直接 import `pipeline/dsp/graphic_eq.rs`（保持解耦）。

use crate::config::error::ConfigError;
use crate::config::parser::ParseContext;
use crate::pipeline::dsp::factory::OutcomeKind;

/// 处理 GraphicEQ: 命令。
pub fn handle(value: &str, ctx: &mut ParseContext) -> Result<(), ConfigError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        // v9.5：空参数 = 显式移除 EQ（不产生滤波器，等同 passthrough）。
        // 与“配置里根本没有 GraphicEQ”不同——parser 会对空参数也产出 spec
        // 指纹，热重载据此把旧 EQ 链切换为空链。
        return Ok(());
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
            &format!("GraphicEQ: cannot parse bands '{trimmed}'"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_references_valid() {
        let _ = ConfigError::AbortFile;
        assert!(true);
    }
}
