//! config/commands/rew.rs — REW 导出格式（v6.2 规范 6.15）
//!
//! 语法：`Filter 1: ON PK Fc 50,0 Hz Gain -10,0 dB Q 2,50`
//!
//! 语义：解析 REW Room EQ V5 格式行（含逗号小数点），通过 `registry.try_create("PK", ...)`
//! 动态创建。REW 使用逗号作为小数点分隔符，需先转换为标准格式。

use crate::config::error::ConfigError;
use crate::config::parser::ParseContext;
use crate::pipeline::dsp::factory::OutcomeKind;

/// REW 行前缀匹配。
const REW_PREFIX: &str = "Filter ";

/// 处理 REW 格式行。
///
/// 格式：`Filter N: ON TYPE 参数...`
/// REW 使用逗号作为小数分隔符（如 `50,0` = 50.0）。
pub fn handle(value: &str, ctx: &mut ParseContext) -> Result<(), ConfigError> {
    let trimmed = value.trim();

    // 移除 "Filter N:" 前缀（规范 6.15 语法）。
    let rest = match strip_reew_prefix(trimmed) {
        Some(r) => r,
        None => {
            return Err(syntax(ctx, "REW: expected 'Filter N:' prefix"));
        }
    };

    // 解析 ON/OFF 开关。
    let (enabled, params) = if let Some(r) = rest.strip_prefix("ON").map(str::trim_start) {
        (true, r)
    } else if let Some(r) = rest.strip_prefix("OFF").map(str::trim_start) {
        (false, r)
    } else {
        // 省略开关：默认 ON。
        (true, rest)
    };

    if !enabled {
        return Ok(());
    }

    if params.is_empty() {
        return Err(syntax(ctx, "REW: missing filter parameters"));
    }

    // 转换逗号小数点为标准句点（50,0 → 50.0）。
    let normalized = params.replace(',', ".");

    let outcome = ctx
        .registry
        .try_create(&normalized, ctx.dsp_ctx, &crate::config::parser::NullConfigLoader);

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
            &format!("REW: cannot parse '{normalized}'"),
        )),
    }
}

/// 剥离 `Filter N:` 前缀（N 为数字）。
fn strip_reew_prefix(s: &str) -> Option<&str> {
    if !s.starts_with(REW_PREFIX) {
        return None;
    }
    let rest = &s[REW_PREFIX.len()..];
    // 跳过数字
    let num_len = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    if num_len == 0 {
        return None;
    }
    let after_num = &rest[num_len..];
    // 跳过 ":"
    after_num.strip_prefix(':').map(str::trim_start)
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
    fn strip_prefix_valid() {
        assert_eq!(strip_reew_prefix("Filter 1: ON PK ..."), Some("ON PK ..."));
        assert_eq!(strip_reew_prefix("Filter 12: OFF"), Some("OFF"));
    }

    #[test]
    fn strip_prefix_invalid() {
        assert!(strip_reew_prefix("GraphicEQ: ...").is_none());
        assert!(strip_reew_prefix("Filter : ON").is_none());
    }

    #[test]
    fn comma_decimal_normalization() {
        let original = "PK Fc 50,0 Hz Gain -10,0 dB Q 2,50";
        let normalized = original.replace(',', ".");
        assert_eq!(normalized, "PK Fc 50.0 Hz Gain -10.0 dB Q 2.50");
    }
}