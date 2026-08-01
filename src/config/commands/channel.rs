//! config/commands/channel.rs — Channel: 命令（v6.3 规范 6.4）
//!
//! 语法：
//! - `Channel: L R C LFE` — 仅操作指定通道
//! - `Channel: *` — 操作所有通道（重置为 allChannels）
//!
//! 语义：解析通道名列表，创建 `ChannelFilter`。Filter 的 `initialize()`
//! 返回 `Some(names)` 触发通道选择。

use crate::config::error::ConfigError;
use crate::config::parser::ParseContext;
use crate::pipeline::dsp::filter::Filter;

/// 通道选择过滤器。
///
/// `initialize()` 返回选定的通道名列表，Chain 据此设置通道子集。
#[derive(Debug, Clone)]
pub struct ChannelFilter {
    /// 选定的通道名。
    channels: Vec<String>,
}

impl ChannelFilter {
    /// 创建通道选择过滤器。
    pub fn new(channels: Vec<String>) -> Self {
        Self { channels }
    }
}

impl Filter for ChannelFilter {
    fn process(&mut self, _samples: &mut [Vec<f32>], _frame_count: usize) {
        // 通道选择不处理音频采样。
    }

    fn initialize(&mut self, _sample_rate: u32, _channel_names: &[String]) -> Option<Vec<String>> {
        Some(self.channels.clone())
    }

    fn is_channel_select(&self) -> bool {
        true
    }
}

/// 处理 Channel: 命令。
///
/// - `*`：重置为所有通道（不产生过滤器）。
/// - 其他：验证通道名存在后创建 `ChannelFilter`。
pub fn handle(value: &str, ctx: &mut ParseContext) -> Result<(), ConfigError> {
    let trimmed = value.trim();

    // `Channel: *` → 重置为所有通道。
    if trimmed == "*" {
        ctx.current_channels = ctx.all_channels.clone();
        return Ok(());
    }

    // 解析通道名列表。
    let names: Vec<String> = trimmed
        .split_whitespace()
        .map(|s| s.to_owned())
        .collect();

    if names.is_empty() {
        return Err(syntax(ctx, "Channel: requires channel names or '*'"));
    }

    // 验证每个通道名存在于 all_channels。
    for name in &names {
        if !ctx.all_channels.contains(name) {
            return Err(syntax(
                ctx,
                &format!("unknown channel '{}'", name),
            ));
        }
    }

    // 创建 ChannelFilter 并加入过滤器列表。
    let filter = ChannelFilter::new(names.clone());
    ctx.filters.push(Box::new(filter));
    ctx.current_channels = names;
    Ok(())
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
    use crate::config::parser::{ParseContext, ParseStage};
    use crate::pipeline::dsp::filter::DspContext;
    use crate::pipeline::dsp::factory::FilterRegistry;
    use std::collections::HashMap;
    use std::path::Path;

    fn test_ctx() -> ParseContext<'static> {
        let filters: &'static mut Vec<Box<dyn Filter>> = Box::leak(Box::new(Vec::new()));
        let dsp: &'static DspContext = Box::leak(Box::new(DspContext {
            sample_rate: 48000,
            channel_count: 2,
            channel_mask: 0x3,
            channel_names: vec!["L".into(), "R".into()],
            max_frame_count: 480,
            bits_per_sample: 32,
            device_type: crate::pipeline::dsp::filter::DeviceType::Render,
            stage: crate::pipeline::dsp::filter::ProcessingStage::None,
            variables: HashMap::new(),
            rt_marker: std::marker::PhantomData,
        }));
        let registry: &'static FilterRegistry = Box::leak(Box::new(FilterRegistry::new()));
        ParseContext {
            filters,
            registry,
            dsp_ctx: dsp,
            stage: ParseStage::None,
            is_capture: false,
            current_file: Path::new("<test>"),
            line_number: 1,
            abort_file: false,
            cond_stack: Vec::new(),
            variables: HashMap::new(),
            include_depth: 0,
            current_channels: vec!["L".into(), "R".into()],
            all_channels: vec!["L".into(), "R".into()],
            current_device: None,
        }
    }

    #[test]
    fn star_resets_all_channels() {
        let mut ctx = test_ctx();
        ctx.current_channels = vec!["L".into()];
        handle("*", &mut ctx).unwrap();
        assert_eq!(ctx.current_channels, ctx.all_channels);
    }

    #[test]
    fn valid_channels_create_filter() {
        let mut ctx = test_ctx();
        handle("L", &mut ctx).unwrap();
        assert_eq!(ctx.filters.len(), 1);
        assert_eq!(ctx.current_channels, vec!["L".to_string()]);
    }

    #[test]
    fn unknown_channel_errors() {
        let mut ctx = test_ctx();
        assert!(handle("X", &mut ctx).is_err());
    }

    #[test]
    fn empty_value_errors() {
        let mut ctx = test_ctx();
        assert!(handle("", &mut ctx).is_err());
    }

    #[test]
    fn filter_initialize_returns_channels() {
        let mut filter = ChannelFilter::new(vec!["L".into()]);
        let result = filter.initialize(48000, &["L".into(), "R".into()]);
        assert_eq!(result, Some(vec!["L".to_string()]));
        assert!(filter.is_channel_select());
    }
}