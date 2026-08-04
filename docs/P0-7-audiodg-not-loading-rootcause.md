# P0-7 audiodg 不加载 VxAPO 根因（2026-08-04 实证）

## 症状
VxAPO 装在 EDIFIER 设备 0 独立父槽位（SFX={41C34613...} / EFX={B4A97313...}）时，
audiodg **不加载** vxapo_driver.dll；EAPO 同设备同条件加载。

## 排查过程（逐项排除）
| 项 | 结果 |
|---|---|
| CLSID 注册 / InprocServer32 / ThreadingModel | HKLM\SOFTWARE\Classes 下与 EAPO 一致 |
| DisableProtectedAudioDG | =1（install 强制写） |
| 槽位值格式 | REG_SZ GUID 字符串，与 EAPO 一致 |
| ProcessingModes | REG_MULTI_SZ {C18E2F7E-...}，与 EAPO 一致 |
| APO_FLAGS | 0xD，与 EAPO 一致 |
| fxTitle | 新建时写，与 EAPO 语义一致 |
| DLL 二进制（dumpbin） | 导出四件套一致；VCRUNTIME140.dll（非 debug CRT）；headers 一致 |
| u32NumAPOInterfaces | 曾误写 3（数组 len=1）→ 已修为 1（编译期断言） |
| IAudioSystemEffects | EAPO 实现（marker），VxAPO 已补（impl 空体） |

## ProcMon 决定性铁证（2026-08-04 21:29，svchost=AudioEndpointBuilder 服务重启枚举）

VxAPO 槽位被读到，但 AudioEngine 注册键缺失 → 引擎静默跳过：

```
RegQueryValue  ...\FxProperties\{d04e05a6-...},5  SUCCESS  Data: {41C34613-...}   ← 读到 VxAPO CLSID
RegOpenKey     HKCR\AudioEngine\AudioProcessingObjects\{41C34613-...}  NAME NOT FOUND  ← 缺键 → 拒载
RegQueryValue  ...\FxProperties\{d04e05a6-...},7  SUCCESS  Data: {B4A97313-...}
RegOpenKey     HKCR\AudioEngine\AudioProcessingObjects\{B4A97313-...}  NAME NOT FOUND
```

EAPO 对照（2026-08-04 21:41，用户装回 EAPO 后重抓）：

```
RegOpenKey     HKCR\AudioEngine\AudioProcessingObjects\{EACD2258-...}  SUCCESS  ← EAPO 有键 → 能加载
```

## EAPO 该键的完整结构（reg query 实证）

```
HKCR\AudioEngine\AudioProcessingObjects\{EACD2258-FCAC-4FF4-B36D-419E924A6D79}
    FriendlyName      REG_SZ    EqualizerAPO
    Copyright         REG_SZ    Copyright (C) 2015
    MajorVersion      REG_DWORD  0x1
    MinorVersion      REG_DWORD  0x0
    Flags             REG_DWORD  0xd
    MinInputConnections  REG_DWORD  0x1
    MaxInputConnections  REG_DWORD  0x1
    MinOutputConnections REG_DWORD  0x1
    MaxOutputConnections REG_DWORD  0x1
    MaxInstances      REG_DWORD  0xffffffff
    NumAPOInterfaces  REG_DWORD  0x1
    APOInterface0     REG_SZ    {FD7F2B29-24D0-4B5C-B177-592C39F9CA10}  ← IAudioProcessingObject::IID
```

## 根因结论
Windows 音频引擎读端点槽位 CLSID 后，从
`HKCR\AudioEngine\AudioProcessingObjects\{CLSID}` 取 APO 注册属性（Flags/接口数等）。
**缺失该键 → 引擎静默跳过该 APO（DLL 不加载、无事件日志）**。
VxAPO 之前只写了 `HKCR\CLSID\{...}`，漏了 `HKCR\AudioEngine\AudioProcessingObjects\{...}`。

## 修复
- `register_com_class` 补写 `AudioEngine\AudioProcessingObjects\{CLSID}` 键
  （FriendlyName/Copyright/Flags=0xd/NumAPOInterfaces=1/APOInterface0）
- `unregister_com_class` 删除该键
- 手动验证用键已补写（write_audioengine_keys.bat）