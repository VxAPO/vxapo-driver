//! object/apo/rtdump.rs — RT 转储诊断（临时调试用）

//! HKLM\\SOFTWARE\\VxAPO\\RtDumpSecs > 0 时把流的前 N 秒逐帧写入
//! C:\\ProgramData\\VxAPO\\rt_dump_*.f32，供离线分析。RT 路径只写内存，落盘在控制线程。

/// 临时 RT 转储开关（诊断）：读 `HKLM\SOFTWARE\VxAPO\RtDumpSecs`（DWORD）。
/// 返回 `Some((路径, 缓冲, 总帧数))` 表示本次 Lock 需要采集该流前 N 秒的
/// [in_L,in_R,out_L,out_R] 平面数据（f32 小端，4 值/帧）。
pub(crate) fn rt_dump_open(
    sample_rate: u32,
) -> Option<(std::path::PathBuf, Vec<f32>, usize)> {
    use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
    let secs = crate::sys::registry::RegKey::open(
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\VxAPO",
    )
    .and_then(|k| k.read_dword_value("RtDumpSecs"))
    .unwrap_or(0) as usize;
    if secs == 0 {
        return None;
    }
    // 独立时间戳文件名：旧 dump 可能被锁（上次实例句柄未释放），避免覆盖失败。
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = format!(r"C:\ProgramData\VxAPO\rt_dump_{ts}.f32");
    let frames = secs * sample_rate.max(1) as usize;
    Some((
        std::path::PathBuf::from(path),
        Vec::with_capacity(frames * 4),
        frames,
    ))
}

/// 控制线程落盘（Unlock 时调用）：内存缓冲 → 文件，失败静默。
pub(crate) fn rt_dump_flush(rt_dump: Option<(std::path::PathBuf, Vec<f32>, usize)>) {
    use std::io::Write;
    let Some((path, buf, _)) = rt_dump else { return };
    if buf.is_empty() {
        return;
    }
    // SAFETY: buf 为 f32 向量，按小端原始字节写出（与 Python/NumPy frombuffer 兼容）。
    let bytes = unsafe {
        std::slice::from_raw_parts(buf.as_ptr() as *const u8, buf.len() * 4)
    };
    if let Ok(mut f) = std::fs::File::create(&path) {
        let _ = f.write_all(bytes);
    }
}

/// 把本帧 `[in_L, in_R, out_L, out_R]` 追加进转储缓冲（RT 路径：只写内存，
/// 不做文件 I/O、不额外分配）。
///
/// # Safety
/// `in_ptr` 必须指向引擎按 `max_frame_count × in_ch` 分配的输入缓冲（同
/// `checked_interleaved_slice` 的契约）或为 null；仅在 `in_ch >= 1` 时解引用。
pub(crate) unsafe fn rt_dump_push(
    rt_dump: &mut Option<(std::path::PathBuf, Vec<f32>, usize)>,
    frames: usize,
    in_ch: usize,
    out_ch: usize,
    in_ptr: *const f32,
    out_slice: &[f32],
) {
    let Some((_, buf, remaining)) = rt_dump.as_mut() else {
        return;
    };
    if *remaining == 0 {
        return;
    }
    let n = frames.min(*remaining);
    for f in 0..n {
        let in_base = f * in_ch;
        let out_base = f * out_ch;
        let il = if in_ch >= 1 { unsafe { *in_ptr.add(in_base) } } else { 0.0 };
        let ir = if in_ch >= 2 { unsafe { *in_ptr.add(in_base + 1) } } else { 0.0 };
        let ol = if out_ch >= 1 { out_slice[out_base] } else { 0.0 };
        let or_ = if out_ch >= 2 { out_slice[out_base + 1] } else { 0.0 };
        buf.extend_from_slice(&[il, ir, ol, or_]);
    }
    *remaining -= n;
}