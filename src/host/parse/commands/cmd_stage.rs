//! host/parse/commands/cmd_stage.rs — Stage: 命令
//!
//! 语法：`Stage: PreMix | PostMix | Capture`
//!
//! 设置当前处理阶段标志。影响后续过滤器的注册属性匹配。

use crate::host::parse::parser::{ConfigError, ParseContext, ProcessingStage};

/// 处理 `Stage:` 命令。
pub fn handle(value: &str, ctx: &mut ParseContext) -> Result<(), ConfigError> {
    let stage = match value.to_lowercase().as_str() {
        "premix" | "pre-mix" | "pre_mix" => ProcessingStage::PreMix,
        "postmix" | "post-mix" | "post_mix" => ProcessingStage::PostMix,
        "capture" => ProcessingStage::Capture,
        _ => {
            return Err(ConfigError::SyntaxError {
                file: ctx.current_file.display().to_string(),
                line: ctx.line_number,
                message: format!("unknown stage '{}', expected PreMix/PostMix/Capture", value),
            });
        }
    };

    ctx.stage = stage;
    log::debug!("{}: stage set to {:?}", ctx.current_file.display(), stage);
    Ok(())
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::stream::chain::Chain;
    use std::path::Path;

    fn make_ctx<'a>(chain: &'a mut Chain) -> ParseContext<'a> {
        ParseContext {
            chain: chain,
            is_capture: false,
            stage: ProcessingStage::None,
            current_file: Path::new("test.txt"),
            line_number: 0,
            abort_file: false,
            cond_stack: Vec::new(),
            variables: std::collections::HashMap::new(),
            include_depth: 0,
        }
    }

    #[test]
    fn stage_premix() {
        let mut chain = Chain::new(2, 480, vec!["L".into(), "R".into()]);
        let mut ctx = make_ctx(&mut chain);
        handle("PreMix", &mut ctx).unwrap();
        assert_eq!(ctx.stage, ProcessingStage::PreMix);
    }

    #[test]
    fn stage_postmix() {
        let mut chain = Chain::new(2, 480, vec!["L".into(), "R".into()]);
        let mut ctx = make_ctx(&mut chain);
        handle("PostMix", &mut ctx).unwrap();
        assert_eq!(ctx.stage, ProcessingStage::PostMix);
    }

    #[test]
    fn stage_capture() {
        let mut chain = Chain::new(2, 480, vec!["L".into(), "R".into()]);
        let mut ctx = make_ctx(&mut chain);
        handle("Capture", &mut ctx).unwrap();
        assert_eq!(ctx.stage, ProcessingStage::Capture);
    }

    #[test]
    fn stage_case_insensitive() {
        let mut chain = Chain::new(2, 480, vec!["L".into(), "R".into()]);
        let mut ctx = make_ctx(&mut chain);
        handle("premix", &mut ctx).unwrap();
        assert_eq!(ctx.stage, ProcessingStage::PreMix);
    }

    #[test]
    fn stage_hyphenated() {
        let mut chain = Chain::new(2, 480, vec!["L".into(), "R".into()]);
        let mut ctx = make_ctx(&mut chain);
        handle("pre-mix", &mut ctx).unwrap();
        assert_eq!(ctx.stage, ProcessingStage::PreMix);
    }

    #[test]
    fn stage_invalid() {
        let mut chain = Chain::new(2, 480, vec!["L".into(), "R".into()]);
        let mut ctx = make_ctx(&mut chain);
        let result = handle("Invalid", &mut ctx);
        assert!(result.is_err());
    }

    #[test]
    fn stage_overwrite() {
        let mut chain = Chain::new(2, 480, vec!["L".into(), "R".into()]);
        let mut ctx = make_ctx(&mut chain);
        handle("PreMix", &mut ctx).unwrap();
        assert_eq!(ctx.stage, ProcessingStage::PreMix);
        handle("PostMix", &mut ctx).unwrap();
        assert_eq!(ctx.stage, ProcessingStage::PostMix);
    }
}