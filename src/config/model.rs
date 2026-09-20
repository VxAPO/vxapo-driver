//! config/model.rs — 文件格式模型（FileModel）
//!
//! TOML 反序列化目标：`version` / `[meta]` / `[[effects]]`（含 APP 元数据
//! `name` / `group`）。`into_chain_model()` 丢弃 APP 元数据并完成范围/段数/
//! 声道名校验，转换为 dsp 层 `ChainModel`——**转换是 config 层职责**，
//! 依赖方向保持 `config → pipeline/dsp`。

use std::collections::HashMap;

use serde::Deserialize;

use crate::config::error::ConfigError;
use crate::pipeline::dsp::aural::AuralParams;
use crate::pipeline::dsp::compressor::CompressorParams;
use crate::pipeline::dsp::model::{
    ChainModel, EffectConfig, EffectParams, EffectType, LoudnessParams, PeqBand, PeqBandType,
    PeqParams, PreampParams, MAX_PEQ_BANDS, MIN_PEQ_BANDS,
};
use crate::pipeline::dsp::reverb::ReverbParams;
use crate::pipeline::dsp::wide::WideParams;


// ── 子模块（类型定义 / 校验转换）───────────────────────────────────────

mod convert;
mod types;

pub use types::{FileEffect, FileModel, FilePeqBand, Meta};

#[cfg(test)]
mod tests;
