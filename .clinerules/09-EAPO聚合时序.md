# EAPO 聚合委托 + 格式协商完整时序（源码事实，2026-08-05 从 EAPO 三文件逐行梳理）

> 依据：`EqualizerAPO/ClassFactory.cpp`、`EqualizerAPO/EqualizerAPO.cpp`(1-555)、`EqualizerAPO/EqualizerAPO.h`。
> 目的：锁定「引擎怎么创建 APO、聚合怎么委托、协商怎么走」，不再猜 inner/outer/委托/非委托、先后顺序。

## 1. 对象布局（多重继承，EqualizerAPO.h:36）

```cpp
class EqualizerAPO : public CBaseAudioProcessingObject, public IAudioSystemEffects, public INonDelegatingUnknown
```

- `CBaseAudioProcessingObject`（微软 SDK 基类）提供 IUnknown + IAPO + IAPO_RT + IAPO_CFG
- `IAudioSystemEffects` 独立 marker
- `INonDelegatingUnknown`（**EAPO 自定义**，EqualizerAPO.h:29-34）：
  ```cpp
  class INonDelegatingUnknown {
      virtual HRESULT NonDelegatingQueryInterface(const IID&, void**) = 0;
      virtual ULONG    NonDelegatingAddRef() = 0;   // 自维护 refCount
      virtual ULONG    NonDelegatingRelease() = 0;  // refCount==0 → delete
  };
  ```
- **关键事实**：对象内每个接口视图（IAPO/RT/CFG/ASE）的 vtable **槽 0-2 = EqualizerAPO 的 QI/AddRef/Release（委托 outer 版本）**；而「非委托 IUnknown 视图」的 vtable 槽 0-2 = NonDelegating 三方法（自维护 + 直接暴露接口）。

## 2. 引擎创建链（ClassFactory.cpp:64-77）

```cpp
HRESULT ClassFactory::CreateInstance(IUnknown* pUnknownOuter, const IID& iid, void** ppv) {
    if (pUnknownOuter != NULL && iid != __uuidof(IUnknown))  // 聚合必须请求 IUnknown
        return E_NOINTERFACE;
    EqualizerAPO* apo = new EqualizerAPO(pUnknownOuter);     // 构造：refCount=1, pUnkOuter=外壳
    HRESULT hr = apo->NonDelegatingQueryInterface(iid, ppv); // ★ 返回「非委托」接口！
    apo->NonDelegatingRelease();                             // 工厂临时引用释放（自维护 refCount 1→0）
    return hr;
}
```

构造（EqualizerAPO.cpp:42-58）：`pUnkOuter = 引擎外壳`（非聚合时 = 自己 INonDelegatingUnknown 视图）。

## 3. NonDelegatingQueryInterface（EqualizerAPO.cpp:519-539）—— 唯一接口暴露点

```cpp
HRESULT EqualizerAPO::NonDelegatingQueryInterface(const IID& iid, void** ppv) {
    if (iid == IUnknown)                    *ppv = static_cast<INonDelegatingUnknown*>(this);
    else if (iid == IAudioProcessingObject) *ppv = static_cast<IAudioProcessingObject*>(this);
    else if (iid == IAudioProcessingObjectRT)  *ppv = static_cast<IAudioProcessingObjectRT*>(this);
    else if (iid == IAudioProcessingObjectConfiguration) *ppv = static_cast<IAudioProcessingObjectConfiguration*>(this);
    else if (iid == IAudioSystemEffects)    *ppv = static_cast<IAudioSystemEffects*>(this);
    else { *ppv = NULL; return E_NOINTERFACE; }
    reinterpret_cast<IUnknown*>(*ppv)->AddRef();   // ←★ 调的是对象视图的 AddRef
    return S_OK;
}
```

**铁律**：`reinterpret_cast<IUnknown*>(*ppv)->AddRef()` 调用的是「该接口视图」的 AddRef：
- 对 IUnknown 视图 → `INonDelegatingUnknown` vtable 槽 1 = **NonDelegatingAddRef**（refCount++ 自维护）
- 对 IAPO/RT/CFG/ASE 视图 → 槽 1 = **EqualizerAPO::AddRef = 委托 outer->AddRef**（委托！）

## 4. 完整时序（引擎视角，事实链）

```
① 引擎 CoCreateInstance(clsid, pUnkOuter=音频引擎外壳, riid=IUnknown)
② ClassFactory::CreateInstance(外壳, IUnknown):
     new EqualizerAPO(外壳)                    → refCount=1（工厂临时）
     NonDelegatingQI(IUnknown, &ppv)           → ppv = INonDelegatingUnknown*（非委托视图）
                                                → 其 AddRef=NonDAddRef → refCount=2（引擎持有）
     NonDelegatingRelease()                    → refCount=1（工厂释放临时）
③ 引擎拿到 ppv = 非委托 IUnknown（NonDQI/NonDAddRef/NonDRelease vtable）
④ 引擎对 ppv 调 QI(IAudioProcessingObject):
     → NonDelegatingQI(IAudioProcessingObject) → ppv = IAudioProcessingObject*（对象 IAPO 视图）
     → 其 AddRef = EqualizerAPO::AddRef = 委托 outer->AddRef（外壳 +1）
⑤ 引擎对 IAPO 调 Initialize(cbData, APOInitSystemEffects):
     → EqualizerAPO::Initialize（EqualizerAPO.cpp:97-226）
       - 校验 cbDataSize==sizeof(APOInitSystemEffects)（硬性，EqualizerAPO.cpp:107）
       - 读 APOInit.clsid 判 Pre/PostMix
       - pAPOEndpointProperties->GetValue(PKEY_AudioEndpoint_GUID) 得 deviceGuid
       - DeviceAPOInfo.load(deviceGuid) 读安装信息（child APO GUID 等）
       - 有 child → CoCreateInstance(childGuid) + child->QueryInterface(RT/CFG) + child->Initialize
       - return S_OK
⑥ 引擎对 IAPO 调 QI(IAPO_RT) → NonDelegatingQI(RT) → RT 视图（委托 IUnknown）
⑦ 引擎对 IAPO 调 QI(IAPO_CFG) → NonDelegatingQI(CFG) → CFG 视图（委托 IUnknown）
⑧ 引擎对 IAPO 调 QI(IAudioSystemEffects) → NonDelegatingQI(ASE) → ASE 视图（委托 IUnknown）
⑨ 引擎对 IAPO 调 IsInputFormatSupported(pOutputFormat, pRequestedInputFormat, ppSupported):
     EqualizerAPO::IsInputFormatSupported（EqualizerAPO.cpp:228-306）
     a. GetUncompressedAudioFormat 读 requested（in）
     b. GetUncompressedAudioFormat 读 outputFormat（opposite/out）
     c. 有 child → child->IsInputFormatSupported（失败 resetChild）
     d. CBaseAudioProcessingObject::IsInputFormatSupported（基类核心检查）
     e. ★ 降混拒绝：hr==S_OK 且 in.dwSamplesPerFrame>2 且 in>out
          → CreateAudioMediaTypeFromUncompressedAudioFormat(&outFormat, ppSupported) + hr=S_FALSE
     f. 返回：S_OK（接受）或 S_FALSE（拒绝并给 supported 格式）
⑩ 引擎对 IAPO 调 LockForProcess(nin, ppin, nout, ppout):
     EqualizerAPO::LockForProcess（EqualizerAPO.cpp:308-389）
     a. ppInputConnections[0]->pFormat->GetUncompressedAudioFormat → inFormat
     b. ppOutputConnections[0]->pFormat->GetUncompressedAudioFormat → outFormat
     c. childCfg->LockForProcess（失败仅 Trace 不 return）
     d. CBaseAudioProcessingObject::LockForProcess（基类）
     e. maxFrameCount = in 的，若 0 用 out 的
     f. realChannelCount = childCfg?out.dwSamplesPerFrame : in.dwSamplesPerFrame（无 child 用 in）
     g. channelMask：capture?in.mask : out.mask；mask==0 且 in.ch==out.ch → 回退另一侧 mask
     h. engine.initialize(out.sampleRate, in.ch, realChannelCount, out.ch, channelMask, maxFrameCount)
⑪ 引擎每帧调 APOProcess（EqualizerAPO.cpp:458-516）:
     - 有 childRT → childRT->APOProcess 先处理（引擎输出缓冲被 child 写入）→ engine.process(output,output,...)
     - 无 child  → engine.process(output, input, frames)
     - u32ValidFrameCount = input 的（passthrough 同帧数）
     - BUFFER_SILENT 输入 → 允许修改时检查非零则 BUFFER_VALID 否则保持 SILENT；不允许则清零+SILENT
```

## 5. 引用计数事实（EqualizerAPO.cpp:72-80 / 541-555）

| 方法 | 聚合时行为 |
|------|-----------|
| `QueryInterface` | `pUnkOuter->QueryInterface`（委托外壳） |
| `AddRef` | `pUnkOuter->AddRef`（委托外壳） |
| `Release` | `pUnkOuter->Release`（委托外壳） |
| `NonDelegatingQueryInterface` | 直接暴露 inner 各接口（对象自己响应） |
| `NonDelegatingAddRef/Release` | `InterlockedIncrement/Decrement(refCount)`（自维护）；refCount 0 → delete |

- 引擎拿到的**非委托 IUnknown**（CreateInstance 返回值）：
  - QI 槽 → NonDQI（**不委托**，直接暴露 inner 接口）
  - AddRef/Release 槽 → NonDAddRef/NonDRelease（**自维护 refCount**）
- 引擎随后 QI 出的**各接口视图**（IAPO/RT/CFG/ASE）：
  - QI/AddRef/Release 槽 → **委托 outer**（引擎身份检查、统一引用计数经外壳）
  - 方法槽 → inner 实现

## 6. 我们（vxapo-driver aggregate.rs）与 EAPO 的差异 → 根因

| # | EAPO（正确） | 我们 | 后果 |
|---|-------------|------|------|
| 1 | `CreateInstance` 返回 **INonDelegatingUnknown*（非委托 IUnknown）** | `create_aggregate` 返回 **NApo 基址 = IAPO vtable（委托 QI）** | 引擎对返回值调 QI(IAPO) → 我们的 `apo_dl_qi` → **委托 outer->QI(IAPO)**；外壳不认 IAPO → 引擎拿不到接口 → **弃用对象、零方法调用**（探针实证：qi/initialize/lock 全不刷新） |
| 2 | 非委托 IUnknown 的 QI 槽 = NonDQI（**直接返回 inner 接口**） | 无此视图（IAPO vtable 槽 0 = `apo_dl_qi` 委托） | 见上 |
| 3 | AddRef/Release 自维护 + 委托 outer 双轨 | `delegate_addref/release` 委托 outer（对）但返回指针错位 | —— |

## 7. 修复方向（依据第 6 表 #1）

`create_aggregate` 应返回**非委托 IUnknown 视图**（vtable 槽 0-2 = na_qi/na_addref/na_release），不是 IAPO 视图：

- NApo 布局新增 `vtbl_nondeg_unknown` 字段（offset 32）：`na_qi`（暴露 inner 接口）/ `na_addref` / `na_release`
- `create_aggregate` 返回 `&apobox.vtbl_nondeg_unknown`
- 引擎 QI(IAPO) → `na_qi` 走 NonDQI 逻辑 → 返回 `&apobox.vtbl_apo`（IAPO 视图，槽 0-2 = 委托 outer）
- 引擎对 IAPO 调方法（Initialize/Lock）→ 正常转发；对 IAPO 调 QI/AddRef/Release → 委托 outer（身份统一）

对齐后引擎行为应与 EAPO 第 4 节时序完全一致。