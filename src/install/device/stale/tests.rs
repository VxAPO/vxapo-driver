use super::*;
use super::detect::*;
use super::migrate::*;

#[test]
fn extracts_device_instance_from_sysfx_path() {
    let path = r"SYSTEM\CurrentControlSet\Control\DeviceClasses\{65E8773E-8F56-11D0-A3B9-00A0C9223196}\##?#USB#VID_2D99&PID_A037&MI_00#6&20be7186&2&0000#{65e8773e-8f56-11d0-a3b9-00a0c9223196}\#GLOBAL\Device Parameters\MSFX\0";
    assert_eq!(
        extract_device_instance_id(path).as_deref(),
        Some(r"USB\VID_2D99&PID_A037&MI_00\6&20BE7186&2&0000")
    );
}

#[test]
fn mode_inference_covers_three_modes() {
    let n = "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},";
    assert_eq!(
        mode_from_slots(&format!("{n}5"), &format!("{n}7")),
        Some(InstallMode::SfxEfx)
    );
    assert_eq!(
        mode_from_slots(&format!("{n}5"), &format!("{n}6")),
        Some(InstallMode::SfxMfx)
    );
    assert_eq!(
        mode_from_slots(&format!("{n}0"), &format!("{n}3")),
        Some(InstallMode::LfxGfx)
    );
}

#[test]
fn transient_sharing_errors_are_retryable() {
    // 5 ACCESS_DENIED / 32 SHARING_VIOLATION / 33 LOCK_VIOLATION。
    for code in [5, 32, 33] {
        let e = std::io::Error::from_raw_os_error(code);
        assert!(is_transient_sharing_error(&e), "code {code} 应可重试");
    }
    // 路径不存在等确定性错误不重试（避免无意义等待）。
    let missing = std::io::Error::from_raw_os_error(2);
    assert!(!is_transient_sharing_error(&missing));
    let not_found = std::io::Error::new(std::io::ErrorKind::NotFound, "x");
    assert!(!is_transient_sharing_error(&not_found));
}

#[test]
fn rename_with_retry_replaces_destination() {
    let dir = std::env::temp_dir().join("vxapo_stale_rename_test");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.tmp");
    let dst = dir.join("dst.toml");
    fs::write(&src, b"new-content").unwrap();
    fs::write(&dst, b"old-content").unwrap();
    rename_with_retry(&src, &dst).unwrap();
    assert_eq!(fs::read_to_string(&dst).unwrap(), "new-content");
    assert!(!src.exists());
    let _ = fs::remove_dir_all(&dir);
}
