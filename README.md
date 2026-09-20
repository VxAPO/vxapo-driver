# VxAPO Driver

<!-- 徽章区（待补）：CI 状态 · 许可证 · 最近发布 -->

[中文](#vxapo-driver) · [English](#vxapo-driver-english) · [项目总览](../vxapo-docs/overview/zh/项目概览.md)

在 Windows 音频引擎（`audiodg`）内实现逐端点的实时音频处理。VxAPO Driver 以标准 APO
（Audio Processing Object）形式注册并被引擎加载，按端点读取
`C:\ProgramData\VxAPO\{GUID}\config.toml`，逐帧处理音频流。配置是只读输入：写入路径
仅存在于 CLI（提权）与 App，DLL 不修改任何文件。

阅读顺序：工程价值 → 生态位 → 架构 → 实时契约 → 配置 → 效果器与支持面 → 性能 →
上手 → 三端契约 → 测试与排障 → 参考。

## 1 · 工程价值

核心结论：**Windows 音频链路的驱动级实时处理可以由 Rust 完整实现，且遵守 COM/APO 的
既有约定，而非绕开这些约定。** 以下判断均可复核：

| 维度 | 结论 | 证据 |
|---|---|---|
| 系统集成 | 以标准 APO 注册，由引擎在聚合模式（`pUnkOuter` 非空）下加载 | 注册项 `HKLM\SOFTWARE\…\AudioEngine\AudioProcessingObjects\{CLSID}`；实现 `IAudioProcessingObject` / `…RT` / `IAudioFormat` / `IPropertyStore`；导出 `DllGetClassObject` / `DllRegisterServer`，自维护实例与锁计数以支撑 `DllCanUnloadNow` |
| 端到端可用 | 真机完成「安装 → 验证 → 卸载」闭环，且装卸对称 | `install --verify` 依次执行：停/启音频服务 → `CoCreateInstance` → `GetMixFormat` → `Initialize` → `test_pipe` 回环。耳机（Octave）端点槽位 `41C34613…` / `B4A97313…` → `NoValue` → 复原，`childApoKeyExists` 同向翻转；结束后设备配置与起始**逐字节一致** |
| API 层 | 使用 windows-rs 类型化绑定，而非手写 COM 样板 | APO 接口取自 windows-rs 0.62 `Win32::Media::Audio::Apo`（含 `*_Impl` traits）；`ClassFactory` 由 `#[implement]` 生成 vtable 与引用计数 |
| `unsafe` 治理 | 集中于 FFI 边界，逐处附 SAFETY 说明 | 392 处，分布于 27 个文件：`aggregate.rs` 108 · `audiodg.rs` 57 · `process.rs` 31 · `child.rs` 30 · `factory.rs` 27 …，配套 126 处 SAFETY |
| 实时约束 | 以编译期约束表达，而非运行时约定 | `unsafe trait RtSafe` / `RtCopy` 将「禁止分配、加锁、I/O、panic」编码为类型约束；`RealtimeContext` 为零尺寸编译期见证；`panic = "abort"`；处理链零分配 |
| 工程可持续 | 具备可回归基线 | 491 个测试 / 57 个源文件；`cargo build` 与 `cargo build --tests` 均 0 告警；热重载按 `spec()` 指纹幂等跳过 |

**边界说明**：唯一手写 vtable 偏移的位置是 COM 聚合外壳（`object/apo/aggregate.rs`）——
Windows 引擎要求多接口按固定 offset 布局；该处逐条注明 SAFETY 与布局依据。其余 COM
实现均通过 windows-rs 的声明式接口完成。

## 2 · 生态位：与 Equalizer APO 的取舍

两者面向同一问题——系统级、逐端点、可脚本化的音频处理，工程取舍不同。下表只列差异：

| 维度 | Equalizer APO | VxAPO |
|---|---|---|
| 配置载体 | 文本配置（`config.txt` 语法） | 每端点一份 TOML：`C:\ProgramData\VxAPO\{GUID}\config.toml` |
| 配置生效 | 重载/重启后生效 | 事件驱动热重载（`FindFirstChangeNotificationW`，10 ms 去抖）+ 双链过渡抑制爆音 |
| 界面 | 独立 GUI 编辑器 | Tauri 2 + React 19 桌面应用（参数视图 / 语义视图）+ CLI |
| 设备模型 | APO 安装到端点，配置按设备文件组织 | 同上，并额外提供旧 GUID 残留的检测 / 迁移 / 清理（以设备实例 ID 为稳定身份） |
| PEQ 策略 | 多段 EQ（含 GraphicEQ） | 混合 PEQ：`fc < 200 Hz` 用 IIR（RBJ），`≥ 200 Hz` 用线性相位 FIR（1024–8192 抽头，> 2048 分块 FFT），≤ 31 段 |
| 效果集 | 由配置语法驱动的文本指令 | 7 个内置效果器，参数带范围 / 步进 / 默认值契约（见 §6） |
| 许可 | GPL-2.0 | GPL-3.0-or-later，独立实现（不含 EAPO 代码） |

差异根源在于配置模型的定位：EAPO 以文本指令表达信号链，VxAPO 以结构化契约（driver 产出
参数表、CLI 透传、App 生成界面），换取三端行为一致性（见 §9）。

## 3 · 架构

链路与数据流：

```text
┌───────────────────────────┐
│ vxapo-app                 │  界面：React 19 + Tauri 2
└─────────────┬─────────────┘
              │  config.toml（热重载）
              ▼
┌───────────────────────────┐
│ vxapo-cli                 │  枚举 · 安装/卸载 · 快照 · 验证
└─────────────┬─────────────┘
              │  HKLM 槽位 · CLSID 绑定 · 子 APO 记录
              ▼
┌───────────────────────────┐
│ audiodg.exe               │  音频引擎（实时线程）
└───────────────────────────┘
              ↑ 加载 vxapo_driver.dll：标准 APO，逐帧处理
```

driver 内部按实时性划分为两条路径：

```text
实时路径（每帧执行；禁止分配、加锁、I/O、panic）
┌──────────────────────────────────────────────────────────────┐
│ pipeline/realtime/  RtSafe / RtCopy contracts, witness       │
│ pipeline/dsp/       effect chain (7 effects, section 6)      │
└──────────────────────────────────────────────────────────────┘

控制路径（初始化、热重载、诊断；允许分配与加锁）
┌──────────────────────────────────────────────────────────────┐
│ object/apo/         APO object, aggregate shell, child APO,  │
│                     negotiation, hot reload, RT dump         │
│ config/             chain model, validation, watcher         │
│ install/            enumeration, slots, transaction, stale   │
│ sys/                COM, registry, audio-format constants    │
│ telemetry/          logging, panic records                   │
│ utils/              ring buffer, alignment, GUID, errors     │
└──────────────────────────────────────────────────────────────┘
```

实时路径只依赖 `pipeline/`；`install/` 与 `sys/` 仅在初始化与安装阶段进入。模块职责与
依赖方向详见 [`../vxapo-docs/driver`](../vxapo-docs/driver)。

## 4 · 实时安全契约（`src/pipeline/realtime/`）

实时性以类型系统表达，而非依赖约定：

- `unsafe trait RtSafe` / `RtCopy` 规定实现者的所有 `&self` / `&mut self` 方法不得分配、
  不得加锁、不得 I/O、不得 panic；DSP 滤波器全部满足该约束。
- `RealtimeContext` 为零尺寸见证类型，无字段、无用户可达构造函数，由 RT harness 按引用
  传递；其出现在调用栈中即表示「此配置服务于实时路径」，把运行时断言前移为编译期约束。
- 非规格化值防护：RT 入口设置硬件 FTZ/DAZ（`math.rs`），输出端以 `is_finite` 兜底。
- 处理链零分配：`buffer` / `interleave` / `ring` 在初始化阶段预分配，逐帧复用。
- 发布构建 `panic = "abort"`，RT 路径内不存在可触发 unwind 的调用。

## 5 · 配置模型与热重载（`src/config/`）

- 模型转换：TOML `FileModel` → DSP `ChainModel`（`version` / `enabled` / `[meta]` /
  `[[effects]]`）；`name` / `group` 等 UI 元数据在转换时丢弃，不进入 DSP 指纹。
- 校验与限幅：声道名、参数范围、PEQ 段数（单块 1–31，全局合计 ≤ 31）在 config 层完成；
  越界值仅在内存中 clamp，不回写文件。
- 热重载：`FindFirstChangeNotificationW` 目录监控（10 ms 去抖，合并编辑器「临时文件 +
  rename」模式）→ 128 KB 内容闸门 → 重新解析 → 与 `active_spec` 指纹逐项比对；内容未变
  则幂等跳过，变更则经双链过渡（`transition.rs`）切换，避免爆音。
- 总开关 `enabled = false`：整链直通，文件内容保留但不再参与校验。

## 6 · 效果器与支持面

| 效果器 | 实现 |
|---|---|
| `peq` | 混合 PEQ：`fc < 200 Hz` 走 IIR（RBJ 双线性），`≥ 200 Hz` 走线性相位 FIR；高采样率 / 长 IR 走分块 FFT；≤ 31 段，支持 peaking / low_shelf / high_shelf / low_pass / high_pass |
| `preamp` | 基准电平增益（-120..+48 dB），峰值对齐 |
| `aural` | 谐波激励器：二阶 Butterworth 高通提取频段 → 电平无关软饱和（tanh 奇次 + 半波整流偶次，DC 阻断）→ 湿干混合 |
| `reverb` | Dattorro 板式混响：4 级 AllPass 扩散 + 双槽交叉反馈环路（调制 AllPass → 主延迟 → 槽内滤波 → 扩散 → 尾延迟）+ 14 抽头输出；房间大小 / 衰减 / 阻尼 / 带宽 / 密度 / 预延迟 / 调制可调 |
| `compressor` | 全声道联动 RMS 检测，软膝静态曲线，dB 域 attack/release 平滑，makeup 增益 |
| `wide` | 线性相位 FIR 分频（Kaiser，抽头随采样率与分频点缩放）→ 低频直通、高频 M/S；中置走空气吸收（4k–5.5k 高架 + 10k–16k 二阶 Bessel 低通，按 f² 物理曲线）；侧通道动态增益（10 ms 攻击 / 120 ms 释放）+ 双路全通 / ITD 去相关（限 1.5 kHz 以上泛音区）；增量 tanh 限幅后按 `mix` 渗入 |
| `loudness` | 等响度补偿：按目标 / 参考 phon 以 1/3 倍频程 GraphicEq **近似** ISO 226 等响曲线（简化实现，非完整查表；完整查表与曲线拟合见 `CHANGELOG.md` 的 roadmap） |

支持面（逐行注明实现位置，便于复核）：

| 项 | 范围 | 实现位置 |
|---|---|---|
| 采样率 | 44.1 – 192 kHz | `object/apo/negotiate.rs`（范围校验） |
| 通道数 | 1 – 8（含 7.1） | 同上；通道名由 `dwChannelMask` 推导 |
| 位深 | 引擎侧 16 / 24 / 32-bit；APO 内部与连接格式为 32-bit float | `pipeline/context.rs`；`negotiate.rs` 校验 `WAVE_FORMAT_IEEE_FLOAT` |
| 音频方向 | 播放与采集端点 | `pipeline/context.rs`（`DeviceType::Render / Capture`） |
| 安装模式 | `LfxGfx`（Win8.1+ 传统槽位）/ `SfxMfx`（Win11 蓝牙组合）/ `SfxEfx`（默认） | `install/device/slots`、`install/selector` |
| 系统要求 | Windows 8.1+；LFX/GFX 需 8.1+，SFX 槽位需 Win10+，蓝牙 MFX 需 Win11 | `install/device/info.rs`（`is_windows_version_at_least(6,3,9600)`） |

## 7 · 性能

- **PEQ 初始化启用 SIMD 点积（AVX2 + FMA）**：1024 抽头直通链由标量 **3.2 ms / 480 帧**
  降至 **0.1 ms**，消除初始化期实时欠载导致的杂音（提交 `c89c959`）。
- **RT 路径零分配**：`buffer` / `interleave` / `ring` 初始化期预分配、逐帧复用；`process`
  内无堆分配、无加锁、无 I/O、无 panic。
- **FIR 自适应**：`fc ≥ 200 Hz` 段按（频段指纹, 采样率）在进程内缓存最小相位 FIR
  （1024–8192 抽头；≤ 2048 直接卷积，> 2048 分块 FFT）。
- **热重载幂等**：内容未变时按 `spec()` 指纹跳过，不重建 DSP 链。

## 8 · 快速上手

前提：Windows 8.1+ 与 Rust（MSVC）工具链。driver 不单独安装，由 CLI 或 App 部署。

```bash
# 构建驱动 DLL
cargo build --release           # 产物 vxapo_driver.dll

# 安装到目标端点（CLI 自动将自身同级目录下的 vxapo_driver.dll 注册为 COM 类）
vxapo-cli list                                  # 查设备（序号或 {GUID}）
vxapo-cli install -d 0 --mode SfxEfx --verify   # 安装并闭环验证

# 核对变更 / 卸载
vxapo-cli snapshot diff -d 0
vxapo-cli uninstall -d 0
```

- 配置目录：`C:\ProgramData\VxAPO\{端点 GUID}\config.toml`（不存在时使用默认链）。
- 完整命令见 [`../vxapo-cli`](../vxapo-cli) 与 [`../vxapo-app`](../vxapo-app)；
  driver 不提供命令行入口。

## 9 · 三端契约对齐

App / CLI / Driver 共享同一份配置契约，行为必须一致：

- **配置契约**：`version = 1`、顶层 `enabled`、`[meta]`、`[[effects]]`；PEQ 块固定写
  `crossover_hz = 200`；`channels` 声明作用声道（缺省 = 全部）。
- **类型兼容**：driver 自动映射旧类型名——`maximizer` / `leveler` → `compressor`、
  `auralenhancer` → `aural`、`loudnesscorrection` → `loudness`；App 只写当前名。
- **数值边界**：增益 `[-120, +48]` dB，滤波深切地板 -60 dB，拒绝 NaN/inf；限幅仅在内存
  中执行，写回侧限幅由 App 负责，DLL 不写文件。
- **段数上限**：31 段（沿用 GraphicEQ 上限），App 的 `applyPreset` / `addBand` 与 driver
  校验一致。
- **指纹与热重载**：`spec()` 指纹不含 `name` / `group`，因此改名 / 改分组不触发 DSP 重建。
- **延迟上报**：`latency()` 计入 wide FIR 与分块 FFT 的固定延迟；单声道直通。

## 10 · 测试与排障

测试规模与运行方式：

```bash
cargo test              # 491 个测试，覆盖 57 个源文件
cargo build --tests     # 构建测试目标（预期 0 告警）
```

- 部分用例写入 `HKCU\SOFTWARE\VxAPO`（注册表往返），需具备写权限；不触碰 `HKLM` 与真实
  端点槽位。
- `install/device/*` 的枚举用例在无设备或无权限环境下仍返回 `Ok`（可能为空列表）。

排障路径：

| 现象 | 排查方式 |
|---|---|
| 安装后无效果 | `vxapo-cli snapshot diff -d <device>` 确认槽位是否写入；`vxapo-cli list` 查看「槽位失守」标记 |
| 音频异常（爆音 / 杂音） | 确认热重载是否被反复触发（编辑器重复写文件）；内容未变时不应重建链 |
| 需要 RT 实际数据 | RT 转储：`HKLM\SOFTWARE\VxAPO\RtDumpSecs`（DWORD）设为秒数 > 0，前 N 秒逐帧写入 `C:\ProgramData\VxAPO\rt_dump_*.f32`（`[in_L, in_R, out_L, out_R]`）。RT 路径只写内存，落盘在控制线程；分析后应删除该值 |
| 热重载 / 协商过程 | 驱动诊断日志 `diag.log`（路径解析见 `object/apo/config.rs`） |
| 参数范围与默认值 | `vxapo-cli effects schema --json`（与 App 参数界面同源） |

## 11 · 参考与致谢

**Equalizer APO**：逐设备注册 APO 槽位的安装模型、以配置文件驱动 DSP 的思路、31 段
GraphicEQ 上限、事件驱动配置热重载（对齐其 notification thread 模式）、聚合委托语义与
安装后验证流程，均参考
[Equalizer APO](https://sourceforge.net/projects/equalizerapo/) 的公开实践。
**VxAPO 为独立实现，不含 Equalizer APO 任何代码**；Equalizer APO 由 Jonas Thedering
开发，GPL-2.0 许可。

**算法参考**：混响算法依据 Jon Dattorro《Effect Design Part 1》公开论文实现；拓扑正确性
与 ValleyRackFree（GPL-3.0-or-later）及 johnhw/dattoro_reverb（MIT）交叉核对，本仓库为
独立的 Rust 实现。

## 文档与许可

- 项目文档：[`../vxapo-docs`](../vxapo-docs)，模块规范见
  [`../vxapo-docs/driver`](../vxapo-docs/driver)。
- 许可证：GPL-3.0-or-later。

---

<a id="vxapo-driver-english"></a>

# VxAPO Driver

<!-- Badges (TODO): CI status · license · latest release -->

[中文](#vxapo-driver) · [English](#vxapo-driver-english) · [Project overview](../vxapo-docs/overview/en/Project%20Overview.md)

Per-endpoint real-time audio processing inside the Windows audio engine (`audiodg`).
VxAPO Driver registers and is loaded as a standard APO (Audio Processing Object), reads
`C:\ProgramData\VxAPO\{GUID}\config.toml` per endpoint, and processes audio frame by frame.
Configuration is a read-only input: the only write paths are the CLI (elevated) and the App;
the DLL never modifies files.

Reading order: engineering value → positioning → architecture → real-time contracts →
configuration → effects and support surface → performance → getting started →
cross-component contracts → testing and troubleshooting → references.

## 1 · Engineering value

Core claim: **driver-level real-time processing in the Windows audio chain can be
implemented entirely in Rust while respecting the existing COM/APO contracts rather than
bypassing them.** Each statement below is verifiable:

| Aspect | Conclusion | Evidence |
|---|---|---|
| System integration | Registered as a standard APO and loaded by the engine in aggregated mode (`pUnkOuter` non-null) | Registry entry `HKLM\SOFTWARE\…\AudioEngine\AudioProcessingObjects\{CLSID}`; implements `IAudioProcessingObject` / `…RT` / `IAudioFormat` / `IPropertyStore`; exports `DllGetClassObject` / `DllRegisterServer`; self-manages instance and lock counts for `DllCanUnloadNow` |
| End-to-end viability | Full install → verify → uninstall cycle on real hardware, symmetric in both directions | `install --verify` performs: stop/start audio service → `CoCreateInstance` → `GetMixFormat` → `Initialize` → `test_pipe` round trip. On the headphones (Octave) endpoint, slots `41C34613…` / `B4A97313…` → `NoValue` → restored and `childApoKeyExists` flips both ways; the device config ends **byte-identical** to its starting state |
| API layer | Typed windows-rs bindings instead of hand-written COM plumbing | APO interfaces come from windows-rs 0.62 `Win32::Media::Audio::Apo` (including `*_Impl` traits); `ClassFactory` uses `#[implement]` to generate vtables and reference counting |
| `unsafe` governance | Concentrated at the FFI boundary, documented per site | 392 occurrences across 27 files: `aggregate.rs` 108 · `audiodg.rs` 57 · `process.rs` 31 · `child.rs` 30 · `factory.rs` 27 …, with 126 SAFETY notes |
| Real-time constraints | Expressed as compile-time constraints, not runtime conventions | `unsafe trait RtSafe` / `RtCopy` encode "no allocation, no locking, no I/O, no panics" as type constraints; `RealtimeContext` is a zero-sized compile-time witness; `panic = "abort"`; zero-allocation processing chain |
| Sustainability | Reproducible baseline | 491 tests across 57 source files; `cargo build` and `cargo build --tests` both report zero warnings; hot reload is idempotent via `spec()` fingerprints |

**Scope note**: the only hand-written vtable offsets are in the COM aggregate shell
(`object/apo/aggregate.rs`) — the Windows engine requires fixed multi-interface offsets
there; every such site documents its SAFETY and layout rationale. All other COM work uses
windows-rs declarative interfaces.

## 2 · Positioning: trade-offs versus Equalizer APO

Both projects address the same problem — system-wide, per-endpoint, scriptable audio
processing — with different engineering trade-offs. Only the differences are listed:

| Aspect | Equalizer APO | VxAPO |
|---|---|---|
| Config carrier | Text config (`config.txt` syntax) | One TOML per endpoint: `C:\ProgramData\VxAPO\{GUID}\config.toml` |
| Applying config | Effective after reload/restart | Event-driven hot reload (`FindFirstChangeNotificationW`, 10 ms dedup) with dual-chain transition to suppress clicks |
| UI | Standalone GUI editor | Tauri 2 + React 19 desktop app (parameter / semantic views) plus CLI |
| Device model | APO installed per endpoint, config organised per device file | Same, plus detection / migration / cleanup of stale GUIDs (device instance ID as stable identity) |
| PEQ strategy | Multi-band EQ (incl. GraphicEQ) | Hybrid PEQ: IIR (RBJ) below 200 Hz, linear-phase FIR at/above 200 Hz (1024–8192 taps, partitioned FFT above 2048), ≤ 31 bands |
| Effects | Text directives driven by config syntax | 7 built-in effects with range / step / default contracts (see §6) |
| License | GPL-2.0 | GPL-3.0-or-later, independent implementation (no EAPO code) |

The difference follows from the configuration model: EAPO expresses the signal chain as text
directives, while VxAPO uses structured contracts (the driver publishes the parameter table,
the CLI relays it, the App generates its UI) to keep all three components consistent (§9).

## 3 · Architecture

Chain and data flow:

```text
┌───────────────────────────┐
│ vxapo-app                 │  UI: React 19 + Tauri 2
└─────────────┬─────────────┘
              │  config.toml (hot-reloaded)
              ▼
┌───────────────────────────┐
│ vxapo-cli                 │  enumeration · install/uninstall · snapshot · verify
└─────────────┬─────────────┘
              │  HKLM slots · CLSID binding · child-APO records
              ▼
┌───────────────────────────┐
│ audiodg.exe               │  audio engine (real-time thread)
└───────────────────────────┘
              ^ loads vxapo_driver.dll: standard APO, per-frame processing
```

Inside the driver, code is split by real-time eligibility:

```text
Real-time path (per frame; no allocation, locking, I/O or panics)
┌──────────────────────────────────────────────────────────────┐
│ pipeline/realtime/  RtSafe / RtCopy contracts, witness       │
│ pipeline/dsp/       effect chain (7 effects, section 6)      │
└──────────────────────────────────────────────────────────────┘

Control path (initialization, hot reload, diagnostics; allocation allowed)
┌──────────────────────────────────────────────────────────────┐
│ object/apo/         APO object, aggregate shell, child APO,  │
│                     negotiation, hot reload, RT dump         │
│ config/             chain model, validation, watcher         │
│ install/            enumeration, slots, transaction, stale   │
│ sys/                COM, registry, audio-format constants    │
│ telemetry/          logging, panic records                   │
│ utils/              ring buffer, alignment, GUID, errors     │
└──────────────────────────────────────────────────────────────┘
```

The real-time path depends only on `pipeline/`; `install/` and `sys/` are entered during
initialization and installation only. Module responsibilities and dependency direction:
[`../vxapo-docs/driver`](../vxapo-docs/driver).

## 4 · Real-time contracts (`src/pipeline/realtime/`)

Real-time safety is expressed in the type system rather than by convention:

- `unsafe trait RtSafe` / `RtCopy` require that every `&self` / `&mut self` method of an
  implementor performs no allocation, acquires no lock, performs no I/O and does not panic;
  all DSP filters satisfy this.
- `RealtimeContext` is a zero-sized witness with no fields and no user-reachable constructor,
  passed by reference along the RT harness stack. Its presence on the call path means "this
  configuration serves the real-time path", moving runtime assertions to compile time.
- Denormal protection: hardware FTZ/DAZ is set at the RT entry (`math.rs`), with an
  `is_finite` guard on the output side.
- Zero-allocation chain: `buffer` / `interleave` / `ring` are pre-allocated during
  initialization and reused every frame.
- Release builds use `panic = "abort"`, so no unwinding call can occur on the RT path.

## 5 · Configuration model and hot reload (`src/config/`)

- Model conversion: TOML `FileModel` → DSP `ChainModel` (`version` / `enabled` / `[meta]` /
  `[[effects]]`); UI metadata such as `name` / `group` is dropped and never enters the DSP
  fingerprint.
- Validation and clamping: channel names, parameter ranges and PEQ band counts (1–31 per
  block, ≤ 31 total) are validated in the config layer; out-of-range values are clamped in
  memory only and never written back.
- Hot reload: `FindFirstChangeNotificationW` watch (10 ms dedup, merging the editor's
  "temp file + rename" pattern) → 128 KB content gate → re-parse → per-item comparison
  against the `active_spec` fingerprint. Unchanged content is skipped; changes switch over
  through a dual-chain transition (`transition.rs`) to avoid clicks.
- Master switch `enabled = false`: whole-chain passthrough, file content preserved but
  excluded from validation.

## 6 · Effects and support surface

| Effect | Implementation |
|---|---|
| `peq` | Hybrid PEQ: IIR (RBJ bilinear) below 200 Hz, linear-phase FIR at/above 200 Hz; partitioned FFT for high sample rates / long IRs; ≤ 31 bands; peaking / low_shelf / high_shelf / low_pass / high_pass |
| `preamp` | Reference-level gain (-120..+48 dB), peak alignment |
| `aural` | Harmonic exciter: 2nd-order Butterworth high-pass → level-independent soft saturation (odd-order tanh + half-wave-rectified even order, DC-blocked) → wet/dry mix |
| `reverb` | Dattorro plate reverb: 4-stage AllPass diffusion plus dual cross-coupled feedback loops (modulated AllPass → main delay → in-loop filtering → diffusion → tail delay) with 14-tap output; room size / decay / damping / bandwidth / density / pre-delay / modulation |
| `compressor` | Linked all-channel RMS detection, soft-knee static curve, dB-domain attack/release smoothing, makeup gain |
| `wide` | Linear-phase FIR crossover (Kaiser; taps scale with sample rate and crossover frequency) → low band bypassed, high band into M/S; center channel through air absorption (4k–5.5k shelf + 10k–16k 2nd-order Bessel low-pass, following the f² physical curve); dynamic side gain (10 ms attack / 120 ms release) plus dual allpass / ITD decorrelation (1.5 kHz+ region only); tanh-limited delta mixed in by `mix` |
| `loudness` | Loudness compensation: 1/3-octave GraphicEq **approximating** the ISO 226 equal-loudness curves by target/reference phon (simplified, no full table lookup; full lookup and curve fitting are tracked in the `CHANGELOG.md` roadmap) |

Support surface (implementation location noted per row for verification):

| Item | Range | Where implemented |
|---|---|---|
| Sample rate | 44.1 – 192 kHz | `object/apo/negotiate.rs` (range check) |
| Channels | 1 – 8 (incl. 7.1) | same; channel names derived from `dwChannelMask` |
| Bit depth | 16 / 24 / 32-bit on the engine side; 32-bit float internally and on the connection format | `pipeline/context.rs`; `negotiate.rs` checks `WAVE_FORMAT_IEEE_FLOAT` |
| Direction | Render and capture endpoints | `pipeline/context.rs` (`DeviceType::Render / Capture`) |
| Install mode | `LfxGfx` (Win8.1+ legacy slots) / `SfxMfx` (Win11 Bluetooth) / `SfxEfx` (default) | `install/device/slots`, `install/selector` |
| OS | Windows 8.1+; LFX/GFX need 8.1+, SFX slots need Win10+, Bluetooth MFX needs Win11 | `install/device/info.rs` (`is_windows_version_at_least(6,3,9600)`) |

## 7 · Performance

- **SIMD dot products (AVX2 + FMA) enabled during PEQ initialization**: a 1024-tap
  pass-through chain dropped from **3.2 ms / 480 frames** (scalar) to **0.1 ms**, removing
  the audible artefact caused by real-time underruns during initialization (commit
  `c89c959`).
- **Zero-allocation RT path**: buffers / interleave / ring are pre-allocated at init and
  reused per frame; `process` performs no heap allocation, locking, I/O or panics.
- **Adaptive FIR**: bands with `fc ≥ 200 Hz` generate min-phase FIRs cached in-process per
  (band fingerprint, sample rate) — 1024–8192 taps; direct convolution up to 2048,
  partitioned FFT above.
- **Idempotent hot reload**: unchanged content is skipped by `spec()` fingerprint, so the
  DSP chain is not rebuilt.

## 8 · Getting started

Prerequisites: Windows 8.1+ and a Rust (MSVC) toolchain. The driver is not installed on its
own; the CLI or App deploys it.

```bash
# Build the driver DLL
cargo build --release           # produces vxapo_driver.dll

# Install onto a target endpoint (the CLI registers the vxapo_driver.dll
# sitting next to its own executable as a COM class)
vxapo-cli list                                  # find the device (index or {GUID})
vxapo-cli install -d 0 --mode SfxEfx --verify   # install with closed-loop verification

# Inspect changes / uninstall
vxapo-cli snapshot diff -d 0
vxapo-cli uninstall -d 0
```

- Config directory: `C:\ProgramData\VxAPO\{endpoint GUID}\config.toml` (a default chain is
  used when absent).
- Full command reference: [`../vxapo-cli`](../vxapo-cli) and
  [`../vxapo-app`](../vxapo-app); the driver has no CLI entry point.

## 9 · Cross-component contract alignment

App, CLI and Driver share one configuration contract and must stay consistent:

- **Config contract**: `version = 1`, top-level `enabled`, `[meta]`, `[[effects]]`; PEQ
  blocks always carry `crossover_hz = 200`; `channels` declares the scope (default = all).
- **Type compatibility**: the driver maps legacy names automatically — `maximizer` /
  `leveler` → `compressor`, `auralenhancer` → `aural`, `loudnesscorrection` → `loudness`;
  the App writes current names only.
- **Numeric bounds**: gain `[-120, +48]` dB, filter deep-cut floor -60 dB, NaN/inf rejected;
  clamping is in-memory only, write-side clamping is the App's responsibility, and the DLL
  never writes files.
- **Band limit**: 31 (inherited from GraphicEQ); the App's `applyPreset` / `addBand` and the
  driver's validation enforce the same limit.
- **Fingerprint and hot reload**: `spec()` fingerprints exclude `name` / `group`, so renaming
  or regrouping does not rebuild the DSP chain.
- **Latency reporting**: `latency()` accounts for the fixed delay of the wide FIR and
  partitioned FFT; mono passthrough.

## 10 · Testing and troubleshooting

Scale and invocation:

```bash
cargo test              # 491 tests across 57 source files
cargo build --tests     # build test targets (expect zero warnings)
```

- Some cases write `HKCU\SOFTWARE\VxAPO` (registry round trips) and need write access; they
  do not touch `HKLM` or real endpoint slots.
- Enumeration cases under `install/device/*` still return `Ok` (possibly an empty list)
  without devices or permissions.

Troubleshooting paths:

| Symptom | What to check |
|---|---|
| Installed but inaudible effect | `vxapo-cli snapshot diff -d <device>` to confirm slots were written; `vxapo-cli list` for "slot lost" markers |
| Audio artefacts (clicks / noise) | Check whether hot reload fires repeatedly (an editor rewriting the file); the chain should not rebuild when content is unchanged |
| Need real RT data | RT dump: set `HKLM\SOFTWARE\VxAPO\RtDumpSecs` (DWORD) to N seconds > 0; the first N seconds are written frame by frame to `C:\ProgramData\VxAPO\rt_dump_*.f32` (`[in_L, in_R, out_L, out_R]`). The RT path writes memory only; flushing happens on the control thread. Remove the value afterwards |
| Hot-reload / negotiation details | Driver diagnostics log `diag.log` (path resolution in `object/apo/config.rs`) |
| Parameter ranges and defaults | `vxapo-cli effects schema --json` (same source as the App parameter UI) |

## 11 · References and acknowledgments

**Equalizer APO**: the per-device APO slot installation model, config-file-driven DSP, the
31-band GraphicEQ limit, event-driven config hot reload (aligned with its notification thread
pattern), aggregate delegation semantics and the post-install verification workflow all
reference the public practice of
[Equalizer APO](https://sourceforge.net/projects/equalizerapo/).
**VxAPO is an independent implementation and contains no Equalizer APO code**; Equalizer APO
is developed by Jonas Thedering and licensed under GPL-2.0.

**Algorithm references**: the reverb follows Jon Dattorro's public paper "Effect Design
Part 1"; its topology was cross-checked against ValleyRackFree (GPL-3.0-or-later) and
johnhw/dattoro_reverb (MIT). The implementation in this repository is independent Rust.

## Documentation and license

- Project documentation: [`../vxapo-docs`](../vxapo-docs); module reference under
  [`../vxapo-docs/driver`](../vxapo-docs/driver).
- License: GPL-3.0-or-later.
