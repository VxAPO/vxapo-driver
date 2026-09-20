# VxAPO Driver

<!-- 徽章区（待补）：CI 状态 · 许可证 · 最近发布 -->

[中文](#vxapo-driver) · [English](#vxapo-driver-english) · [项目总览](../vxapo-docs/overview/zh/项目概览.md)

在 Windows 音频引擎（`audiodg`）内实现逐端点的实时音频处理。VxAPO Driver 以标准 APO
（Audio Processing Object）形式注册并被引擎加载，按端点读取
`C:\ProgramData\VxAPO\{GUID}\config.toml`，逐帧处理音频流。配置是只读输入：写入路径
仅存在于 CLI（提权）与 App，DLL 不修改任何文件。

阅读顺序：实现要点 → 与 Equalizer APO 的差异 → 架构 → 实时契约 → 配置 → 效果器与支持面 →
性能 → 上手 → 三端契约 → 测试与排障 → 参考。

## 1 · 实现要点

本项目围绕一个问题展开：**驱动级实时音频处理能否完整用 Rust 实现，同时遵守既有
COM/APO 约定而不绕开它们。** 本节只列做法与可核验位置：

| 做法 | 说明 | 可核验位置 |
|---|---|---|
| 按标准 APO 契约实现并被引擎加载 | 实现 `IAudioProcessingObject` / `…RT` / `IAudioFormat` / `IPropertyStore`，在聚合模式（`pUnkOuter` 非空）下工作；导出 `DllGetClassObject` / `DllRegisterServer`，自维护实例与锁计数以支撑 `DllCanUnloadNow` | `object/apo/`、`object/apo/dll_exports.rs`；注册项 `HKLM\SOFTWARE\…\AudioEngine\AudioProcessingObjects\{CLSID}` |
| 安装/卸载走事务，并带安装后自检 | `install --verify`：停/启音频服务 → `CoCreateInstance` → `GetMixFormat` → `Initialize` → `test_pipe` 回环；卸载后槽位与 `childApoKeyExists` 复原，设备配置与起始逐字节一致 | `install/selector/operation.rs`；CLI `vxapo-cli install --verify` |
| 用 windows-rs 类型化绑定代替手写 COM 样板 | APO 接口取自 windows-rs 0.62 `Win32::Media::Audio::Apo`（含 `*_Impl` traits）；`ClassFactory` 由 `#[implement]` 生成 vtable 与引用计数 | `object/apo/factory.rs` |
| `unsafe` 收敛在 FFI 边界并逐处注明 | 392 处 / 27 个文件（`aggregate.rs` 108 · `audiodg.rs` 57 · `process.rs` 31 · `child.rs` 30 · `factory.rs` 27 …），配 126 处 SAFETY 说明；唯一手写 vtable 偏移在 COM 聚合外壳（引擎要求多接口固定 offset 布局），该处注明布局依据 | `src/`（搜索 `unsafe`）、`object/apo/aggregate.rs` |
| 实时约束写进类型系统 | `unsafe trait RtSafe` / `RtCopy` 约束「无分配、无锁、无 I/O、无 panic」；`RealtimeContext` 为零尺寸编译期见证；发布构建 `panic = "abort"`；链上零分配 | `pipeline/realtime/` |
| 以测试与告警作为回归基线 | 491 个测试 / 57 个源文件；`cargo build` 与 `cargo build --tests` 均 0 告警 | `cargo test`、`cargo build --tests` |

## 2 · 与 Equalizer APO 的差异

两者面向同一问题（系统级、逐端点、脚本可驱动的音频处理），实现路线不同。下表只列差异：

| 面 | Equalizer APO | VxAPO |
|---|---|---|
| 配置载体 | `config.txt` 逐行命令语法（`GraphicEQ:` 等） | v9.11 起每端点一份 TOML；旧命令体系整体移除，`vxapo-cli config convert` 做一次性转换 |
| 语法错误处理 | 无冒号行**静默跳过**（`FilterEngine.cpp` 329-330：`pos == -1` 时整行不解析、不报错） | 无冒号 / 多冒号一律 `SyntaxError`，整体失败且不产出 spec（有意更严格） |
| 安装模式探测 | `load()` 396-413（C41-C44）三档自动探测 LfxGfx / SfxMfx / SfxEfx | 移植为 `slots::detect_install_mode`，判定改为「VxAPO CLSID 成对」（EDIFIER 实证：按任意 GUID 占槽会误判 SfxMfx） |
| 子 APO 注册表 | `APP_REGPATH = HKLM\SOFTWARE\EqualizerAPO`（`RegistryHelper.h` 33）、`childApoPath`（`DeviceAPOInfo.cpp` 43）、值名 `PreMixChild` / `PostMixChild`（`DeviceAPOInfo.cpp` 558-563） | 机制相同但**路径隔离**：`HKLM\SOFTWARE\VxAPO\Child APOs\{deviceGuid}`；禁止读写 EAPO 路径，否则污染其安装信息区 |
| 槽位值类型 | 第三方实测写入 `REG_SZ` GUID 字符串 | 双格式兼容（`REG_SZ` / 16 字节 LE `REG_BINARY`），全零 GUID 归一为「无 APO」 |
| 配置热重载 | `notificationThread`：`FindNextChangeNotification` 后 `WaitFor`，以 10 ms 窗口合并编辑器「写临时文件 + rename」 | `config/watcher.rs` 同构：事件驱动（非轮询）+ 10 ms 去重 + `spec()` 指纹幂等跳过 |
| 安装后自检 | `CoCreateInstance` 验证 | 同一机制，扩展为「停/启服务 → `CoCreateInstance` → `GetMixFormat` → `Initialize` → `test_pipe`」闭环 |
| 停服与管道权限 | `ServiceHelper` 停/启序；管道 DACL 授予 `Everyone` | 同款停服序与 DACL |
| 卷积与延迟 | GraphicEQ：对数频率插值 + 最小相位 FIR + 1024 点直接时域卷积；分块卷积 `libHybridConv`；无 child 时 `GetLatency` 返回 0 | 同思路：`< 200 Hz` IIR（RBJ）、`≥ 200 Hz` 线性相位 FIR，> 2048 抽头改分块 FFT；延迟上报见 §9 |
| 设备状态判定 | `DEVICE_STATE_DISABLED` / `DEVICE_STATE_NOTPRESENT` | `is_disabled()` / `is_unplugged()` 同判定 |
| 许可 | GPL-2.0（© Jonas Thedering） | GPL-3.0-or-later，独立实现，不含 EAPO 代码 |

EAPO 侧行为记录见 `vxapo-docs/driver/zh/配置与DSP设计.md` §5「EqualizerAPO 行为参考」与
`模块引用规范/` 各模块规范（`Equalizer 行为文档` 已随 v9.11 移除）。当前保留的可对照机制
为四项：子 APO 创建与委托、安装模式探测、槽位备份与恢复、安装后自检。

## 3 · 架构

driver 内部按实时性划分为两条路径：

| 路径 | 进入时机 | 模块 |
|---|---|---|
| 实时路径 | 每帧（`APOProcess`） | `pipeline/realtime/`（`RtSafe` / `RtCopy` 契约、编译期见证、无锁环形缓冲）、`pipeline/dsp/`（效果器链，7 个效果器见 §6） |
| 控制路径 | 初始化、热重载、诊断 | `object/apo/`（APO 对象、聚合外壳、子 APO、协商、热重载、RT 转储）、`config/`（TOML → 链模型、校验、目录监控）、`install/`（枚举、槽位、安装事务、残留迁移）、`sys/`（COM、注册表、音频格式常量）、`telemetry/`（日志、panic 记录）、`utils/`（环形缓冲、对齐、GUID、错误类型） |

实时路径只依赖 `pipeline/`；`install/` 与 `sys/` 仅在初始化与安装阶段进入。模块树、
依赖规则与三条数据流（安装 / 配置加载 / 实时处理）见
[`../vxapo-docs/driver/zh/架构与模块规范.md`](../vxapo-docs/driver/zh/架构与模块规范.md)。

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

Reading order: implementation notes → differences from Equalizer APO → architecture →
real-time contracts → configuration → effects and support surface → performance →
getting started → cross-component contracts → testing and troubleshooting → references.

## 1 · Implementation notes

The project answers one question: **can driver-level real-time audio processing be written
entirely in Rust while respecting the existing COM/APO contracts instead of bypassing
them.** This section lists the practices and where each can be checked:

| Practice | What it means | Where to check |
|---|---|---|
| Implements the standard APO contracts and is loaded by the engine | Implements `IAudioProcessingObject` / `…RT` / `IAudioFormat` / `IPropertyStore`, works in aggregated mode (`pUnkOuter` non-null); exports `DllGetClassObject` / `DllRegisterServer` and tracks instance/lock counts for `DllCanUnloadNow` | `object/apo/`, `object/apo/dll_exports.rs`; registry entry `HKLM\SOFTWARE\…\AudioEngine\AudioProcessingObjects\{CLSID}` |
| Install/uninstall as transactions with a post-install self-check | `install --verify`: stop/start audio service → `CoCreateInstance` → `GetMixFormat` → `Initialize` → `test_pipe`; after uninstall the slots and `childApoKeyExists` are restored and the device config is byte-identical to its starting state | `install/selector/operation.rs`; CLI `vxapo-cli install --verify` |
| Typed windows-rs bindings instead of hand-written COM plumbing | APO interfaces from windows-rs 0.62 `Win32::Media::Audio::Apo` (incl. `*_Impl` traits); `ClassFactory` uses `#[implement]` for vtables and reference counting | `object/apo/factory.rs` |
| `unsafe` confined to the FFI boundary, documented per site | 392 occurrences / 27 files (`aggregate.rs` 108 · `audiodg.rs` 57 · `process.rs` 31 · `child.rs` 30 · `factory.rs` 27 …) with 126 SAFETY notes; the only hand-written vtable offsets are in the COM aggregate shell (the engine requires fixed multi-interface offsets there), with the layout rationale documented on site | `src/` (search `unsafe`), `object/apo/aggregate.rs` |
| Real-time constraints encoded in the type system | `unsafe trait RtSafe` / `RtCopy` require no allocation, no locking, no I/O and no panics; `RealtimeContext` is a zero-sized compile-time witness; release builds use `panic = "abort"`; zero-allocation chain | `pipeline/realtime/` |
| Tests and warning-free builds as the regression baseline | 491 tests / 57 source files; `cargo build` and `cargo build --tests` report zero warnings | `cargo test`, `cargo build --tests` |

## 2 · Differences from Equalizer APO

Both projects address system-wide, per-endpoint, script-driven audio processing, along
different routes. Only the **differences** are listed:

| Aspect | Equalizer APO | VxAPO |
|---|---|---|
| Config carrier | `config.txt` line-command syntax (`GraphicEQ:` etc.) | One TOML per endpoint since v9.11; the old command set was removed entirely, with `vxapo-cli config convert` for one-time migration |
| Syntax errors | Lines without a colon are **silently skipped** (`FilterEngine.cpp` 329-330: nothing parsed and no error when `pos == -1`) | Missing or extra colons raise `SyntaxError`, fail the whole file and produce no spec (intentionally stricter) |
| Install mode detection | `load()` 396-413 (C41-C44) auto-detects LfxGfx / SfxMfx / SfxEfx | Ported as `slots::detect_install_mode`, with the decision changed to "paired VxAPO CLSIDs" (EDIFIER evidence: arbitrary GUIDs in slots misdetect as SfxMfx) |
| Child APO registry | `APP_REGPATH = HKLM\SOFTWARE\EqualizerAPO` (`RegistryHelper.h` 33), `childApoPath` (`DeviceAPOInfo.cpp` 43), value names `PreMixChild` / `PostMixChild` (`DeviceAPOInfo.cpp` 558-563) | Same mechanism but with an **isolated path**: `HKLM\SOFTWARE\VxAPO\Child APOs\{deviceGuid}`; reading or writing EAPO's path is prohibited (it would corrupt EAPO's install record) |
| Slot value types | `REG_SZ` GUID strings observed from third parties | Accepts both (`REG_SZ` and 16-byte LE `REG_BINARY`) and normalises the all-zero GUID to "no APO" |
| Config hot reload | `notificationThread`: `FindNextChangeNotification` + `WaitFor` with a 10 ms window to merge the editor's "temp file + rename" | `config/watcher.rs` mirrors it: event-driven (not polling), 10 ms dedup, idempotent skip via the `spec()` fingerprint |
| Post-install self-check | `CoCreateInstance` validation | Same mechanism, extended into a loop: stop/start service → `CoCreateInstance` → `GetMixFormat` → `Initialize` → `test_pipe` |
| Service control and pipe ACL | `ServiceHelper` stop/start sequence; pipe DACL grants `Everyone` | Same sequence and DACL |
| Convolution and latency | GraphicEQ: logarithmic frequency interpolation, minimum-phase FIR, 1024-point direct convolution; `libHybridConv` for partitioned convolution; `GetLatency` returns 0 with no child | Same approach: IIR (RBJ) below 200 Hz, minimum-phase FIR at/above 200 Hz, partitioned FFT above 2048 taps; latency reporting in §9 |
| Device state | `DEVICE_STATE_DISABLED` / `DEVICE_STATE_NOTPRESENT` | `is_disabled()` / `is_unplugged()` with the same checks |
| License | GPL-2.0 (© Jonas Thedering) | GPL-3.0-or-later, independent implementation, no EAPO code |

The EAPO-side behaviour is recorded in `vxapo-docs/driver/zh/配置与DSP设计.md` §5
("EqualizerAPO 行为参考") and in the per-module specs under `模块引用规范/` (the
`Equalizer 行为文档` document itself was removed with v9.11). Four comparable mechanisms
remain: child APO creation and delegation, install mode detection, slot backup and restore,
and post-install self-check.

## 3 · Architecture

Inside the driver, code is split by real-time eligibility:

| Path | Entered | Modules |
|---|---|---|
| Real-time | every frame (`APOProcess`) | `pipeline/realtime/` (`RtSafe` / `RtCopy` contracts, compile-time witness, lock-free ring buffer), `pipeline/dsp/` (effect chain; the 7 effects are listed in §6) |
| Control | initialization, hot reload, diagnostics | `object/apo/` (APO object, aggregate shell, child APO, negotiation, hot reload, RT dump), `config/` (TOML → chain model, validation, directory watcher), `install/` (enumeration, slots, install transaction, stale migration), `sys/` (COM, registry, audio-format constants), `telemetry/` (logging, panic records), `utils/` (ring buffer, alignment, GUID, error types) |

The real-time path depends only on `pipeline/`; `install/` and `sys/` are entered during
initialization and installation only. Module tree, dependency rules and the three data flows
(install / config load / real-time processing) are in
[`../vxapo-docs/driver/zh/架构与模块规范.md`](../vxapo-docs/driver/zh/架构与模块规范.md).

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
