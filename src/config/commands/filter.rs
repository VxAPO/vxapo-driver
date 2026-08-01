//! config/commands/filter.rs — Filter: ON HP/LP/PK/... 命令（v6.2 规范 6.14）
//!
//! 语法：`Filter: ON PK Fc 1000 Hz Gain +3.0 dB Q 1.0`
//!
//! 语义：解析 ON/OFF 开关和滤波器类型，通过 `FilterRegistry.try_create(type, args, ctx)`
//! 动态创建。`OFF` 标志由 ParseContext 中的启用标志控制（创建 PassthroughFilter）。
//! 不直接 import 具体滤波器实现（保持 config 层解耦）。

use crate::config::error::ConfigError;
use crate::config::parser::ParseContext;
use crate::pipeline::dsp::factory::OutcomeKind;

/// 处理 Filter: 命令。
///
/// 格式：`ON/OFF TYPE 参数...`
/// 例：`ON PK Fc 1000 Hz Gain +3.0 dB Q 1.0`
pub fn handle(value: &str, ctx: &mut ParseContext) -> Result<(), ConfigError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(syntax(ctx, "Filter: requires ON/OFF and filter type"));
    }

    // 解析 ON/OFF 开关。
    let (enabled, rest) = if let Some(r) = trimmed.strip_prefix("ON").map(str::trim_start) {
        (true, r)
    } else if let Some(r) = trimmed.strip_prefix("OFF").map(str::trim_start) {
        (false, r)
    } else {
        // 省略开关：默认 ON。
        (true, trimmed)
    };

    // OFF：不创建滤波器（Passthrough 语义由上层保证，或跳过）。
    if !enabled {
        return Ok(());
    }

    if rest.is_empty() {
        return Err(syntax(ctx, "Filter: missing filter type"));
    }

    let outcome = ctx
        .registry
        .try_create(rest, ctx.dsp_ctx, &crate::config::parser::NullConfigLoader);

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
            &format!("Filter: cannot parse '{rest}'"),
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