# VxAPO Driver

VxAPO Driver is the Windows APO DLL that provides real-time audio DSP processing.

## Features

- Windows APO COM object implementation
- Real-time DSP pipeline with zero-allocation RT path
- TOML-based per-device configuration
- Hot reload with dual-chain transition
- Device install/uninstall support
- Hybrid PEQ, preamp, reverb, maximizer, wide, loudness, and aural effects

## Build

```bash
cargo build --release
```

## Documentation

See `../vxapo-docs` for project documentation, including the detailed module reference under `../vxapo-docs/driver`.

## License

GPL-3.0-or-later

---

# VxAPO Driver

VxAPO Driver 是提供实时音频 DSP 处理的 Windows APO DLL。

## 功能

- Windows APO COM 对象实现
- 实时 DSP 处理链路，RT 路径零分配
- 基于 TOML 的每设备配置
- 热重载与双链过渡
- 设备安装/卸载支持
- 混合 PEQ、preamp、reverb、maximizer、wide、loudness、aural 效果器

## 构建

```bash
cargo build --release
```

## 文档

项目文档见 `../vxapo-docs`，详细模块规范见 `../vxapo-docs/driver`。

## 许可证

GPL-3.0-or-later
