//! sys/consts.rs — 跨模块共享常量（VxAPO CLSID / APO 错误码）
//!
//! 单一来源：CLSID 与 APOERR_* 只在此定义，其余模块 re-export
//! （`sys/com/prelude.rs`、`sys/com/apo_types.rs`、`object/vx_reg_props.rs`、`utils/vx_error.rs`）。
//!
//! 只含常量定义，无逻辑、无 Windows API 调用。

use windows::core::{GUID, HRESULT};

// ── VxAPO COM 类 CLSID ──────────────────────────────────────────────────────
// 由 PowerShell `[guid]::NewGuid()` 生成，**定死不改动**；备份见 .clinerules/07-VxAPO_GUID.md。
// PRE_MIX = 41C34613-D391-459D-A039-72B2B15A1A1D
// POST_MIX = B4A97313-ABC0-45ED-9C33-428B20D39428

/// PreMix APO CLSID。
pub const CLSID_VXAPO_PRE_MIX: GUID = GUID::from_values(
    0x41C34613,
    0xD391,
    0x459D,
    [0xA0, 0x39, 0x72, 0xB2, 0xB1, 0x5A, 0x1A, 0x1D],
);

/// PostMix APO CLSID。
pub const CLSID_VXAPO_POST_MIX: GUID = GUID::from_values(
    0xB4A97313,
    0xABC0,
    0x45ED,
    [0x9C, 0x33, 0x42, 0x8B, 0x20, 0xD3, 0x94, 0x28],
);

// ── APO 专用 HRESULT 错误码（规范 3.3.7）────────────────────────────────────
pub const APOERR_ALREADY_INITIALIZED: HRESULT = HRESULT(0x887D_0001u32 as i32);
pub const APOERR_NOT_INITIALIZED: HRESULT = HRESULT(0x887D_0002u32 as i32);
pub const APOERR_FORMAT_NOT_SUPPORTED: HRESULT = HRESULT(0x887D_0003u32 as i32);
pub const APOERR_INVALID_APO_CLSID: HRESULT = HRESULT(0x887D_0004u32 as i32);
pub const APOERR_BUFFERS_OVERLAP: HRESULT = HRESULT(0x887D_0005u32 as i32);
pub const APOERR_ALREADY_UNLOCKED: HRESULT = HRESULT(0x887D_0006u32 as i32);
pub const APOERR_NUM_CONNECTIONS_INVALID: HRESULT = HRESULT(0x887D_0007u32 as i32);
pub const APOERR_INVALID_OUTPUT_MAXFRAMECOUNT: HRESULT = HRESULT(0x887D_0008u32 as i32);
pub const APOERR_INVALID_CONNECTION_FORMAT: HRESULT = HRESULT(0x887D_0009u32 as i32);
pub const APOERR_APO_LOCKED: HRESULT = HRESULT(0x887D_000Au32 as i32);
pub const APOERR_INVALID_COEFFCOUNT: HRESULT = HRESULT(0x887D_000Bu32 as i32);
pub const APOERR_INVALID_COEFFICIENT: HRESULT = HRESULT(0x887D_000Cu32 as i32);
pub const APOERR_INVALID_CURVE_PARAM: HRESULT = HRESULT(0x887D_000Du32 as i32);
pub const APOERR_INVALID_INPUTID: HRESULT = HRESULT(0x887D_000Eu32 as i32);
