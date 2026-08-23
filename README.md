# VxAPO Driver

VxAPO Driver 是运行在 Windows `audiodg` 进程内的 APO（Audio Processing Object）COM DLL，
负责逐设备的实时音频 DSP 处理。它**只读配置、不写回**：配置来自
`C:\ProgramData\VxAPO\{GUID}\config.toml`，变更由事件驱动监控并热重载。

## 功能与实现

### APO 生命周期（`src/object/`）

- 标准 Windows APO COM 对象：`IAudioProcessingObject` / `IAudioProcessingObjectRT` /
  `IAudioFormat` / `IPropertyStore` 等接口实现，`dll_exports` 导出 `DllGetClassObject` /
  `DllRegisterServer`，自维护引用计数。
- 支持 Aggregate / Child APO 链：可作为子 APO 挂到原效果器之下（安装器选择），
  `child.rs` 负责子链委托。
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
| `loudness` | ISO 226 等响度补偿：按目标/参考 phon 自动调整频响 |

### 安装与验证（`src/install/`）

- 端点枚举与槽位选择：`LfxGfx` / `SfxMfx` / `SfxEfx`，支持保留原 APO 为子 APO
  （`--no-child` 关闭）。
- 注册表写入走统一事务层（driver 是唯一写入口），安装后可通过 `verify`
  （CoCreateInstance + 格式协商）闭环验证。

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

## 致谢 Equalizer APO

VxAPO 的许多设计决策受到 [Equalizer APO](https://sourceforge.net/projects/equalizerapo/)
的启发，包括：逐设备注册 APO 槽位的安装模型、以配置文件驱动 DSP 的思路、
31 段 GraphicEQ 上限、事件驱动配置热重载（对齐 EAPO 的 notification thread 模式）、
以及安装后的验证流程。**VxAPO 是独立实现，不包含 Equalizer APO 的任何代码**；
Equalizer APO 由 Jonas Thedering 开发，GPL-2.0 许可。

混响算法依据 Jon Dattorro《Effect Design Part 1》公开论文实现，
拓扑正确性与 ValleyRackFree（GPL-3.0-or-later）及 johnhw/dattoro_reverb（MIT）
交叉核对，本仓库内为独立的 Rust 实现。

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

# VxAPO Driver

VxAPO Driver is the Windows APO (Audio Processing Object) COM DLL that runs inside
`audiodg` and performs per-device real-time audio DSP. It is **read-only** regarding
configuration: settings come from `C:\ProgramData\VxAPO\{GUID}\config.toml` and are
hot-reloaded through an event-driven directory watcher.

## Features & implementation

### APO lifecycle (`src/object/`)

- Standard Windows APO COM object: `IAudioProcessingObject` / `IAudioProcessingObjectRT` /
  `IAudioFormat` / `IPropertyStore` implementations, `DllGetClassObject` /
  `DllRegisterServer` exports, self-managed reference counting.
- Aggregate / child APO chain support: can be attached below an existing effect
  (selected by the installer); `child.rs` delegates to the child chain.
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
| `loudness` | ISO 226 loudness compensation: frequency response adjusted by target/reference phon |

### Install & verification (`src/install/`)

- Endpoint enumeration and slot selection: `LfxGfx` / `SfxMfx` / `SfxEfx`, with
  optional preservation of the original APO as a child APO (`--no-child` to disable).
- Registry writes go through a unified transaction layer (the driver is the single
  write path); installs can be closed-loop verified via `verify`
  (CoCreateInstance + format negotiation).

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

## Acknowledgments: Equalizer APO

Many design decisions in VxAPO are inspired by
[Equalizer APO](https://sourceforge.net/projects/equalizerapo/): the per-device APO
slot installation model, config-file-driven DSP, the 31-band GraphicEQ limit,
event-driven config hot reload (aligned with EAPO's notification thread), and the
install verification workflow. **VxAPO is an independent implementation and contains
no Equalizer APO code**; Equalizer APO is developed by Jonas Thedering and licensed
under GPL-2.0.

The reverb algorithm follows Jon Dattorro's public paper "Effect Design Part 1";
its topology was cross-checked against ValleyRackFree (GPL-3.0-or-later) and
johnhw/dattoro_reverb (MIT), with an independent Rust implementation in this repo.

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
