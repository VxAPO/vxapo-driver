//! engine/context.rs — EngineContext 构建与辅助（Note 44）
//!
//! `EngineContext` 结构体定义在 `engine/filter.rs` 中，以保持 `dsp/` 模块的独立可测试性（Note 53）。
//! 本模块提供构建器、从 WAVEFORMATEX 构造及通道名生成等辅助逻辑。
//!
//! 所有引擎子模块通过 `&EngineContext` 获取音频参数（采样率、通道数、掩码、
//! 最大帧数、设备类型、阶段）。构建时机为 `engine.initialize()`，解析时由
//! `parser.rs` 读取，filter 初始化时传入。
//!
//! 此模块不含实时路径代码，构建阶段允许堆分配。

use crate::engine::channel;
use crate::engine::filter::{DeviceType, EngineContext, ProcessingStage};

// ══════════════════════════════════════════════════════════════════════════════
// EngineContextBuilder — 构建器
// ══════════════════════════════════════════════════════════════════════════════

/// `EngineContext` 的构建器。
///
/// 必填字段通过 `new()` 传入，可选字段通过 setter 链式调用。
///
/// ```ignore
/// let ctx = EngineContextBuilder::new(48000, 2, 0x3, 480)
///     .bits_per_sample(32)
///     .device_type(DeviceType::Render)
///     .stage(ProcessingStage::PreMix)
///     .build();
/// ```
#[derive(Debug, Clone)]
pub struct EngineContextBuilder {
    sample_rate: u32,
    channel_count: u32,
    channel_mask: u32,
    max_frame_count: u32,
    bits_per_sample: u32,
    device_type: DeviceType,
    stage: ProcessingStage,
}

impl EngineContextBuilder {
    /// 创建构建器（必填字段）。
    pub fn new(
        sample_rate: u32,
        channel_count: u32,
        channel_mask: u32,
        max_frame_count: u32,
    ) -> Self {
        Self {
            sample_rate,
            channel_count,
            channel_mask,
            max_frame_count,
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

    /// 构建 `EngineContext`。
    ///
    /// 验证：
    /// - `sample_rate` > 0
    /// - `channel_count` > 0 且 <= 256
    /// - `max_frame_count` > 0
    /// - `bits_per_sample` 为 8/16/24/32 之一
    ///
    /// # Panics
    ///
    /// 参数不合法时 panic（在 `initialize` 阶段，不在实时路径）。
    pub fn build(self) -> EngineContext {
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

        // 如果 channel_mask 为 0，生成默认掩码
        let channel_mask = if self.channel_mask == 0 {
            channel::default_channel_mask(self.channel_count)
        } else {
            self.channel_mask
        };

        EngineContext {
            sample_rate: self.sample_rate,
            channel_count: self.channel_count,
            channel_mask,
            max_frame_count: self.max_frame_count,
            bits_per_sample: self.bits_per_sample,
            device_type: self.device_type,
            stage: self.stage,
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// EngineContext 扩展方法
// ══════════════════════════════════════════════════════════════════════════════

impl EngineContext {
    /// 使用构建器创建。
    pub fn builder(
        sample_rate: u32,
        channel_count: u32,
        channel_mask: u32,
        max_frame_count: u32,
    ) -> EngineContextBuilder {
        EngineContextBuilder::new(sample_rate, channel_count, channel_mask, max_frame_count)
    }

    /// 获取当前通道名列表。
    ///
    /// 基于 `channel_count` 和 `channel_mask` 生成（L/R/C/LFE/SL/SR 等）。
    pub fn channel_names(&self) -> Vec<String> {
        channel::get_channel_names(self.channel_count, self.channel_mask)
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
            bits_per_sample: self.bits_per_sample,
            device_type: self.device_type,
            stage,
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 常用预设
// ══════════════════════════════════════════════════════════════════════════════

impl EngineContext {
    /// 立体声 48kHz 回放（最常见配置）。
    pub fn stereo_48k() -> Self {
        Self::builder(48000, 2, 0x3, 480)
            .bits_per_sample(32)
            .device_type(DeviceType::Render)
            .build()
    }

    /// 立体声 44.1kHz 回放（CD 品质）。
    pub fn stereo_441k() -> Self {
        Self::builder(44100, 2, 0x3, 441)
            .bits_per_sample(16)
            .device_type(DeviceType::Render)
            .build()
    }

    /// 7.1 环绕声 48kHz 回放。
    pub fn surround_71_48k() -> Self {
        Self::builder(48000, 8, 0x3F, 480)
            .bits_per_sample(32)
            .device_type(DeviceType::Render)
            .build()
    }

    /// 单声道采集（麦克风）。
    pub fn mono_capture_48k() -> Self {
        Self::builder(48000, 1, 0x1, 480)
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

    // ── Builder 基础 ────────────────────────────────────────────────────────

    #[test]
    fn builder_defaults() {
        let ctx = EngineContextBuilder::new(48000, 2, 0x3, 480).build();
        assert_eq!(ctx.sample_rate, 48000);
        assert_eq!(ctx.channel_count, 2);
        assert_eq!(ctx.channel_mask, 0x3);
        assert_eq!(ctx.max_frame_count, 480);
        assert_eq!(ctx.bits_per_sample, 32); // 默认
        assert_eq!(ctx.device_type, DeviceType::Render); // 默认
        assert_eq!(ctx.stage, ProcessingStage::None); // 默认
    }

    #[test]
    fn builder_with_setters() {
        let ctx = EngineContext::builder(96000, 6, 0x3F, 960)
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
    fn builder_zero_mask_uses_default() {
        // channel_mask = 0 → 自动生成默认掩码
        let ctx = EngineContextBuilder::new(48000, 2, 0, 480).build();
        assert_ne!(ctx.channel_mask, 0);
        assert_eq!(ctx.channel_mask, channel::default_channel_mask(2));
    }

    // ── Builder Panics ──────────────────────────────────────────────────────

    #[test]
    #[should_panic(expected = "sample_rate must be > 0")]
    fn builder_panics_zero_sample_rate() {
        EngineContextBuilder::new(0, 2, 0x3, 480).build();
    }

    #[test]
    #[should_panic(expected = "channel_count must be 1..=256")]
    fn builder_panics_zero_channels() {
        EngineContextBuilder::new(48000, 0, 0x3, 480).build();
    }

    #[test]
    #[should_panic(expected = "channel_count must be 1..=256")]
    fn builder_panics_too_many_channels() {
        EngineContextBuilder::new(48000, 257, 0x3, 480).build();
    }

    #[test]
    #[should_panic(expected = "max_frame_count must be > 0")]
    fn builder_panics_zero_frame_count() {
        EngineContextBuilder::new(48000, 2, 0x3, 0).build();
    }

    #[test]
    #[should_panic(expected = "bits_per_sample must be 8/16/24/32")]
    fn builder_panics_invalid_bits() {
        EngineContextBuilder::new(48000, 2, 0x3, 480)
            .bits_per_sample(20)
            .build();
    }

    // ── 扩展方法 ────────────────────────────────────────────────────────────

    #[test]
    fn channel_names_stereo() {
        let ctx = EngineContext::stereo_48k();
        let names = ctx.channel_names();
        assert_eq!(names.len(), 2);
        assert_eq!(names[0], "L");
        assert_eq!(names[1], "R");
    }

    #[test]
    fn channel_names_71() {
        let ctx = EngineContext::surround_71_48k();
        let names = ctx.channel_names();
        assert_eq!(names.len(), 8);
        assert_eq!(names[0], "L");
        assert_eq!(names[1], "R");
        assert_eq!(names[2], "C");
        assert_eq!(names[3], "LFE");
    }

    #[test]
    fn bytes_per_sample() {
        assert_eq!(EngineContext::stereo_48k().bytes_per_sample(), 4); // 32bit
        assert_eq!(EngineContext::stereo_441k().bytes_per_sample(), 2); // 16bit
    }

    #[test]
    fn bytes_per_frame() {
        let ctx = EngineContext::stereo_48k();
        assert_eq!(ctx.bytes_per_frame(), 8); // 4 bytes × 2 channels
    }

    #[test]
    fn is_render_capture() {
        assert!(EngineContext::stereo_48k().is_render());
        assert!(!EngineContext::stereo_48k().is_capture());
        assert!(EngineContext::mono_capture_48k().is_capture());
        assert!(!EngineContext::mono_capture_48k().is_render());
    }

    // ── stage_matches ───────────────────────────────────────────────────────

    #[test]
    fn stage_none_matches_everything() {
        let ctx = EngineContext::stereo_48k(); // stage = None
        assert!(ctx.stage_matches(ProcessingStage::PreMix));
        assert!(ctx.stage_matches(ProcessingStage::PostMix));
        assert!(ctx.stage_matches(ProcessingStage::None));
    }

    #[test]
    fn stage_premix_only_matches_premix() {
        let ctx = EngineContext::builder(48000, 2, 0x3, 480)
            .stage(ProcessingStage::PreMix)
            .build();
        assert!(ctx.stage_matches(ProcessingStage::PreMix));
        assert!(!ctx.stage_matches(ProcessingStage::PostMix));
        assert!(!ctx.stage_matches(ProcessingStage::None));
    }

    #[test]
    fn stage_postmix_only_matches_postmix() {
        let ctx = EngineContext::builder(48000, 2, 0x3, 480)
            .stage(ProcessingStage::PostMix)
            .build();
        assert!(!ctx.stage_matches(ProcessingStage::PreMix));
        assert!(ctx.stage_matches(ProcessingStage::PostMix));
    }

    // ── with_stage ──────────────────────────────────────────────────────────

    #[test]
    fn with_stage_preserves_fields() {
        let original = EngineContext::stereo_48k();
        let modified = original.with_stage(ProcessingStage::PostMix);

        assert_eq!(modified.sample_rate, original.sample_rate);
        assert_eq!(modified.channel_count, original.channel_count);
        assert_eq!(modified.channel_mask, original.channel_mask);
        assert_eq!(modified.max_frame_count, original.max_frame_count);
        assert_eq!(modified.bits_per_sample, original.bits_per_sample);
        assert_eq!(modified.device_type, original.device_type);
        assert_eq!(modified.stage, ProcessingStage::PostMix);
        assert_ne!(modified.stage, original.stage);
    }

    // ── 预设 ────────────────────────────────────────────────────────────────

    #[test]
    fn stereo_48k_preset() {
        let ctx = EngineContext::stereo_48k();
        assert_eq!(ctx.sample_rate, 48000);
        assert_eq!(ctx.channel_count, 2);
        assert_eq!(ctx.channel_mask, 0x3);
        assert_eq!(ctx.bits_per_sample, 32);
    }

    #[test]
    fn stereo_441k_preset() {
        let ctx = EngineContext::stereo_441k();
        assert_eq!(ctx.sample_rate, 44100);
        assert_eq!(ctx.channel_count, 2);
        assert_eq!(ctx.bits_per_sample, 16);
    }

    #[test]
    fn surround_71_preset() {
        let ctx = EngineContext::surround_71_48k();
        assert_eq!(ctx.channel_count, 8);
        assert_eq!(ctx.channel_mask, 0x3F);
    }

    #[test]
    fn mono_capture_preset() {
        let ctx = EngineContext::mono_capture_48k();
        assert_eq!(ctx.channel_count, 1);
        assert!(ctx.is_capture());
        assert_eq!(ctx.channel_mask, 0x1);
    }

    // ── Clone / Debug ───────────────────────────────────────────────────────

    #[test]
    fn context_clone() {
        let ctx = EngineContext::stereo_48k();
        let cloned = ctx.clone();
        assert_eq!(ctx.sample_rate, cloned.sample_rate);
        assert_eq!(ctx.channel_count, cloned.channel_count);
    }

    #[test]
    fn context_debug() {
        let ctx = EngineContext::stereo_48k();
        let debug = format!("{ctx:?}");
        assert!(debug.contains("sample_rate"));
        assert!(debug.contains("48000"));
    }

    #[test]
    fn builder_debug() {
        let builder = EngineContext::builder(48000, 2, 0x3, 480);
        let debug = format!("{builder:?}");
        assert!(debug.contains("EngineContextBuilder"));
    }

    // ── 边界条件 ────────────────────────────────────────────────────────────

    #[test]
    fn builder_max_channel_count() {
        let ctx = EngineContextBuilder::new(48000, 256, 0xFFFF_FFFF, 480).build();
        assert_eq!(ctx.channel_count, 256);
    }

    #[test]
    fn builder_min_channel_count() {
        let ctx = EngineContextBuilder::new(48000, 1, 0x1, 480).build();
        assert_eq!(ctx.channel_count, 1);
    }

    #[test]
    fn builder_all_valid_bit_depths() {
        for bits in [8, 16, 24, 32] {
            let ctx = EngineContextBuilder::new(48000, 2, 0x3, 480)
                .bits_per_sample(bits)
                .build();
            assert_eq!(ctx.bits_per_sample, bits);
            assert_eq!(ctx.bytes_per_sample(), bits / 8);
        }
    }
}