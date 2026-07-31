//! config/parser.rs — 配置文件解析器（v6.2 规范 6.1）

use std::collections::HashMap;
use std::path::Path;

use crate::config::ConfigError;
use crate::pipeline::dsp::factory::{FilterRegistry, OutcomeKind};
use crate::pipeline::dsp::filter::{ConfigLoader, DspContext, Filter};

/// 解析阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseStage { None, PreMix, PostMix, Capture }
impl Default for ParseStage { fn default() -> Self { Self::None } }

/// 解析期间可变状态。
pub struct ParseContext<'a> {
    pub filters: &'a mut Vec<Box<dyn Filter>>,
    pub registry: &'a FilterRegistry,
    pub dsp_ctx: &'a DspContext,
    pub stage: ParseStage,
    pub is_capture: bool,
    pub current_file: &'a Path,
    pub line_number: usize,
    pub abort_file: bool,
    pub cond_stack: Vec<(bool, bool)>,
    pub variables: HashMap<String, f64>,
    pub include_depth: usize,
    pub current_channels: Vec<String>,
    pub all_channels: Vec<String>,
}

/// 配置解析器。
pub struct ConfigParser {
    registry: FilterRegistry,
}

impl ConfigParser {
    pub fn new(registry: FilterRegistry) -> Self { Self { registry } }

    pub fn parse_file(&self, path: &str, ctx: &DspContext) -> Result<Vec<Box<dyn Filter>>, ConfigError> {
        let content = read_config_file(Path::new(path))?;
        let filters = self.parse_string(&content, ctx)?;
        Ok(filters)
    }

    pub fn parse_string(&self, content: &str, ctx: &DspContext) -> Result<Vec<Box<dyn Filter>>, ConfigError> {
        let lines: Vec<String> = content.lines().map(|s| s.to_owned()).collect();
        self.parse_lines(&lines, ctx)
    }

    pub fn parse_lines(&self, lines: &[String], ctx: &DspContext) -> Result<Vec<Box<dyn Filter>>, ConfigError> {
        let mut filters: Vec<Box<dyn Filter>> = Vec::new();
        let mut pc = ParseContext {
            filters: &mut filters,
            registry: &self.registry,
            dsp_ctx: ctx,
            stage: ParseStage::None,
            is_capture: false,
            current_file: Path::new("<string>"),
            line_number: 0,
            abort_file: false,
            cond_stack: Vec::new(),
            variables: HashMap::new(),
            include_depth: 0,
            current_channels: ctx.channel_names.clone(),
            all_channels: ctx.channel_names.clone(),
        };
        parse_lines_impl(lines, &mut pc, 0)?;
        Ok(filters)
    }
}

/// 读取配置文件（UTF-8 优先）。
pub fn read_config_file(path: &Path) -> Result<String, ConfigError> {
    let content = std::fs::read_to_string(path).map_err(|e| ConfigError::IoError {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;
    Ok(content.trim_start_matches('\u{feff}').to_owned())
}

/// 逐行解析实现。
fn parse_lines_impl(lines: &[String], ctx: &mut ParseContext, _depth: usize) -> Result<(), ConfigError> {
    for (i, line) in lines.iter().enumerate() {
        ctx.line_number = i + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let (cmd, value) = split_command_value(trimmed);
        // 通过 FilterRegistry 分发（含 IIR/Preamp 等 DSP 命令）
        let outcome = ctx.registry.try_create(value, ctx.dsp_ctx, &NullConfigLoader);
        match outcome.result {
            OutcomeKind::FilterAdded(f) => {
                // 校验命令名映射：先检查纯配置命令
                match cmd {
                    "Device" | "If" | "ElseIf" | "Else" | "EndIf" | "Stage" | "Channel" | "Eval" | "Include" => {
                        return Err(ConfigError::SyntaxError {
                            file: ctx.current_file.display().to_string(),
                            line: ctx.line_number,
                            message: format!("command '{cmd}' requires config command handler"),
                        });
                    }
                    _ => ctx.filters.push(f),
                }
            }
            OutcomeKind::MatchedNoFilter => {}
            OutcomeKind::Aborted => { ctx.abort_file = true; }
            OutcomeKind::Unmatched => {
                // 未知命令：容忍（与规范一致，记录警告）
            }
        }
        if ctx.abort_file { break; }
    }
    Ok(())
}

/// 分割 `Command: value`。
pub fn split_command_value(line: &str) -> (&str, &str) {
    match line.split_once(':') {
        Some((c, v)) => (c.trim(), v.trim()),
        None => (line.trim(), ""),
    }
}

/// 空 ConfigLoader（骨架，Include 未实现时用）。
struct NullConfigLoader;
impl ConfigLoader for NullConfigLoader {
    fn load_config(&self, _path: &str, _ctx: &DspContext) -> Vec<Box<dyn Filter>> { vec![] }
}