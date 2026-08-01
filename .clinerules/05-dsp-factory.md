# DSP 工厂注册表（8 个 DSP 工厂实现 + VST 预留）

## 注册顺序与 index 常量

| index | 常量名 | 工厂名 | 状态 |
|-------|--------|--------|------|
| 0 | FACTORY_DEVICE | (config 命令) | — |
| 1 | FACTORY_IF | (config 命令) | — |
| 2 | FACTORY_EVAL | (config 命令) | — |
| 3 | FACTORY_INCLUDE | (config 命令) | — |
| 4 | FACTORY_STAGE | (config 命令) | — |
| 5 | FACTORY_CHANNEL | (config 命令) | — |
| 6 | FACTORY_IIR | IirFactory | done |
| 7 | FACTORY_BIQUAD | BiquadFactory | done |
| 8 | FACTORY_PREAMP | PreampFactory | done |
| 9 | FACTORY_DELAY | DelayFactory | done |
| 10 | FACTORY_COPY | CopyFactory | done |
| 11 | FACTORY_CONVOLUTION | ConvolutionFactory | done |
| 12 | FACTORY_GRAPHIC_EQ | GraphicEqFactory | done |
| 13 | FACTORY_VST_PLUGIN | VstFactory | 预留（恒 NoMatch） |
| 14 | FACTORY_LOUDNESS_CORRECTION | LoudnessFactory | done |

index 0-5 对应纯配置命令，由 parser 分发而非 FilterRegistry

## DSP 滤波器构造器签名
- GainFilter::new(db: f32)
- DelayFilter::new(delay_ms: f32)
- CopyFilter::new(ops: Vec<CopyOp>)
- PeakingFilter::new(fc: f32, gain_db: f32, q: f32)
- HighLowPassFilter::high_pass(fc, q) / low_pass(fc, q) / new(ftype, fc, q)
- BiquadFilter::new(coeffs: BiquadCoeffs, structure: BiquadStructure)
- compute_coeffs(btype, fc, gain_db, q, sample_rate) -> BiquadCoeffs
- GraphicEqFilter::new(Vec<EqBand>)
- ConvolutionFilter::new(path, gain)
- LoudnessFilter::new(phon, ref)
- ~~VstFilter~~（已删除：VST 评估后不需要，vst.rs 仅注释，VstFactory 恒 NoMatch）

## DspContext 字段
sample_rate / channel_count / channel_mask / channel_names / max_frame_count / bits_per_sample / device_type / stage / variables / rt_marker（O1：`PhantomData<RealtimeContext>`，v6.6）

## 测试
417 passed / 0 failed（含 20+ 工厂测试：各工厂解析/无效 NoMatch/iir_modal/注册顺序等）

## 完成度结论（已验证）
biquad / peq / hp_lp / loudness / graphic_eq / copy / gain / delay / transition / convolution 共 10 个滤波器 process() 全部真实实现，无空壳。
convolution = 短 IR（<256 采样）时域 FIR 卷积 + WAV 解析（PCM16/f32）+ 环形延迟线（RT 安全）。VST = 预留 NoMatch。