# DSP 数学优化方案（pipeline/dsp）— 整合版 v2

> 目标：在保证高保真的前提下，优化性能并保护数值边界。
> 触发问题：`Preamp: -1000 dB` / `+1000 dB`、PEQ/GraphicEQ 极端增益等输入会产生明显“哨声”。
> 平台：`x86_64-pc-windows`（本方案内的指令级优化均针对该平台，不做跨平台兜底）。
>
> 实施状态：**M1（P0）已完成**（2026-08-05）：math.rs / biquad f64 系数与 BiquadState /
> gain 比例平滑 / factory 解析 clamp / guarded interleave / RT 入口 FTZ/DAZ 全部落地，
> 单线程全量测试 473 通过（4 个失败均为无管理员权限的环境性测试）。
> 另已完成：**立体声水平 SIMD**（`__m128d` 双路 f64 并行处理左右通道，FMA 运行时检测 + SSE2 回退），
> 新增 2 测试后单线程全量 475 通过。
> 再完成：**Channel 选择槽位语义修复**（`Filter::set_channel_indices` + Chain 按下发索引，
> `Channel: R` 等非前缀选择不再误处理 L）+ **M2 边界项**（Delay 非有限/采样率护栏、
> Copy 零拷贝、Convolution pow2 mask + 全零 IR 直通）；单线程全量 481 通过。
> 再完成：**GraphicEQ 宏递归展开级联**（`cascade_body!` tt-list 展开 0..31，
> `process_cascade_dynamic` match dispatch + 防御回退循环）；单线程全量 484 通过。
> 再完成：**分块 FFT 卷积**（引入 `rustfft 6.4.1`；uniform partitioned overlap-add，
> 块 128 / FFT 256，IR >128 走 FFT、>65536 跳过；算法延迟 = 块大小）；单线程全量 485 通过。
> 修复：**热重载过渡按采样推进**（原按 APOProcess 调用次数推进，10ms 过渡实际耗时数秒且整块
> 常量 factor 不平滑）；**深切地板 -60 dB**（-120 dB 原被整段直通 → 现在回退稳定深切，
> “拉低没效果”修复）；单线程全量 491 通过。
>
> 本版整合自 `D:/APO_Project/dsp.md` 与初版方案，并对两处表述做了修正：
> 1. `GAIN_DB_MAX = +48 dB` 时 biquad 幅度因子是 `a = 10^(48/40) ≈ 15.8`（线性增益 `10^(48/20) ≈ 251`，两者不可混用）。
> 2. 比例平滑的 `MAX_GAIN_STEP_RATIO = 0.05` 与“128 步到达”存在矛盾：120 dB 跳变最快约 283 步（≈5.9 ms @48 kHz），
>    “128 步保证”只对 ≤ ~54 dB 的跳变成立；测试断言按实际数学修正，而不是改大步进。

## 1. 根因分析（实测）

f32 关键范围：

| 量 | 值 | 说明 |
| --- | --- | --- |
| 最大有限值 | `3.4028235e38` | 超过即 `+inf` |
| 最小正规数 | `1.1754944e-38` | 低于即次正规数（denormal） |
| 最小次正规数 | `1.4012985e-45` | 低于即 `0.0` |
| `10^50` | `inf` | `+1000 dB` 线性增益直接溢出 |
| `10^-50` | `0.0` | `-1000 dB` 纯增益下溢为 0（静音） |

实测（rustc 快速验证，48 kHz / fc=1 kHz / Q=1）：

| 输入 | 结果 | 后果 |
| --- | --- | --- |
| `Gain +1000 dB` | `db_to_linear = +inf` | 采样变 inf → 后续 IIR 状态变 NaN → 啸叫/哨声 |
| `Peaking +1000 dB` | `b0 ≈ 6.5e23`，`a2 → 1.0` | 极点贴单位圆 → 中心频率持续振荡（哨声） |
| `Peaking -1000 dB` | `a2 → -1.0` | 极点贴单位圆（Nyquist 附近）→ 高频振荡 |
| `LowShelf +1000 dB` | 归一化 `b0 = inf` | 系数无效，输出 inf/NaN |
| `Gain 1.0 → 60 dB` 线性平滑 | 首步 `≈ 7.8/采样` | 巨大跳变 → click / 瞬态爆音 |
| `fc=10 Hz @ 192 kHz`（f32） | `cos(w0) ≈ 1.0`，`1-cos(w0)` 精度丢失 | 低通/高通系数误差被放大 |

结论：需要三层防线。

1. **解析层**：把用户输入 clamp 到安全范围（非 RT，成本为零）。
2. **系数层**：biquad 系数在 f64 下计算，完成后做有限性 + 稳定性校验，非法回退直通。
3. **RT 层**：硬件 FTZ/DAZ 冲刷次正规数 + NaN/Inf 兜底，保证 APO 永不向引擎输出非有限值。

## 2. 平台初始化：FTZ/DAZ（RT 线程入口调用一次）

次正规数在 x86 上触发慢路径（延迟可增加 100 倍以上）。通过设置 SSE 控制寄存器的 FTZ/DAZ
标志位，硬件直接把次正规数当 0 处理，**零每采样开销**，DSP 代码无需任何逐采样检测。

```rust
use std::arch::x86_64::*;
use std::cell::Cell;

thread_local! {
    static FTZ_DAZ_SET: Cell<bool> = const { Cell::new(false) };
}

/// 音频线程入口调用一次，对当前线程生效；thread_local 保证幂等。
pub fn init_audio_thread() {
    FTZ_DAZ_SET.with(|flag| {
        if !flag.get() {
            unsafe {
                _MM_SET_FLUSH_ZERO_MODE(_MM_FLUSH_ZERO_ON);
                _MM_SET_DENORMALS_ZERO_MODE(_MM_DENORMALS_ZERO_ON);
            }
            flag.set(true);
        }
    });
}
```

集成点（实现时）：

- `object/apo/process.rs::apo_process`（RT 入口，每次调用先 `init_audio_thread()`，thread_local 判断后为纯返回）。
- `CalcInputFrames / CalcOutputFrames` 与 RT 线程同源，可不重复调用；若未来出现独立 RT 线程再补。
- 控制线程（Initialize/Lock/watcher）不需要，也不应改全局 MXCSR。

## 3. 统一数值策略：新增 `pipeline/dsp/math.rs`

在 `src/pipeline/dsp.rs` 增加 `pub mod math;`，所有 DSP 共享以下常量与函数。

### 3.1 常量（已确认，见第 9 节）

| 常量 | 值 | 依据 |
| --- | --- | --- |
| `GAIN_DB_MIN` | `-120.0` | 低于 -120 dB 已不可闻，且避免 `1/a` 爆掉 |
| `GAIN_DB_MAX` | `48.0` | 滤波器因子 `a = 10^(48/40) ≈ 15.8`，极点余量充足；线性增益 `≈ 251` |
| `FILTER_CUT_FLOOR_DB` | `-60.0` | 滤波深切地板：负增益不稳定时回退该值，仍为有效切除（不再整段直通） |
| `FILTER_FREQ_MIN_HZ` | `10.0` | 避免 DC 附近数值病态 |
| `FILTER_FREQ_MAX_RATIO` | `0.45` | fc 不得超过 0.45 × sample_rate（Nyquist 以内） |
| `Q_MIN` | `0.05` | alpha = sin(w0)/(2q) 不爆炸 |
| `Q_MAX` | `18.0` | 超出后极点贴近单位圆且无实际意义 |
| `STABILITY_MARGIN` | `1e-3` | 极点余量，等价“衰减时间 ≈ 1000 采样” |
| `MAX_GAIN_STEP_RATIO` | `0.05` | 增益平滑每采样最大比例变化（≈0.42 dB） |
| `GAIN_SNAP_THRESHOLD` | `1e-6` | 平滑接近目标时的跳转阈值（相对误差）；f32 机器精度 ≈1.19e-7，取 1e-7 可能永不命中导致停滞 |
| `GAIN_SMOOTH_RERATE` | `32` | 每 N 采样重算一次目标 ratio，避免每采样 powf |
| `GAIN_SMOOTH_STEPS_DEFAULT` | `128` | 默认目标步数（≈2.7 ms @ 48 kHz） |
| `COPY_COEFF_MAX` | `16.0` | Copy 混音系数幅值上限 |
| `DELAY_MS_MAX` | `1000.0` | 延迟上限（当前环形缓冲设计上限） |
| `PHON_MIN / PHON_MAX` | `0.0 / 120.0` | Loudness 参数范围 |
| `MAX_GRAPHIC_EQ_BANDS` | `31` | 1/3 倍频程全带数量 |
| `MAX_FRAME_COUNT` | `8192` | Copy 临时缓冲预分配上限（RT 安全） |
| `CONVOLUTION_PARTITION_SIZE` | `128` | 分块卷积块大小（Phase 8） |

### 3.2 核心函数

```rust
// dB → 线性，f64 计算后转 f32，先 clamp 再 powf，保证有限
pub fn db_to_linear(db: f32) -> f32;

// 线性 → dB，处理 0 / 负 / NaN / Inf
pub fn linear_to_db(linear: f32) -> f32;

// 输入合法性归一：NaN/Inf → 0 dB（直通），超界 → clamp
pub fn clamp_gain_db(db: f32) -> f32;

// 二阶极点稳定性（非 RT，initialize 时调用）
// 极点半径判据：解 z^2 + a1 z + a2 = 0，max(|pole|) < 1 - STABILITY_MARGIN
pub fn is_stable_biquad(a1: f32, a2: f32) -> bool;

// 非 RT 日志限频：同一参数 10 秒内最多一条 warn（Windows 上走 QPC）
pub fn warn_rate_limited(key: &str, msg: std::fmt::Arguments<'_>);
```

## 4. 逐文件方案

### 4.1 `biquad.rs`（P0，哨声主战场）

**问题**

- `compute_coeffs` 使用 f32：`fc << sr`（如 10 Hz @ 192 kHz）时 `cos(w0) ≈ 1`，`1-cos(w0)` 精度丢失。
- 极端增益使极点贴单位圆（实测 a2→±1）。
- `LowShelf/HighShelf` 存在 `a²` 项，+1000 dB 时溢出为 inf。
- `normalize` 不防 `a0 = 0 / inf`。
- DF2T 状态无 NaN/Inf 兜底，一旦污染会自我维持（哨声来源）。
- 高 Q + 低频时 f32 状态因舍入长期漂移。

**动作**

1. **`compute_coeffs` 全程 f64 中间计算**（initialize 路径，零 RT 成本），最后一步转 f32：
   ```rust
   pub fn compute_coeffs(
       filter_type: BiquadType,
       fc: f32,
       gain_db: f32,
       q: f32,
       sample_rate: u32,
   ) -> BiquadCoeffs {
       let fc = fc.clamp(FILTER_FREQ_MIN_HZ, sample_rate as f32 * FILTER_FREQ_MAX_RATIO);
       let q = q.clamp(Q_MIN, Q_MAX);
       let gain_db = clamp_gain_db(gain_db);
       if sample_rate == 0 || !fc.is_finite() {
           return BiquadCoeffs::BYPASS;
       }

       let sr = sample_rate as f64;
       let f = fc as f64;
       let qq = q as f64;
       let g = gain_db as f64;

       let w0 = 2.0 * std::f64::consts::PI * f / sr;
       let sin_w0 = w0.sin();
       let cos_w0 = w0.cos();
       let alpha = sin_w0 / (2.0 * qq);

       let sqrt_a = 10.0_f64.powf(g / 80.0);
       let a = sqrt_a * sqrt_a; // 恒等于 10^(dB/40)
       // ... RBJ 公式全部在 f64 下计算 ...
       // 最后转 f32 并归一化
   }
   ```
   f64 中间计算从根源解决 `cos(w0) ≈ 1` 的精度丢失，统一走 sin/cos 路径，不需要 `tan(w0/2)` 替代。
2. `normalize` 前检查 `a0.is_finite() && a0.abs() > 1e-30`，否则返回 `BYPASS`。
3. 归一化后调用 `math::is_stable_biquad(a1, a2)`；负增益不稳定时**回退到
   `FILTER_CUT_FLOOR_DB`（-60 dB）重算一次**（稳定深切，修复 -120 dB 整段直通），
   仍不稳定才回退 `BYPASS`。
4. **生产路径统一 `repr(C)` f64 状态**（DF2T）：
   ```rust
   #[repr(C)] // 16 字节固定布局：无 tag、无 padding
   struct BiquadState {
       s1: f64,
       s2: f64,
   }

   impl BiquadState {
       #[inline]
       fn process_sample(&mut self, coeffs: &BiquadCoeffs, input: f32) -> f32 {
           let x = input as f64;
           let b0 = coeffs.b0 as f64;
           let b1 = coeffs.b1 as f64;
           let b2 = coeffs.b2 as f64;
           let a1 = coeffs.a1 as f64;
           let a2 = coeffs.a2 as f64;

           let out = b0.mul_add(x, self.s1);
           self.s1 = b1.mul_add(x, (-a1).mul_add(out, self.s2));
           self.s2 = b2.mul_add(x, -a2 * out);

           let out_f32 = out as f32;
           if !out_f32.is_finite() {
               self.clear();
               0.0
           } else {
               out_f32
           }
       }
   }
   ```
   - `size_of::<BiquadState>() == 16`、`align_of == 8`；31 段级联数组 496 B 连续内存，cache 友好。
   - x86_64 上 `vcvtps2pd / vcvtpd2ps` 均为 1 cycle，转换成本可忽略。
   - 系数仍以 f32 存储（接口统一）；f64 系数预转副本的存储/转换 tradeoff 列入 M3 评估。
5. **无需逐采样 denormal 冲刷**：硬件 FTZ/DAZ 已在 `init_audio_thread` 设置。
6. 保持 DF2T 为生产默认；DF1/DF2 仅保留测试与兼容，状态仍用各自 f32 数组，不并入 `BiquadState`。
7. **立体声水平 SIMD（已实现）**：DF2T 且**滤波器作用域恰好为 2 个通道**时
   （默认立体声或显式 `Channel: L R`），两通道共用同一份系数，用 `__m128d` 双路并行；
   `Channel: L / Channel: R`（作用域 1 通道）不触发——标量路径只处理选中通道，不碰另一通道；
   非 2 通道回退标量。选中通道 → 平面缓冲前 N 槽位的映射与标量路径一致。
   `is_x86_feature_detected!("fma")` 运行时选择 FMA（与标量 `mul_add` 逐位一致）或 SSE2 回退。

**保真**：正常范围系数与现实现完全一致；f64 中间计算 + `mul_add` 精度更高。

**性能**：FTZ/DAZ 零开销消除 denormal 慢路径；`mul_add` 单指令；状态布局固定、无分支 dispatch。

### 4.2 `gain.rs`（P0）

**问题**

- `db_to_linear(+1000) = +inf`。
- `set_gain_linear` 目标可为 NaN/Inf。
- 线性平滑大步长（1.0 → 1000 首步 ≈ 7.8/采样）产生爆音。
- `linear_to_db` NaN 未处理。
- 极端衰减后恢复时间无上限（-120 dB → 0 dB 可能需要数百毫秒）。

**动作**

1. `db_to_linear / linear_to_db` 改用 `math::*`（f64 计算 + clamp + 有限保证）。
2. `set_gain_db / set_gain_linear`：目标非有限 → 忽略；目标 clamp 到 `[0, db_to_linear(GAIN_DB_MAX)]`。
3. 平滑改为**带到达时间保证与跳转阈值的比例平滑**：
   ```rust
   struct GainSmooth {
       current: f32,
       target: f32,
       ratio: f32,
       step_counter: u32,
       steps_to_reach: u32, // 默认 GAIN_SMOOTH_STEPS_DEFAULT = 128
   }

   impl GainSmooth {
       fn advance(&mut self) -> f32 {
           if (self.current - self.target).abs()
               < GAIN_SNAP_THRESHOLD * self.target.abs().max(1.0)
           {
               self.current = self.target;
               return self.current;
           }
           if self.current.abs() < GAIN_SNAP_THRESHOLD {
               self.current = self.target * 1e-4;
           }
           if self.step_counter % GAIN_SMOOTH_RERATE == 0 {
               let remaining = (self.steps_to_reach.saturating_sub(self.step_counter)).max(1);
               let ideal_ratio = (self.target / self.current).powf(1.0 / remaining as f32);
               self.ratio = ideal_ratio.clamp(
                   1.0 - MAX_GAIN_STEP_RATIO,
                   1.0 + MAX_GAIN_STEP_RATIO,
               );
           }
           self.current *= self.ratio;
           self.step_counter += 1;
           self.current
       }
   }
   ```
4. **热重载过渡（实现版修正）**：`apo_process` 的过渡混合**逐采样推进 factor**
   （`SmoothingProvider::advance()` 每采样一次），过渡长度按采样数计
   （10ms = 480 采样 @48k），而非按 APOProcess 调用次数——修复前每次调用只推进 1 步，
   实际过渡被放大到调用周期 × 步数（约 2~5s）且整块常量 factor 不平滑。

5. **到达时间修正**：
   - `remaining` 设下限 `GAIN_SMOOTH_RERATE (32)`：否则 `counter ≥ steps` 后“单步到位”控制器
     会被 ±5% clamp 反复振荡（实测 0.684 → 1.23 → 0.98 → … 永不收敛）；下限 32 保证收缩收敛。
   - `MAX_GAIN_STEP_RATIO = 0.05` 时单步最大 ×1.05，128 步最多到 ×518（≈54 dB）；
   - 120 dB 跳变实际需要 `ln(10^6)/ln(1.05) ≈ 283` 步（≈5.9 ms @ 48 kHz），仍无 click；
   - “128 步保证”仅对 ≤ ~54 dB 跳变成立；测试断言按实际步数修正（见第 7 节）。
   - 若产品要求任何跳变都 ≤2.7 ms，需把 `MAX_GAIN_STEP_RATIO` 提到 ~0.12（≈1 dB/步），默认不采用。
5. `process` 每采样一次乘 + 一次 clamp，保持零分配；`powf` 每 32 采样一次，开销可忽略。

**保真**：比例平滑在 1.0 附近每步 ≈ 0.42 dB，远小于可辨步长；跳转阈值避免进入不可见微步长阶段。

### 4.3 `factory.rs`（P0，解析层统一收口）

**问题**：各工厂只做 `is_finite`/范围检查，不做 clamp；`parse::<f32>()` 接受 `NaN`/`inf` 字符串。

**动作**（非 RT；原则：数字合法但离谱 → clamp + warn；缺参数/非数字 → NoMatch）

| 工厂 | 校验 |
| --- | --- |
| `IIR` | fc clamp 到 `[FILTER_FREQ_MIN_HZ, 0.45*sr]`；q clamp 到 `[Q_MIN, Q_MAX]`；gain 经 `clamp_gain_db` |
| `Biquad`（裸系数） | 5 个系数必须有限；`is_stable_biquad` 失败 → NoMatch |
| `Preamp` | gain 经 `clamp_gain_db` |
| `Delay` | ms 有限，clamp 到 `[0, DELAY_MS_MAX]`；初始化把延迟线长度补到 2 的幂 + mask |
| `Convolution` | gain 经 `clamp_gain_db`；IR 加载后过滤 NaN/Inf 采样 |
| `GraphicEQ` | 每段 gain clamp；段数 ≤ `MAX_GRAPHIC_EQ_BANDS`；fc clamp |
| `LoudnessCorrection` | phon/reference clamp 到 `[PHON_MIN, PHON_MAX]` |
| `Copy` | 系数必须有限，clamp 到 `[-COPY_COEFF_MAX, COPY_COEFF_MAX]` |

**日志限频**：用 `std::time::Instant`（Windows 上走 QPC），同一参数 10 秒内最多一条 warn。

### 4.4 `peq.rs` / `hp_lp.rs`（P1，跟随 biquad 加固）

- 依赖 `compute_coeffs` 的全部护栏，无需重复。
- `PeakingFilter::initialize`：`|gain_db| < 0.05` 时直接 `BYPASS`（省一段级联）。
- 生产路径使用 `BiquadState` + DF2T。

### 4.5 `graphic_eq.rs`（P1）

- 解析：段数上限 + 每段增益 clamp（见 4.3）。
- `initialize`：跳过 `|gain_db| < 0.05` 的段（0 dB 段不建 biquad，`H(z)=1`）。
- **级联展开宏（递归展开，推荐）**：不做“带循环的函数 + `#[inline(never)]`”，而是用 tt-list 递归
  在**展开期**直接生成直线依赖链，展开是编译期保证的，不依赖 LLVM 的循环展开启发式：
  ```rust
  // 递归展开：每步处理一个固定索引段，输出喂给下一段
  macro_rules! cascade_body {
      ($coeffs:ident, $states:ident, $input:expr, ) => { $input };
      ($coeffs:ident, $states:ident, $input:expr, $head:tt $($tail:tt)*) => {{
          let x = $states[$head].process_sample(&$coeffs[$head], $input);
          cascade_body!($coeffs, $states, x, $($tail)*)
      }};
  }

  // 按有效段数 dispatch：match 每帧一次（n 固定，分支预测良好），之后纯直线计算
  fn process_cascade_dynamic(
      n: usize,
      coeffs: &[BiquadCoeffs],
      states: &mut [BiquadState],
      input: f32,
  ) -> f32 {
      match n {
          0 => input,
          1 => cascade_body!(coeffs, states, input, 0),
          2 => cascade_body!(coeffs, states, input, 0 1),
          3 => cascade_body!(coeffs, states, input, 0 1 2),
          // ... 展开到 31：
          31 => cascade_body!(
              coeffs, states, input,
              0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30
          ),
          _ => unreachable!("GraphicEQ bands capped at MAX_GRAPHIC_EQ_BANDS = 31"),
      }
  }
  ```
  `process_sample` 标 `#[inline(always)]`，展开后形成纯 FMA 依赖链：无循环计数器、无索引运算、
  无分支；编译器可跨段重排调度。31 段 match 臂可由一个小生成宏产出，避免手写重复。
- 保真：跳过的段 `H(z)=1`，输出逐位不变。

### 4.6 `loudness.rs`（P1，增益值不改）

- `parse_loudness_params`：phon/reference clamp；非法 → `None`。
- `initialize`：`diff` clamp 到 ±60；沿用 `|gain| > 0.05` 才建段。
- `iso_226_approx`：加 `freq > 0` 守卫；结果经 `clamp_gain_db`。
- **不改 ISO 226 增益值**：Peaking 频响是 `(freq, Q, gain)` 的非线性函数，改 gain 等于改变补偿精度；
  数值安全由 P0 的 f64 系数 + 稳定性检查 + clamp 保证。
- 感知问题（“拉了 EQ 没效果”）由 UI/CLI 层解决，见第 6 节。
- 性能：同 GraphicEQ 宏展开级联；Phase 8 用 ISO 226 查表 + 插值替代级联。

### 4.7 `delay.rs`（P1）

**问题**：`delay_ms` 可为 NaN/Inf；`NaN as usize = 0`，`frac = NaN` → 输出 NaN。

**动作**

1. `initialize`：`delay_ms` 非有限 → 按 0 ms（直通）；clamp 到 `[0, DELAY_MS_MAX]`。
2. `sample_rate == 0` 守卫。
3. 延迟线长度在初始化时补到 2 的幂，用 mask 替代取模（当前 `next_power_of_two` 只约束缓冲大小，需改为同时约束实际延迟长度）。
4. 亚采样插值保持 `s0 + frac*(s1-s0)`。
5. **内存序**：当前单线程处理，x86_64 TSO 保证同线程内 load/store 顺序，无需 fence；
   若未来延迟补偿反馈路径跨线程（处理线程写 → 监控线程读），写侧 `Release`、读侧 `Acquire`，列入 M4 评估。

### 4.8 `convolution.rs`（P1 + Phase 8）

**问题**：`gain = 10^(db/20)` 可 inf；WAV 可能带 NaN/Inf 采样；长 IR 时域卷积效率低；`% delay_len` 有取模开销。

**动作**

1. 增益用 `math::db_to_linear`；IR 系数乘增益后做有限性检查，非有限置 0 并 warn。
2. 全零 IR → `loaded = false`（直通）。
3. 内层循环用 2 的幂 mask 消除取模与分支：
   ```rust
   let mask = delay_len - 1; // delay_len 初始化时保证为 2 的幂
   let mut read = pos;
   for &coef in ir.iter() {
       read = read.wrapping_sub(1) & mask;
       acc = coef.mul_add(delay[read], acc);
   }
   ```
4. **分块 FFT 卷积（Phase 8，M3）**：
   - IR ≤ 128 采样：时域直接卷积（已实现）；
   - IR > 128 且 ≤ 65536：uniform partitioned overlap-add 分块卷积（已实现，rustfft 6.4.1），
     块大小 = `CONVOLUTION_PARTITION_SIZE`（128），FFT 长度 256；
   - FFT 计划与块缓冲全部在 `initialize`（非 RT）预分配，`process` 保持零分配；
   - 注意：rustfft 逆 FFT 未归一化，IFFT 后需乘 `1/fft_len`（实现已处理）；
   - 算法延迟 = 块大小（128 采样），`latency()` 如实报告；
   - 估算：1024 长度 IR 从 `O(N²) ≈ 21 ms` 降到 `O(N log N) ≈ 0.5 ms`（@48 kHz 每处理块）。

### 4.9 `copy.rs`（P1）

**问题**

- `process` 中 `temp_buf.resize()` 是 **RT 堆分配违规**。
- `parse_coeff_and_name` 用 `unwrap_or(1.0)`，`NaN` 会解析成功并传播。

**动作**

1. `initialize` 预分配 `temp_buf` 到 `MAX_FRAME_COUNT`，实现 `max_frame_count() = Some(MAX_FRAME_COUNT)`，彻底移除 RT resize。
2. 解析：系数必须有限并 clamp 到 `±COPY_COEFF_MAX`；非法返回 `None`。
3. **零拷贝路径**（平面缓冲语义适配）：
   - 单源且 `coeff == 1.0`：若 `target_index == source_index` → 直接 return（等价 `ptr::eq` 零拷贝）；
   - 单源且 `coeff == 1.0`、目标不同通道 → `samples[target][..frame_count].copy_from_slice(&samples[src][..frame_count])`；
   - 多源 → `temp_buf.fill(0.0)` + `mul_add` 累加后写目标。

### 4.10 `transition.rs`（P2）

- `mix_buffers / mix_plane_buffers`：factor 先 `clamp(0.0, 1.0)`（NaN → 1.0）。
- `SmoothingProvider::set_length` clamp 到 ≥1。
- 性能：`raised_cosine` 保持；后续可查表或 SIMD 平面混合。

### 4.11 `filter.rs`（P2）

- 文档明确 `max_frame_count` 契约（Copy 依赖）。
- `DspContext` 已含 `max_frame_count`；`initialize` 接收帧数上限列为 API 演进项。

### 4.12 `vst.rs`

- 预留模块，无 DSP 路径，不做优化。

### 4.13 通道选择语义（已修复）

- `Filter` trait 新增 `set_channel_indices(&[usize])`（默认忽略）；`Chain::initialize`
  按当前选中名字在原始平面缓冲中的槽位号下发，`Channel: R` 不再误处理 L。
- biquad / gain / delay / convolution 只处理选中槽位；graphic_eq / loudness / peq / hp_lp
  转发给内部 biquad；Copy 保持绝对槽位语义（通道路由全局生效）。
- 立体声 SIMD 路径对任意两个选中槽位生效（不再要求 0/1 前缀）。

## 5. 链级防线（P0，兜底，全构建启用）

- 新增 `interleave_from_guarded`：交织写回时 `!v.is_finite()` → 写 `0.0`。
- `is_finite()` 在 x86_64 编译为一次整数比较，成本极低；**所有构建启用**（已确认）。
- 保真：正常路径从不命中；命中即表示上游 bug 或极端配置，静音优于啸叫。

## 6. 响度补偿的感知问题（UI/CLI 层解决，P2）

用户低音量下调 PEQ（如高频 +6 dB），ISO 226 补偿在背后拉低高频，用户感知是“拉了没效果”。

- **不在 DSP 层改增益**：Peaking 频响是 `(freq, Q, gain)` 的非线性函数，改 gain 等于删除滤波器或破坏补偿精度。
- **UI/CLI 增强**（vxapo-cli 或未来 GUI）：
  1. 显示两条曲线：用户配置 EQ 曲线（蓝）+ 实际输出频响 = ISO 226 补偿叠加后（橙）；
  2. 显示当前补偿值（如 `-12 dB @ 50 Hz, +3 dB @ 200 Hz, ...`）；
  3. 提供 Loudness Compensation 独立 Bypass 开关，便于 A/B。
- 优先级：数值安全 P0（M1）先落地，UI 增强 P2（M3/M4）。

## 7. 测试矩阵

| 场景 | 断言 |
| --- | --- |
| `Gain ±1000 / NaN / inf` | 输出有限；10 s 后无振荡 |
| biquad 极端增益 ±1000 | 系数有限且稳定；负增益走 -60 dB 深切地板（非 BYPASS）；脉冲响应 0.5 s 内衰减到 < 1e-3 |
| biquad `fc > Nyquist`、`q = 0 / inf` | 回退 BYPASS |
| biquad f64 中间计算精度 | 10 Hz @ 192 kHz / Q=1 系数与参考值误差 < 1e-6 |
| biquad f64 状态漂移 | 100 Hz / Q=10 连续 10 s ±1 交替信号，状态无漂移 |
| `BiquadState` 布局 | `size_of == 16`、`align_of == 8`；31 段数组 == 496 B 连续无 padding |
| 稳定性检查 | `is_stable_biquad`（极点半径 < 1-margin，实现即求根）与 Schur-Cohn 在稳定/不稳定区一致；高增益 shelf 以极点半径为准 |
| 增益平滑到达时间 | -120 dB → 0 dB 在 `min(128 步, 数学所需步数)` 内到达；误差 < 0.1 dB（120 dB 实际 ≈ 283 步，断言按 ≤300 步） |
| 增益平滑跳转阈值 | 当前与目标相对误差 < 1e-6 时直接跳转，不再微步 |
| 增益平滑无 click | 1.0 → 1000（+60 dB，clamp 后为 +48 dB）跳变，峰值 < 2.0 |
| Delay `NaN ms` / `1e9 ms` | 直通 / clamp，输出有限 |
| Delay 缓冲 2 的幂 | `buffer.len()` 为 2 的幂，mask 取模与 `%` 结果一致 |
| Convolution `+1000 dB`、IR 含 NaN | 增益 clamp / IR 净化，输出有限 |
| Convolution 分块（Phase 8） | 与直接时域卷积误差 < 1e-4（同 IR） |
| Copy 系数 `NaN` / `1000` | 解析拒绝 / clamp，process 无分配 |
| Copy 零拷贝 | 单源 coeff=1.0 且目标==源通道时零内存拷贝（路径计数/探针验证） |
| GraphicEQ / Loudness 极端值 | 段数受限、输出有限 |
| GraphicEQ 全 0 dB | 输出逐位等于输入（`H(z)=1`） |
| GraphicEQ 级联 dispatch | `process_cascade_dynamic(N, ...)` 对 N=0..31 各自正确 |
| 全链注入 NaN | `process_audio` 出口全为 0.0（guarded interleave） |
| FTZ/DAZ 生效 | 状态衰减到 < 1e-40 后 process 无性能退化（criterion 微基准） |
| FTZ/DAZ 幂等 | 同一线程调用 1000 次 `init_audio_thread` 无副作用 |

## 8. 实施顺序

- **M1（P0，解决哨声）**：`init_audio_thread`（FTZ/DAZ）+ `math.rs` + `biquad.rs`（f64 系数 + `repr(C)` BiquadState + 稳定性检查）+ `gain.rs`（clamp + 带跳转阈值与到达时间的比例平滑）+ `factory.rs`（解析 clamp + 限频 warn）+ `process.rs`（guarded interleave）。
- **M2（P1，边界补全）**：`delay.rs`（clamp/护栏）、`copy.rs`（RT 分配修复 + 零拷贝）、`convolution.rs`（pow2 mask + IR 净化 + 全零直通）、`loudness.rs`（参数 clamp，增益不改）、`graphic_eq.rs`（0 dB 跳过）——**已完成**。
- **M3（P2，性能 + UI）**：`mul_add` 全面落地、GraphicEQ 宏展开级联、`convolution` 分块 FFT（rustfft 6.4.1）——**已完成**；剩余：响度补偿 UI/CLI 可视化 + Bypass、f64 系数预转副本评估（存储 vs 转换）。
- **M4**：测试矩阵 + 微基准（criterion）固化回归；评估级联 SIMD 4 通道版（通道 ≥4 且 biquad ≥4 时启用；立体声版已在 M1 落地）；评估延迟缓冲跨线程 fence 需求。

## 9. 已确认项

| 项 | 决定 |
| --- | --- |
| `GAIN_DB_MAX` | `+48 dB`（biquad 因子 `a ≈ 15.8`，线性增益 `≈ 251`，极点余量与实用性平衡） |
| 滤波深切地板 | `-60 dB`：负增益不稳定时回退该值重算，不再整段直通 |
| 热重载过渡 | 逐采样推进 factor（10ms = 480 采样 @48k），整块不再用常量 factor |
| 越界配置策略 | clamp + warn（裸系数 Biquad 例外：不稳定 → NoMatch） |
| RT 出口有限性检查 | 所有构建启用（`is_finite()` = 一次整数比较） |
| Denormal 处理 | 硬件 FTZ/DAZ（`init_audio_thread`，thread_local 幂等） |
| 系数计算精度 | 全程 f64，统一 sin/cos，不需要 tan 替代路径 |
| DF2T 状态存储 | 统一 `repr(C)` f64 `BiquadState`（16 B），31 段级联 496 B，无 enum tag/padding |
| 级联展开 | 宏递归展开（tt-list）生成直线依赖链，`#[inline(always)]`，按有效段数 match dispatch |
| 立体声水平 SIMD | 已实现：DF2T 双通道时 `__m128d` 双路 f64；FMA 运行时检测 + SSE2 回退；与标量路径误差 < 1e-9 |
| 分块卷积 | 已实现：uniform partitioned overlap-add，块 128 / FFT 256，IFFT 归一化已处理；算法延迟 = 块大小；IR > 65536 跳过 |
| 增益平滑 | 比例平滑 + 到达时间（默认 128 步，remaining 下限 32 防振荡）+ 跳转阈值（1e-6 相对误差） |
| 响度补偿增益 | 不改；数值安全由 P0 护栏保证 |
| 响度补偿感知问题 | UI/CLI 解决（实际频响曲线 + 补偿值显示 + Bypass） |
| Copy 零拷贝 | 单源 coeff=1.0 且目标==源通道时直接 return |
| 延迟缓冲内存序 | 单线程无需 fence；跨线程场景记录为 M4 评估项 |
| 目标平台 | `x86_64-pc-windows`，可直接使用 x86_64 指令集 |

## 10. 待确认 / 评估项

1. ~~`rustfft` 依赖引入~~ —— **已确认并加入**（`rustfft = "6.4.1"`，纯 Rust、MIT/Apache，无 C 依赖）。
2. f64 状态 vs f32 状态的吞吐基准：`vcvtps2pd/vcvtpd2ps` 理论 1 cycle，但 31 段级联的寄存器压力需实测；若 f64 状态明显更慢，退路是“f64 只算系数 + f32 状态 + 周期性状态清零”。
3. 级联 SIMD 4 通道版：通道数与段数同时 ≥4 才启用，M4 评估收益（立体声 2 路版已实现并验证）。
4. 延迟补偿跨线程 fence 需求，M4 评估。
