//! config/parser.rs — TOML 配置解析
//!
//! 流程：读取 `config.toml` → `toml::from_str::<FileModel>`（文件格式模型）→
//! `FileModel::into_chain_model`（校验，丢弃 APP 元数据）→
//! `factory::create_from_model` 构造 Filter 链 + spec 指纹。
//! 依赖方向：config → pipeline/dsp（model / factory / filter）。

use std::path::Path;

use crate::config::error::ConfigError;
use crate::config::model::FileModel;
use crate::pipeline::dsp::factory::create_from_model;
use crate::pipeline::dsp::filter::{ChannelScopedFilter, DspContext, Filter};
use crate::pipeline::dsp::model::ChainModel;

/// 配置指纹（一次完整解析产出的 filter spec 有序序列，热重载比较用）。
pub type FilterSpec = String;
pub type SpecChain = Vec<FilterSpec>;

/// 配置文件大小闸门（控制线程 IO 安全上限）。
pub const MAX_CONFIG_FILE_SIZE: u64 = 128 * 1024;

/// TOML 配置解析器（无状态，模型校验在 config/model.rs）。
pub struct ConfigParser;

impl ConfigParser {
    pub fn new() -> Self {
        Self
    }

    /// 解析配置文件（丢弃 spec）。
    pub fn parse_file(
        &self,
        path: &str,
        ctx: &DspContext,
    ) -> Result<Vec<Box<dyn Filter>>, ConfigError> {
        let (filters, _) = self.parse_file_with_spec(path, ctx)?;
        Ok(filters)
    }

    /// 解析配置文件，同时产出配置指纹（语义保留，指纹改模型 spec）。
    pub fn parse_file_with_spec(
        &self,
        path: &str,
        ctx: &DspContext,
    ) -> Result<(Vec<Box<dyn Filter>>, SpecChain), ConfigError> {
        let path_ref = Path::new(path);
        // 防御：配置文件缺失 = 无配置 passthrough（空链）。
        // 若按解析失败处理，新流 LockForProcess 会失败 → APO 不生效 → 无声
        // （实证：config.toml 被删后热重载保留旧链、新流直接无声）。
        if !path_ref.exists() {
            return Ok((Vec::new(), SpecChain::new()));
        }
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
        self.parse_content_with_spec(&content, ctx, path_ref)
    }

    /// 解析配置字符串（丢弃 spec）。
    pub fn parse_string(
        &self,
        content: &str,
        ctx: &DspContext,
    ) -> Result<Vec<Box<dyn Filter>>, ConfigError> {
        let (filters, _) = self.parse_content_with_spec(content, ctx, Path::new("<string>"))?;
        Ok(filters)
    }

    /// 解析行列表（兼容入口：按换行拼接后走 TOML 解析）。
    pub fn parse_lines(
        &self,
        lines: &[String],
        ctx: &DspContext,
    ) -> Result<Vec<Box<dyn Filter>>, ConfigError> {
        let content = lines.join("\n");
        self.parse_string(&content, ctx)
    }

    fn parse_content_with_spec(
        &self,
        content: &str,
        ctx: &DspContext,
        path: &Path,
    ) -> Result<(Vec<Box<dyn Filter>>, SpecChain), ConfigError> {
        let file_model: FileModel =
            toml::from_str(content).map_err(|e| ConfigError::TomlError {
                file: path.display().to_string(),
                message: e.to_string(),
            })?;
        let chain = file_model.into_chain_model(&path.display().to_string())?;
        build_chain(&chain, ctx, path)
    }
}

impl Default for ConfigParser {
    fn default() -> Self {
        Self::new()
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

/// ChainModel → (Filter 链, SpecChain)。校验声道存在性，per-effect channels
/// 包 `ChannelScopedFilter`。
fn build_chain(
    model: &ChainModel,
    ctx: &DspContext,
    path: &Path,
) -> Result<(Vec<Box<dyn Filter>>, SpecChain), ConfigError> {
    let mut filters = Vec::with_capacity(model.effects.len());
    let mut specs = Vec::with_capacity(model.effects.len());
    for effect in &model.effects {
        if let Some(names) = &effect.channels {
            for name in names {
                if !ctx.channel_names.iter().any(|c| c == name) {
                    return Err(ConfigError::ModelError {
                        file: path.display().to_string(),
                        message: format!("channel '{name}' not present in device channels"),
                    });
                }
            }
        }
        let indices: Option<Vec<usize>> = effect.channels.as_ref().map(|names| {
            names
                .iter()
                .filter_map(|name| ctx.channel_names.iter().position(|c| c == name))
                .collect()
        });

        let filter = create_from_model(effect, ctx);
        let filter: Box<dyn Filter> = match indices {
            Some(idx) => Box::new(ChannelScopedFilter::new(filter, idx)),
            None => filter,
        };
        specs.push(effect.spec());
        filters.push(filter);
    }
    Ok((filters, specs))
}

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

    fn peq_toml(bands: usize) -> String {
        let mut s = String::from("[[effects]]\ntype = \"peq\"\n");
        for i in 0..bands {
            s.push_str(&format!(
                "[[effects.bands]]\nfc = {}\ngain_db = 0.0\nq = 1.0\n",
                100.0 * (i as f32 + 1.0)
            ));
        }
        s
    }

    #[test]
    fn parses_toml_and_produces_specs() {
        let toml = r#"
[[effects]]
type = "preamp"
gain_db = -3.0

[[effects]]
type = "wide"
intensity = 0.5
"#;
        let parser = ConfigParser::new();
        let (filters, specs) = parser
            .parse_content_with_spec(toml, &test_ctx(), Path::new("test.toml"))
            .unwrap();
        assert_eq!(filters.len(), 2);
        assert_eq!(specs.len(), 2);
        assert!(specs[0].starts_with("preamp:true"));
        assert!(specs[1].starts_with("wide:true"));
    }

    #[test]
    fn spec_changes_with_params() {
        let parser = ConfigParser::new();
        let (_, s1) = parser
            .parse_content_with_spec(&peq_toml(6), &test_ctx(), Path::new("t"))
            .unwrap();
        let (_, s2) = parser
            .parse_content_with_spec(&peq_toml(7), &test_ctx(), Path::new("t"))
            .unwrap();
        assert_ne!(s1, s2);
        // 段顺序变化 → 指纹变化（band 参数参与）。
        let mut t = peq_toml(6);
        t.push_str("[[effects.bands]]\nfc = 100\ngain_db = 1.0\nq = 1.0\n"); // 第 7 段不同参数
        let (_, s3) = parser
            .parse_content_with_spec(&t, &test_ctx(), Path::new("t"))
            .unwrap();
        assert_ne!(s2, s3);
    }

    #[test]
    fn empty_chain_is_passthrough() {
        let parser = ConfigParser::new();
        let (filters, specs) = parser
            .parse_content_with_spec("", &test_ctx(), Path::new("t"))
            .unwrap();
        assert!(filters.is_empty());
        assert!(specs.is_empty());
    }

    #[test]
    fn disabled_file_is_passthrough_chain() {
        let toml = "version = 1\nenabled = false\n[[effects]]\ntype = \"wide\"\nintensity = 0.5\n";
        let parser = ConfigParser::new();
        let (filters, specs) = parser
            .parse_content_with_spec(toml, &test_ctx(), Path::new("t"))
            .unwrap();
        assert!(filters.is_empty(), "总开关关闭后链为空（passthrough）");
        assert!(specs.is_empty());
    }

    #[test]
    fn unknown_channel_rejected() {
        let toml = "[[effects]]\ntype = \"wide\"\nchannels = [\"XX\"]\nintensity = 0.5\n";
        let parser = ConfigParser::new();
        let err = parser
            .parse_content_with_spec(toml, &test_ctx(), Path::new("t"))
            .unwrap_err();
        assert!(err.to_string().contains("not present in device channels"));
    }

    #[test]
    fn channel_scoped_effect_built() {
        let toml = "[[effects]]\ntype = \"wide\"\nchannels = [\"L\"]\nintensity = 0.5\n";
        let parser = ConfigParser::new();
        let (filters, _) = parser
            .parse_content_with_spec(toml, &test_ctx(), Path::new("t"))
            .unwrap();
        assert_eq!(filters.len(), 1, "scoped filter constructed");
    }

    #[test]
    fn invalid_toml_rejected() {
        let parser = ConfigParser::new();
        let err = parser
            .parse_content_with_spec("[[effects]\ntype=\"peq\"\n", &test_ctx(), Path::new("t"))
            .unwrap_err();
        assert!(err.to_string().contains("TOML error"));
    }

    #[test]
    fn disabled_effect_is_passthrough_in_chain() {
        let toml = "[[effects]]\ntype = \"wide\"\nenabled = false\nintensity = 0.5\n";
        let parser = ConfigParser::new();
        let (mut filters, specs) = parser
            .parse_content_with_spec(toml, &test_ctx(), Path::new("t"))
            .unwrap();
        assert_eq!(filters.len(), 1);
        assert_eq!(specs[0], "wide:false|intensity=0.500000");
        let mut samples = vec![vec![0.3f32; 8], vec![0.2f32; 8]];
        let before = samples.clone();
        filters[0].process(&mut samples, 8);
        assert_eq!(samples, before);
    }

    #[test]
    fn parse_lines_compat() {
        let parser = ConfigParser::new();
        // 行列表入口仍可用（内部按换行拼接为 TOML）。
        let lines = vec![
            "[[effects]]".to_string(),
            "type = \"preamp\"".to_string(),
            "gain_db = -1.0".to_string(),
        ];
        let filters = parser.parse_lines(&lines, &test_ctx()).unwrap();
        assert_eq!(filters.len(), 1);
    }
}
