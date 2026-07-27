//! dsp/context.rs — DspContext 构建与辅助（Note 44）
//!
//! `DspContext` 结构体定义在 `dsp/filter.rs` 中，以保持 `dsp/` 模块的独立可测试性（Note 53）。
//! 本模块提供构建器及辅助逻辑。
//!
//! 所有引擎子模块通过 `&DspContext` 获取音频参数（采样率、通道数、掩码、
//! 最大帧数、设备类型、阶段、通道名列表）。构建时机为 `pipeline/stream/process.rs`
//! 的 `initialize()`，解析时由 `host/parse/parser.rs` 读取，filter 初始化时传入。
//!
//! **dsp/ 边界（Note 58）**：本模块不调用 `pipeline::stream::channel` 函数。
//! `channel_names` 字段由调用方（`pipeline/stream/process.rs`）预计算后传入。
//! `channel_mask` 为 0 时，调用方负责调用 `default_channel_mask()` 解析后传入。
//!
//! 此模块不含实时路径代码，构建阶段允许堆分配。

use crate::dsp::filter::{DeviceType, DspContext, ProcessingStage};

// ══════════════════════════════════════════════════════════════════════════════
// DspContextBuilder — 构建器
// ══════════════════════════════════════════════════════════════════════════════

/// `DspContext` 的构建器。
///
/// 必填字段通过 `new()` 传入，可选字段通过 setter 链式调用。
///
/// ```ignore
/// let ctx = DspContextBuilder::new(48000, 2, 0x3, 480, vec!["L".into(), "R".into()])
///     .bits_per_sample(32)
///     .device_type(DeviceType::Render)
///     .stage(ProcessingStage::PreMix)
///     .build();
/// ```
#[derive(Debug, Clone)]
pub struct DspContextBuilder {
    sample_rate: u32,
    channel_count: u32,
    channel_mask: u32,
    max_frame_count: u32,
    channel_names: Vec<String>,
    bits_per_sample: u32,
    device_type: DeviceType,
    stage: ProcessingStage,
}

impl DspContextBuilder {
    /// 创建构建器（必填字段）。
    ///
    /// `channel_names` 由调用方通过 `pipeline::stream::channel::get_channel_names()` 预计算。
    /// `channel_mask` 为 0 时，调用方需先调用 `default_channel_mask()` 解析后再传入。
    pub fn new(
        sample_rate: u32,
        channel_count: u32,
        channel_mask: u32,
        max_frame_count: u32,
        channel_names: Vec<String>,
    ) -> Self {
        Self {
            sample_rate,
            channel_count,
            channel_mask,
            max_frame_count,
            channel_names,
            bits_per_sample: 32,
            device_type: DeviceType::Render,
            stage: ProcessingStage::None,
        }
    }

    /// 设置每样本位数。
    pub fn bits_per_sample(mut self, v: u32) -> Self {
        self.bits_per_sample = v;
        self
    }

    /// 设置设备类型。
    pub fn device_type(mut self, v: DeviceType) -> Self {
        self.device_type = v;
        self
    }

    /// 设置处理阶段。
    pub fn stage(mut self, v: ProcessingStage) -> Self {
        self.stage = v;
        self
    }

    /// 构建 `DspContext`。
    ///
    /// 验证：
    /// - `sample_rate` > 0
    /// - `channel_count` > 0 且 <= 256
    /// - `max_frame_count` > 0
    /// - `bits_per_sample` 为 8/16/24/32 之一
    /// - `channel_names.len() == channel_count`
    ///
    /// # Panics
    ///
    /// 参数不合法时 panic（在 `initialize` 阶段，不在实时路径）。
    pub fn build(self) -> DspContext {
        assert!(self.sample_rate > 0, "sample_rate must be > 0");
        assert!(
            self.channel_count > 0 && self.channel_count <= 256,
            "channel_count must be 1..=256, got {}",
            self.channel_count
        );
        assert!(self.max_frame_count > 0, "max_frame_count must be > 0");
        assert!(
            matches!(self.bits_per_sample, 8 | 16 | 24 | 32),
            "bits_per_sample must be 8/16/24/32, got {}",
            self.bits_per_sample
        );
        assert!(
            self.channel_names.len() == self.channel_count as usize,
            "channel_names.len() ({}) must equal channel_count ({})",
            self.channel_names.len(),
            self.channel_count
        );

        DspContext {
            sample_rate: self.sample_rate,
            channel_count: self.channel_count,
            channel_mask: self.channel_mask,
            max_frame_count: self.max_frame_count,
            channel_names: self.channel_names,
            bits_per_sample: self.bits_per_sample,
            device_type: self.device_type,
            stage: self.stage,
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// DspContext 扩展方法
// ══════════════════════════════════════════════════════════════════════════════

impl DspContext {
    /// 使用构建器创建。
    pub fn builder(
        sample_rate: u32,
        channel_count: u32,
        channel_mask: u32,
        max_frame_count: u32,
        channel_names: Vec<String>,
    ) -> DspContextBuilder {
        DspContextBuilder::new(sample_rate, channel_count, channel_mask, max_frame_count, channel_names)
    }

    /// 获取通道名列表的引用。
    ///
    /// 名称在构造时由调用方通过 `pipeline::stream::channel::get_channel_names()` 预计算并传入。
    pub fn channel_names(&self) -> &[String] {
        &self.channel_names
    }

    /// 每样本字节数。
    pub fn bytes_per_sample(&self) -> u32 {
        self.bits_per_sample / 8
    }

    /// 每帧字节数（所有通道合计）。
    pub fn bytes_per_frame(&self) -> u32 {
        self.bytes_per_sample() * self.channel_count
    }

    /// 是否为回放设备。
    pub fn is_render(&self) -> bool {
        self.device_type == DeviceType::Render
    }

    /// 是否为采集设备。
    pub fn is_capture(&self) -> bool {
        self.device_type == DeviceType::Capture
    }

    /// 当前阶段是否匹配指定阶段。
    ///
    /// `ProcessingStage::None` 匹配一切。
    /// `ProcessingStage::PreMix` / `PostMix` 仅匹配自身。
    pub fn stage_matches(&self, required: ProcessingStage) -> bool {
        match self.stage {
            ProcessingStage::None => true,
            s => s == required,
        }
    }

    /// 克隆上下文并替换阶段。
    pub fn with_stage(&self, stage: ProcessingStage) -> Self {
        Self {
            sample_rate: self.sample_rate,
            channel_count: self.channel_count,
            channel_mask: self.channel_mask,
            max_frame_count: self.max_frame_count,
            channel_names: self.channel_names.clone(),
            bits_per_sample: self.bits_per_sample,
            device_type: self.device_type,
            stage,
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 常用预设
//
// 预设中硬编码通道名，不调用 pipeline::stream::channel 函数。
// 保持 dsp/ 边界纯净（Note 58）。
// ══════════════════════════════════════════════════════════════════════════════

impl DspContext {
    /// 立体声 48kHz 回放（最常见配置）。
    pub fn stereo_48k() -> Self {
        Self::builder(48000, 2, 0x3, 480, vec!["L".into(), "R".into()])
            .bits_per_sample(32)
            .device_type(DeviceType::Render)
            .build()
    }

    /// 立体声 44.1kHz 回放（CD 品质）。
    pub fn stereo_441k() -> Self {
        Self::builder(44100, 2, 0x3, 441, vec!["L".into(), "R".into()])
            .bits_per_sample(16)
            .device_type(DeviceType::Render)
            .build()
    }

    /// 7.1 环绕声 48kHz 回放。
    pub fn surround_71_48k() -> Self {
        Self::builder(
            48000, 8, 0x063F, 480,
            vec!["L".into(), "R".into(), "C".into(), "LFE".into(),
                 "SL".into(), "SR".into(), "RL".into(), "RR".into()],
        )
        .bits_per_sample(32)
        .device_type(DeviceType::Render)
        .build()
    }

    /// 单声道采集（麦克风）。
    pub fn mono_capture_48k() -> Self {
        Self::builder(48000, 1, 0x1, 480, vec!["M".into()])
            .bits_per_sample(16)
            .device_type(DeviceType::Capture)
            .build()
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── 辅助 ─────────────────────────────────────────────────────────────────

    fn stereo_names() -> Vec<String> {
        vec!["L".into(), "R".into()]
    }

    fn surround_71_names() -> Vec<String> {
        vec!["L".into(), "R".into(), "C".into(), "LFE".into(),
             "SL".into(), "SR".into(), "RL".into(), "RR".into()]
    }

    // ── Builder 基础 ────────────────────────────────────────────────────────

    #[test]
    fn builder_defaults() {
        let ctx = DspContextBuilder::new(48000, 2, 0x3, 480, stereo_names()).build();
        assert_eq!(ctx.sample_rate, 48000);
        assert_eq!(ctx.channel_count, 2);
        assert_eq!(ctx.channel_mask, 0x3);
        assert_eq!(ctx.max_frame_count, 480);
        assert_eq!(ctx.bits_per_sample, 32);
        assert_eq!(ctx.device_type, DeviceType::Render);
        assert_eq!(ctx.stage, ProcessingStage::None);
        assert_eq!(ctx.channel_names(), &["L", "R"]);
    }

    #[test]
    fn builder_with_setters() {
        let ctx = DspContext::builder(
            96000, 6, 0x3F, 960,
            vec!["L".into(), "R".into(), "C".into(), "LFE".into(), "SL".into(), "SR".into()],
        )
        .bits_per_sample(24)
        .device_type(DeviceType::Capture)
        .stage(ProcessingStage::PreMix)
        .build();
        assert_eq!(ctx.sample_rate, 96000);
        assert_eq!(ctx.channel_count, 6);
        assert_eq!(ctx.bits_per_sample, 24);
        assert_eq!(ctx.device_type, DeviceType::Capture);
        assert_eq!(ctx.stage, ProcessingStage::PreMix);
    }

    #[test]
    fn builder_mask_zero_transparent() {
        // mask 为 0 时直接透传，不再自动推导。
        // 调用方（pipeline/stream/process.rs）负责预计算。
        let ctx = DspContextBuilder::new(48000, 2, 0, 480, stereo_names()).build();
        assert_eq!(ctx.channel_mask, 0);
    }

    // ── Builder Panics ──────────────────────────────────────────────────────

    #[test]
    #[should_panic(expected = "sample_rate must be > 0")]
    fn builder_panics_zero_sample_rate() {
        DspContextBuilder::new(0, 2, 0x3, 480, stereo_names()).build();
    }

    #[test]
    #[should_panic(expected = "channel_count must be 1..=256")]
    fn builder_panics_zero_channels() {
        DspContextBuilder::new(48000, 0, 0x3, 480, vec![]).build();
    }

    #[test]
    #[should_panic(expected = "channel_count must be 1..=256")]
    fn builder_panics_too_many_channels() {
        DspContextBuilder::new(48000, 257, 0x3, 480, vec!["X".into(); 257]).build();
    }

    #[test]
    #[should_panic(expected = "max_frame_count must be > 0")]
    fn builder_panics_zero_frame_count() {
        DspContextBuilder::new(48000, 2, 0x3, 0, stereo_names()).build();
    }

    #[test]
    #[should_panic(expected = "bits_per_sample must be 8/16/24/32")]
    fn builder_panics_invalid_bits() {
        DspContextBuilder::new(48000, 2, 0x3, 480, stereo_names())
            .bits_per_sample(20)
            .build();
    }

    #[test]
    #[should_panic(expected = "channel_names.len()")]
    fn builder_panics_names_count_mismatch() {
        // 2 通道但只传 1 个名字
        DspContextBuilder::new(48000, 2, 0x3, 480, vec!["L".into()]).build();
    }

    // ── 扩展方法 ────────────────────────────────────────────────────────────

    #[test]
    fn channel_names_stereo() {
        let ctx = DspContext::stereo_48k();
        let names = ctx.channel_names();
        assert_eq!(names.len(), 2);
        assert_eq!(names[0], "L");
        assert_eq!(names[1], "R");
    }

    #[test]
    fn channel_names_71() {
        let ctx = DspContext::surround_71_48k();
        let names = ctx.channel_names();
        assert_eq!(names.len(), 8);
        assert_eq!(names[0], "L");
        assert_eq!(names[1], "R");
        assert_eq!(names[2], "C");
        assert_eq!(names[3], "LFE");
    }

    #[test]
    fn bytes_per_sample() {
        assert_eq!(DspContext::stereo_48k().bytes_per_sample(), 4); // 32bit
        assert_eq!(DspContext::stereo_441k().bytes_per_sample(), 2); // 16bit
    }

    #[test]
    fn bytes_per_frame() {
        let ctx = DspContext::stereo_48k();
        assert_eq!(ctx.bytes_per_frame(), 8); // 4 bytes × 2 channels
    }

    #[test]
    fn is_render_capture() {
        assert!(DspContext::stereo_48k().is_render());
        assert!(!DspContext::stereo_48k().is_capture());
        assert!(DspContext::mono_capture_48k().is_capture());
        assert!(!DspContext::mono_capture_48k().is_render());
    }

    // ── stage_matches ───────────────────────────────────────────────────────

    #[test]
    fn stage_none_matches_everything() {
        let ctx = DspContext::stereo_48k();
        assert!(ctx.stage_matches(ProcessingStage::PreMix));
        assert!(ctx.stage_matches(ProcessingStage::PostMix));
        assert!(ctx.stage_matches(ProcessingStage::None));
    }

    #[test]
    fn stage_premix_only_matches_premix() {
        let ctx = DspContext::builder(48000, 2, 0x3, 480, stereo_names())
            .stage(ProcessingStage::PreMix)
            .build();
        assert!(ctx.stage_matches(ProcessingStage::PreMix));
        assert!(!ctx.stage_matches(ProcessingStage::PostMix));
        assert!(!ctx.stage_matches(ProcessingStage::None));
    }

    #[test]
    fn stage_postmix_only_matches_postmix() {
        let ctx = DspContext::builder(48000, 2, 0x3, 480, stereo_names())
            .stage(ProcessingStage::PostMix)
            .build();
        assert!(!ctx.stage_matches(ProcessingStage::PreMix));
        assert!(ctx.stage_matches(ProcessingStage::PostMix));
    }

    // ── with_stage ──────────────────────────────────────────────────────────

    #[test]
    fn with_stage_preserves_fields() {
        let original = DspContext::stereo_48k();
        let modified = original.with_stage(ProcessingStage::PostMix);

        assert_eq!(modified.sample_rate, original.sample_rate);
        assert_eq!(modified.channel_count, original.channel_count);
        assert_eq!(modified.channel_mask, original.channel_mask);
        assert_eq!(modified.max_frame_count, original.max_frame_count);
        assert_eq!(modified.bits_per_sample, original.bits_per_sample);
        assert_eq!(modified.device_type, original.device_type);
        assert_eq!(modified.channel_names, original.channel_names);
        assert_eq!(modified.stage, ProcessingStage::PostMix);
        assert_ne!(modified.stage, original.stage);
    }

    // ── 预设 ────────────────────────────────────────────────────────────────

    #[test]
    fn stereo_48k_preset() {
        let ctx = DspContext::stereo_48k();
        assert_eq!(ctx.sample_rate, 48000);
        assert_eq!(ctx.channel_count, 2);
        assert_eq!(ctx.channel_mask, 0x3);
        assert_eq!(ctx.bits_per_sample, 32);
    }

    #[test]
    fn stereo_441k_preset() {
        let ctx = DspContext::stereo_441k();
        assert_eq!(ctx.sample_rate, 44100);
        assert_eq!(ctx.channel_count, 2);
        assert_eq!(ctx.bits_per_sample, 16);
    }

    #[test]
    fn surround_71_preset() {
        let ctx = DspContext::surround_71_48k();
        assert_eq!(ctx.channel_count, 8);
        assert_eq!(ctx.channel_mask, 0x063F);
    }

    #[test]
    fn mono_capture_preset() {
        let ctx = DspContext::mono_capture_48k();
        assert_eq!(ctx.channel_count, 1);
        assert!(ctx.is_capture());
        assert_eq!(ctx.channel_mask, 0x1);
    }

    // ── Clone / Debug ───────────────────────────────────────────────────────

    #[test]
    fn context_clone() {
        let ctx = DspContext::stereo_48k();
        let cloned = ctx.clone();
        assert_eq!(ctx.sample_rate, cloned.sample_rate);
        assert_eq!(ctx.channel_count, cloned.channel_count);
        assert_eq!(ctx.channel_names, cloned.channel_names);
    }

    #[test]
    fn context_debug() {
        let ctx = DspContext::stereo_48k();
        let debug = format!("{ctx:?}");
        assert!(debug.contains("sample_rate"));
        assert!(debug.contains("48000"));
    }

    #[test]
    fn builder_debug() {
        let builder = DspContext::builder(48000, 2, 0x3, 480, stereo_names());
        let debug = format!("{builder:?}");
        assert!(debug.contains("DspContextBuilder"));
    }

    // ── 边界条件 ────────────────────────────────────────────────────────────

    #[test]
    fn builder_max_channel_count() {
        let names: Vec<String> = (0..256).map(|i| format!("ch{i}")).collect();
        let ctx = DspContextBuilder::new(48000, 256, 0xFFFF_FFFF, 480, names).build();
        assert_eq!(ctx.channel_count, 256);
    }

    #[test]
    fn builder_min_channel_count() {
        let ctx = DspContextBuilder::new(48000, 1, 0x1, 480, vec!["M".into()]).build();
        assert_eq!(ctx.channel_count, 1);
    }

    #[test]
    fn builder_all_valid_bit_depths() {
        for bits in [8, 16, 24, 32] {
            let ctx = DspContextBuilder::new(48000, 2, 0x3, 480, stereo_names())
                .bits_per_sample(bits)
                .build();
            assert_eq!(ctx.bits_per_sample, bits);
            assert_eq!(ctx.bytes_per_sample(), bits / 8);
        }
    }
}