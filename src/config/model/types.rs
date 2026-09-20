//! config/model/types.rs — TOML 文件模型（结构体与默认值）

//! 共享导入见父模块 config/model.rs。

use super::*;

/// 顶层 TOML 文件模型。
#[derive(Debug, Clone, Deserialize)]
pub struct FileModel {
    #[serde(default = "default_version")]
    pub version: u32,
    /// 总开关：`false` = 整链 passthrough，但文件内容保留、不参与校验。
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub meta: Option<Meta>,
    #[serde(default)]
    pub effects: Vec<FileEffect>,
}

pub(super) fn default_version() -> u32 {
    1
}

/// APP 元数据（driver 忽略）。
#[derive(Debug, Clone, Deserialize)]
pub struct Meta {
    #[serde(default)]
    pub app: Option<String>,
    #[serde(default)]
    pub schema: Option<u32>,
}

/// 单个效果器文件表示（参数平铺；`name`/`group` 仅供 APP）。
#[derive(Debug, Clone, Deserialize)]
pub struct FileEffect {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub channels: Option<Vec<String>>,

    // —— 各效果器参数（按 type 取用；不适用字段视为错误）——
    #[serde(default)]
    pub gain_db: Option<f32>,
    #[serde(default)]
    pub crossover_hz: Option<f32>,
    #[serde(default)]
    pub depth: Option<f32>,
    #[serde(default)]
    pub air: Option<f32>,
    #[serde(default)]
    pub air_side: Option<f32>,
    #[serde(default)]
    pub mix: Option<f32>,
    #[serde(default)]
    pub gain: Option<f32>,
    #[serde(default)]
    pub bands: Option<Vec<FilePeqBand>>,
    #[serde(default)]
    pub tune_hz: Option<f32>,
    #[serde(default)]
    pub drive: Option<f32>,
    #[serde(default)]
    pub odd: Option<f32>,
    #[serde(default)]
    pub even: Option<f32>,
    #[serde(default)]
    pub wet: Option<f32>,
    #[serde(default)]
    pub dry: Option<f32>,
    #[serde(default)]
    pub room_size: Option<f32>,
    #[serde(default)]
    pub decay: Option<f32>,
    #[serde(default)]
    pub damping: Option<f32>,
    #[serde(default)]
    pub bandwidth: Option<f32>,
    #[serde(default)]
    pub density: Option<f32>,
    #[serde(default)]
    pub lat5: Option<f32>,
    #[serde(default)]
    pub lat6: Option<f32>,
    #[serde(default)]
    pub pre_delay_ms: Option<f32>,
    #[serde(default)]
    pub motion_rate: Option<f32>,
    #[serde(default)]
    // 旧配置写的是 motion_depth_ms；该参数实际是归一化值（0..2，2=论文满调制），
    // 改名 motion_depth 并兼容旧键。
    #[serde(alias = "motion_depth_ms")]
    pub motion_depth: Option<f32>,
    #[serde(default)]
    pub low_cut_hz: Option<f32>,
    #[serde(default)]
    pub gain_boost_db: Option<f32>,
    #[serde(default)]
    pub max_output_db: Option<f32>,
    #[serde(default)]
    pub release_ms: Option<f32>,
    #[serde(default)]
    pub target: Option<f32>,
    #[serde(default)]
    pub lookahead_ms: Option<f32>,
    #[serde(default)]
    pub dither: Option<String>,
    #[serde(default)]
    pub target_rms_db: Option<f32>,
    #[serde(default)]
    pub response_s: Option<f32>,
    #[serde(default)]
    pub max_gain_db: Option<f32>,
    #[serde(default)]
    pub dynamic_preserve: Option<f32>,
    #[serde(default)]
    pub noise_gate_db: Option<f32>,
    #[serde(default)]
    pub peak_limit_db: Option<f32>,
    #[serde(default)]
    pub threshold_db: Option<f32>,
    #[serde(default)]
    pub ratio: Option<f32>,
    #[serde(default)]
    pub knee_db: Option<f32>,
    #[serde(default)]
    pub attack_ms: Option<f32>,
    #[serde(default)]
    pub makeup_gain_db: Option<f32>,
    #[serde(default)]
    pub intensity: Option<f32>,
    #[serde(default)]
    pub phon: Option<f32>,
    #[serde(default)]
    pub reference_phon: Option<f32>,

    /// 未声明键（严格校验用）。
    #[serde(flatten)]
    pub extra: HashMap<String, toml::Value>,
}

pub(super) fn default_true() -> bool {
    true
}

/// PEQ 段（TOML `[[effects.bands]]`）。
#[derive(Debug, Clone, Deserialize)]
pub struct FilePeqBand {
    pub fc: f32,
    pub gain_db: f32,
    pub q: f32,
    /// 段类型：`peaking`（默认）/ `low_shelf` / `high_shelf` / `low_pass` / `high_pass`。
    #[serde(rename = "type", default)]
    pub band_type: Option<String>,
}

