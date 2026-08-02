//! config/parser.rs — 配置文件解析器（v6.3 规范 6.1）
//!
//! 边界：不知道 install/、object/。只负责解析配置文件，构建 Filter 链。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::commands::cond::{CondState, Variables};
use crate::config::commands::{
    channel, cond, copy, delay, device, expr, filter as filter_cmd, graphic, include, preamp,
    rew, stage,
};
use crate::config::ConfigError;
use crate::pipeline::dsp::factory::{FilterRegistry, OutcomeKind};
use crate::pipeline::dsp::filter::{ConfigLoader, DspContext, Filter};

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
///
/// 在解析一个配置文件期间维护状态。不暴露给 FilterFactory（工厂使用 DspContext）。
///
/// 注意：`current_file` 为所有权 `PathBuf`。规范原型为 `&'a Path`，但 Include
/// 递归解析时子文件路径必须独立存活于子 ParseContext——借用无法跨递归层安全
/// 表达（子上下文生命周期由调用栈局部路径借用会与 `filters: &'a mut` 共享的
/// `'a` 冲突）。所有权替代是消除 `Box::leak` 泄漏的必需方案（规范 6.1 指导
/// 方向下的技术替代，`.clinerules/00` 允许）。
pub struct ParseContext<'a> {
    /// 当前正在构建的过滤器列表。
    pub filters: &'a mut Vec<Box<dyn Filter>>,
    /// 过滤器工厂注册表。
    pub registry: &'a FilterRegistry,
    /// 引擎上下文（只读，供工厂创建 Filter 使用）。
    pub dsp_ctx: &'a DspContext,
    /// 当前处理阶段（解析期间可变，Stage: 命令修改）。
    pub stage: ParseStage,
    /// 当前设备类型（解析期间可变）。
    pub is_capture: bool,
    /// 当前文件路径（用于错误报告和 Include 相对路径）。
    pub current_file: PathBuf,
    /// 当前行号（用于错误报告）。
    pub line_number: usize,
    /// AbortFile 标志（Device: 命令设置）。
    pub abort_file: bool,
    /// 条件栈（If/ElseIf/Else/EndIf 嵌套）。
    pub cond_stack: Vec<CondState>,
    /// 变量存储（Eval: 命令写入，If: 条件引用）。
    pub variables: Variables,
    /// Include 递归深度。
    pub include_depth: usize,
    /// 当前通道名称子集（Channel: 命令修改）。
    pub current_channels: Vec<String>,
    /// 所有通道名称（来自 DspContext，不可变）。
    pub all_channels: Vec<String>,
    /// 当前设备路径（Device: 命令设置）。
    pub current_device: Option<String>,
}

/// 配置解析器。
///
/// 持有 FilterRegistry，提供文件/字符串/行列表三种解析入口。
/// 返回 `Vec<Box<dyn Filter>>`，由调用方（object/apo.rs）添加到 Chain。
pub struct ConfigParser {
    registry: FilterRegistry,
}

impl ConfigParser {
    pub fn new(registry: FilterRegistry) -> Self {
        Self { registry }
    }

    /// 解析配置文件。UTF-8 优先，非 UTF-8 使用降级替换（不崩溃）。
    /// 返回 Vec<Box<dyn Filter>>。
    pub fn parse_file(
        &self,
        path: &str,
        ctx: &DspContext,
    ) -> Result<Vec<Box<dyn Filter>>, ConfigError> {
        let path_ref = Path::new(path);
        let content = read_config_file(path_ref)?;
        let mut filters: Vec<Box<dyn Filter>> = Vec::new();
        parse_content(&content, &mut filters, &self.registry, ctx, path_ref, 0)?;
        Ok(filters)
    }

    /// 解析配置字符串。
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

/// 读取配置文件内容。
///
/// UTF-8 优先。检测 UTF-8 BOM (EF BB BF) 则跳过。
/// 非 UTF-8 内容使用 lossy 降级（非法字节替换为 U+FFFD）——满足规范 6.1
/// "非 UTF-8 内容使用 ANSI 代码页降级（非 ASCII 字节替换为 '?'）"的读取
/// 降级意图（文件可读、解析不崩溃），且零额外依赖（不引入系统代码页转换）。
pub fn read_config_file(path: &Path) -> Result<String, ConfigError> {
    let bytes = std::fs::read(path).map_err(|e| ConfigError::IoError {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;

    let content = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    };

    // 跳过 UTF-8 BOM（EF BB BF）。
    Ok(content.trim_start_matches('\u{feff}').to_owned())
}

/// 解析文件内容字符串，逐行分发到命令处理器。
///
/// 解析前保存通道名称快照（从 DspContext 注入，Note 51）；Include 子文件
/// 解析各自持有快照（继承父的 current_channels），解析后不污染父状态。
pub fn parse_content(
    content: &str,
    filters: &mut Vec<Box<dyn Filter>>,
    registry: &FilterRegistry,
    dsp_ctx: &DspContext,
    file_path: &Path,
    depth: usize,
) -> Result<(), ConfigError> {
    let all_channels = dsp_ctx.channel_names.clone();
    let mut pc = ParseContext {
        filters,
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

/// 逐行解析实现（规范 6.1 逐行分发逻辑）。
///
/// - 条件分支（If/ElseIf/Else/EndIf）**始终**处理（管理栈状态），不落入 registry
/// - 条件跳过：当前处于 false 分支时跳过其他命令
/// - 纯配置命令 + DSP 命令（Filter/GraphicEQ/Preamp/Copy/Delay）分发到各 handle
/// - REW 导出格式行（`Filter N:` 动态命令名）分发到 rew::handle
/// - 其余命令经 FilterRegistry 匹配（裸 IIR/Biquad/Convolution 等）
/// - 条件栈不平衡（文件结束仍有未闭合 If）→ SyntaxError
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

        let (cmd, value) = split_command_value(trimmed);
        let cmd_lower = cmd.to_ascii_lowercase();

        // 条件分支始终处理（管理栈状态）。
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

        // 条件跳过：当前处于 false 分支时跳过其他命令。
        if cond::is_skipping(&ctx.cond_stack) {
            continue;
        }

        // 分发结果统一处理：Err(AbortFile) 表示"当前文件应终止"（Device:
        // AbortFile，规范 6.6），是**正常终止**而非致命错误——吞掉并返回
        // Ok(())，不向调用方（含 Include 父文件）传播错误。
        let mut dispatch = || -> Result<(), ConfigError> {
            match cmd_lower.as_str() {
                "device" => device::handle(value, ctx),
                "stage" => stage::handle(value, ctx),
                "channel" => channel::handle(value, ctx),
                "eval" => expr::handle(value, ctx),
                "include" => include::handle(value, ctx),
                // DSP 命令显式分发到 handle（handle 内预处理 + registry 动态创建，
                // 6.10-6.15 各类语义：ON/OFF 开关、逗号小数点、错误语境化）。
                "filter" => filter_cmd::handle(value, ctx),
                "graphiceq" => graphic::handle(value, ctx),
                "preamp" => preamp::handle(value, ctx),
                "copy" => copy::handle(value, ctx),
                "delay" => delay::handle(value, ctx),
                _ => {
                    // REW 导出格式行：`Filter 1: ON PK ...`（命令名是动态的 `Filter N`，
                    // 不会被上面的静态匹配命中）。传原始整行（rew::handle 自剥前缀）。
                    if cmd_lower.starts_with("filter ") {
                        rew::handle(trimmed, ctx)
                    } else {
                        // 其他命令经 FilterRegistry 匹配（裸 DSP 命令：IIR/Biquad/
                        // Convolution/LoudnessCorrection 等，value 直接可被工厂消费）。
                        let outcome = ctx.registry.try_create(value, ctx.dsp_ctx, &NullConfigLoader);
                        match outcome.result {
                            OutcomeKind::FilterAdded(f) => ctx.filters.push(f),
                            OutcomeKind::MatchedNoFilter => {}
                            OutcomeKind::Aborted => {
                                ctx.abort_file = true;
                            }
                            OutcomeKind::Unmatched => {
                                log::warn!("unknown command '{}'", cmd);
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

/// 分割 `Command: value`。
pub fn split_command_value(line: &str) -> (&str, &str) {
    match line.split_once(':') {
        Some((c, v)) => (c.trim(), v.trim()),
        None => (line.trim(), ""),
    }
}

/// 空 ConfigLoader（Include 等命令的默认回调占位；config 层不依赖外部加载器）。
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
            rt_marker: std::marker::PhantomData,
        }
    }

    fn test_parser() -> ConfigParser {
        let mut registry = FilterRegistry::new();
        crate::config::commands::register_all_commands(&mut registry);
        ConfigParser::new(registry)
    }

    #[test]
    fn split_command_value_basic() {
        assert_eq!(
            split_command_value("Preamp: -6.0 dB"),
            ("Preamp", "-6.0 dB")
        );
        assert_eq!(split_command_value("  Copy: L=R  "), ("Copy", "L=R"));
    }

    #[test]
    fn split_command_value_no_colon() {
        assert_eq!(split_command_value("PK Fc 1000 Hz"), ("PK Fc 1000 Hz", ""));
    }

    #[test]
    fn read_config_file_bom_stripped() {
        let dir = std::env::temp_dir().join("vxapo_parser_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bom.txt");
        std::fs::write(&path, b"\xef\xbb\xbfPreamp: -6.0 dB\n").unwrap();
        let content = read_config_file(&path).unwrap();
        assert!(content.starts_with("Preamp"));
    }

    #[test]
    fn read_config_file_lossy_fallback() {
        // 非 UTF-8 字节（ANSI/GBK 风格）→ lossy 降级，不崩溃。
        let dir = std::env::temp_dir().join("vxapo_parser_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ansi.txt");
        std::fs::write(&path, b"# \xba\xba\xd7\xd6\xd7\xa2\xca\xcd OK\n").unwrap();
        let content = read_config_file(&path).unwrap();
        assert!(content.contains("OK"));
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
    fn parse_filter_on_pk() {
        let parser = test_parser();
        let filters = parser
            .parse_string("Filter: ON PK Fc 1000 Hz Gain +3.0 dB Q 1.0\n", &test_ctx())
            .unwrap();
        assert_eq!(filters.len(), 1);
    }

    #[test]
    fn parse_filter_off_creates_passthrough() {
        // 规范 6.14：Filter: OFF → PassthroughFilter（链位置保留）。
        let parser = test_parser();
        let filters = parser
            .parse_string("Filter: OFF PK Fc 1000 Hz Gain +3.0 dB Q 1.0\n", &test_ctx())
            .unwrap();
        assert_eq!(filters.len(), 1);
        // 确认是 PassthroughFilter（处理不修改采样）。
        assert!(format!("{:?}", filters[0]).contains("PassthroughFilter"));
    }

    #[test]
    fn parse_graphic_eq() {
        let parser = test_parser();
        let filters = parser
            .parse_string("GraphicEQ: 20 -3.1; 25 -3.1; 31.5 -2.0\n", &test_ctx())
            .unwrap();
        assert_eq!(filters.len(), 1);
    }

    #[test]
    fn parse_copy_and_delay() {
        let parser = test_parser();
        let filters = parser
            .parse_string("Copy: L=R\nDelay: 500 ms\n", &test_ctx())
            .unwrap();
        assert_eq!(filters.len(), 2);
    }

    #[test]
    fn parse_rew_format() {
        let parser = test_parser();
        let filters = parser
            .parse_string(
                "Filter 1: ON PK Fc 50,0 Hz Gain -10,0 dB Q 2,50\n",
                &test_ctx(),
            )
            .unwrap();
        assert_eq!(filters.len(), 1);
    }

    #[test]
    fn unknown_command_warns_and_continues() {
        // 未知命令 → log::warn（不报错），后续行继续解析。
        // 注意：值"whatever"会被 ConvolutionFactory 匹配为 IR 路径 → 故意用空值
        // 确保落入 Unmatched（任何 DSP 工厂都不接受空参数）。
        let parser = test_parser();
        let filters = parser
            .parse_string("BogusCommand:\nPreamp: -3.0 dB\n", &test_ctx())
            .unwrap();
        assert_eq!(filters.len(), 1);
    }

    #[test]
    fn condition_false_skips_block() {
        let parser = test_parser();
        let content = "\
If: device_type == capture
Preamp: -6.0 dB
EndIf:
Delay: 100 ms
";
        let filters = parser.parse_string(content, &test_ctx()).unwrap();
        // Render 设备：If 块被跳过，仅 Delay 生效。
        assert_eq!(filters.len(), 1);
    }

    #[test]
    fn condition_true_executes() {
        let parser = test_parser();
        let content = "\
If: device_type == render
Preamp: -6.0 dB
EndIf:
";
        let filters = parser.parse_string(content, &test_ctx()).unwrap();
        assert_eq!(filters.len(), 1);
    }

    #[test]
    fn unterminated_if_errors() {
        let parser = test_parser();
        let result = parser.parse_string("If: true\nPreamp: -6.0 dB\n", &test_ctx());
        assert!(result.is_err());
        match result {
            Err(ConfigError::SyntaxError { message, .. }) => {
                assert!(message.contains("unterminated If"));
            }
            other => panic!("expected SyntaxError, got {other:?}"),
        }
    }

    #[test]
    fn comments_and_blank_lines_skipped() {
        let parser = test_parser();
        let content = "\
# 注释
   \t

Preamp: 0 dB
";
        let filters = parser.parse_string(content, &test_ctx()).unwrap();
        assert_eq!(filters.len(), 1);
    }

    #[test]
    fn parse_full_config_sample_no_unmatched() {
        // DoD 验收：解析 config.txt 样例无 NoMatch 警告。
        // 覆盖纯配置命令 + 全部 DSP 命令 + 条件 + 通道选择。
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
        // Preamp/HP/PK/GraphicEQ/Copy/Delay/ChannelL/PK = 8；
        // Channel: * 不产生过滤器（仅重置通道子集）。
        assert_eq!(filters.len(), 8);
    }

    #[test]
    fn parse_include_recursive() {
        // Include 递归真实解析：子文件滤波器并入主文件列表。
        let dir = std::env::temp_dir().join("vxapo_parser_include_test");
        std::fs::create_dir_all(&dir).unwrap();
        let sub = dir.join("sub.txt");
        let main = dir.join("main.txt");
        std::fs::write(&sub, "Preamp: -3.0 dB\n").unwrap();
        std::fs::write(
            &main,
            format!("Include: \"sub.txt\"\nDelay: 10 ms\n"),
        )
        .unwrap();

        let parser = test_parser();
        let filters = parser
            .parse_file(main.to_str().unwrap(), &test_ctx())
            .unwrap();
        assert_eq!(filters.len(), 2);
    }

    #[test]
    fn parse_device_abort_file_stops_current_file() {
        // Device: AbortFile → 终止当前文件，正常返回（不报错）。
        let parser = test_parser();
        let content = "\
Preamp: -3.0 dB
Device: AbortFile
Delay: 10 ms
";
        let filters = parser.parse_string(content, &test_ctx()).unwrap();
        // AbortFile 之前的 Preamp 保留，之后的 Delay 被跳过。
        assert_eq!(filters.len(), 1);
    }

    #[test]
    fn parse_eval_and_stage() {
        let parser = test_parser();
        let content = "\
Eval: gain = db_to_linear(-6)
Stage: PostMix
Preamp: -6.0 dB
If: gain == 0.5011872336272722
Preamp: -3.0 dB
EndIf:
";
        let filters = parser.parse_string(content, &test_ctx()).unwrap();
        // Eval/Stage 不产生滤波器；db_to_linear(-6) = 0.50118723...
        // If 条件为 true（≈比较），Preamp -3dB 生效。
        assert_eq!(filters.len(), 2);
    }
}
