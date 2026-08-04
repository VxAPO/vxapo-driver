# P0-7 audiodg 加载/无声诊断记录（2026-08-04 现场深挖 + 解决方案方向）

> 本文记录 audiodg 不加载 VxAPO DLL → 加载后无声的完整排查链，含根因、探针证据、EAPO/tympan 对照、聚合委托解决方案。供后续实现聚合委托时直接参照，避免重复排查。

## 1. 阶段一：DLL 不加载（已解决 ✅ commit fb4bb82）

### 根因
Windows 音频引擎读端点槽位 CLSID 后，从 `HKCR\AudioEngine\AudioProcessingObjects\{CLSID}` 取 APO 注册属性。
VxAPO 之前只写 `HKCR\CLSID\{...}`，**漏了 AudioEngine 键** → 引擎 NAME NOT FOUND → 静默拒载（无事件日志）。

### 证据（ProcMon 双重铁证）
```
RegQueryValue ...FxProperties\{d04e05a6...},5 SUCCESS Data: {41C34613...}  ← 引擎读到 VxAPO CLSID
RegOpenKey    HKCR\AudioEngine\AudioProcessingObjects\{41C34613...} NAME NOT FOUND ← 缺键 → 拒载
RegOpenKey    HKCR\AudioEngine\AudioProcessingObjects\{EACD2258...} SUCCESS       ← EAPO 对照：有键 → 能加载
```

### 修复（已入代码）
`register_com_class` 补写 AudioEngine 键；`unregister_com_class` 对称删。

## 2. 阶段二：键字段不全（已解决 ✅）

### 根因
AudioEngine 键需完整 11 字段（EAPO 注册树实证），VxAPO 手动/代码补写时漏了 6 个：
`MajorVersion, MinorVersion, MinInputConnections, MaxInputConnections, MinOutputConnections, MaxOutputConnections`。
字段不全 → 引擎可能只 LoadLibrary 不实例化对象。

### EAPO 键完整结构（reg query 实证）
```
FriendlyName        REG_SZ
Copyright           REG_SZ
MajorVersion        REG_DWORD 1
MinorVersion        REG_DWORD 0
Flags               REG_DWORD 0xd
MinInputConnections REG_DWORD 1
MaxInputConnections REG_DWORD 1
MinOutputConnections REG_DWORD 1
MaxOutputConnections REG_DWORD 1
MaxInstances        REG_DWORD 0xffffffff
NumAPOInterfaces    REG_DWORD 1
APOInterface0       REG_SZ {FD7F2B29-24D0-4B5C-B177-592C39F9CA10}  ← IAudioProcessingObject IID（windows-rs 已确认一致）
```

### 修复（已入代码）
`register_com_class` 已补全 11 字段。

## 3. 阶段三：聚合被拒（已解决 ✅ commit b256de8）

### 根因
Windows 音频引擎**强制用聚合（pUnkOuter 非空）创建 APO**。探针实证：
```
CreateInstance clsid=41C34613 riid=IUnknown punkouter_null=false   ← 引擎传了 outer
```
而我们 `if !punkouter.is_null() { return CLASS_E_NOAGGREGATION }` 直接拒绝 → 引擎静默放弃 → 无声。

### EAPO 对照（Determinative）
EAPO 构造函数 `EqualizerAPO(IUnknown* pUnkOuter)` 明确支持聚合。

### 修复（已入代码）
CreateInstance 聚合时 riid==IUnknown 放行（不再拒绝）。

## 4. 阶段四（当前未解）：聚合接受但引擎弃用对象 → 缺 NonDelegatingUnknown 委托

### 现象
聚合修复后：`CreateInstance SUCCESS`（PostMix）但 **method_probe/initialize_probe/lock_probe 全不存在** —— 引擎创建对象后**一个方法都没调**。

### 探针链完整证据（23:36 服务重启后）
```
DllGetClassObject clsid=B4A97313 riid=IClassFactory hr=0        ← 引擎拿到工厂 ✓
CreateInstance SUCCESS clsid=B4A97313 selfQI_IAPO_hr=0           ← 工厂创建对象 ✓（inner 自检能提供 IAPO）
(method_probe / initialize / lock 全不存在)                       ← 引擎 QI IAudioProcessingObject 失败 → 弃用对象 ✗
```

### 根因
windows-rs `#[implement]` 生成的 IUnknown **自包含、不委托**——它从 inner 上 QI，返回 inner 自己的身份。
引擎聚合创建后第一步 **QI(IAudioProcessingObject) 经 outer 聚合链**，走不到我们 inner（没有 NonDelegating 分离），QI 失败 → 对象被弃用释放 → 从不进入方法调用链。

### EAPO 聚合语义（必须参照，EqualizerAPO.cpp 67-80/519-539）
```cpp
// 外层 IUnknown：全部委托 pUnkOuter（聚合根）
HRESULT QueryInterface(const IID& iid, void** ppv) { return pUnkOuter->QueryInterface(iid, ppv); }
ULONG AddRef() { return pUnkOuter->AddRef(); }
ULONG Release() { return pUnkOuter->Release(); }

// 非委托接口（真正的接口暴露）：inner 自己响应
HRESULT NonDelegatingQueryInterface(const IID& iid, void** ppv) {
    if (iid == IUnknown)                *ppv = static_cast<INonDelegatingUnknown*>(this);
    else if (iid == IAudioProcessingObject)          *ppv = static_cast<IAudioProcessingObject*>(this);
    else if (iid == IAudioProcessingObjectRT)        *ppv = static_cast<IAudioProcessingObjectRT*>(this);
    else if (iid == IAudioProcessingObjectConfiguration) *ppv = static_cast<IAudioProcessingObjectConfiguration*>(this);
    else if (iid == IAudioSystemEffects) *ppv = static_cast<IAudioSystemEffects*>(this);
    else { *ppv = NULL; return E_NOINTERFACE; }
    reinterpret_cast<IUnknown*>(*ppv)->AddRef();
    return S_OK;
}
```

### 为什么 selfQI_IAPO_hr=0 不代表成功
探针是对 **inner 自 QI**（我们内部能提供 IAPO）；引擎是 **经 outer 聚合链 QI**——两条路径不同。引擎路径因无 NonDelegating 委托而失败。

## 5. tympan-apo 对照（无参考实现可抄）

- `D:\Source_Code\tympan-apo-main`（Rust APO）
- `src/raw/class_factory.rs:157` 和 `src/aec/class_factory.rs:119` **都拒绝聚合**（CLASS_E_NOAGGREGATION）
- `src/raw/instance_com.rs:48-55` 也是 `#[implement]`（非手写 vtable）
- target/debug/deps 无主 APO DLL（只有 windows-implement/interface 辅助 DLL）——从未 build 出可加载产物
- **结论**：tympan 未绕过聚合，与「VxAPO 修复前拒绝聚合→DLL 不加载」完全对应。Rust windows-rs 生态无聚合委托先例，需自研。

## 6. 解决方案：手写 COM 聚合委托（待实现）

### 核心
放弃 `#[implement]` 自包含 IUnknown，改手写 vtable 层实现 **INonDelegatingUnknown 聚合委托**：

```
引擎 CoCreateInstance(pUnkOuter=外壳IUnknown)
  → IClassFactory::CreateInstance(pUnkOuter=外壳)
    → 手写 NApo { vtbl, cref, p_unk_outer=外壳 }
引擎 QI(IAudioProcessingObject) → outer QI → 委托回 NApo 的 NonDelegatingQI → 暴露 inner IAPO ✓
引擎 QI(IUnknown)              → outer 自己（聚合身份）✓
引擎 AddRef/Release            → 委托 outer（统一引用计数）✓
```

### vtable 布局（参照 EAPO + windows-rs Vtbl 结构）
- IAudioProcessingObject：IUnknown(3) + Reset/GetLatency/GetRegistrationProperties/Initialize/IsInputFormatSupported/IsOutputFormatSupported/GetInputChannelCount
- IAudioProcessingObjectRT：IUnknown(3) + APOProcess/CalcInputFrames/CalcOutputFrames
- IAudioProcessingObjectConfiguration：IUnknown(3) + LockForProcess/UnlockForProcess
- IAudioSystemEffects：IUnknown(3)（marker）

### 关键函数（伪代码）
```
qi(this, riid, ppv):
    if p_unk_outer == NULL: 自处理（NonDelegatingQI 直接暴露）
    else:
        if riid == IUnknown: 委托 p_unk_outer->QI（返回聚合身份）
        else:                委托 p_unk_outer->QI（引擎外壳会转发到 outer 链）
addref(this): p_unk_outer 非空 → 委托 outer->AddRef
release(this): p_unk_outer 非空 → 委托 outer->Release
```

### 注意点
- 聚合对象**不应自己维护引用计数**（委托 outer）；非聚合时自维护
- CreateInstance 聚合时 riid 必须为 IUnknown（COM 规范），否则 E_NOINTERFACE
- 异常/日志用 AOInitSystemEffects clsid 区分 Pre/Post（架构决策 03 记录）

## 7. 验证命令链（EAPO 语义参照）
```powershell
# 1. 杀 audiodg（释放旧 DLL 句柄）
taskkill /f /im audiodg.exe
# 2. 正规重启服务（net stop/start audiosrv 连带 audiodg 重启，勿用 taskkill 代替）
net stop audiosrv & timeout /t 3 /nobreak >nul & net start audiosrv
# 3. 触发建图（用户音乐播到 EDIFIER，或 SoundPlayer 循环播 tada）
# 4. 读探针（必须提权）
cat C:\ProgramData\VxAPO\createinstance_probe.txt  # CreateInstance SUCCESS
cat C:\ProgramData\VxAPO\method_probe.txt          # 方法是否被调（聚合委托修复后应出现）
cat C:\ProgramData\VxAPO\initialize_probe.txt      # Initialize 应被调
cat C:\ProgramData\VxAPO\lock_probe.txt            # LockForProcess 应被调
```

## 8. 遗留探针清单（全部 debug 门控，实现聚合委托后删除）
| 文件 | 内容 |
|------|------|
| getclassobject_probe.txt | 引擎请求的 CLSID/riid + hr |
| createinstance_probe.txt | CreateInstance punkouter/Success/selfQI |
| initialize_probe.txt | Initialize 被调与否 |
| lock_probe.txt | LockForProcess 被调与否 |
| method_probe.txt | Reset/GetLatency/GetInputChannelCount/Unlock 被调 |
| apo_process_probe.txt | APOProcess 每 200 帧计数 |