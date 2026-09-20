# Changelog

本文件承接代码注释中清理出的历史信息。代码注释只描述「现在的实现是什么、为什么这样设计」；
阶段编号、修复史与日期戳不再写入注释，需要留档的内容集中到本文件。

## 已知限制 / roadmap

- **loudness**：当前为简化实现（1/3 倍频程 GraphicEq 近似 ISO 226 等响曲线），
  完整 ISO 226 查表 + 曲线拟合待实现。
- **长 FIR 分块卷积**：`pipeline/dsp/math.rs::CONVOLUTION_PARTITION_SIZE` 已定义，
  分块卷积处理待接入。
- **biquad 平滑过渡**：`BiquadFilter::set_coeffs` 现为立即切换，参数变化时的平滑过渡待实现。
- **windows-rs 升级**：`DllGetClassObject` 与测试中的 vtable 直调（QI / Release）受
  windows-interface 0.59.3 跨模块方法不可见限制；升级后改用类型化调用
  （`factory.query` / `.release()`）。

## 历史修复背景（自注释转存）

- **聚合模式（无声根因）**：Windows 引擎强制以聚合模式（`pUnkOuter` 非空）创建 APO。
  拒绝聚合、或对多个接口返回同一个 this（vtable 错位），都会让引擎静默弃用对象
  （完全无声）。现由 `object/apo/aggregate.rs` 提供 EAPO 同款 NonDelegating 语义与
  4 个独立 vtable 指针布局。
- **AudioEngine APO 注册键**：缺 `HKCR\AudioEngine\AudioProcessingObjects\{CLSID}` 时
  引擎静默拒载 DLL（不加载、无事件日志）；注册/注销必须与 CLSID 键对称维护，
  11 个属性字段需齐全（缺字段 → 只 LoadLibrary 不实例化）。
- **.reg 导出编码**：QWORD 必须为 `hex(b)` 8 字节小端；REG_MULTI_SZ 必须为 `hex(7)`
  UTF-16LE + 双终止。只导低 2 字节 / 写转义文本都会让导出的 .reg 无法恢复。
- **极端衰减**：-1000 dB clamp 到 -120 dB 后极点贴单位圆不稳定，回退为 -60 dB 稳定
  深切而非整段直通（直通会让极端衰减看起来“没生效”）。
- **槽位删除失败语义**：0x80070005 来自句柄权限（`SAM_ALL` 含未授予的 CreateSubKey
  位、或只读句柄打开），不是“端点被占用”。槽位值的写/删只需 `KEY_SET_VALUE` 句柄；
  停服/重启端点只用于让变更生效（引擎缓存端点 APO 链）。
- **端点定位快速路径**：DeviceClasses 实例键名可由归一化设备 ID 直接构造，避免首次
  切换端点时数百次注册表打开阻塞音频服务控制线程（否则切换设备后首秒音频断续）。
