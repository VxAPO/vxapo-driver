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
- APO_CONNECTION_PROPERTY: { pBuffer, u32ValidFrameCount, u32BufferFlags, u32Signature }
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

## 错误映射
- RegOpenKeyExW 的 ERROR_FILE_NOT_FOUND → 文件未找到
- RegQueryValueExW 的 0x80070005 → 拒绝访问（SAM_READ 缺 KEY_QUERY_VALUE 导致，已修复）
