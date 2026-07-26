# DESIGN

## 目录结构
```
vxapo-driver/
├── Cargo.toml
├── vxapo.def
├── build.rs
├── .cargo/
│   └── config.toml
└── src/
    ├── lib.rs
    ├── utils.rs
    ├── utils/
    │   ├── align.rs
    │   ├── error.rs
    │   └── reg_read.rs
    ├── com.rs
    ├── com/
    │   ├── abi.rs
    │   ├── apo_abi.rs
    │   ├── non_delegating.rs
    │   ├── iid.rs
    │   ├── factory.rs
    │   ├── vtable.rs
    │   ├── reg_props.rs
    │   └── clsid_reg.rs
    ├── instance.rs
    ├── instance/
    │   ├── ref_count.rs
    │   ├── object.rs
    │   ├── apo_child.rs
    │   ├── init.rs
    │   ├── audio_format.rs
    │   ├── audio_proc_obj.rs
    │   ├── audio_proc_obj_rt.rs
    │   └── audio_proc_obj_conf.rs
    ├── engine.rs
    ├── engine/
    │   ├── rt_contract.rs
    │   ├── context.rs
    │   ├── pipeline.rs
    │   ├── filter.rs
    │   ├── registry.rs
    │   ├── chain.rs
    │   ├── parser.rs
    │   ├── swap.rs
    │   ├── watcher.rs
    │   ├── channel.rs
    │   ├── deinterleave.rs
    │   ├── buffer.rs
    │   ├── transition.rs
    │   └── commands.rs
    ├── engine/commands/
    │   ├── cmd_device.rs
    │   ├── cmd_cond.rs
    │   ├── cmd_expr.rs
    │   ├── cmd_include.rs
    │   ├── cmd_stage.rs
    │   └── cmd_channel.rs
    ├── dsp.rs
    ├── dsp/
    │   ├── biquad.rs
    │   ├── peq.rs
    │   ├── hp_lp.rs
    │   ├── graph_eq.rs
    │   ├── gain.rs
    │   ├── delay.rs
    │   ├── convolution.rs
    │   ├── vst.rs
    │   ├── loudness.rs
    │   └── copy.rs
    ├── realtime.rs
    ├── realtime/
    │   └── ring.rs
    ├── device.rs
    ├── device/
    │   ├── endpoint.rs
    │   ├── slots.rs
    │   ├── format.rs
    │   └── info.rs
    ├── installation.rs
    ├── installation/
    │   ├── exports.rs
    │   ├── reg_write.rs
    │   ├── notify.rs
    │   ├── rollback.rs
    │   └── audiodg.rs
    ├── telemetry.rs
    └── telemetry/
        ├── logger.rs
        └── panic.rs
```

## 完整注意事项（1–60）

以下所有文件名、目录名、模块路径均对齐定稿目录树。修订条目标注 `[修订]`，新增条目标注 `[新增]`。

---

### com/ — COM 基础设施

**1.** vtable 必须是编译期静态常量。`repr(C)` 布局，函数指针顺序与 Windows ABI 严格一致。加编译期断言验证结构体大小，不能用运行时构造。

**2.** 两套独立的原子计数。`INST_COUNT`（instance/ref_count.rs）管活跃实例数，`LOCK_COUNT`（com/factory.rs）管客户端显式锁定。`DllCanUnloadNow` 必须两者均零才返回 `S_OK`。

**3.** ClassFactory 支持聚合。`pUnkOuter` 非空时只能请求 `IUnknown`，实例用 `NonDelegatingQueryInterface`，创建后立即 `NonDelegatingRelease`。构造函数将引用计数初始化为 1，QueryInterface 成功后 +1，工厂释放后最终为 1 归客户端。

实现建议：优先使用 `windows-rs` 的 `implement` 宏自动生成聚合和 `NonDelegatingUnknown`，减少手写 COM 引用计数的出错概率。若手写，必须在每步操作旁标注引用计数值变化。

**4.** 双 CLSID 支持。`DllGetClassObject` 只接受 PreMix / PostMix 两个 CLSID，其余返回 `CLASS_E_CLASSNOTAVAILABLE`。

**5.** ThreadingModel = "Both"。COM 类注册写入 `InprocServer32` 下必须为 `"Both"`。

**42.** reg_props.rs 定义 APO 注册属性。两个属性对象（PreMix / PostMix）包含 CLSID、名称、版权、APO 标志位（`FRAMESPERSECOND_MUST_MATCH | BITSPERSAMPLE_MUST_MATCH | INPLACE`），共享名称和标志，只有 CLSID 不同。

**59.** DllMain 的 Rust 特有约束。

`installation/exports.rs` 中的 DllMain 必须保持极简（仅保存模块句柄到全局 `static`，始终返回 TRUE）。禁止在 `DllMain`（`DLL_PROCESS_ATTACH`）中：
- 初始化 COM（`CoInitializeEx`）
- 创建线程（`std::thread::spawn`）
- 触发全局 `static` 的复杂初始化（`once_cell::Lazy::force()`、`lazy_static!` 的首次访问）
- 使用 `#[ctor]` 宏标注的初始化函数
- 调用任何 `windows-rs` 的 COM 初始化宏

所有 COM 初始化和 APO 对象构造必须延迟到 `DllGetClassObject` 或 `CreateInstance` 被调用时进行。这是 Windows 加载器锁（Loader Lock）的铁律——在 DllMain 中执行同步操作极易导致进程死锁。

此约束适用于 Rust 生态中所有隐式全局初始化路径：`once_cell`、`lazy_static`、`thread_local!`（首次访问时的初始化函数）、`std::sync::Once`。如果这些结构在 DllMain 阶段被首次访问，其初始化函数会在 Loader Lock 持有期间执行。

### instance/ — APO 实例生命周期

**6.** APOGUID 特殊值常量。`object.rs` 中定义 `APOGUID_NOKEY`（FxProperties 键不存在）和 `APOGUID_NOVALUE`（值为空或已被占据），`init.rs` 和 `audio_proc_obj_conf.rs` 都依赖。

**7.** APOInitSystemEffects 解析顺序。验证 cbDataSize → 提取 APO CLSID 确定模式 → 获取端点 GUID → 加载设备配置 → 获取子 APO GUID → 读取 allowSilentBufferModification → CoCreateInstance 子 APO → 命名管道测试通信。

优先使用 `windows` crate 提供的 `APOInitSystemEffects` 类型定义。若因版本或 feature 原因不可用，手写时必须加编译期大小断言作为防护：`const _: () = assert!(std::mem::size_of::<APOInitSystemEffects>() > 0);`。

**8.** IsInputFormatSupported 的通道约束。不支持多于 2 通道下混到较少通道，检测到时返回输出格式替代（`S_FALSE`）。采样率和位深必须匹配。

**9.** LockForProcess 的通道数确定规则。有子 APO 用输出通道数，无子 APO 用输入通道数。采集设备用输入掩码，回放用输出掩码，优先非零。

**10.** apo_child.rs 的接口管理。Drop 按序释放三接口（apo → rt → cfg），每个释放前检查空指针。暴露 `get_latency()` 供 `audio_proc_obj_rt.rs` 使用——零基准加子 APO 延迟。

**11.** audio_proc_obj_rt.rs 的静音缓冲区判定。BUFFER_SILENT + allowSilentBuffer → 遍历采样，全 ≤1e-10 则 SILENT 否则 VALID；不允许 → 强制清零 + SILENT。BUFFER_VALID → 直接 VALID。其他标志不处理。

### engine/ — 实时处理引擎

**12.** RT-safety 契约。处理函数执行在多媒体实时线程。禁止堆分配、互斥锁、I/O、panic。所有缓冲区初始化时预分配。混合函数用裸指针。`engine.rs` 顶部需加入模块级 RT-safety 说明，让读者在打开 `engine/` 目录时第一时间了解"这个目录下的代码运行在实时音频线程中"。

**13.** filter.rs — 过滤器抽象接口。对应 EqualizerAPO 的 IFilter。`process` 操作预分配缓冲区，`initialize` 返回输出通道名列表，`get_select_channels` 标识 `Channel:` 类型命令。

**13b.** APO_FLAG_INPLACE 的双重含义。对外告诉 Windows 该 APO 支持输入输出同缓冲区，但在引擎内部只有标记 `inPlace = true` 的 FilterInfo 才在原地缓冲区上操作，非原地过滤器使用 allSamples2 交换。两层"原地"含义不同。

**14.** registry.rs — 工厂注册机制。按硬编码顺序注册 15 个工厂（Device → If → Eval → Include → Stage → Channel → IIR → BiQuad → Preamp → Delay → Copy → Convolution → GraphicEQ → VSTPlugin → LoudnessCorrection）。用 `fn create_default_registry() -> Vec<Box<dyn FilterFactory>>` 按顺序构造，`parser.rs` 按下标顺序遍历，第一个返回 `Filter` 或 `NoFilter` 的工厂胜出。包含 `FilterFactory` trait、`FilterCreateResult` 枚举、`ConfigLoader` trait。初期只实现 PassthroughFactory。

**15.** chain.rs — 双缓冲区架构 + 通道映射。`allSamples` 主缓冲区 + `allSamples2` 辅助缓冲区，总通道 = 实际通道 + 辅助通道。FilterInfo 含 filter 指针 + 输入输出通道索引 + inPlace 标志。原地处理直接操作主缓冲区，非原地处理用辅助缓冲区后交换指针。维护三组通道名称列表（all / current / last）。`addFilters` 中若当前 `currentChannelNames` 与上次相同，复用上次的映射结果，避免重复字符串→索引查找。

**16.** parser.rs — 配置文件解析。UTF-8 打开，退 ANSI 解码。冒号分隔键值，键做 trim。遍历工厂 `create_filter`，首个返回 `Filter` 或 `NoFilter` 者胜出（`NoMatch` 继续尝试下一工厂，`AbortFile` 终止当前文件解析并 `return`）。空键为注释行。`Include` 通过 `ConfigLoader` 递归调用。共享冲突最多重试 3 次，间隔 1ms。文件解析后恢复外层 `currentChannelNames`。`Include:` 命令的路径解析规则：若 `path` 为相对路径，相对于当前配置文件目录（`current_dir`）解析。

编码降级实现步骤：
1. `read_to_end` 读为 `Vec<u8>`
2. `String::from_utf8` 尝试 UTF-8 解码
3. 若失败或结果包含 `\uFFFD` 替换字符，调用 `MultiByteToWideChar(CP_ACP)` 转换为 UTF-16，再转 `String`

替换字符检查处理了"字节序列恰好是合法 UTF-8 但实际是 ANSI 编码"的边缘情况。这是 CJK 用户配置文件可用性的关键细节。

**17.** channel.rs — 通道工具。`get_channel_names(channel_count, channel_mask)` 生成 L/R/C/LFE/SL/SR 等名称，`default_channel_mask(channel_count)` 掩码为 0 时生成默认值。同时被 device/ 和 engine/ 使用。

**18.** pipeline.rs 完整处理流程。检查 BufferFlags → 静音清零 → 快速路径（空配置时 memcpy 或直返）→ 去交织 → **清零额外通道**（`memset allSamples[c], c 从 realChannelCount 到 allChannelCount`）→ mono 上混（通道 0→1，仅单声道且输出 ≥2）→ 过滤器链处理 → 过渡混合（有 nextConfig 时）→ 交织输出 → 输出标志控制。

**19.** swap.rs 的信号量协调。加载线程构建新配置 → 存为 nextConfig → 释放信号量。实时线程过渡完成后切换三指针 → 释放信号量。信号量初始 1 最大 1。暴露 `has_pending_swap()` 和 `current_config()` 接口。

**20.** deinterleave.rs 的编译期优化。通道数 1/2/6/8 用特化宏，其他用通用循环。交织和非交织版本都提供，非交织版本直接 memcpy。

**21.** transition.rs。升余弦混合因子 `raised_cosine(counter, length)` = `0.5 * (1.0 - cos(PI * counter / length))`。counter ≥ length 时返回 1.0。混合函数用裸指针签名。

**43.** 配置加载的完整时序。`initialize()` 首次调用且非自定义路径时启动 notificationThread 并设 `notification_started = true`。`loadConfig()` 获取 load_mutex，首次加载（currentConfig 为空）直通设为 currentConfig，否则设为 nextConfig 等待 process 触发过渡。`loadConfigFile()` 保存 currentChannelNames 快照，解析后恢复。

**44.** EngineContext 作为统一上下文传递。所有引擎子模块通过 `&EngineContext` 获取音频参数（采样率、通道数、掩码、最大帧数、设备类型、阶段）。在 `engine.initialize()` 时构建，`parser.rs` 解析时读取，filter 初始化时接收。

**45.** 过滤器链通道映射缓存。`addFilters` 中若当前 `currentChannelNames` 与上次相同，复用上次的映射结果，避免重复字符串→索引查找。

### engine/commands/ — 配置命令处理器

**49.** engine/ 内部的 include 递归依赖。`parser.rs` → 遍历工厂 → `cmd_include.rs` 的 `create_filter` → 通过 `ConfigLoader` trait 回调 `parser.rs` 递归。这是运行时递归，不是编译期循环引用，但 `cmd_include.rs` 不能直接 import `parser.rs`，必须通过 `ConfigLoader` trait 解耦。

**50.** 配置文件变更监控。放在 `engine/watcher.rs`。

- **目录监控**：在 `engine.initialize()` 中一次性启动，监控配置目录的文件名变更和最后写入时间（递归）。
- **注册表监控**：`parser.rs` 在解析过程中调用 `watchRegistryKey` 向 `watched_registry_keys`（`Arc<Mutex<Vec<String>>>`）追加键路径。通知线程主循环每次迭代重新为所有已注册键注册 `RegNotifyChangeKeyValue` 通知。
- **通知去重**：检测到目录变更后，调用 `FindNextChangeNotification` 重置通知，然后等待最多 10ms 看是否有更多变更，将连续变更合并为一次配置重载（编辑器通常先写临时文件再 rename，会触发多次通知）。10ms 窗口是参考 EAPO 的原始实现，后续可优化为信号量计数模式。
- 首次 `initialize()` 且非自定义路径时启动，用 `notification_started: bool` 标志控制不重复启动。变更时调用 `loadConfig()`。

**51.** Channel: 命令的通道选择恢复。`parser.rs` 在解析每个文件（包括 `Include:` 的子文件）前保存 `currentChannelNames` 快照，文件解析完毕后恢复。这确保 `Include:` 文件内的 `Channel:` 命令不会污染外层文件的通道选择。

**52.** Copy 通道复制的特殊行为。`dsp/copy.rs` 的 `initialize()` 可能创建新的通道名（如辅助通道），返回值中包含这些新名称。`chain.rs` 的 `addFilters` 检测到 `allChannelNames` 中不存在的新通道时，将其追加到末尾，扩大 `allSamples` / `allSamples2` 的通道数组。

**55.** engine/commands/ 的六个命令处理器实现要点：

| 文件 | 命令 | FilterFactory 实现 | Filter 实现 |
|------|------|-------------------|-------------|
| `cmd_device.rs` | `Device:` | 设备匹配时返回 `NoFilter`（继续解析），不匹配时返回 `AbortFile`（停止当前文件解析） | 不实现 |
| `cmd_cond.rs` | `If:` | 返回 `NoFilter` | 不实现 |
| `cmd_expr.rs` | `Eval:` | 返回 `NoFilter`（设置 muParserX 变量） | 不实现 |
| `cmd_include.rs` | `Include:` | 返回 `NoFilter`（通过 ConfigLoader 递归加载，子过滤器由 parser 追加） | 不实现 |
| `cmd_stage.rs` | `Stage:` | 返回 `NoFilter`（设置阶段标志） | 不实现 |
| `cmd_channel.rs` | `Channel:` | 返回 `Filter(Box::new(ChannelFilter {...}))` | **实现 Filter trait**，`get_select_channels() = true` |

其中 `cmd_device.rs`/`cmd_cond.rs`/`cmd_expr.rs`/`cmd_include.rs`/`cmd_stage.rs` 只需要实现 `FilterFactory` trait，`create_filter` 返回 `NoFilter` 或 `AbortFile`，在内部执行副作用（设置变量、递归加载等），不产生 `Filter` 实例。`cmd_channel.rs` 是唯一需要实现 `Filter` trait 并加入过滤器链的命令。

**56.** 工厂的副作用执行与过滤器链添加的分离。`parser.rs` 中的处理逻辑：

```rust
for factory in &mut self.factories {
    match factory.create_filter(params, ctx, loader) {
        FilterCreateResult::Filter(filter) => { self.add_filters(filter); break; }
        FilterCreateResult::NoFilter       => { break; }       // 匹配但无过滤器，跳出循环
        FilterCreateResult::AbortFile      => { return; }      // 匹配失败，终止当前文件解析
        FilterCreateResult::NoMatch        => { continue; }    // 不匹配，继续
    }
}
```

`Channel:` 的核心逻辑在 `addFilters` 阶段发生——它修改了 `currentChannelNames`，后续过滤器只看到被选中的通道子集。`process()` 调用是空操作但不跳过（保持遍历一致性）。

### dsp/ — 数字信号处理滤波器

**53.** dsp/ 的独立可测试性。`engine/filter.rs` 是纯 Rust trait 定义，不包含任何 Windows API。`dsp/` 只依赖这个文件，不依赖 `engine/` 的其他部分。`dsp/` 的每个模块可以用标准 `#[test]` 编译测试，不需要 Windows 环境。这是 trait 放在 `engine/filter.rs`（而非 `dsp/`）的核心收益。

**54.** VST 插件的 feature gate。`dsp/vst.rs` 涉及动态库加载和 COM 接口查询，有显著平台依赖。用 Cargo feature gate 隔离：

```toml
[features]
default = []
vst = ["libloading"]
```

初期只留文件占位和 trait 实现骨架。

**58.** dsp/ 的 import 边界。

- `dsp/` 模块只允许 import 以下内容：
  - `crate::engine::filter`（Filter trait）
  - 标准库（`std::collections`、`std::f32` 等）
  - 第三方纯 Rust 库（如 num-complex，卷积 FFT 所需）
- 禁止 `dsp/` import 以下模块：
  - `crate::com`、`crate::instance`、`crate::installation`（Windows COM 依赖）
  - `crate::device`（设备查询层）
  - `crate::utils::error`（`VxApoError` 包含 `HResult(HRESULT)` 变体，间接引入 Windows 类型，破坏独立可测试性）
  - `crate::utils::reg_read`（注册表操作，Windows 依赖）
  - `crate::engine` 的其他子模块（`context`、`chain`、`pipeline` 等）
- 当前设计中 `Filter::initialize` 返回 `Vec<String>`、`process` 返回 `()`，dsp/ 不需要任何错误类型。工厂侧的错误通过 `FilterCreateResult::NoMatch` + 日志处理，不向 dsp/ 传播
- 如果未来 dsp/ 内部需要独立的错误类型，在 dsp/ 内部定义纯 Rust 的 `DspError` 枚举，不依赖 `utils::error`
- CI 中通过 cargo-deny 或 grep 验证 `dsp/` 的依赖边界
- `dsp/` 中所有 `Filter::process` 实现禁止以下隐式堆分配操作：`Vec::clone()`、`.to_vec()`、`.to_string()`、`format!()`、`Box::new()`、`HashMap::insert()`、`String::new()`。约束范围仅限 `process` 方法（实时路径），`initialize` 和工厂相关函数允许分配。CI 中通过 clippy 的 `disallowed_methods` 配置检查，配合代码审查
- 如果未来出现跨平台 DSP 库的独立需求，再考虑 workspace 拆分

### realtime/ — 实时安全基础设施

**22.** ring.rs 共用。无锁环形缓冲区被 engine（信号量抽象）和 telemetry（logger）共同使用，单独放在 realtime/ 下。目录定位为"实时安全基础设施"，ring.rs 作为第一个成员，未来扩展更多无锁数据结构时保持一致性。

### device/ — 设备查询层

**23.** info.rs 只做查询。组合 Endpoint、Slots、Format，提供 is_installed、can_be_upgraded、is_experimental、is_enhancements_disabled、has_changes。实际操作委托 installation/。

**24.** 安装版本常量。`info.rs` 中定义 `INSTALL_VERSION = "2"` 和 `INSTALL_VERSION_LEGACY = "1"`。版本不匹配时抛异常。

**25.** slots.rs 的 5 个 GUID 槽位。LFX(0)/GFX(1)/SFX(2)/MFX(3)/EFX(4)，三种特殊值状态：NOKEY、NOVALUE、具体 GUID。

**26.** 三种安装模式。LFX/GFX（Win8.1+ Legacy）、SFX/MFX（Win11 蓝牙）、SFX/EFX（默认）。切换模式删除旧槽位，写入默认处理模式 GUID `{C18E2F7E-933D-4965-B7D1-1EEF228D2AF3}`。

**27.** format.rs 的 WAVEFORMATEX 解析。读取 formatValueName 二进制值 → 解析 WAVEFORMATEX → EXTENSIBLE 时读 dwChannelMask → 掩码为 0 时读 channelMaskValueName → 仍为 0 时调用 default_channel_mask。

**46.** 原始 APO GUID 的回退逻辑。`get_original_pre_mix()` 按安装模式取对应 PreMix 槽位，若为 NOVALUE 且同组另一槽位也是 NOVALUE 则回退另一组。`get_original_post_mix()` 类似但涉及 GFX/MFX/EFX 三槽位。NOKEY 或无回退时返回空字符串。

### installation/ — DLL 注册与安装

**28.** exports.rs 的四个导出函数。`DllRegisterServer`、`DllUnregisterServer`、`DllGetClassObject`、`DllCanUnloadNow`，全部 `#[no_mangle] pub extern "system"`。

**29.** DllRegisterServer 的注册顺序和回滚。PostMix APO 注册 → 失败回滚 PostMix。PreMix APO 注册 → 失败回滚两者。COM 类注册 → 失败回滚两者 + 删除 COM 类键。写入 `HKLM\...\CLSID\{GUID}\InprocServer32` 路径和 ThreadingModel。

**30.** DllUnregisterServer 的注销顺序。先删 InprocServer32 子键再删 CLSID 父键，然后 UnregisterAPO 两个 GUID。

**31.** reg_write.rs 的权限提升。`makeWritable` 修改 DACL 添加 Administrators 完全控制。`takeOwnership` 获取键所有权需 `SE_TAKE_OWNERSHIP_NAME` 特权。都涉及 unsafe，必须有 SAFETY 注释。

**32.** rollback.rs 的 .reg 备份。安装前备份原始 APO GUID，文件名 `backup_{设备名}_{连接名}.reg`。

**33.** audiodg.rs 的检查逻辑。`DisableProtectedAudioDG` 不存在或不为 1 则阻止第三方加载，fix 时写入 1。

**47.** install() 的完整步骤清单。创建 Child APOs 键 → FxProperties 不存在则创建（失败则权限提升重试）→ 已存在则备份原始 GUID 到 .reg → 写入子 APO 配置（childGuid / allowSilentBuffer / autoAdjust / version）→ 按模式写入 APO GUID → 写入默认处理模式 GUID → 删除 DisableEnhancements。

### telemetry/ — 可观测性

**34.** logger.rs 必须无锁。使用 realtime::ring 的无锁环形缓冲区。

**35.** panic.rs 必须防止 unwind。`Cargo.toml` 设 `panic = "abort"`。panic hook 记录日志后 `std::process::abort()`。

**60.** FFI 边界的 panic 防御。

`audio_proc_obj_rt.rs` 的 `APOProcess` 入口处用 `std::panic::catch_unwind` 包裹所有实时处理逻辑。`catch_unwind` 内部捕获到 panic 时：
- 记录到 `logger.rs` 的 ring_logger
- 将输出缓冲区清零
- 设置输出标志为 `BUFFER_SILENT`
- 返回（不重新 panic）

**注意**：当 `panic = "abort"`（release 默认配置）时，`catch_unwind` 不会执行闭包内的清理代码——panic 触发后直接 abort，`catch_unwind` 是空操作。此时真正的防护是 `telemetry/panic.rs` 的 hook + `panic = "abort"` 的进程终止。

`catch_unwind` 在以下场景发挥作用：
- Debug 构建临时切换为 `panic = "unwind"` 以获取回溯信息
- 测试场景中需要 unwind 来报告测试失败

因此 `catch_unwind` 是 debug/测试环境下的防御层，release 环境下的真正防线是 `panic = "abort"`。

对于 `process` 路径中的 `dsp/` 调用链，`pipeline.rs` 入口处同样需要 `catch_unwind` 包裹（覆盖 dsp 模块中的意外 panic）。

### utils/ — 共享工具

**36.** VxApoError 统一错误枚举。变体：HResult / Registry / Io / DeviceNotFound / FormatUnsupported / VersionMismatch。为 HRESULT 和 io::Error 实现 From。

**37.** align.rs 的用途。SIMD 宽度对齐（16/32 字节），初始化时分配，实时线程直接使用。

**48.** reg_read.rs 的 splitKey 实现要点。根键名大小写不敏感（转大写比较），第一个 `\` 分隔根键和子键路径。支持 5 个标准根键（HKEY_CLASSES_ROOT / HKEY_CURRENT_CONFIG / HKEY_CURRENT_USER / HKEY_LOCAL_MACHINE / HKEY_USERS）。未知根键返回错误。只读操作（openKey / readValue / readDWORDValue / readBinaryValue / readMultiValue / keyExists / enumSubKeys / valueExists / getGuidString / isWindowsVersionAtLeast / saveToFile）在此实现。写入和权限操作留在 installation/reg_write.rs。

### 工程层面

**38.** Cargo.toml 关键配置。`crate-type = ["cdylib"]`、`panic = "abort"`（release profile）、`lto = true`。feature gate：

```toml
[features]
default = []
vst = ["libloading"]
```

**39.** vxapo.def 声明。四个导出函数标记 PRIVATE。通过 build.rs 或链接属性引入。CI 加导出符号验证。

**40.** 所有 unsafe 块必须有 SAFETY 注释。CI 启用 `#![deny(clippy::undocumented_unsafe_blocks)]`。

**41.** 每个模块的测试骨架。每个 `.rs` 文件放 `#[cfg(test)] mod tests`。Phase 1–4 完成后至少有：factory 创建 object 引用计数测试、pipeline passthrough 单元测试、channel 掩码/名称测试、format WAVEFORMATEX 解析测试。

项目根目录添加 `.cargo/config.toml`，固定目标平台为 `x86_64-pc-windows-msvc`，避免在非 Windows 环境下开发时 rust-analyzer 报错。这在 windows-rs 0.58+ 版本中尤其重要。

### 错误处理总则

**57.** 错误处理总则。

- `FilterFactory::create_filter` 的所有状态通过 `FilterCreateResult` 枚举表达，不使用 `Result`。工厂内部的非致命错误（如参数解析失败）通过日志记录，返回 `NoMatch`。
- `parser.rs` 的文件级错误（打开失败、解码失败、共享冲突重试 3 次耗尽）通过日志记录，返回空过滤器链（降级为 passthrough），不中断音频服务。
- `Filter::initialize` 不执行 I/O，不应失败。验证在工厂的 `create_filter` 阶段完成。如果 `initialize` 返回空 Vec，表示该过滤器输出通道不变。
- `instance/init.rs` 中子 APO CoCreateInstance 失败时降级为无子 APO 模式（继续初始化，不中止 APO 注册）。
- 实时线程中的 `process` 路径不允许任何错误传播：无 `Result` 返回、无 panic、无堆分配。违反者由 `telemetry/panic.rs` 的 hook 捕获并 abort。
- 所有非实时路径的错误统一为 `VxApoError`（注意事项 36），通过 `log` crate 的 `error!` 宏记录到 `logger.rs`。

---

总编号范围：**1–60**。不改变目录结构、trait 定义、枚举定义或实现顺序。所有修订均为编码阶段的补充规范。