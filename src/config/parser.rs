//! config/parser.rs — 配置文件解析器（v6.3 规范 6.1）
//!
//! 边界：不知道 install/、object/。只负责解析配置文件，构建 Filter 链。
//!
//! v7.9（P0-4）：新增 filter_spec 配置指纹产出（`parse_file_with_spec` 双返回），
//! 供 object 层热重载判定配置是否实质变化（含 Include 递归展开）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::commands::cond::{CondState, Variables};
use crate::config::commands::{
    channel, cond, copy, delay, device, expr, filter as filter_cmd, graphic, include, loudness,
    preamp, rew, stage,
};
use crate::config::ConfigError;
use crate::pipeline::dsp::factory::{FilterRegistry, OutcomeKind};
use crate::pipeline::dsp::filter::{ConfigLoader, DspContext, Filter};

/// 一条成功解析命令的规范化指纹（v7.9，配置变更检测）。
pub type FilterSpec = String;

/// 配置指纹（spec chain）：一次完整解析产出的 filter_spec 有序序列。
pub type SpecChain = Vec<FilterSpec>;

/// 配置文件大小上限（128KB，v7.9 P0-4）。
pub const MAX_CONFIG_FILE_SIZE: u64 = 128 * 1024;

/// 产出单条 filter_spec。统一在分发层调用。
fn produce_spec(cmd: &str, value: &str) -> String {
    let sep = '\x1F';
    if value.is_empty() {
        // 无冒号行（裸命令）：整行小写 + token 规范化（v7.9 契约）。
        let lower = cmd.trim().to_ascii_lowercase();
        normalize_tokens(&lower, sep)
    } else {
        format!(
            "{}{}{}",
            cmd.trim().to_ascii_lowercase(),
            sep,
            normalize_tokens(value, sep)
        )
    }
}

/// token 级规范化：按逗号/空格拆 token；可 parse 为 f64 的经 normalize_number。
fn normalize_tokens(s: &str, sep: char) -> String {
    let mut out = String::new();
    let mut first = true;
    for tok in s.split(|c: char| c == ',' || c.is_whitespace()) {
        if tok.is_empty() {
            continue;
        }
        if !first {
            out.push(sep);
        }
        first = false;
        match tok.parse::<f64>() {
            Ok(f) => out.push_str(&normalize_number(f)),
            Err(_) => out.push_str(tok),
        }
    }
    out
}

/// 数值规范化：整数去尾零；非整数统一 6 位有效数字去尾零。
fn normalize_number(f: f64) -> String {
    if f == f.trunc() && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        let s = format!("{:.6}", f);
        let s = s.trim_end_matches('0').trim_end_matches('.');
        s.to_owned()
    }
}

/// 解析阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseStage {
    None,
    PreMix,
    PostMix,
    Capture,
}
impl Default for ParseStage {
    fn default() -> Self {
        Self::None
    }
}

/// 解析期间可变状态。
pub struct ParseContext<'a> {
    /// 当前正在构建的过滤器列表。
    pub filters: &'a mut Vec<Box<dyn Filter>>,
    /// 配置指纹产出（v7.9，P0-4）。
    pub specs: &'a mut SpecChain,
    /// 过滤器工厂注册表。
    pub registry: &'a FilterRegistry,
    /// 引擎上下文（只读）。
    pub dsp_ctx: &'a DspContext,
    /// 当前处理阶段。
    pub stage: ParseStage,
    /// 当前设备类型。
    pub is_capture: bool,
    /// 当前文件路径。
    pub current_file: PathBuf,
    /// 当前行号。
    pub line_number: usize,
    /// AbortFile 标志。
    pub abort_file: bool,
    /// 条件栈。
    pub cond_stack: Vec<CondState>,
    /// 变量存储。
    pub variables: Variables,
    /// Include 递归深度。
    pub include_depth: usize,
    /// 当前通道名称子集。
    pub current_channels: Vec<String>,
    /// 所有通道名称。
    pub all_channels: Vec<String>,
    /// 当前设备路径。
    pub current_device: Option<String>,
}

/// 配置解析器。
pub struct ConfigParser {
    registry: FilterRegistry,
}

impl ConfigParser {
    pub fn new(registry: FilterRegistry) -> Self {
        Self { registry }
    }

    /// 解析配置文件（丢弃 spec）。
    pub fn parse_file(
        &self,
        path: &str,
        ctx: &DspContext,
    ) -> Result<Vec<Box<dyn Filter>>, ConfigError> {
        let (filters, _specs) = self.parse_file_with_spec(path, ctx)?;
        Ok(filters)
    }

    /// 解析配置文件，同时产出配置指纹（v7.9，P0-4 配置变更检测主入口）。
    pub fn parse_file_with_spec(
        &self,
        path: &str,
        ctx: &DspContext,
    ) -> Result<(Vec<Box<dyn Filter>>, SpecChain), ConfigError> {
        let path_ref = Path::new(path);
        // 128KB 逐文件闸门：主文件超限 → 整体解析失败。
        if std::fs::metadata(path_ref)
            .map(|m| m.len() > MAX_CONFIG_FILE_SIZE)
            .unwrap_or(false)
        {
            return Err(ConfigError::IoError {
                path: path_ref.display().to_string(),
                message: format!("config exceeds {} bytes", MAX_CONFIG_FILE_SIZE),
            });
        }
        let content = read_config_file(path_ref)?;
        let mut filters: Vec<Box<dyn Filter>> = Vec::new();
        let mut specs: SpecChain = Vec::new();
        parse_content_with_spec(
            &content,
            &mut filters,
            &self.registry,
            ctx,
            path_ref,
            0,
            &mut specs,
        )?;
        Ok((filters, specs))
    }

    /// 解析配置字符串（丢弃 spec）。
    pub fn parse_string(
        &self,
        content: &str,
        ctx: &DspContext,
    ) -> Result<Vec<Box<dyn Filter>>, ConfigError> {
        let mut filters: Vec<Box<dyn Filter>> = Vec::new();
        parse_content(
            content,
            &mut filters,
            &self.registry,
            ctx,
            Path::new("<string>"),
            0,
        )?;
        Ok(filters)
    }

    /// 解析行列表。
    pub fn parse_lines(
        &self,
        lines: &[String],
        ctx: &DspContext,
    ) -> Result<Vec<Box<dyn Filter>>, ConfigError> {
        let content = lines.join("\n");
        self.parse_string(&content, ctx)
    }
}

/// 读取配置文件内容（UTF-8 优先 + BOM 跳过 + lossy 降级）。
pub fn read_config_file(path: &Path) -> Result<String, ConfigError> {
    let bytes = std::fs::read(path).map_err(|e| ConfigError::IoError {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;

    let content = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    };

    Ok(content.trim_start_matches('\u{feff}').to_owned())
}

/// 解析文件内容字符串（丢弃 spec）。
pub fn parse_content(
    content: &str,
    filters: &mut Vec<Box<dyn Filter>>,
    registry: &FilterRegistry,
    dsp_ctx: &DspContext,
    file_path: &Path,
    depth: usize,
) -> Result<(), ConfigError> {
    let mut specs: SpecChain = Vec::new();
    parse_content_with_spec(content, filters, registry, dsp_ctx, file_path, depth, &mut specs)
}

/// 解析文件内容字符串并产出配置指纹（v7.9，P0-4）。
pub fn parse_content_with_spec(
    content: &str,
    filters: &mut Vec<Box<dyn Filter>>,
    registry: &FilterRegistry,
    dsp_ctx: &DspContext,
    file_path: &Path,
    depth: usize,
    out_specs: &mut SpecChain,
) -> Result<(), ConfigError> {
    let all_channels = dsp_ctx.channel_names.clone();
    let mut pc = ParseContext {
        filters,
        specs: out_specs,
        registry,
        dsp_ctx,
        stage: ParseStage::None,
        is_capture: false,
        current_file: file_path.to_path_buf(),
        line_number: 0,
        abort_file: false,
        cond_stack: Vec::new(),
        variables: HashMap::new(),
        include_depth: depth,
        current_channels: all_channels.clone(),
        all_channels,
        current_device: None,
    };
    parse_lines_impl(content, &mut pc, depth)
}

/// 逐行解析实现（规范 6.1 逐行分发逻辑 + v7.9 filter_spec 产出）。
pub(crate) fn parse_lines_impl(
    content: &str,
    ctx: &mut ParseContext,
    _depth: usize,
) -> Result<(), ConfigError> {
    for (i, line) in content.lines().enumerate() {
        ctx.line_number = i + 1;
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // v7.11：split_command_value 严格化（缺冒号/多余冒号 → SyntaxError，
        // v7.9 裸命令可达性废弃——无冒号行拒绝解析）。
        let (cmd, value) = split_command_value(trimmed, ctx)?;
        let cmd_lower = cmd.to_ascii_lowercase();

        // 条件分支始终处理。
        match cmd_lower.as_str() {
            "if" => {
                cond::handle_if(value, ctx)?;
                continue;
            }
            "elseif" => {
                cond::handle_elseif(value, ctx)?;
                continue;
            }
            "else" => {
                cond::handle_else(ctx)?;
                continue;
            }
            "endif" => {
                cond::handle_endif(ctx)?;
                continue;
            }
            _ => {}
        }

        // 条件跳过。
        if cond::is_skipping(&ctx.cond_stack) {
            continue;
        }

        let mut dispatch = || -> Result<(), ConfigError> {
            match cmd_lower.as_str() {
                "device" => device::handle(value, ctx),
                "stage" => stage::handle(value, ctx),
                "channel" => channel::handle(value, ctx),
                "eval" => expr::handle(value, ctx),
                "include" => include::handle(value, ctx),
                "loudness" => {
                    let r = loudness::handle(value, ctx);
                    if r.is_ok() {
                        ctx.specs.push(produce_spec(cmd, value));
                    }
                    r
                }
                // DSP 命令：spec 产出 = filters 数量增长（含 OFF→Passthrough）时 push。
                "filter" => {
                    let before = ctx.filters.len();
                    let r = filter_cmd::handle(value, ctx);
                    if r.is_ok() && ctx.filters.len() > before {
                        ctx.specs.push(produce_spec(cmd, value));
                    }
                    r
                }
                "graphiceq" => {
                    let before = ctx.filters.len();
                    let r = graphic::handle(value, ctx);
                    if r.is_ok() && ctx.filters.len() > before {
                        ctx.specs.push(produce_spec(cmd, value));
                    }
                    r
                }
                "preamp" => {
                    let before = ctx.filters.len();
                    let r = preamp::handle(value, ctx);
                    if r.is_ok() && ctx.filters.len() > before {
                        ctx.specs.push(produce_spec(cmd, value));
                    }
                    r
                }
                "copy" => {
                    let before = ctx.filters.len();
                    let r = copy::handle(value, ctx);
                    if r.is_ok() && ctx.filters.len() > before {
                        ctx.specs.push(produce_spec(cmd, value));
                    }
                    r
                }
                "delay" => {
                    let before = ctx.filters.len();
                    let r = delay::handle(value, ctx);
                    if r.is_ok() && ctx.filters.len() > before {
                        ctx.specs.push(produce_spec(cmd, value));
                    }
                    r
                }
                _ => {
                    // REW 动态命令名（v7.4：starts_with("filter ") 前缀）。
                    if cmd_lower.starts_with("filter ") {
                        let before = ctx.filters.len();
                        let r = rew::handle(value, ctx);
                        if r.is_ok() && ctx.filters.len() > before {
                            ctx.specs.push(produce_spec(cmd, value));
                        }
                        r
                    } else {
                        // ── 白名单校验（v7.12，P0-4 二次反馈）──
                        // 静态命令（Device/Stage/Channel/Eval/Include/Filter/GraphicEQ/Preamp/
                        // Copy/Delay）与 REW `Filter N:` 已在上方命中；此处命令名必须 ∈
                        // registry.factory_names()（IIR/Biquad/Convolution/VSTPlugin/
                        // LoudnessCorrection）——否则 SyntaxError「未知命令」，**不落 registry**
                        // （修复 v7.11「Unmatched 判定失效」：Convolution 宽容解析
                        //  不再接管未知命令）。
                        let registry_names = ctx.registry.factory_names();
                        if !is_known_dsp_command(&cmd_lower, &registry_names) {
                            return Err(ConfigError::SyntaxError {
                                file: ctx.current_file.display().to_string(),
                                line: ctx.line_number,
                                message: format!("未知命令 '{}'", cmd),
                            });
                        }

                        // VSTPlugin 特判（v7.12）：白名单命中但功能未启用（v7.11 预留，
                        // VstFactory 恒 NoMatch）——不过 try_create，直接明确报错。
                        if cmd_lower == "vstplugin" {
                            return Err(ConfigError::SyntaxError {
                                file: ctx.current_file.display().to_string(),
                                line: ctx.line_number,
                                message: format!(
                                    "命令无效 '{}'：该命令当前未启用（预留）",
                                    cmd
                                ),
                            });
                        }

                        // 已知命令 → 按命令名精确分派（v9.1）。裸命令已由
                        // split_command_value 冒号检查拒绝（value 恒为参数体）。
                        // 精确分派避免 Convolution 等宽容工厂吞掉
                        // 已知命令的非法参数（v7.12「命令无效」契约）。
                        let outcome = ctx
                            .registry
                            .try_create_named(&cmd, value, ctx.dsp_ctx, &NullConfigLoader);
                        match outcome.result {
                            OutcomeKind::FilterAdded(f) => {
                                ctx.filters.push(f);
                                ctx.specs.push(produce_spec(cmd, value));
                            }
                            OutcomeKind::MatchedNoFilter => {}
                            OutcomeKind::Aborted => {
                                ctx.abort_file = true;
                            }
                            OutcomeKind::Unmatched => {
                                // v7.12：Unmatched 收窄为「已知命令的参数无效」——
                                // 命令已过白名单（参数无法解析），不再误报「未知命令」。
                                return Err(ConfigError::SyntaxError {
                                    file: ctx.current_file.display().to_string(),
                                    line: ctx.line_number,
                                    message: format!("命令无效 '{}'：参数无法解析", cmd),
                                });
                            }
                        }
                        Ok(())
                    }
                }
            }
        };

        match dispatch() {
            Ok(()) => {}
            Err(ConfigError::AbortFile) => return Ok(()),
            Err(e) => return Err(e),
        }

        if ctx.abort_file {
            break;
        }
    }

    // 条件栈平衡检查。
    if !ctx.cond_stack.is_empty() {
        return Err(ConfigError::SyntaxError {
            file: ctx.current_file.display().to_string(),
            line: ctx.line_number,
            message: "unterminated If: block".to_owned(),
        });
    }

    Ok(())
}

/// 判断命令名是否为已知 DSP 命令（v7.12 白名单校验）。
///
/// 静态命令与 REW `Filter N:` 前缀已由上层分支拦截；本函数仅检查
/// registry 命令名集合（`FilterRegistry::factory_names()`，pipeline 4.10，
/// 小写比较）。命令名不在此集合 → 未知命令 → 直接
/// `SyntaxError「未知命令」`——**不落 registry**（修复 Convolution 宽容接管）。
fn is_known_dsp_command(cmd_lower: &str, registry_command_names: &[&str]) -> bool {
    registry_command_names
        .iter()
        .any(|n| n.eq_ignore_ascii_case(cmd_lower))
}

/// 分割 `Command: value`（v7.11 语法严格化）。
///
/// - 恰好一个冒号：正常返回 `(命令关键字, 参数体)`
/// - 零个冒号 → `Err(SyntaxError「缺少冒号：命令必须为 关键字: 参数 格式」)`
///   （无冒号行拒绝解析——对齐 EAPO 严格关键字语法，intent.md「语法严格性」；
///     v7.9 裸命令可达性修正废弃，v7.11 反转）
/// - 多于一个冒号 → `Err(SyntaxError「多余冒号：一行仅允许一个冒号」)`
///   （参数内再含冒号拒绝，确保 value 恒非空、单语义）
pub fn split_command_value<'a>(
    line: &'a str,
    ctx: &ParseContext,
) -> Result<(&'a str, &'a str), ConfigError> {
    let trimmed = line.trim();
    let mut colon_iter = trimmed.match_indices(':');
    match (colon_iter.next(), colon_iter.next()) {
        (Some((idx, _)), None) => Ok((trimmed[..idx].trim(), trimmed[idx + 1..].trim())),
        (None, None) => Err(ConfigError::SyntaxError {
            file: ctx.current_file.display().to_string(),
            line: ctx.line_number,
            message: "缺少冒号：命令必须为 关键字: 参数 格式".to_owned(),
        }),
        _ => Err(ConfigError::SyntaxError {
            file: ctx.current_file.display().to_string(),
            line: ctx.line_number,
            message: "多余冒号：一行仅允许一个冒号".to_owned(),
        }),
    }
}

/// 空 ConfigLoader。
pub(crate) struct NullConfigLoader;
impl ConfigLoader for NullConfigLoader {
    fn load_config(&self, _path: &str, _ctx: &DspContext) -> Vec<Box<dyn Filter>> {
        vec![]
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::dsp::filter::{DeviceType, ProcessingStage};
    use std::collections::HashMap;

    fn test_ctx() -> DspContext {
        DspContext {
            sample_rate: 48000,
            channel_count: 2,
            channel_mask: 0x3,
            channel_names: vec!["L".into(), "R".into()],
            max_frame_count: 480,
            bits_per_sample: 32,
            device_type: DeviceType::Render,
            stage: ProcessingStage::None,
            variables: HashMap::new(),
            loudness_enabled: std::cell::Cell::new(true),
            rt_marker: std::marker::PhantomData,
        }
    }

    fn test_parser() -> ConfigParser {
        let mut registry = FilterRegistry::new();
        crate::config::commands::register_all_commands(&mut registry);
        ConfigParser::new(registry)
    }

    /// 测试辅助：解析字符串 + 产出 spec（Result 版，允许断言错误）。
    fn parse_str_spec_result(
        content: &str,
        ctx: &DspContext,
    ) -> Result<(Vec<Box<dyn Filter>>, SpecChain), ConfigError> {
        let mut registry = FilterRegistry::new();
        crate::config::commands::register_all_commands(&mut registry);
        let mut filters: Vec<Box<dyn Filter>> = Vec::new();
        let mut specs: SpecChain = Vec::new();
        parse_content_with_spec(
            content,
            &mut filters,
            &registry,
            ctx,
            Path::new("<string>"),
            0,
            &mut specs,
        )?;
        Ok((filters, specs))
    }

    /// 测试辅助：解析字符串 + 产出 spec（成功路径）。
    fn parse_str_spec(content: &str, ctx: &DspContext) -> (Vec<Box<dyn Filter>>, SpecChain) {
        let mut registry = FilterRegistry::new();
        crate::config::commands::register_all_commands(&mut registry);
        let mut filters: Vec<Box<dyn Filter>> = Vec::new();
        let mut specs: SpecChain = Vec::new();
        parse_content_with_spec(
            content,
            &mut filters,
            &registry,
            ctx,
            Path::new("<string>"),
            0,
            &mut specs,
        )
        .unwrap();
        (filters, specs)
    }

    fn split_ctx() -> ParseContext<'static> {
        // 仅需 current_file/line_number 供 SyntaxError 构造。
        let filters: &'static mut Vec<Box<dyn Filter>> = Box::leak(Box::new(Vec::new()));
        let specs: &'static mut Vec<String> = Box::leak(Box::new(Vec::new()));
        let dsp: &'static DspContext = Box::leak(Box::new(test_ctx()));
        let registry: &'static FilterRegistry = Box::leak(Box::new(FilterRegistry::new()));
        ParseContext {
            filters,
            specs,
            registry,
            dsp_ctx: dsp,
            stage: ParseStage::None,
            is_capture: false,
            current_file: Path::new("<test>").to_path_buf(),
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
    fn split_command_value_basic() {
        let ctx = split_ctx();
        assert_eq!(
            split_command_value("Preamp: -6.0 dB", &ctx).unwrap(),
            ("Preamp", "-6.0 dB")
        );
        assert_eq!(split_command_value("  Copy: L=R  ", &ctx).unwrap(), ("Copy", "L=R"));
        assert_eq!(split_command_value("  Preamp :  -6.0 ", &ctx).unwrap(), ("Preamp", "-6.0"));
    }

    #[test]
    fn split_command_value_no_colon() {
        // v7.11：无冒号行 → SyntaxError「缺少冒号」。
        let ctx = split_ctx();
        let err = split_command_value("PK Fc 1000 Hz", &ctx).unwrap_err();
        assert!(err.to_string().contains("缺少冒号"));
    }

    #[test]
    fn split_command_value_multi_colon() {
        // v7.11：多余冒号 → SyntaxError「多余冒号」。
        let ctx = split_ctx();
        let err = split_command_value("Copy: L=R : R", &ctx).unwrap_err();
        assert!(err.to_string().contains("多余冒号"));
    }

    #[test]
    fn parse_preamp_via_config_parser() {
        let parser = test_parser();
        let filters = parser
            .parse_string("Preamp: -6.0 dB\n", &test_ctx())
            .unwrap();
        assert_eq!(filters.len(), 1);
    }

    #[test]
    fn parse_full_config_sample_no_unmatched() {
        let parser = test_parser();
        let content = "\
# VxAPO 示例配置
Preamp: -6.0 dB
Filter: ON HP Fc 40 Hz Q 0.707
Filter: ON PK Fc 1000 Hz Gain +3.0 dB Q 1.0
GraphicEQ: 20 -3.1; 25 -3.1; 31.5 -2.0
Copy: L=R
Delay: 20 ms
Channel: L
Filter: ON PK Fc 2000 Hz Gain +1.0 dB Q 2.0
Channel: *
";
        let filters = parser.parse_string(content, &test_ctx()).unwrap();
        assert_eq!(filters.len(), 8);
    }

    #[test]
    fn parse_include_recursive() {
        let dir = std::env::temp_dir().join("vxapo_parser_include_test");
        std::fs::create_dir_all(&dir).unwrap();
        let sub = dir.join("sub.txt");
        let main = dir.join("main.txt");
        std::fs::write(&sub, "Preamp: -3.0 dB\n").unwrap();
        std::fs::write(&main, format!("Include: \"sub.txt\"\nDelay: 10 ms\n")).unwrap();

        let parser = test_parser();
        let filters = parser
            .parse_file(main.to_str().unwrap(), &test_ctx())
            .unwrap();
        assert_eq!(filters.len(), 2);
    }

    #[test]
    fn produce_spec_basic_formats() {
        assert_eq!(
            produce_spec("Preamp", "-6.0 dB"),
            "preamp\x1F-6\x1FdB"
        );
        assert_eq!(
            produce_spec("PK Fc 1000 Hz Gain +3.0 dB Q 1.0", ""),
            "pk\x1Ffc\x1F1000\x1Fhz\x1Fgain\x1F3\x1Fdb\x1Fq\x1F1"
        );
    }

    #[test]
    fn parse_file_with_spec_returns_filters_and_specs() {
        let (filters, specs) = parse_str_spec("Preamp: -6.0 dB\nDelay: 500 ms\n", &test_ctx());
        assert_eq!(filters.len(), 2);
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0], "preamp\x1F-6\x1FdB");
        assert!(specs[1].starts_with("delay\x1F500"));
    }

    #[test]
    fn spec_unchanged_ignores_comments_and_whitespace() {
        let (_f, s1) = parse_str_spec("Preamp: -6.0 dB\n", &test_ctx());
        let (_f, s2) = parse_str_spec("# 注释\n\nPreamp:  -6.0  dB\n", &test_ctx());
        assert_eq!(s1, s2, "注释/空白/额外空格不应改变 spec");
    }

    #[test]
    fn spec_changes_with_numeric_diff() {
        let (_f, s1) = parse_str_spec("Preamp: -6.0 dB\n", &test_ctx());
        let (_f, s2) = parse_str_spec("Preamp: -7.5 dB\n", &test_ctx());
        assert_ne!(s1, s2);
    }

    // ── v9.1 FxSound 效果器命令（AuralEnhancer/Reverb/Maximizer） ────────

    #[test]
    fn fxsound_commands_parse_and_produce_filters() {
        let content = "\
AuralEnhancer: TuneHz 1760 Drive 1.77 Odd 1.5 Even 0.0 Wet 1.0 Dry 0.0
Reverb: RoomSize 1.0 Decay 0.566 Damping 0.408 Bandwidth 0.350 PreDelay 0 ms MotionRate 0.11 MotionDepth 0.63 ms Wet 0.3 Dry 0.9
Maximizer: GainBoost 6 dB MaxOutput -0.3 dB Release 100 ms Target 0.32 Lookahead 0.75 ms Dither Shaped
Wide: Intensity 0.354331
";
        let (filters, specs) = parse_str_spec(content, &test_ctx());
        assert_eq!(filters.len(), 4);
        assert_eq!(specs.len(), 4);
        assert!(specs[0].starts_with("auralenhancer"));
        assert!(specs[1].starts_with("reverb"));
        assert!(specs[2].starts_with("maximizer"));
        assert!(specs[3].starts_with("wide"));
    }

    #[test]
    fn fxsound_spec_changes_with_params() {
        let (_f, s1) = parse_str_spec("AuralEnhancer: Drive 1.0\n", &test_ctx());
        let (_f, s2) = parse_str_spec("AuralEnhancer: Drive 2.0\n", &test_ctx());
        assert_ne!(s1, s2);
        let (_f, s3) = parse_str_spec("Reverb: RoomSize 0.8\n", &test_ctx());
        let (_f, s4) = parse_str_spec("Reverb: RoomSize 1.2\n", &test_ctx());
        assert_ne!(s3, s4);
        let (_f, s5) = parse_str_spec("Maximizer: GainBoost 3 dB\n", &test_ctx());
        let (_f, s6) = parse_str_spec("Maximizer: GainBoost 9 dB\n", &test_ctx());
        assert_ne!(s5, s6);
    }

    #[test]
    fn fxsound_invalid_params_reject() {
        // 已知命令但参数非法 → SyntaxError（不是静默跳过）。
        let err = parse_str_spec_result("AuralEnhancer: Bogus 1\n", &test_ctx()).unwrap_err();
        assert!(matches!(err, ConfigError::SyntaxError { .. }));
        let err = parse_str_spec_result("Maximizer: Dither Pink\n", &test_ctx()).unwrap_err();
        assert!(matches!(err, ConfigError::SyntaxError { .. }));
    }

    // ── v7.9 三类 config 复杂度覆盖（用户反馈） ──────────────────────────

    #[test]
    fn spec_for_complex_config_mixed_commands() {
        // 正常复杂 config：Include 子文件 + 条件 + 裸命令 + DSP 显式命令混合。
        // 验证 spec 序列反映完整链（含子文件展开 + 滤波器顺序）。
        let dir = std::env::temp_dir().join("vxapo_spec_complex_test");
        std::fs::create_dir_all(&dir).unwrap();
        let main = dir.join("main.txt");
        std::fs::write(
            &main,
            "If: device_type == render\nPreamp: -6.0 dB\nEndIf:\n\
             Filter: ON PK Fc 1000 Hz Gain +3.0 dB Q 1.0\n\
             Copy: L=R\nInclude: \"sub.txt\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("sub.txt"), "Delay: 20 ms\nGraphicEQ: 20 -3.1; 40 -2.0\n").unwrap();

        let parser = test_parser();
        let (filters, specs) = parser.parse_file_with_spec(main.to_str().unwrap(), &test_ctx()).unwrap();
        // Preamp(If true) + PK + Copy + Delay(sub) + GraphicEQ(sub) = 5 滤波器
        assert_eq!(filters.len(), 5);
        assert_eq!(specs.len(), 5);
        // 顺序保持（sub 文件内联在 Include 位置）。
        // 注意：value token 规范化只对数值，非数值 token（ON/PK/Fc/Hz/Gain/dB/Q）保持原样
        // 大小写——`Filter: ON PK...` 产出 `filter\x1FON\x1FPK...`（v7.9 契约：不剥离单位、
        // 非数值不转小写）。
        assert_eq!(specs[0], "preamp\x1F-6\x1FdB");
        assert!(specs[1].starts_with("filter\x1FON\x1FPK"));
        assert!(specs[2].starts_with("copy\x1F"));
        assert!(specs[3].starts_with("delay\x1F20"));
        assert!(specs[4].starts_with("graphiceq"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spec_with_error_config_reports_error() {
        // 携带错误 config：未闭合 If → SyntaxError（parse_file_with_spec 传播）。
        let dir = std::env::temp_dir().join("vxapo_spec_err_test");
        std::fs::create_dir_all(&dir).unwrap();
        let main = dir.join("bad.txt");
        std::fs::write(&main, "If: true\nPreamp: -6.0 dB\n").unwrap();

        let parser = test_parser();
        let result = parser.parse_file_with_spec(main.to_str().unwrap(), &test_ctx());
        assert!(result.is_err());
        match result {
            Err(ConfigError::SyntaxError { message, .. }) => {
                assert!(message.contains("unterminated If"));
            }
            other => panic!("expected SyntaxError, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spec_with_unknown_command_reports_error() {
        // v7.11：未知命令 → Unmatched → SyntaxError「未知命令」整体失败（不再 warn 跳过）。
        let err = parse_str_spec_result("BogusCommand: x\nDelay: 10 ms\n", &test_ctx()).unwrap_err();
        assert!(matches!(err, ConfigError::SyntaxError { message, .. } if message.contains("未知命令")));
    }

    #[test]
    fn spec_with_missing_colon_reports_error() {
        // v7.11：无冒号行 → SyntaxError「缺少冒号」（裸命令拒绝，v7.9 可达性反转）。
        let err = parse_str_spec_result("Preamp: -3.0 dB\nBogusNoColon\n", &test_ctx()).unwrap_err();
        assert!(matches!(err, ConfigError::SyntaxError { message, .. } if message.contains("缺少冒号")));
    }

    #[test]
    fn spec_with_abort_file_stops_current_file() {
        // 携带错误/控制 config：Device: AbortFile → 终止当前文件（后续命令不解析）。
        let (_f, specs) = parse_str_spec("Preamp: -3.0 dB\nDevice: AbortFile\nDelay: 10 ms\n", &test_ctx());
        // AbortFile 前的 Preamp 保留；Delay 被跳过。
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0], "preamp\x1F-3\x1FdB");
    }

    #[test]
    fn spec_with_garbled_text_reports_missing_colon() {
        // 乱码：非 UTF-8 字节经 lossy 降级为 U+FFFD 行（无冒号）→ v7.11「缺少冒号」
        // SyntaxError 整体失败（不 panic；「配置写错必有反馈」）。
        // 注意：Rust `\xFF` 是非法转义（`\x` 只支持 ASCII ≤0x7F），
        // 非 UTF-8 字节用 byte string + from_utf8_lossy 构造。
        let mut bytes = b"Preamp: -6.0 dB\n".to_vec();
        // GBK 风格汉字乱码的 UTF-8 非法字节序列（lossy → 无冒号的 U+FFFD 行）。
        bytes.extend_from_slice(&[0xBA, 0xBA, 0xD7, 0xD6, 0xFF, 0xFE, b'\n']);
        bytes.extend_from_slice(b"Delay: 5 ms\n");
        let content = String::from_utf8_lossy(&bytes).into_owned();
        let err = parse_str_spec_result(&content, &test_ctx()).unwrap_err();
        assert!(matches!(err, ConfigError::SyntaxError { message, .. } if message.contains("缺少冒号")));
    }

    #[test]
    fn spec_with_oversized_include_rejects_whole() {
        // Include 子文件超 128KB → 整体解析失败（v7.9 逐文件闸门）。
        let dir = std::env::temp_dir().join("vxapo_spec_oversize_test");
        std::fs::create_dir_all(&dir).unwrap();
        let sub = dir.join("big.txt");
        // 130KB 填充
        let big = vec![b'#'; 130 * 1024];
        std::fs::write(&sub, big).unwrap();
        let main = dir.join("main.txt");
        std::fs::write(&main, format!("Include: \"big.txt\"\n")).unwrap();

        let parser = test_parser();
        let result = parser.parse_file_with_spec(main.to_str().unwrap(), &test_ctx());
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
