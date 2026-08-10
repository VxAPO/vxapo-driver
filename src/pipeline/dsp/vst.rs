//! dsp/vst.rs — VST 插件加载（**预留，当前不启用**）
//!
//! # 当前行为
//!
//! 本模块不提供任何实现，仅作为 `VSTPlugin:` 命令的**预留入口**。
//! 对应工厂 `VstFactory` 恒返回 `FilterCreateResult::NoMatch`
//! （见 `factory.rs`），配置中出现 `VSTPlugin:` 时静默跳过，
//! 不会产生过滤器、不会报错。
//!
//! # 为何预留
//!
//! 项目评估当前不需要 VST 插件加载。保留 `VSTPlugin` 工厂槽位与
//! `index::VST_PLUGIN` 常量（v9.1 起 `FACTORY_COUNT = 18`，
//! VST 槽位固定为 13），未来需要时无需调整注册表/索引结构，只需：
//!
//! 1. 在本模块实现 `VstFilter`（实现 `Filter` trait）
//! 2. 在 `factory.rs` 的 `VstFactory::create_filter` 恢复参数解析并创建
//! 3. 引入动态库加载依赖（libloading）与 VST2 `AEffect` / VST3 `IPluginFactory` 协议绑定
//!
//! # 非目标（不做）
//!
//! - VST GUI
//! - MIDI 通路
//! - 多输出通道
//! - VST2 / VST3 宿主调度
