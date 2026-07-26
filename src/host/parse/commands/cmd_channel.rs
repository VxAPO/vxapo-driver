//! host/parse/commands/cmd_channel.rs — Channel: 命令
//!
//! 语法：`Channel: name1 name2 ...` 或 `Channel: *`
//!
//! 选择后续过滤器操作的通道子集。
//! - `Channel: L R` — 仅操作 L 和 R 通道
//! - `Channel: *` — 操作所有通道（重置为 allChannelNames）
//!
//! Note 51：通道选择变更时需清除映射缓存。

use crate::host::parse::parser::{ConfigError, ParseContext};

/// 处理 `Channel:` 命令。
///
/// 解析空格分隔的通道名列表，设置到 `chain.current_channel_names`。
pub fn handle(value: &str, ctx: &mut ParseContext) -> Result<(), ConfigError> {
    if value.is_empty() {
        return Err(ConfigError::SyntaxError {
            file: ctx.current_file.display().to_string(),
            line: ctx.line_number,
            message: "Channel: requires at least one channel name or '*'".into(),
        });
    }

    // Channel: * → 重置为 allChannelNames
    if value.trim() == "*" {
        let all_names: Vec<String> = ctx
            .chain
            .all_channel_names()
            .iter()
            .cloned()
            .collect();
        ctx.chain.set_current_channel_names(all_names);
        ctx.chain.invalidate_channel_cache();
        log::debug!("{}: channel set to * (all)", ctx.current_file.display());
        return Ok(());
    }

    // 解析通道名列表
    let names: Vec<String> = value
        .split_whitespace()
        .map(|s| s.to_owned())
        .collect();

    if names.is_empty() {
        return Err(ConfigError::SyntaxError {
            file: ctx.current_file.display().to_string(),
            line: ctx.line_number,
            message: "Channel: requires at least one channel name".into(),
        });
    }

    // 验证每个通道名存在于 allChannelNames 或 ensure_channel_exists
    for name in &names {
        ctx.chain.ensure_channel_exists(name);
    }

    ctx.chain.set_current_channel_names(names.clone());
    ctx.chain.invalidate_channel_cache();

    log::debug!(
        "{}: channel set to [{}]",
        ctx.current_file.display(),
        names.join(", ")
    );

    Ok(())
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::stream::chain::Chain;
    use crate::host::parse::parser::ProcessingStage;
    use std::path::Path;

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
    fn channel_select_stereo() {
        let mut chain = Chain::new(6, 480, vec![
            "L".into(), "R".into(), "C".into(),
            "LFE".into(), "SL".into(), "SR".into(),
        ]);
        let file = Path::new("test.txt");
        let mut ctx = make_ctx(&mut chain, file);
        handle("L R", &mut ctx).unwrap();
        assert_eq!(ctx.chain.current_channel_names(), &["L", "R"]);
    }

    #[test]
    fn channel_select_single() {
        let mut chain = Chain::new(2, 480, vec!["L".into(), "R".into()]);
        let file = Path::new("test.txt");
        let mut ctx = make_ctx(&mut chain, file);
        handle("L", &mut ctx).unwrap();
        assert_eq!(ctx.chain.current_channel_names(), &["L"]);
    }

    #[test]
    fn channel_select_all() {
        let mut chain = Chain::new(2, 480, vec!["L".into(), "R".into()]);
        let file = Path::new("test.txt");
        let mut ctx = make_ctx(&mut chain, file);

        // 先选子集
        handle("L", &mut ctx).unwrap();
        assert_eq!(ctx.chain.current_channel_names().len(), 1);

        // Channel: * 恢复全部
        handle("*", &mut ctx).unwrap();
        assert_eq!(ctx.chain.current_channel_names(), &["L", "R"]);
    }

    #[test]
    fn channel_empty_value() {
        let mut chain = Chain::new(2, 480, vec!["L".into(), "R".into()]);
        let file = Path::new("test.txt");
        let mut ctx = make_ctx(&mut chain, file);
        let result = handle("", &mut ctx);
        assert!(result.is_err());
    }

    #[test]
    fn channel_unknown_name_creates_aux() {
        // ensure_channel_exists 会创建新的辅助通道
        let mut chain = Chain::new(2, 480, vec!["L".into(), "R".into()]);
        let file = Path::new("test.txt");
        let mut ctx = make_ctx(&mut chain, file);
        handle("L R AUX1", &mut ctx).unwrap();
        // AUX1 应被添加到 all_channel_names
        assert!(ctx.chain.channel_index("AUX1").is_some());
    }

    #[test]
    fn channel_overwrite() {
        let mut chain = Chain::new(6, 480, vec![
            "L".into(), "R".into(), "C".into(),
            "LFE".into(), "SL".into(), "SR".into(),
        ]);
        let file = Path::new("test.txt");
        let mut ctx = make_ctx(&mut chain, file);
        handle("L R", &mut ctx).unwrap();
        handle("C LFE", &mut ctx).unwrap();
        assert_eq!(ctx.chain.current_channel_names(), &["C", "LFE"]);
    }

    #[test]
    fn channel_map_resolves() {
        let mut chain = Chain::new(6, 480, vec![
            "L".into(), "R".into(), "C".into(),
            "LFE".into(), "RL".into(), "RR".into(),
        ]);
        let file = Path::new("test.txt");
        let mut ctx = make_ctx(&mut chain, file);
        handle("C LFE", &mut ctx).unwrap();
        let map = ctx.chain.resolve_channel_map();
        // C → index 2, LFE → index 3
        assert_eq!(map, vec![2, 3]);
    }
}