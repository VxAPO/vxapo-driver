# VxAPO 项目 GUID 备份（定死不改）

> 本文件仅用于备份本项目（vxapo-driver）的 CLSID / IID，**这些值一旦发布即定死，不随代码重构改动**。
> 修改 CLSID 会破坏已安装用户的注册表（regsvr32 注册的键失效、per-device 配置目录失联）。

## CLSID（APO 对象类标识）

| 名称 | 值 | 用途 |
|------|-----|------|
| `CLSID_VXAPO_PRE_MIX` | `{41C34613-D391-459D-A039-72B2B15A1A1D}` | PreMix APO 对象（COM 类注册 + GetRegistrationProperties） |
| `CLSID_VXAPO_POST_MIX` | `{B4A97313-ABC0-45ED-9C33-428B20D39428}` | PostMix APO 对象（COM 类注册 + GetRegistrationProperties） |

- 生成时间：2026-08-02，由 PowerShell `[guid]::NewGuid()` 生成（稳定 V4 UUID，非占位值）
- 定义位置：`src/object/vx_reg_props.rs`（`GUID::from_values` 常量）
- 注册/注销顺序：注册 PostMix → PreMix；注销 PreMix → PostMix（全局 COM 类键，见 `object 7.6`）

## IID（系统接口标识，re-export 自 windows-rs 0.62.2）

系统级接口 IID（`IAudioProcessingObject` 等）由 windows-rs 提供，定义于 `src/sys/com/apo_interfaces.rs`，非本项目自定义——本文件不备份（避免与 windows crate 版本漂移造成误解）。