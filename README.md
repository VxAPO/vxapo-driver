# VxAPO Driver

<!-- 徽章区（待补）：CI 状态 / 许可证 / 最新发布 -->

[中文](#vxapo-driver) · [English](#vxapo-driver-english) · [项目总览](../vxapo-docs/overview/zh/项目概览.md)

在 Windows 音频引擎里加一层**可编程的实时 DSP**：VxAPO Driver 是运行在 `audiodg` 进程内的
APO（Audio Processing Object）COM DLL，按音频端点加载各自的 `config.toml` 并逐样本处理音频。
**只读配置、不写回**——配置写入口只经 CLI（提权）与 App，DLL 自身从不改文件。

## 生态位：与 Equalizer APO 的关系

同一类问题（系统级、设备级、可脚本化的音频处理），设计取舍不同。下表只列事实与差异：

| 维度 | Equalizer APO | VxAPO |
|---|---|---|
| 配置载体 | 文本配置（`config.txt` 语法） | 每端点一份 TOML：`C:\ProgramData\VxAPO\{GUID}\config.toml` |
| 配置生效 | 重载/重启后生效 | 事件驱动热重载（`FindFirstChangeNotificationW`，10 ms 去抖合并）+ 双链过渡防爆音 |
| 界面 | 独立 GUI 编辑器 | Tauri 2 + React 桌面 App（参数视图 / 语义视图）+ CLI |
| 设备模型 | APO 装到端点，配置按设备文件组织 | 同上，另含旧 GUID 残留的检测/迁移/清理（以设备实例 ID 为稳定身份） |
| PEQ | 多段 EQ（含 GraphicEQ） | 混合 PEQ：`fc < 200 Hz` 走 IIR（RBJ），`≥ 200 Hz` 走线性相位 FIR（1024–8192 抽头，>2048 分块 FFT），≤ 31 段 |
| 效果集 | 由配置语法驱动的文本指令 | 7 个内置效果器（peq / preamp / aural / reverb / compressor / wide / loudness），参数带范围/步进/默认值契约 |
| 许可 | GPL-2.0 | GPL-3.0-or-later，**独立实现**（不含 EAPO 代码） |

> 设计上受益于 Equalizer APO 的公开实践（逐设备槽位安装、配置文件驱动 DSP、31 段上限、
> 热重载的 notification 思路），细节见文末「设计参考与致谢」。

## 这个项目证明了什么

不是"设想用 Rust 重写音频栈"，而是一条**已经跑通的完整链路**：从注册表里的 COM 类，
到 `audiodg` 进程内的实时处理，到装进真设备、真的出声。

| 命题 | 证据（可自行核对） |
|---|---|
| **Rust 能按 Windows 的规矩接管音频链路**，而不是绕开它 | 以标准 APO 形式注册（`HKLM\…\AudioEngine\AudioProcessingObjects\{CLSID}`），由 `audiodg` 在**聚合模式**（`pUnkOuter` 非空）下加载；实现 `IAudioProcessingObject` / `…RT` / `IAudioFormat` / `IPropertyStore`，导出 `DllGetClassObject` / `DllRegisterServer`，自维护 `DllCanUnloadNow` 的实例/锁计数 |
| **真机上真的出声**，不是 mock 出来的 | 真机闭环：`install --verify` → 停/启音频服务 → `CoCreateInstance` + `GetMixFormat` + `Initialize` 建图 → `test_pipe` 回环。本机枚举覆盖 7 台端点（含 4 台采集），并在耳机（Octave）端点跑完装/卸全流程——严格对称（槽位 `41C34613…`/`B4A97313…` → `NoValue` → 复原，`childApoKeyExists` 同向翻转），结束后设备配置**逐字节未变** |
| **用 windows-rs 的抽象，不是把 Rust 当 C** | APO 接口直接取自 windows-rs 0.62 的 `Win32::Media::Audio::Apo`（含 `*_Impl` traits）；`ClassFactory` 用 `#[implement]` 宏生成 vtable 与 COM 引用计数。`unsafe` 集中在 **27 个文件**、**FFI 边界**（`aggregate.rs` 108 · `audiodg.rs` 57 · `process.rs` 31 · `child.rs` 30 · `factory.rs` 27…），配 **126 处 SAFETY 说明** |
| **Rust 的抽象能落到驱动层** | 编译期 RT 契约：`unsafe trait RtSafe` / `RtCopy` 把"禁分配、禁锁、禁 I/O、禁 panic"写成类型约束，`RealtimeContext` 是零尺寸编译期见证——源码原话："**编译期能解决的问题，绝不拖到运行时**"；子 APO 与 COM 引用靠 RAII（`Drop`）释放；错误统一 `Result` + `thiserror`；处理链零分配 |
| **长期可维护** | 491 例测试 / 57 个源文件；`cargo build` 与 `cargo build --tests` 均 **0 告警**；热重载按指纹幂等跳过；旧 GUID 残留的分层匹配、迁移与 ACL 自修复 |

> 唯一必须手写 vtable 偏移的地方是 **COM 聚合外壳**（`object/apo/aggregate.rs`）——
> Windows 引擎要求多接口按固定 offset 布局；该处逐条注明 SAFETY 与布局依据，
> 其余 COM 全部走 windows-rs 的声明式接口与 `#[implement]`。

## 架构

**链路**（谁在什么时候进入画面）：

```text
  ┌─────────────┐
  │  vxapo-app  │  Tauri 2 + React 3 · 参数视图 / 语义视图
  └──────┬──────┘
         │  提权子进程 · config.toml
  ┌──────▼──────┐
  │  vxapo-cli  │  设备枚举 · 安装/卸载 · 配置 · 快照 · 诊断
  └──────┬──────┘
         │  HKLM 槽位 · CLSID 绑定 · 子 APO 记录
  ┌──────▼──────────────┐
  │  Windows audiodg    │  音频引擎（实时线程）
  │   └ vxapo_driver.dll│  以标准 APO 形式被加载，逐帧处理
  └─────────────────────┘
```

**driver 内部两条路径**（右侧为职责）：

```text
  实时路径  每帧 · 禁分配 / 禁锁 / 禁 I/O / 禁 panic
  ├─ pipeline/realtime/   RtSafe · RtCopy 契约 + 编译期见证
  └─ pipeline/dsp/        peq · preamp · aural · reverb · compressor · wide · loudness

  控制路径  初始化 / 热重载 / 诊断 · 可分配可加锁
  ├─ object/apo/          进程 · 聚合外壳 · 子 APO · 协商 · 热重载 · RT 转储
  ├─ config/              TOML → 链模型 · 校验限幅 · 目录监控
  ├─ install/             端点枚举 · 槽位选择 · 安装事务 · 残留迁移/修复
  ├─ sys/                 COM · 注册表 · 音频格式 / APO 常量
  ├─ telemetry/           日志 · panic 记录
  └─ utils/               环形缓冲 · 对齐 · GUID · 错误类型
```

实时路径只依赖 `pipeline/`；`install/` 与 `sys/` 只在初始化/安装阶段进入。
模块职责与依赖方向详见 `../vxapo-docs/driver`。

## 功能与实现

### APO 生命周期（`src/object/`）

- 标准 Windows APO COM 对象：`IAudioProcessingObject` / `IAudioProcessingObjectRT` /
  `IAudioFormat` / `IPropertyStore` 等接口实现，`dll_exports` 导出 `DllGetClassObject` /
  `DllRegisterServer`，自维护引用计数。
- **COM 聚合委托外壳（`aggregate.rs`）**：Windows 音频引擎以聚合模式（`pUnkOuter` 非空）
  创建 APO。实现按标准 COM 多接口 offset 布局：`repr(C)` 结构体持有 4 个独立 vtable
  指针字段（IAPO / RT / Config / ASE），`QI` 返回对应字段地址、stub 方法用偏移还原对象
  基址；`AddRef` / `Release` 自维护（NonDelegating 语义）；各接口方法转发到内部
  `ApoObject`，复用全部 DSP 逻辑。
- **子 APO 委托（`child.rs`）**：安装器选择保留原 APO 为子 APO 时，`Initialize` 阶段
  `CoCreateInstance` 创建子实例，持有三个类型化接口（`IAudioProcessingObject` /
  `IAudioProcessingObjectRT` / `IAudioProcessingObjectConfiguration`）；延迟、重置、
  帧数计算、Lock/Unlock 全部委托给子 APO；GUID 来自
  `HKLM\SOFTWARE\VxAPO\Child APOs\{deviceGuid}\{PreMix|PostMix}`；委托失败降级为无子
  APO 不阻塞父链，`Drop` 自动释放引用。
- 会话与格式化协商：`IsFormatSupported` / `LockForProcess` / `UnlockForProcess`，
  采样率/通道/位深变化时重建链；`lock_key` 与 `test_pipe` 支撑验证闭环。

### 实时安全（`src/pipeline/realtime/`）

- `RtSafe` / `RtCopy` 标记契约：`process` 路径禁止堆分配、禁止加锁、禁止 I/O、
  禁止 panic；所有 DSP 滤波器满足该约束。
- 非规格化（denormal）防护：RT 入口设置硬件 FTZ/DAZ（`math.rs`），
  输出端 `is_finite` 兜底，防止非有限值泄漏。
- 处理链零分配：`buffer` / `interleave` / `ring` 在初始化阶段预分配，每帧复用。

### 配置与热重载（`src/config/`）

- TOML 文件模型 `FileModel` → DSP 链模型 `ChainModel`：`version` / `enabled` /
  `[meta]` / `[[effects]]`；`name` / `group` 等 APP 元数据在转换时丢弃。
- 校验与限幅：声道名、参数范围、PEQ 段数（单块 1–31，全局合计 ≤ 31）均在 config 层完成；
  越界值在内存中 clamp，**不写回文件**。
- 热重载：Win32 事件驱动目录监控（`FindFirstChangeNotificationW`，10ms 去抖合并
  编辑器“临时文件 + rename”），128KB 内容闸门 → 重新解析 → 与 `active_spec`
  指纹逐项比对，内容未变幂等跳过；切换时双链过渡（`transition.rs`）避免爆音。
- 总开关 `enabled = false`：整链 passthrough，文件内容保留但不再参与校验。

### DSP 效果器（`src/pipeline/dsp/`）

| 效果器 | 实现要点 |
|---|---|
| `peq` | 混合 PEQ：`fc < 200Hz` 走 IIR（RBJ 双线性），`≥ 200Hz` 走线性相位 FIR；高采样率/长 IR 走分块 FFT；≤ 31 段，支持 peaking / low_shelf / high_shelf / low_pass / high_pass |
| `preamp` | 基准电平增益（-120..+48 dB），峰值对齐 |
| `aural` | 谐波激励器：二阶 Butterworth 高通提取频段 → 电平独立软饱和（tanh 奇次 + 半波整流偶次，DC 阻塞）→ 湿干混合 |
| `reverb` | Dattorro 板式混响（1997 论文）：4 级 AllPass 扩散 + 双槽交叉反馈环路（调制 AllPass → 主延迟 → 槽内滤波 → 扩散 → 尾延迟）+ 14 抽头输出；房间大小/衰减/阻尼/带宽/密度/预延迟/调制可调 |
| `compressor` | 全声道联动 RMS 检测，软膝静态曲线，dB 域 attack/release 平滑，makeup 增益 |
| `wide` | 声场处理：线性相位 FIR 分频（Kaiser，抽头随采样率/分频点缩放）→ 低频直通、高频 M/S；中置走空气吸收（4k–5.5k 高架 + 10k–16k 二阶 Bessel 低通，按 f² 物理曲线）；侧通道动态增益（10ms 攻击/120ms 释放）+ 双路全通/ITD 去相关（仅 1.5kHz 以上泛音区）；增量 tanh 限幅后按 `mix` 渗入 |
| `loudness` | 等响度补偿：按目标/参考 phon 以 1/3 倍频程 GraphicEq **近似** ISO 226 等响曲线（简化实现，非完整查表；完整查表与曲线拟合见 `CHANGELOG.md` 的 roadmap） |

### 支持矩阵

| 项 | 范围 | 实现位置 |
|---|---|---|
| 采样率 | 44.1 – 192 kHz | `object/apo/negotiate.rs`（范围校验） |
| 通道数 | 1 – 8（含 7.1） | 同上；通道名按 `dwChannelMask` 推导 |
| 位深 | 引擎侧 16 / 24 / 32-bit；APO 内部与连接格式为 32-bit float | `pipeline/context.rs`；`negotiate.rs` 校验 `WAVE_FORMAT_IEEE_FLOAT` |
| 音频方向 | 播放与采集端点 | `pipeline/context.rs`（`DeviceType::Render / Capture`） |
| 安装模式 | `LfxGfx`（Win8.1+ Legacy 槽位）/ `SfxMfx`（Win11 蓝牙组合）/ `SfxEfx`（默认） | `install/device/slots`、`install/selector` |
| 系统 | Windows 8.1+；LFX/GFX 需 Win8.1+，SFX 槽位 Win10+，蓝牙 MFX Win11 | `install/device/info.rs`（`is_windows_version_at_least(6,3,9600)`） |

### 性能

- **PEQ 初始化启用 SIMD 点积（AVX2+FMA）**：1024 抽头直通链由标量 **3.2 ms/480 帧**
  降到 **0.1 ms**，消除了初始化期实时欠载导致的杂音（提交 `c89c959`）。
- **RT 路径零分配**：`buffer` / `interleave` / `ring` 初始化阶段预分配、逐帧复用；
  `process` 内不做堆分配、不加锁、不做 I/O、不 panic。
- **FIR 自适应**：`fc ≥ 200 Hz` 段按（频段指纹, 采样率）在进程内缓存生成最小相位 FIR
  （1024–8192 抽头，≤ 2048 直接卷积 / > 2048 分块 FFT）。
- **热重载幂等**：内容未变时按 `spec()` 指纹跳过，不重建 DSP 链。

### 安装与验证（`src/install/`）

- 端点枚举与槽位选择：`LfxGfx` / `SfxMfx` / `SfxEfx`，支持保留原 APO 为子 APO
  （`--no-child` 关闭）。
- 注册表写入走统一事务层（driver 是唯一写入口），安装后可通过 `verify`
  （CoCreateInstance + 格式协商）闭环验证。

## 快速上手

前提：Windows 8.1+ 与 Rust（MSVC）工具链。driver 不单独安装，由 CLI / App 部署：

```bash
# 1) 构建驱动 DLL
cargo build --release           # 产物 vxapo_driver.dll

# 2) 用 CLI 装到目标端点（CLI 会自动把 exe 同级的 vxapo_driver.dll 注册为 COM 类）
vxapo-cli list                                  # 找设备（序号或 {GUID}）
vxapo-cli install -d 0 --mode SfxEfx --verify   # 安装并闭环验证

# 3) 查看变更 / 卸载
vxapo-cli snapshot diff -d 0
vxapo-cli uninstall -d 0
```

- 配置目录：`C:\ProgramData\VxAPO\{端点 GUID}\config.toml`（不存在时使用默认链）。
- 完整命令见 `../vxapo-cli` 与 `../vxapo-app`；driver 本身不提供命令行入口。

## 与 App / CLI 的行为对齐

VxAPO 三层（App / CLI / Driver）共享同一份 config 契约，行为必须一致：

- **配置契约**：`version = 1`、顶层 `enabled`、`[meta]`、`[[effects]]`；
  PEQ 块固定写 `crossover_hz = 200`；`channels` 声明作用声道（缺省 = 全部）。
- **类型兼容**：driver 侧旧类型自动映射——`maximizer` / `leveler` → `compressor`、
  `auralenhancer` → `aural`、`loudnesscorrection` → `loudness`；App 不再写旧名。
- **数值边界**：增益 `[-120, +48]` dB、滤波深切地板 -60 dB、NaN/inf 拒绝；
  限幅只在内存做，写回限幅由 App 负责，DLL 从不写文件。
- **段数上限**：31 段（沿用 GraphicEQ 上限），App 的 `applyPreset` / `addBand`
  与 driver config 校验一致。
- **指纹与热重载**：`spec()` 生成的指纹不含 `name` / `group` 等 UI 元数据，
  因此改名/改分组不会触发 DSP 重建。
- **延迟报告**：`latency()` 计入 wide FIR 与分块 FFT 的固定延迟；单声道直通。

## 测试与排障

**测试**：`cargo test` 共 **491 例**，分布在 **57 个源文件**（内联 `#[cfg(test)]` 或同目录
`tests.rs` 挂载）：

```bash
cargo test              # 全部测试
cargo build --tests     # 构建测试目标（应为 0 告警）
```

- 部分用例会写 `HKCU\SOFTWARE\VxAPO`（注册表读写），请在有写权限的账户下运行；
  这些用例不触碰 `HKLM` 与真实端点槽位。
- `install/device/*` 的枚举用例在无设备/无权限环境下也返回 `Ok`（可能为空列表）。

**排障**：

| 现象 | 排查方式 |
|---|---|
| 装了但没效果 | `vxapo-cli snapshot diff -d <device>` 看槽位是否写入；`vxapo-cli list` 看「槽位失守」标记 |
| 声音异常（爆音/杂音） | 先确认热重载是否频繁触发（编辑器反复写文件）；内容未变时不应重建链 |
| 需要 RT 实际数据 | RT 转储：`HKLM\SOFTWARE\VxAPO\RtDumpSecs`（DWORD）设为秒数 > 0，前 N 秒逐帧写入 `C:\ProgramData\VxAPO\rt_dump_*.f32`（`[in_L,in_R,out_L,out_R]`）；RT 路径只写内存，落盘在控制线程。分析完记得删除该值 |
| 热重载/协商过程细节 | 驱动诊断日志 `diag.log`（路径解析见 `object/apo/config.rs`） |
| 参数范围/默认值疑问 | `vxapo-cli effects schema --json`（与 App 参数 UI 同源） |

## 模块划分（2026-09 重构后）

- `object/apo/`：`process.rs`（RT 处理）、`config.rs`（配置路径解析 + `diag.log` 输出）、
  `reload.rs`（热重载编排 + watcher）、`rtdump.rs`（RT 转储诊断）、`negotiate.rs`、`state.rs`。
- `pipeline/dsp/specs.rs`：**效果器参数表**（每个参数的范围/步进/精确默认值/单位，默认值
  运行时取自各 `*Params::default()`），同时供 cli `effects schema` 与 App 参数 UI 生成使用。
- `install/selector/operation/`：`execute.rs`（安装/卸载/迁移执行 + 事务回滚）、
  `capx.rs`（CAPX 设备默认效果接管）、`helpers.rs`（注册表写入辅助）。
- `install/device/`：`stale/`（旧 GUID 残留：分层匹配 / ACL / 迁移）、
  `slots/`（槽位与子 APO 读写）。
- `config/model/` 按子模块拆分；`utils/ring.rs` 为环形缓冲（telemetry 不再依赖 pipeline）。
- `CHANGELOG.md` 记录版本与阶段变更；两种构建（`cargo build` / `cargo build --tests`）均为 **0 告警**。

## 设计参考与致谢

**Equalizer APO**：VxAPO 的许多设计决策受到
[Equalizer APO](https://sourceforge.net/projects/equalizerapo/) 的启发，包括逐设备注册
APO 槽位的安装模型、以配置文件驱动 DSP 的思路、31 段 GraphicEQ 上限、事件驱动配置热重载
（对齐 EAPO 的 notification thread 模式）、聚合委托语义以及安装后的验证流程。
**VxAPO 是独立实现，不包含 Equalizer APO 的任何代码**；
Equalizer APO 由 Jonas Thedering 开发，GPL-2.0 许可。

**算法参考**：混响算法依据 Jon Dattorro《Effect Design Part 1》公开论文实现，
拓扑正确性与 ValleyRackFree（GPL-3.0-or-later）及 johnhw/dattoro_reverb（MIT）交叉核对，
本仓库内为独立的 Rust 实现。

## 构建

```bash
cargo build --release
```

产物为 `vxapo_driver.dll`（`cdylib`）。release 使用 `lto` / `codegen-units=1` /
`panic=abort` 以满足 RT 约束。

## 文档

项目文档见 `../vxapo-docs`，详细模块规范见 `../vxapo-docs/driver`。

## 许可证

GPL-3.0-or-later

---

<a id="vxapo-driver-english"></a>

# VxAPO Driver

<!-- Badge area (TODO): CI status / license / latest release -->

[中文](#vxapo-driver) · [English](#vxapo-driver-english) · [Project overview](../vxapo-docs/overview/en/Project%20Overview.md)

A **programmable real-time DSP layer inside the Windows audio engine**: VxAPO Driver is an APO
(Audio Processing Object) COM DLL that runs inside `audiodg`, loads a per-endpoint
`config.toml`, and processes audio sample by sample. It is **read-only regarding
configuration** — writes go through the CLI (elevated) and the App; the DLL itself never
touches files.

## Niche: relationship to Equalizer APO

Same class of problem (system-wide, per-device, scriptable audio processing), different
trade-offs. The table states facts and differences only:

| Aspect | Equalizer APO | VxAPO |
|---|---|---|
| Config carrier | Text config (`config.txt` syntax) | One TOML per endpoint: `C:\ProgramData\VxAPO\{GUID}\config.toml` |
| Applying config | Effective after reload/restart | Event-driven hot reload (`FindFirstChangeNotificationW`, 10 ms dedup) + dual-chain transition to avoid clicks |
| UI | Standalone GUI editor | Tauri 2 + React desktop App (parameter view / semantic view) + CLI |
| Device model | APO installed onto endpoints, config per device file | Same, plus stale-GUID detection/migration/cleanup (device instance ID as stable identity) |
| PEQ | Multi-band EQ (incl. GraphicEQ) | Hybrid PEQ: IIR (RBJ) below 200 Hz, linear-phase FIR at/above 200 Hz (1024–8192 taps, partitioned FFT above 2048), ≤ 31 bands |
| Effects | Text directives driven by config syntax | 7 built-in effects (peq / preamp / aural / reverb / compressor / wide / loudness) with range/step/default contracts |
| License | GPL-2.0 | GPL-3.0-or-later, **independent implementation** (no EAPO code) |

> Design decisions were informed by Equalizer APO's public practice (per-device slot
> installation, config-file-driven DSP, the 31-band limit, notification-based hot reload) —
> see "Design references & acknowledgments".

## What this project demonstrates

It is not "a plan to rewrite the audio stack in Rust" — it is a **working end-to-end chain**:
from a COM class in the registry, into real-time processing inside `audiodg`, onto real
devices that actually play audio.

| Claim | Evidence (verifiable) |
|---|---|
| **Rust can take over the Windows audio chain on Windows' own terms**, not around them | Registered as a standard APO (`HKLM\…\AudioEngine\AudioProcessingObjects\{CLSID}`) and loaded by `audiodg` in **aggregated mode** (`pUnkOuter` non-null); implements `IAudioProcessingObject` / `…RT` / `IAudioFormat` / `IPropertyStore`, exports `DllGetClassObject` / `DllRegisterServer`, self-manages `DllCanUnloadNow` instance/lock counts |
| **It really plays audio on real hardware** — nothing mocked | Real-machine loop: `install --verify` → stop/start audio service → `CoCreateInstance` + `GetMixFormat` + `Initialize` (graph build) → `test_pipe` round trip. Enumeration covered 7 endpoints (4 capture) on this machine, and a full install/uninstall cycle was run on the headphones (Octave) endpoint — strictly symmetric (slots `41C34613…`/`B4A97313…` → `NoValue` → restored, `childApoKeyExists` flips both ways), with the device config **byte-identical** afterwards |
| **It uses windows-rs abstractions — it does not use Rust as C** | APO interfaces come straight from windows-rs 0.62 `Win32::Media::Audio::Apo` (incl. `*_Impl` traits); `ClassFactory` uses the `#[implement]` macro to generate vtables and COM reference counting. `unsafe` is concentrated in **27 files** at the **FFI boundary** (`aggregate.rs` 108 · `audiodg.rs` 57 · `process.rs` 31 · `child.rs` 30 · `factory.rs` 27…), with **126 SAFETY notes** |
| **Rust abstractions reach down to driver level** | Compile-time RT contracts: `unsafe trait RtSafe` / `RtCopy` turn "no allocation, no locking, no I/O, no panics" into type constraints, with a zero-sized `RealtimeContext` witness — in the source's own words: "**anything the compiler can settle never goes to runtime**". Child APO and COM references are released via RAII (`Drop`); errors are uniform `Result` + `thiserror`; the processing chain is zero-allocation |
| **Maintainable over time** | 491 tests across 57 source files; `cargo build` and `cargo build --tests` both report **zero warnings**; hot reload skips no-op changes by fingerprint; stale-GUID layering, migration and ACL self-repair are implemented |

> The only place that hand-writes vtable offsets is the **COM aggregate shell**
> (`object/apo/aggregate.rs`) — the Windows engine requires multi-interface fixed-offset
> layout there. Every such site documents its SAFETY and layout rationale; all other COM
> work goes through windows-rs declarative interfaces and `#[implement]`.

## Architecture

**The chain** (who enters when):

```text
  ┌─────────────┐
  │  vxapo-app  │  Tauri 2 + React 3 · parameter / semantic views
  └──────┬──────┘
         │  elevated subprocess · config.toml
  ┌──────▼──────┐
  │  vxapo-cli  │  enumeration · install/uninstall · config · snapshots · diagnostics
  └──────┬──────┘
         │  HKLM slots · CLSID binding · child APO records
  ┌──────▼──────────────┐
  │  Windows audiodg    │  audio engine (real-time thread)
  │   └ vxapo_driver.dll│  loaded as a standard APO, processes every frame
  └─────────────────────┘
```

**The two paths inside the driver** (right column: responsibilities):

```text
  Real-time path   every frame · no alloc / no locks / no I/O / no panics
  ├─ pipeline/realtime/   RtSafe · RtCopy contracts + compile-time witness
  └─ pipeline/dsp/        peq · preamp · aural · reverb · compressor · wide · loudness

  Control path   init / hot reload / diagnostics · allocation and locks allowed
  ├─ object/apo/          process · aggregate shell · child APO · negotiation ·
  │                       hot reload · RT dump
  ├─ config/              TOML → chain model · validation/clamping · watcher
  ├─ install/             enumeration · slot selection · install transaction ·
  │                       stale migration/repair
  ├─ sys/                 COM · registry · audio-format / APO constants
  ├─ telemetry/           logging · panic records
  └─ utils/               ring buffer · alignment · GUID · error types
```

The real-time path depends only on `pipeline/`; `install/` and `sys/` are entered at
initialization/installation time only. Module responsibilities and dependency direction:
`../vxapo-docs/driver`.

## Features & implementation

### APO lifecycle (`src/object/`)

- Standard Windows APO COM object: `IAudioProcessingObject` / `IAudioProcessingObjectRT` /
  `IAudioFormat` / `IPropertyStore` implementations, `DllGetClassObject` /
  `DllRegisterServer` exports, self-managed reference counting.
- **COM aggregate delegation shell (`aggregate.rs`)**: the Windows audio engine creates
  APOs in aggregated mode (`pUnkOuter` non-null). The implementation follows standard COM
  multi-interface offsets: a `repr(C)` struct holds 4 independent vtable pointer fields
  (IAPO / RT / Config / ASE), `QI` returns the address of the matching field, and stub
  methods recover the base via offsets; `AddRef` / `Release` are self-managed
  (NonDelegating semantics); interface methods forward to the inner `ApoObject`, reusing
  all DSP logic.
- **Child APO delegation (`child.rs`)**: when the installer keeps the original APO as a
  child, `Initialize` creates it via `CoCreateInstance` and holds three typed interfaces
  (`IAudioProcessingObject` / `IAudioProcessingObjectRT` /
  `IAudioProcessingObjectConfiguration`); latency, reset, frame counts, and Lock/Unlock
  are delegated to the child; the GUID comes from
  `HKLM\SOFTWARE\VxAPO\Child APOs\{deviceGuid}\{PreMix|PostMix}`; delegation failures
  degrade to no child without blocking the parent, and `Drop` releases all references.
- Format negotiation: `IsFormatSupported` / `LockForProcess` / `UnlockForProcess`;
  the chain is rebuilt on sample-rate/channel/bit-depth changes; `lock_key` and
  `test_pipe` support the verification loop.

### Real-time safety (`src/pipeline/realtime/`)

- `RtSafe` / `RtCopy` marker contract: the `process` path forbids heap allocation,
  locking, I/O, and panics; all DSP filters comply.
- Denormal protection: hardware FTZ/DAZ set at the RT entry (`math.rs`), with an
  `is_finite` output guard against non-finite leakage.
- Zero-allocation processing chain: buffers/interleave/ring are pre-allocated at
  initialization and reused every frame.

### Config & hot reload (`src/config/`)

- TOML file model `FileModel` → DSP chain model `ChainModel`: `version` / `enabled` /
  `[meta]` / `[[effects]]`; APP metadata (`name` / `group`) is dropped during conversion.
- Validation & clamping: channel names, parameter ranges, and PEQ band count
  (1–31 per block, ≤ 31 total) are validated in the config layer; out-of-range
  values are clamped in memory only, never written back.
- Hot reload: Win32 event-driven directory watcher (`FindFirstChangeNotificationW`,
  10 ms dedup window for "temp file + rename"), 128 KB gate, re-parse, then a
  per-item `spec()` fingerprint comparison — no-op when unchanged; dual-chain
  transition avoids clicks.
- Top-level `enabled = false`: whole-chain passthrough; file content is preserved
  but excluded from validation.

### DSP effects (`src/pipeline/dsp/`)

| Effect | Implementation |
|---|---|
| `peq` | Hybrid PEQ: IIR (RBJ bilinear) below 200 Hz, linear-phase FIR at/above 200 Hz; partitioned FFT for high sample rates / long IRs; ≤ 31 bands (peaking / low_shelf / high_shelf / low_pass / high_pass) |
| `preamp` | Reference-level gain (-120..+48 dB), peak alignment |
| `aural` | Harmonic exciter: 2nd-order Butterworth high-pass → level-independent saturation (tanh odd + half-wave-rectified even, DC-blocked) → wet/dry |
| `reverb` | Dattorro plate reverb (1997 paper): 4-stage AllPass diffusion + dual cross-coupled feedback loops (modulated AllPass → main delay → in-loop filtering → diffusion → tail delay) + 14-tap output; room size / decay / damping / bandwidth / density / pre-delay / modulation |
| `compressor` | Linked all-channel RMS detection, soft-knee static curve, dB-domain attack/release smoothing, makeup gain |
| `wide` | Stereo field processor: linear-phase FIR crossover (Kaiser, taps scale with sample rate / crossover) → low band bypassed, high band into M/S; center air absorption (4k–5.5k shelf + 10k–16k 2nd-order Bessel low-pass, f² physical curve); dynamic side gain (10 ms attack / 120 ms release) + dual allpass/ITD decorrelation (1.5 kHz+ region only); tanh-limited delta mixed via `mix` |
| `loudness` | Loudness compensation: 1/3-octave GraphicEq **approximating** the ISO 226 equal-loudness curves by target/reference phon (simplified, no full table lookup; full table lookup + curve fitting tracked in `CHANGELOG.md` roadmap) |

### Support matrix

| Item | Range | Where implemented |
|---|---|---|
| Sample rate | 44.1 – 192 kHz | `object/apo/negotiate.rs` (range check) |
| Channels | 1 – 8 (incl. 7.1) | same; channel names derived from `dwChannelMask` |
| Bit depth | 16 / 24 / 32-bit on the engine side; 32-bit float internally and on the connection format | `pipeline/context.rs`; `negotiate.rs` checks `WAVE_FORMAT_IEEE_FLOAT` |
| Direction | Render and capture endpoints | `pipeline/context.rs` (`DeviceType::Render / Capture`) |
| Install mode | `LfxGfx` (Win8.1+ legacy slots) / `SfxMfx` (Win11 Bluetooth) / `SfxEfx` (default) | `install/device/slots`, `install/selector` |
| OS | Windows 8.1+; LFX/GFX need Win8.1+, SFX slots Win10+, Bluetooth MFX Win11 | `install/device/info.rs` (`is_windows_version_at_least(6,3,9600)`) |

### Performance

- **SIMD dot products (AVX2+FMA) enabled in PEQ initialization**: a 1024-tap pass-through
  chain went from **3.2 ms / 480 frames** (scalar) to **0.1 ms**, removing the audible
  glitch caused by real-time underruns during initialization (commit `c89c959`).
- **Zero-allocation RT path**: buffers/interleave/ring are pre-allocated at init and
  reused per frame; `process` performs no heap allocation, locking, I/O, or panics.
- **Adaptive FIR**: bands with `fc ≥ 200 Hz` generate min-phase FIRs cached per
  (band fingerprint, sample rate) — 1024–8192 taps, direct convolution ≤ 2048,
  partitioned FFT above.
- **Idempotent hot reload**: unchanged content is skipped by `spec()` fingerprint, so the
  DSP chain is not rebuilt.

### Install & verification (`src/install/`)

- Endpoint enumeration and slot selection: `LfxGfx` / `SfxMfx` / `SfxEfx`, with
  optional preservation of the original APO as a child APO (`--no-child` to disable).
- Registry writes go through a unified transaction layer (the driver is the single
  write path); installs can be closed-loop verified via `verify`
  (CoCreateInstance + format negotiation).

## Quick start

Prerequisites: Windows 8.1+ and a Rust (MSVC) toolchain. The driver is not installed on its
own — the CLI / App deploys it:

```bash
# 1) Build the driver DLL
cargo build --release           # produces vxapo_driver.dll

# 2) Install onto a target endpoint (the CLI auto-registers the vxapo_driver.dll
#    sitting next to its own executable as a COM class)
vxapo-cli list                                  # find the device (index or {GUID})
vxapo-cli install -d 0 --mode SfxEfx --verify   # install + closed-loop verify

# 3) Inspect changes / uninstall
vxapo-cli snapshot diff -d 0
vxapo-cli uninstall -d 0
```

- Config directory: `C:\ProgramData\VxAPO\{endpoint GUID}\config.toml` (default chain when absent).
- Full command reference: `../vxapo-cli` and `../vxapo-app`; the driver has no CLI entry point.

## Alignment with the App / CLI

The three layers (App / CLI / Driver) share one config contract and must stay aligned:

- **Config contract**: `version = 1`, top-level `enabled`, `[meta]`, `[[effects]]`;
  PEQ blocks always carry `crossover_hz = 200`; `channels` declares the scope
  (default = all channels).
- **Type compatibility**: the driver maps legacy names automatically —
  `maximizer` / `leveler` → `compressor`, `auralenhancer` → `aural`,
  `loudnesscorrection` → `loudness`; the App only writes current names.
- **Numerical bounds**: gain `[-120, +48]` dB, filter deep-cut floor -60 dB,
  NaN/inf rejected; clamping is in-memory only, write-side clamping is done by the
  App, and the DLL never writes files.
- **Band limit**: 31 (inherited from GraphicEQ); both the App's
  `applyPreset` / `addBand` and the driver's config validation enforce it.
- **Fingerprint & hot reload**: `spec()` fingerprints exclude `name` / `group`,
  so renaming/regrouping does not rebuild the DSP chain.
- **Latency**: `latency()` accounts for the fixed delay of the wide FIR and
  partitioned FFT; mono passthrough.

## Testing & troubleshooting

**Tests**: `cargo test` runs **491 cases** spread over **57 source files** (inline
`#[cfg(test)]` modules or `tests.rs` next to the implementation):

```bash
cargo test              # everything
cargo build --tests     # build test targets (expected: zero warnings)
```

- Some cases write `HKCU\SOFTWARE\VxAPO` (registry round-trips); run them under an account
  with write access. They do not touch `HKLM` or real endpoint slots.
- Enumeration cases under `install/device/*` return `Ok` (possibly an empty list) even
  without devices or permissions.

**Troubleshooting**:

| Symptom | What to check |
|---|---|
| Installed but no effect | `vxapo-cli snapshot diff -d <device>` to see whether slots were written; `vxapo-cli list` for "slot lost" markers |
| Audio glitches | Check whether hot reload fires repeatedly (editor rewriting the file); the chain should not be rebuilt when content is unchanged |
| Need real RT data | RT dump: set `HKLM\SOFTWARE\VxAPO\RtDumpSecs` (DWORD) to N seconds > 0; the first N seconds are written frame-by-frame to `C:\ProgramData\VxAPO\rt_dump_*.f32` (`[in_L,in_R,out_L,out_R]`). The RT path only writes memory; flushing happens on the control thread. Remove the value afterwards |
| Hot-reload / negotiation details | Driver diagnostics log `diag.log` (path resolution in `object/apo/config.rs`) |
| Parameter ranges / defaults | `vxapo-cli effects schema --json` (same source as the App parameter UI) |

## Module layout (after the 2026-09 refactor)

- `object/apo/`: `process.rs` (RT processing), `config.rs` (config path resolution +
  `diag.log` output), `reload.rs` (hot-reload orchestration + watcher), `rtdump.rs` (RT dump
  diagnostics), `negotiate.rs`, `state.rs`.
- `pipeline/dsp/specs.rs`: the **effect parameter table** (range / step / precise default /
  unit per parameter; defaults are read from each `*Params::default()` at runtime). It feeds
  both the CLI `effects schema` and the app's parameter UI generation.
- `install/selector/operation/`: `execute.rs` (install/uninstall/migrate + transactional
  rollback), `capx.rs` (CAPX default-effect takeover), `helpers.rs` (registry write helpers).
- `install/device/`: `stale/` (stale-GUID layering / ACL / migration), `slots/` (slots and
  child APO I/O).
- `config/model/` split into submodules; `utils/ring.rs` holds the ring buffer (telemetry no
  longer depends on `pipeline`).
- `CHANGELOG.md` tracks versions and phase changes; both build modes (`cargo build` /
  `cargo build --tests`) report **zero warnings**.

## Design references & acknowledgments

**Equalizer APO**: many design decisions in VxAPO are inspired by
[Equalizer APO](https://sourceforge.net/projects/equalizerapo/): the per-device APO slot
installation model, config-file-driven DSP, the 31-band GraphicEQ limit, event-driven
config hot reload (aligned with EAPO's notification thread), aggregate delegation
semantics, and the install verification workflow. **VxAPO is an independent
implementation and contains no Equalizer APO code**; Equalizer APO is developed by
Jonas Thedering and licensed under GPL-2.0.

**Algorithm references**: the reverb algorithm follows Jon Dattorro's public paper
"Effect Design Part 1"; its topology was cross-checked against ValleyRackFree
(GPL-3.0-or-later) and johnhw/dattoro_reverb (MIT), with an independent Rust
implementation in this repo.

## Build

```bash
cargo build --release
```

Output: `vxapo_driver.dll` (`cdylib`). Release uses `lto` / `codegen-units=1` /
`panic=abort` to satisfy RT constraints.

## Documentation

See `../vxapo-docs`, with the detailed module reference under `../vxapo-docs/driver`.

## License

GPL-3.0-or-later
