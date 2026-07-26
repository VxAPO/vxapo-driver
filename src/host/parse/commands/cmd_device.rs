//! host/parse/commands/cmd_device.rs — Device: 命令
//!
//! 语法：`Device: device_path` 或 `Device: AbortFile`
//!
//! - `Device: <path>` — 绑定配置到指定设备路径
//! - `Device: AbortFile` — 终止当前文件解析（不终止 Include 的父文件）

use crate::host::parse::parser::{ConfigError, ParseContext};

/// 处理 `Device:` 命令。
pub fn handle(value: &str, ctx: &mut ParseContext) -> Result<(), ConfigError> {
    if value.eq_ignore_ascii_case("AbortFile") {
        ctx.abort_file = true;
        log::debug!(
            "{}: AbortFile triggered",
            ctx.current_file.display()
        );
        return Err(ConfigError::AbortFile);
    }

    // 设备路径绑定：记录到日志，实际绑定在 Phase 8+ 的设备管理中处理。
    if value.is_empty() {
        return Err(ConfigError::SyntaxError {
            file: ctx.current_file.display().to_string(),
            line: ctx.line_number,
            message: "Device: requires a path or 'AbortFile'".into(),
        });
    }

    log::debug!(
        "{}: device path set to '{}'",
        ctx.current_file.display(),
        value
    );

    // Phase 8+: 实际设备路径绑定逻辑
    // ctx.device_path = Some(value.to_owned());

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
    use crate::host::parse::parser::ProcessingStage;

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
    fn device_path() {
        let mut chain = Chain::new(2, 480, vec!["L".into(), "R".into()]);
        let mut ctx = make_ctx(&mut chain);
        handle("C:\\Audio\\Speakers", &mut ctx).unwrap();
        assert!(!ctx.abort_file);
    }

    #[test]
    fn device_abort_file() {
        let mut chain = Chain::new(2, 480, vec!["L".into(), "R".into()]);
        let mut ctx = make_ctx(&mut chain);
        let result = handle("AbortFile", &mut ctx);
        assert!(result.is_err());
        assert!(ctx.abort_file);
        match result.unwrap_err() {
            ConfigError::AbortFile => {}
            other => panic!("expected AbortFile, got {:?}", other),
        }
    }

    #[test]
    fn device_abort_file_case_insensitive() {
        let mut chain = Chain::new(2, 480, vec!["L".into(), "R".into()]);
        let mut ctx = make_ctx(&mut chain);
        let result = handle("abortfile", &mut ctx);
        assert!(result.is_err());
        assert!(ctx.abort_file);
    }

    #[test]
    fn device_empty_value() {
        let mut chain = Chain::new(2, 480, vec!["L".into(), "R".into()]);
        let mut ctx = make_ctx(&mut chain);
        let result = handle("", &mut ctx);
        assert!(result.is_err());
    }

    #[test]
    fn device_path_with_special_chars() {
        let mut chain = Chain::new(2, 480, vec!["L".into(), "R".into()]);
        let mut ctx = make_ctx(&mut chain);
        handle("\\Device\\{AABBCCDD-1234-5678}", &mut ctx).unwrap();
    }
}