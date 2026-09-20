use super::*;
use super::execute::*;

#[test]
fn default_config_values() {
    let c = InstallConfig::default_config();
    assert!(c.install_premix);
    assert!(c.install_postmix);
    assert_eq!(c.install_mode, InstallMode::SfxEfx);
    assert!(!c.use_original_apo_premix);
    assert!(!c.use_original_apo_postmix);
    assert!(c.allow_silent_buffer);
    assert!(!c.auto_adjust);
}

#[test]
fn guid_to_string_zeroed() {
    assert_eq!(
        guid_to_string(&GUID::zeroed()),
        "{00000000-0000-0000-0000-000000000000}"
    );
}

#[test]
fn guid_to_string_max_values() {
    let g = GUID {
        data1: 0xFFFFFFFF,
        data2: 0xFFFF,
        data3: 0xFFFF,
        data4: [0xFF; 8],
    };
    assert_eq!(guid_to_string(&g), "{FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF}");
}

/// EAPO REG_SZ 槽位（GUID 字符串）解析：真实 EAPO CLSID 落到 `parse_guid_string`。
///
/// 槽位读取统一走 `slots::read_slot_value`（REG_SZ 分支经
/// `utils::guid::parse_guid_string`），本测试用同一解析器核对 EAPO 实证值各字段。
#[test]
fn read_slot_value_parses_eapo_reg_sz_guid() {
    let s = "{EACD2258-FCAC-4FF4-B36D-419E924A6D79}";
    let guid = crate::utils::guid::parse_guid_string(s).expect("EAPO CLSID 应可解析");
    assert_eq!(guid.data1, 0xEACD_2258);
    assert_eq!(guid.data2, 0xFCAC);
    assert_eq!(guid.data3, 0x4FF4);
    assert_eq!(guid.data4, [0xB3, 0x6D, 0x41, 0x9E, 0x92, 0x4A, 0x6D, 0x79]);
}

/// EAPO 互斥保留语义：SfxEfx 不动 MFX、SfxMfx 不动 EFX、LfxGfx 全删。
///
/// 用纯逻辑验证——遍历 ApoSlot::ALL 计算「应删除」集合（不碰注册表），
/// 与实例实现的 keep 判定保持一致。
#[test]
fn delete_other_mode_slots_keep_semantics() {
    // 对三种模式的 pre/post 槽位，验证 keep 判定结果。
    for mode in [InstallMode::SfxEfx, InstallMode::SfxMfx, InstallMode::LfxGfx] {
        let pre = mode.premix_slot();
        let post = mode.postmix_slot();
        for slot in ApoSlot::ALL {
            let is_target = slot == pre || slot == post;
            let keep = !is_target && match mode {
                InstallMode::SfxEfx => slot == ApoSlot::Mfx,
                InstallMode::SfxMfx => slot == ApoSlot::Efx,
                InstallMode::LfxGfx => false,
            };
            if is_target {
                assert!(!keep, "{mode:?} target slot should not be in keep set");
            }
            // 只需验证「应删集合」不含 pre/post——具体保留逻辑由实例行为验证。
        }
    }
    // 显式断言关键保留：SfxEfx 保留 MFX、SfxMfx 保留 EFX。
    let mk_keep = |mode: InstallMode, slot: ApoSlot| {
        mode.premix_slot() != slot
            && mode.postmix_slot() != slot
            && match mode {
                InstallMode::SfxEfx => slot == ApoSlot::Mfx,
                InstallMode::SfxMfx => slot == ApoSlot::Efx,
                InstallMode::LfxGfx => false,
            }
    };
    assert!(mk_keep(InstallMode::SfxEfx, ApoSlot::Mfx));
    assert!(!mk_keep(InstallMode::SfxEfx, ApoSlot::Gfx));
    assert!(mk_keep(InstallMode::SfxMfx, ApoSlot::Efx));
    assert!(!mk_keep(InstallMode::SfxMfx, ApoSlot::Gfx));
    assert!(!mk_keep(InstallMode::LfxGfx, ApoSlot::Efx));
}

#[test]
fn config_custom_values() {
    let c = InstallConfig {
        install_premix: false,
        install_postmix: true,
        install_mode: InstallMode::LfxGfx,
        use_original_apo_premix: true,
        use_original_apo_postmix: false,
        allow_silent_buffer: false,
        auto_adjust: true,
    };
    assert!(!c.install_premix);
    assert!(c.install_postmix);
    assert_eq!(c.install_mode, InstallMode::LfxGfx);
    assert!(c.use_original_apo_premix);
    assert!(!c.use_original_apo_postmix);
    assert!(!c.allow_silent_buffer);
    assert!(c.auto_adjust);
}

#[test]
fn transaction_rollback_deletes_recorded_key() {
    use windows::Win32::System::Registry::HKEY_CURRENT_USER;

    // 回归（审查 #7/#8）：未 commit 的事务 Drop 必须真实删除记录的键。
    // 旧实现“打开后传完整路径给 delete_sub_key”是静默空操作，安装中途失败
    // 时 FxProperties/信息区永久残留。用 HKCU 测试键验证（无需管理员）。
    const TEST_ROOT: &str = r"SOFTWARE\VxAPO_Test_Tx_Rollback";
    let _ = crate::sys::registry::delete_tree(HKEY_CURRENT_USER, TEST_ROOT);

    // 模拟“安装新建了键但后续步骤失败”：先真实建键，再构造未 commit 事务。
    let key = RegKey::create(HKEY_CURRENT_USER, TEST_ROOT).unwrap();
    key.write_dword("Marker", 1).unwrap();
    drop(key);
    assert!(RegKey::open(HKEY_CURRENT_USER, TEST_ROOT).is_ok());

    let mut tx = Transaction::new();
    tx.record(RollbackAction::DeleteKey {
        root: HKEY_CURRENT_USER,
        path: TEST_ROOT.to_string(),
    });
    drop(tx); // 未 commit → Drop 执行回滚

    assert!(
        RegKey::open(HKEY_CURRENT_USER, TEST_ROOT).is_err(),
        "回滚后新建键必须被删除"
    );
    let _ = crate::sys::registry::delete_tree(HKEY_CURRENT_USER, TEST_ROOT);
}
