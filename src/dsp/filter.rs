//! engine/filter.rs — 过滤器抽象接口（Note 13/53/58）
//!
//! 对应 EqualizerAPO 的 `IFilter` 接口。
//! 纯 Rust trait 定义，不包含任何 Windows API 依赖。
//!
//! `dsp/` 模块仅依赖此文件，不依赖 `engine/` 的其他子模块。
//! 这使得 `dsp/` 的每个模块可使用标准 `#[test]` 编译测试，
//! 无需 Windows 环境（Note 53）。
//!
//! 此文件同时定义 `EngineContext` 结构体，供 `context.rs` 构建，
//! 避免 `dsp/` 通过 `engine/context.rs` 间接引入 Windows 依赖（Note 58）。
//!
//! `process` 方法运行在实时音频线程中，实现者必须遵守 RT-safety 约束（Note 12）：
//! 禁止堆分配、互斥锁、I/O、panic。

// ══════════════════════════════════════════════════════════════════════════════
// Filter trait
// ══════════════════════════════════════════════════════════════════════════════

/// 音频处理过滤器 trait。
///
/// 所有 DSP 滤波器（biquad、gain、delay、convolution 等）实现此 trait。
/// 由 `engine/chain.rs` 中的 `FilterInfo` 持有 trait object 指针。
///
/// # 实时安全
///
/// `process` 方法运行在多媒体实时线程上（Note 12）。
/// 禁止堆分配、互斥锁、I/O、panic。
/// `initialize` 和构造在非实时路径中运行，允许分配。
///
/// # 生命周期
///
/// 1. 工厂的 `create_filter` 构造实例 → 调用 `initialize`
/// 2. `initialize` 返回输出通道名列表（用于 `chain.rs` 的通道映射）
/// 3. `process` 在每帧音频处理时被调用（实时路径）
/// 4. Drop 时释放预分配的缓冲区
pub trait Filter: Send + std::fmt::Debug {
    /// 初始化过滤器，返回输出通道名列表。
    ///
    /// - `sample_rate`：采样率（Hz）
    /// - `channel_names`：输入通道名列表（如 `["L", "R", "C", "LFE"]`）
    ///
    /// 返回值：
    /// - `Some(Vec<String>)`：输出通道名列表（可能与输入不同，如 `Channel:` 命令）
    /// - `None`：输出通道不变，沿用输入通道名列表
    ///
    /// **不执行 I/O，不应失败**（Note 57）。验证在工厂的 `create_filter` 阶段完成。
    /// 返回空 Vec 表示输出通道不变。
    fn initialize(&mut self, sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>>;

    /// 处理一帧音频数据（实时路径）。
    ///
    /// - `samples`：交错或多通道采样缓冲区。`samples[channel][frame]`。
    /// - `frame_count`：本帧采样数。
    ///
    /// 就地修改 `samples`（Note 13b：只有标记 `inPlace = true` 的 FilterInfo
    /// 才在原地缓冲区上操作，非原地过滤器使用 `allSamples2` 交换后传入）。
    ///
    /// **不得**进行堆分配、I/O、panic（Note 12/58）。
    fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize);

    /// 是否为 `Channel:` 类型命令（需要通道选择的过滤器）。
    ///
    /// `parser.rs` 遍历工厂时，`cmd_channel.rs` 的工厂返回的 Filter
    /// 将此标记为 `true`，`chain.rs` 的 `addFilters` 据此修改 `currentChannelNames`。
    ///
    /// 默认为 `false`。
    fn is_channel_select(&self) -> bool {
        false
    }

    /// 过滤器的延迟（采样数）。
    ///
    /// `audio_proc_obj_rt.rs` 的 `GetLatency` 汇总所有过滤器的延迟。
    /// 默认为 0（无延迟）。
    fn latency(&self) -> u32 {
        0
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// NoFilter — 空过滤器（passthrough）
// ══════════════════════════════════════════════════════════════════════════════

/// 透传过滤器——不修改任何采样数据。
///
/// 用于：
/// - 空配置时的默认过滤器（pipeline.rs 快速路径）
/// - 工厂返回 `NoFilter` 语义的占位（`FilterCreateResult::NoFilter`）
/// - 测试中的 passthrough 基线
#[derive(Debug, Clone, Copy)]
pub struct PassthroughFilter;

impl Filter for PassthroughFilter {
    fn initialize(&mut self, _sample_rate: u32, _channel_names: &[String]) -> Option<Vec<String>> {
        None // 输出通道不变
    }

    fn process(&mut self, _samples: &mut [Vec<f32>], _frame_count: usize) {
        // 透传，什么都不做
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// FilterCreateResult — 工厂创建结果（Note 14/56）
// ══════════════════════════════════════════════════════════════════════════════

/// 工厂的 `create_filter` 返回值，表达四种状态（Note 56）。
///
/// ```text
/// for factory in &mut self.factories {
///     match factory.create_filter(params, ctx, loader) {
///         FilterCreateResult::Filter(filter) => { self.add_filters(filter); break; }
///         FilterCreateResult::NoFilter       => { break; }       // 匹配但无过滤器
///         FilterCreateResult::AbortFile      => { return; }      // 终止当前文件解析
///         FilterCreateResult::NoMatch        => { continue; }    // 不匹配，继续下一工厂
///     }
/// }
/// ```
#[derive(Debug)]
pub enum FilterCreateResult {
    /// 匹配成功，产生了过滤器实例，加入过滤器链。
    Filter(Box<dyn Filter>),
    /// 匹配成功，但不需要产生过滤器（如 `Device:` 匹配成功、`Include:` 已递归加载）。
    /// `parser.rs` 停止遍历后续工厂，但不向链中添加过滤器。
    NoFilter,
    /// 匹配失败（如参数格式不正确、条件不满足）。
    /// `parser.rs` 继续尝试下一工厂。
    NoMatch,
    /// 当前文件应停止解析（如 `Device:` 不匹配、不可恢复的配置错误）。
    /// `parser.rs` 立即 `return`，跳过当前文件剩余行。
    AbortFile,
}

// ══════════════════════════════════════════════════════════════════════════════
// FilterFactory trait（Note 14）
// ══════════════════════════════════════════════════════════════════════════════

/// 过滤器工厂 trait。
///
/// 每个配置命令（`Device:`、`Channel:`、`Preamp:` 等）对应一个工厂实现。
/// `registry.rs` 按硬编码顺序注册 15 个工厂（Note 14）。
///
/// `parser.rs` 遍历工厂列表，第一个返回 `Filter` 或 `NoFilter` 的工厂胜出。
pub trait FilterFactory: Send {
    /// 尝试根据配置行参数创建过滤器。
    ///
    /// - `params`：配置行冒号后的值部分（已 trim）
    /// - `ctx`：引擎上下文（采样率、通道数等，Note 44）
    /// - `loader`：配置加载器回调（用于 `Include:` 递归，Note 49）
    ///
    /// 返回 `FilterCreateResult` 的四种状态之一。
    ///
    /// 工厂内部的非致命错误（如参数解析失败）通过日志记录，
    /// 返回 `NoMatch`（Note 57）。
    fn create_filter(
        &self,
        params: &str,
        ctx: &EngineContext,
        loader: &dyn ConfigLoader,
    ) -> FilterCreateResult;

    /// 此工厂匹配的命令关键字。
    ///
    /// 如 `"Preamp:"`、`"Channel:"`、`"Device:"`。
    /// 用于日志和调试。
    fn command_name(&self) -> &str;
}

// ══════════════════════════════════════════════════════════════════════════════
// EngineContext — 引擎上下文（Note 44）
// ══════════════════════════════════════════════════════════════════════════════

/// 引擎统一上下文，在 `engine.initialize()` 时构建，
/// `parser.rs` 解析时读取，filter 初始化时接收（Note 44）。
///
/// 纯数据结构，不包含任何 Windows 类型——保持 `dsp/` 可测试性（Note 53）。
#[derive(Debug, Clone)]
pub struct EngineContext {
    /// 采样率（Hz），如 44100、48000、96000
    pub sample_rate: u32,
    /// 通道数（如 2 = 立体声，8 = 7.1）
    pub channel_count: u32,
    /// 通道掩码（Windows `dwChannelMask` 格式，如 0x3 = FL|FR）
    pub channel_mask: u32,
    /// 最大帧数（每帧最大采样数，初始化时固定）
    pub max_frame_count: u32,
    /// 每样本位数（16、24、32）
    pub bits_per_sample: u32,
    /// 设备类型（回放 / 采集）
    pub device_type: DeviceType,
    /// 当前阶段（PreMix / PostMix / None）
    pub stage: ProcessingStage,
}

/// 设备类型枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceType {
    /// 回放设备（扬声器、耳机）
    Render,
    /// 采集设备（麦克风、线路输入）
    Capture,
}

/// 处理阶段枚举。
///
/// `Stage:` 命令设置此标志，后续过滤器只在匹配阶段执行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessingStage {
    /// 无阶段限制，所有过滤器执行
    None,
    /// PreMix 阶段
    PreMix,
    /// PostMix 阶段
    PostMix,
}

// ══════════════════════════════════════════════════════════════════════════════
// ConfigLoader trait（Note 49）
// ══════════════════════════════════════════════════════════════════════════════

/// 配置加载器回调 trait。
///
/// `Include:` 命令通过此 trait 回调 `parser.rs` 递归加载子配置文件（Note 49）。
/// 运行时递归，不是编译期循环引用。
/// `cmd_include.rs` 不能直接 import `parser.rs`，必须通过此 trait 解耦。
pub trait ConfigLoader {
    /// 加载指定路径的配置文件。
    ///
    /// - `path`：配置文件路径（已解析为绝对路径）
    /// - `ctx`：引擎上下文
    ///
    /// 返回该文件产生的过滤器列表。
    /// 文件不存在或解析失败时返回空列表（降级为 passthrough，Note 57）。
    fn load_config(
        &self,
        path: &str,
        ctx: &EngineContext,
    ) -> Vec<Box<dyn Filter>>;
}

// ══════════════════════════════════════════════════════════════════════════════
// PassthroughFactory — 透传工厂（初期占位）
// ══════════════════════════════════════════════════════════════════════════════

/// 透传工厂——匹配一切，返回 `NoFilter`。
///
/// Phase 3 初期用作 `registry.rs` 中唯一的工厂占位，
/// Phase 7/8 逐步替换为真正的命令工厂。
#[derive(Debug)]
pub struct PassthroughFactory;

impl FilterFactory for PassthroughFactory {
    fn create_filter(
        &self,
        _params: &str,
        _ctx: &EngineContext,
        _loader: &dyn ConfigLoader,
    ) -> FilterCreateResult {
        // 匹配一切，返回 NoFilter（passthrough 行为）
        FilterCreateResult::NoFilter
    }

    fn command_name(&self) -> &str {
        "*"
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── PassthroughFilter ───────────────────────────────────────────────────

    #[test]
    fn passthrough_no_change() {
        let mut filter = PassthroughFilter;
        let result = filter.initialize(48000, &["L".into(), "R".into()]);
        assert!(result.is_none());
    }

    #[test]
    fn passthrough_process_noop() {
        let mut filter = PassthroughFilter;
        let mut samples = vec![vec![1.0, 2.0, 3.0], vec![4.0, 5.0, 6.0]];
        let original = samples.clone();
        filter.process(&mut samples, 3);
        assert_eq!(samples, original);
    }

    #[test]
    fn passthrough_defaults() {
        let filter = PassthroughFilter;
        assert!(!filter.is_channel_select());
        assert_eq!(filter.latency(), 0);
    }

    // ── FilterCreateResult ──────────────────────────────────────────────────

    #[test]
    fn filter_create_result_debug() {
        assert!(format!("{:?}", FilterCreateResult::NoFilter).contains("NoFilter"));
        assert!(format!("{:?}", FilterCreateResult::NoMatch).contains("NoMatch"));
        assert!(format!("{:?}", FilterCreateResult::AbortFile).contains("AbortFile"));
    }

    // ── EngineContext ───────────────────────────────────────────────────────

    #[test]
    fn engine_context_defaults() {
        let ctx = EngineContext {
            sample_rate: 48000,
            channel_count: 2,
            channel_mask: 0x3,
            max_frame_count: 480,
            bits_per_sample: 32,
            device_type: DeviceType::Render,
            stage: ProcessingStage::None,
        };
        assert_eq!(ctx.sample_rate, 48000);
        assert_eq!(ctx.channel_count, 2);
        assert_eq!(ctx.device_type, DeviceType::Render);
        assert_eq!(ctx.stage, ProcessingStage::None);
    }

    #[test]
    fn engine_context_clone() {
        let ctx = EngineContext {
            sample_rate: 44100,
            channel_count: 6,
            channel_mask: 0x3F,
            max_frame_count: 441,
            bits_per_sample: 16,
            device_type: DeviceType::Capture,
            stage: ProcessingStage::PreMix,
        };
        let ctx2 = ctx.clone();
        assert_eq!(ctx2.sample_rate, ctx.sample_rate);
        assert_eq!(ctx2.device_type, ctx.device_type);
    }

    // ── DeviceType / ProcessingStage ────────────────────────────────────────

    #[test]
    fn device_type_equality() {
        assert_eq!(DeviceType::Render, DeviceType::Render);
        assert_ne!(DeviceType::Render, DeviceType::Capture);
    }

    #[test]
    fn processing_stage_equality() {
        assert_eq!(ProcessingStage::PreMix, ProcessingStage::PreMix);
        assert_ne!(ProcessingStage::PreMix, ProcessingStage::PostMix);
        assert_ne!(ProcessingStage::None, ProcessingStage::PreMix);
    }

    // ── PassthroughFactory ──────────────────────────────────────────────────

    #[test]
    fn passthrough_factory_returns_nofilter() {
        let factory = PassthroughFactory;
        let ctx = EngineContext {
            sample_rate: 48000,
            channel_count: 2,
            channel_mask: 0x3,
            max_frame_count: 480,
            bits_per_sample: 32,
            device_type: DeviceType::Render,
            stage: ProcessingStage::None,
        };

        struct NullLoader;
        impl ConfigLoader for NullLoader {
            fn load_config(&self, _path: &str, _ctx: &EngineContext) -> Vec<Box<dyn Filter>> {
                vec![]
            }
        }

        let result = factory.create_filter("anything", &ctx, &NullLoader);
        assert!(matches!(result, FilterCreateResult::NoFilter));
    }

    #[test]
    fn passthrough_factory_command_name() {
        assert_eq!(PassthroughFactory.command_name(), "*");
    }

    // ── 编译期验证：Filter 是 object-safe ───────────────────────────────────

    #[test]
    fn filter_is_object_safe() {
        // 这个测试验证 Filter trait 可以作为 trait object 使用
        let _boxed: Box<dyn Filter> = Box::new(PassthroughFilter);
    }

    #[test]
    fn filter_factory_is_object_safe() {
        let _boxed: Box<dyn FilterFactory> = Box::new(PassthroughFactory);
    }

    // ── Box<dyn Filter> 批量操作 ────────────────────────────────────────────

    #[test]
    fn boxed_filter_chain() {
        let mut filters: Vec<Box<dyn Filter>> = vec![
            Box::new(PassthroughFilter),
            Box::new(PassthroughFilter),
            Box::new(PassthroughFilter),
        ];

        let mut samples = vec![vec![0.0; 128], vec![0.0; 128]];

        for filter in &mut filters {
            filter.process(&mut samples, 128);
        }

        // 全部 passthrough，数据不变
        assert!(samples[0].iter().all(|&v| v == 0.0));
        assert!(samples[1].iter().all(|&v| v == 0.0));
    }

    // ── 带实际修改的 Filter 实现（测试 trait 灵活性）─────────────────────────

    #[derive(Debug)]
    struct GainFilter {
        gain: f32,
    }

    impl Filter for GainFilter {
        fn initialize(
            &mut self,
            _sample_rate: u32,
            _channel_names: &[String],
        ) -> Option<Vec<String>> {
            None
        }

        fn process(&mut self, samples: &mut [Vec<f32>], frame_count: usize) {
            for channel in samples.iter_mut() {
                for i in 0..frame_count {
                    channel[i] *= self.gain;
                }
            }
        }
    }

    #[test]
    fn gain_filter_applies() {
        let mut filter = GainFilter { gain: 0.5 };
        let mut samples = vec![vec![1.0, 2.0, 3.0]];
        filter.process(&mut samples, 3);
        assert_eq!(samples[0], vec![0.5, 1.0, 1.5]);
    }

    #[test]
    fn gain_filter_in_chain() {
        let mut filters: Vec<Box<dyn Filter>> = vec![
            Box::new(GainFilter { gain: 2.0 }),
            Box::new(GainFilter { gain: 0.5 }),
        ];

        let mut samples = vec![vec![1.0, 2.0, 3.0]];

        for filter in &mut filters {
            filter.process(&mut samples, 3);
        }

        // 2.0 × 0.5 = 1.0，passthrough
        assert_eq!(samples[0], vec![1.0, 2.0, 3.0]);
    }
}