# windows-rs 0.62.2 已实测 API（源码级确认，勿再猜测）

## GUID
- windows-core 0.62.2 的 GUID 无 Display（实测确认）
- GUID::Debug 不带花括号
- 格式化用 `StringFromGUID2`，封装在 `sys/com/prelude.rs` 的 `guid_to_string`，只需传入 `&GUID`
- 底层 FFI 签名：`(rguid: &GUID, lpsz: &mut [u16]) -> i32`（windows-rs 0.62.2 已合并 cchMax 进 slice 长度）
- 返回带花括号的字符串 `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`（与 `GUID::Debug` 不同）

## APO 模块 _Impl trait（Apo/mod.rs）
// IAudioProcessingObject_Impl（用 windows_core::Result，非裸 HRESULT）
fn Reset(&self) -> Result<()>;
fn GetLatency(&self) -> Result<i64>;
fn GetRegistrationProperties(&self) -> Result<*mut APO_REG_PROPERTIES>;
fn Initialize(&self, cbdatasize: u32, pbydata: *const u8) -> Result<()>;
fn IsInputFormatSupported(&self, poppositeformat: Ref<IAudioMediaType>, prequestedinputformat: Ref<IAudioMediaType>) -> Result<IAudioMediaType>;
fn IsOutputFormatSupported(&self, ...) -> Result<IAudioMediaType>;
fn GetInputChannelCount(&self) -> Result<u32>;

// Configuration_Impl
fn LockForProcess(&self, u32numinputconnections: u32, ppinputconnections: *const *const APO_CONNECTION_DESCRIPTOR, u32numoutputconnections: u32, ppoutputconnections: *const *const APO_CONNECTION_DESCRIPTOR) -> Result<()>;
fn UnlockForProcess(&self) -> Result<()>;

// RT_Impl（返回 ()，不是 Result）
fn APOProcess(&self, u32numinputconnections: u32, ppinputconnections: *const *const APO_CONNECTION_PROPERTY, u32numoutputconnections: u32, ppoutputconnections: *mut *mut APO_CONNECTION_PROPERTY);
fn CalcInputFrames(&self, u32outputframecount: u32) -> u32;
fn CalcOutputFrames(&self, u32inputframecount: u32) -> u32;

text

## APO 类型（camelCase，re-export 自 windows::Win32::Media::Audio::Apo）
- APO_CONNECTION_DESCRIPTOR: { Type, pBuffer, u32MaxFrameCount, pFormat, u32Signature }
- **pFormat 实测类型（windows-rs 0.62.2）**：`ManuallyDrop<Option<IAudioMediaType>>`（非裸指针！）——SDK 头文件写 `IAudioMediaType *pFormat`，但 windows-rs 绑定为 ManuallyDrop 包装；读取用 `.pFormat.as_ref()` 得 `Option<&IAudioMediaType>`，再转 `*mut IAudioMediaType` 传给 extract_format
- APO_CONNECTION_PROPERTY: { pBuffer, u32ValidFrameCount, u32BufferFlags, u32Signature }
- `windows::core::Ref<T>` 无 `as_ptr`/`get` 方法，Deref 到 `T`；对接口 `Ref<IAudioMediaType>` Deref 目标为 `Option<IAudioMediaType>`，用 `.as_ref()` 取 `Option<&T>`，返回接口值用 clone
- APO_REG_PROPERTIES: { clsid, Flags, szFriendlyName, szCopyrightInfo, u32MajorVersion, u32MinorVersion, ... }
- APO_FLAG 常量: INPLACE(1)/SAMPLESPERFRAME_MUST_MATCH(2)/FRAMESPERSECOND_MUST_MATCH(4)/BITSPERSAMPLE_MUST_MATCH(8)/MIXER(16)/DEFAULT(14)/NONE(0)
- BUFFER_INVALID/VALID/SILENT 为 APO_BUFFER_FLAGS 常量

## Registry 模块（Registry/mod.rs）
- RegOpenKeyExW: `(hkey, lpsubkey: &HSTRING, uloptions: Option<u32>, samdesired: REG_SAM_FLAGS, phkresult: *mut HKEY)`
- RegCreateKeyExW: `(hkey, lpsubkey, reserved: Option<u32>, lpclass, dwoptions: REG_OPEN_CREATE_OPTIONS, samdesired: REG_SAM_FLAGS, lpsecurityattributes, phkresult, lpdwdisposition)`
  - REG_OPEN_CREATE_OPTIONS 常量: REG_OPTION_NON_VOLATILE(0)，无 REG_CREATE_NEW_KEY/REG_OPEN_EXISTING_KEY
- RegQueryValueExW: `(hkey, lpvaluename, lpreserved: Option<*const u32>, lptype: Option<*mut REG_VALUE_TYPE>, lpdata: Option<*mut u8>, lpcbdata: Option<*mut u32>)`
- RegSetValueExW: `(hkey, lpvaluename, reserved: Option<u32>, dwtype: REG_VALUE_TYPE, lpdata: Option<&[u8]>)`（无 size 参数）
- RegDeleteValueW/RegDeleteTreeW: `(hkey, lpsubkey: &HSTRING)`
- REG_SZ/REG_DWORD/REG_BINARY/REG_NONE/REG_MULTI_SZ 为 REG_VALUE_TYPE 类型，比较用 `value_type == REG_SZ`
- KEY_READ: REG_SAM_FLAGS = REG_SAM_FLAGS(0x0002_0019) = STANDARD_RIGHTS_READ|KEY_QUERY_VALUE|KEY_ENUMERATE_SUB_KEYS|KEY_NOTIFY
- KEY_ALL_ACCESS = REG_SAM_FLAGS(983103)

## Registry 枚举 API（v6.4 实测追加，经 rust-analyzer 报错 + cargo check 实证）
- RegEnumKeyExW: `(hkey, dwindex: u32, lpname: Option<PWSTR>, lpcchname: &mut u32, lpreserved: ...)` —— 第 3 参是 `Option<PWSTR>`，不是 `&mut [u16]`；用 `Some(PWSTR(buf.as_mut_ptr()))` + `&mut buf.len() as u32`
- RegEnumValueW: 同上，第 3 参也是 `Option<PWSTR>`
- REG_SAM_FLAGS 是 `#[repr(transparent)] pub struct REG_SAM_FLAGS(pub u32)`（非 typealias，需 import 才能用 `REG_SAM_FLAGS(...)`）
- 枚举终止：err.0 == 259（ERROR_NO_MORE_ITEMS）或 is_not_found(2/3) 时 break

## P0-3 实测追加（windows-rs 0.62.2 源码级确认，P0-3 commit bb708b3）
- `SHGetKnownFolderPath(rfid: *const GUID, dwflags: KNOWN_FOLDER_FLAG, htoken: Option<HANDLE>) -> Result<PWSTR>`（非裸 HRESULT；需 Win32_UI_Shell feature）
- `FOLDERID_Documents: GUID = 0xfdd39ad0_238f_46af_adb4_6c85480369c7`（windows::Win32::UI::Shell）
- `CoTaskMemFree(pv: Option<*const c_void>)`（windows::Win32::System::Com）
- `PWSTR::to_string(&self) -> Result<String, FromUtf16Error>`（unsafe，windows-strings 0.5.1 / pwstr.rs:61）
- `APOInitSystemEffects`（windows::Win32::Media::Audio::Apo，需 Win32_UI_Shell_PropertiesSystem feature）：
  - 字段（实测，非规范假设）：`{ APOInit: APOInitBaseStruct, pAPOEndpointProperties: ManuallyDrop<Option<IPropertyStore>>, pAPOSystemEffectsProperties: ManuallyDrop<Option<IPropertyStore>>, pReserved: *mut c_void, pDeviceCollection: ManuallyDrop<Option<IMMDeviceCollection>> }`
  - **无 `pSystemEffectsProperties->pEndpointGuid` 直接字段**——端点 GUID 经 `pAPOSystemEffectsProperties.get()?.GetValue(&PKEY_AudioEndpoint_GUID)` 返回 PROPVARIANT 提取
- `APOInitBaseStruct { cbSize: u32, clsid: GUID }`（Initialize 参数校验 cb_size 用）
- `PKEY_AudioEndpoint_GUID: PROPERTYKEY`（fmtid 0x1da5d803_d492_4edd_8c23_e0c0ffee7f0e, pid 4）
- `IPropertyStore::GetValue(&PROPERTYKEY) -> Result<PROPVARIANT>`（unsafe；需 Win32_UI_Shell_PropertiesSystem）
- `PROPVARIANT`（需 Win32_System_Com_StructuredStorage + Win32_System_Variant）：`{ Anonymous: PROPVARIANT_0 }`（union）→ `Anonymous.Anonymous.vt`（VARENUM）/ `Anonymous.Anonymous.Anonymous.puuid: *mut GUID`
- `VT_CLSID: VARENUM = VARENUM(72)`（windows::Win32::System::Variant）

## 错误映射
- `ERROR_FILE_NOT_FOUND` / `ERROR_PATH_NOT_FOUND` 已在 `windows::Win32::Foundation` 导出：
  `pub const ERROR_FILE_NOT_FOUND: WIN32_ERROR = WIN32_ERROR(2u32)`（Foundation/mod.rs:2355）、
  `pub const ERROR_PATH_NOT_FOUND: WIN32_ERROR = WIN32_ERROR(3u32)`（:3689）
  ——**无需自定义**，直接用 `windows::Win32::Foundation::ERROR_FILE_NOT_FOUND` 等
- RegOpenKeyExW 返回 ERROR_FILE_NOT_FOUND(2) → 键不存在（文件未找到）
- RegQueryValueExW 的 0x80070005 → 拒绝访问（SAM_READ 缺 KEY_QUERY_VALUE 导致，已修复）
