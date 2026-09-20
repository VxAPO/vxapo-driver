use super::*;

// ── ApoSlot ───────────────────────────────────────────────────────────

#[test]
fn slot_indices() {
    assert_eq!(ApoSlot::Lfx.index(), 0);
    assert_eq!(ApoSlot::Gfx.index(), 1);
    assert_eq!(ApoSlot::Sfx.index(), 2);
    assert_eq!(ApoSlot::Mfx.index(), 3);
    assert_eq!(ApoSlot::Efx.index(), 4);
}

#[test]
fn slot_all_contains_5() {
    assert_eq!(ApoSlot::ALL.len(), 5);
}

#[test]
fn slot_all_unique() {
    let mut indices: Vec<u8> = ApoSlot::ALL.iter().map(|s| s.index()).collect();
    indices.sort();
    assert_eq!(indices, vec![0, 1, 2, 3, 4]);
}

#[test]
fn slot_value_names_format() {
    for slot in ApoSlot::ALL {
        let name = slot.value_name();
        assert!(name.starts_with('{'), "value_name should start with '{{': {}", name);
        assert!(name.contains(APO_FX_PROPERTY_GUID));
        assert!(name.ends_with(&format!(",{}", slot.registry_pid())));
    }
}

#[test]
fn slot_registry_pids_match_windows() {
    // Windows 真实 PID：0/3/5/6/7（reg query 实证）。
    assert_eq!(ApoSlot::Lfx.registry_pid(), 0);
    assert_eq!(ApoSlot::Gfx.registry_pid(), 3);
    assert_eq!(ApoSlot::Sfx.registry_pid(), 5);
    assert_eq!(ApoSlot::Mfx.registry_pid(), 6);
    assert_eq!(ApoSlot::Efx.registry_pid(), 7);
}

#[test]
fn slot_value_names_distinct() {
    let names: Vec<String> = ApoSlot::ALL.iter().map(|s| s.value_name()).collect();
    let unique_count = names.iter().collect::<std::collections::HashSet<_>>().len();
    assert_eq!(unique_count, 5);
}

#[test]
fn slot_is_premix() {
    assert!(ApoSlot::Lfx.is_premix());
    assert!(ApoSlot::Sfx.is_premix());
    assert!(!ApoSlot::Gfx.is_premix());
    assert!(!ApoSlot::Mfx.is_premix());
    assert!(!ApoSlot::Efx.is_premix());
}

#[test]
fn slot_is_postmix() {
    assert!(!ApoSlot::Lfx.is_postmix());
    assert!(!ApoSlot::Sfx.is_postmix());
    assert!(ApoSlot::Gfx.is_postmix());
    assert!(ApoSlot::Mfx.is_postmix());
    assert!(ApoSlot::Efx.is_postmix());
}

// ── InstallMode ───────────────────────────────────────────────────────

#[test]
fn mode_default_is_sfx_efx() {
    assert_eq!(InstallMode::default_mode(), InstallMode::SfxEfx);
}

#[test]
fn mode_premix_slots() {
    assert_eq!(InstallMode::LfxGfx.premix_slot(), ApoSlot::Lfx);
    assert_eq!(InstallMode::SfxMfx.premix_slot(), ApoSlot::Sfx);
    assert_eq!(InstallMode::SfxEfx.premix_slot(), ApoSlot::Sfx);
}

#[test]
fn mode_postmix_slots() {
    assert_eq!(InstallMode::LfxGfx.postmix_slot(), ApoSlot::Gfx);
    assert_eq!(InstallMode::SfxMfx.postmix_slot(), ApoSlot::Mfx);
    assert_eq!(InstallMode::SfxEfx.postmix_slot(), ApoSlot::Efx);
}

#[test]
fn mode_premix_slots_are_premix_type() {
    for mode in [InstallMode::LfxGfx, InstallMode::SfxMfx, InstallMode::SfxEfx] {
        assert!(mode.premix_slot().is_premix(), "{:?} premix should be premix type", mode);
    }
}

#[test]
fn mode_postmix_slots_are_postmix_type() {
    for mode in [InstallMode::LfxGfx, InstallMode::SfxMfx, InstallMode::SfxEfx] {
        assert!(mode.postmix_slot().is_postmix(), "{:?} postmix should be postmix type", mode);
    }
}

// ── SlotValue ─────────────────────────────────────────────────────────

#[test]
fn slot_value_is_guid() {
    let g = GUID::zeroed();
    assert!(SlotValue::Guid(g).is_guid());
    assert!(!SlotValue::NoKey.is_guid());
    assert!(!SlotValue::NoValue.is_guid());
}

#[test]
fn slot_value_is_empty() {
    assert!(SlotValue::NoKey.is_empty());
    assert!(SlotValue::NoValue.is_empty());
    assert!(!SlotValue::Guid(GUID::zeroed()).is_empty());
}

#[test]
fn slot_value_as_guid() {
    let g = GUID::zeroed();
    assert_eq!(SlotValue::Guid(g).as_guid(), Some(g));
    assert_eq!(SlotValue::NoKey.as_guid(), None);
    assert_eq!(SlotValue::NoValue.as_guid(), None);
}

#[test]
fn slot_value_debug() {
    assert_eq!(format!("{:?}", SlotValue::NoKey), "NoKey");
    assert_eq!(format!("{:?}", SlotValue::NoValue), "NoValue");
    let dbg = format!("{:?}", SlotValue::Guid(GUID::zeroed()));
    assert!(dbg.starts_with("Guid("));
}

#[test]
fn slot_value_clone() {
    let v = SlotValue::Guid(GUID::zeroed());
    let v2 = v.clone();
    assert_eq!(v, v2);
}

// ── guid 格式化 / 解析 ────────────────────────────────────────────────

#[test]
fn guid_to_string_zeroed() {
    let g = GUID { data1: 0, data2: 0, data3: 0, data4: [0; 8] };
    assert_eq!(guid_to_string(&g), "{00000000-0000-0000-0000-000000000000}");
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

#[test]
fn guid_to_string_has_braces() {
    let g = GUID::zeroed();
    let s = guid_to_string(&g);
    assert!(s.starts_with('{'));
    assert!(s.ends_with('}'));
    assert_eq!(s.len(), 38); // {xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}
}

#[test]
fn guid_from_bytes_roundtrip() {
    let original = GUID {
        data1: 0xC18E2F7E,
        data2: 0x933D,
        data3: 0x4965,
        data4: [0xB7, 0xD1, 0x1E, 0xEF, 0x22, 0x8D, 0x2A, 0xF3],
    };

    // 序列化为 16 字节小端
    let mut bytes = Vec::with_capacity(16);
    bytes.extend_from_slice(&original.data1.to_le_bytes());
    bytes.extend_from_slice(&original.data2.to_le_bytes());
    bytes.extend_from_slice(&original.data3.to_le_bytes());
    bytes.extend_from_slice(&original.data4);

    let parsed = guid_from_bytes(&bytes).unwrap();
    assert_eq!(parsed, original);
}

#[test]
fn guid_from_bytes_zeroed() {
    let bytes = [0u8; 16];
    let g = guid_from_bytes(&bytes).unwrap();
    assert_eq!(g, GUID::zeroed());
}

#[test]
fn guid_from_bytes_max() {
    let bytes = [0xFFu8; 16];
    let g = guid_from_bytes(&bytes).unwrap();
    assert_eq!(g.data1, 0xFFFFFFFF);
    assert_eq!(g.data2, 0xFFFF);
    assert_eq!(g.data3, 0xFFFF);
    assert_eq!(g.data4, [0xFF; 8]);
}

#[test]
fn guid_from_bytes_too_short() {
    assert!(guid_from_bytes(&[0u8; 15]).is_none());
}

// ── 回退逻辑 — get_original_pre_mix ────────────────────────

#[test]
fn premix_primary_has_guid() {
    // SFX 有 GUID → 直接返回，不走回退
    let mut slots = empty_slots();
    slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(test_guid(1));
    assert_eq!(get_original_pre_mix(&slots, InstallMode::SfxEfx), guid_to_string(&test_guid(1)));
}

#[test]
fn premix_primary_nokey_returns_empty() {
    // SFX 是 NoKey → 无法回退
    let slots = empty_slots(); // 所有 NoKey
    assert_eq!(get_original_pre_mix(&slots, InstallMode::SfxEfx), "");
}

#[test]
fn premix_novalue_fallback_to_lfx() {
    // SFX 是 NoValue，LFX 有 GUID → 回退到 LFX
    let mut slots = empty_slots();
    slots[ApoSlot::Sfx.index() as usize] = SlotValue::NoValue;
    slots[ApoSlot::Lfx.index() as usize] = SlotValue::Guid(test_guid(2));
    assert_eq!(get_original_pre_mix(&slots, InstallMode::SfxEfx), guid_to_string(&test_guid(2)));
}

#[test]
fn premix_novalue_lfx_novalue_returns_empty() {
    // SFX 是 NoValue，LFX 也是 NoValue → 无回退
    let mut slots = empty_slots();
    slots[ApoSlot::Sfx.index() as usize] = SlotValue::NoValue;
    slots[ApoSlot::Lfx.index() as usize] = SlotValue::NoValue;
    assert_eq!(get_original_pre_mix(&slots, InstallMode::SfxEfx), "");
}

#[test]
fn premix_lfxgfx_mode_uses_lfx() {
    // LfxGfx 模式：主槽位 = LFX
    let mut slots = empty_slots();
    slots[ApoSlot::Lfx.index() as usize] = SlotValue::Guid(test_guid(3));
    assert_eq!(get_original_pre_mix(&slots, InstallMode::LfxGfx), guid_to_string(&test_guid(3)));
}

#[test]
fn premix_lfxgfx_novalue_fallback_to_sfx() {
    // LfxGfx 模式：LFX 是 NoValue → 回退到 SFX
    let mut slots = empty_slots();
    slots[ApoSlot::Lfx.index() as usize] = SlotValue::NoValue;
    slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(test_guid(4));
    assert_eq!(get_original_pre_mix(&slots, InstallMode::LfxGfx), guid_to_string(&test_guid(4)));
}

#[test]
fn premix_sfxmfx_mode_uses_sfx() {
    // SfxMfx 模式：主槽位 = SFX
    let mut slots = empty_slots();
    slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(test_guid(5));
    assert_eq!(get_original_pre_mix(&slots, InstallMode::SfxMfx), guid_to_string(&test_guid(5)));
}

#[test]
fn premix_novalue_fallback_skips_nokey() {
    // SFX 是 NoValue，LFX 是 NoKey → 回退到 LFX 但 LFX 是 NoKey → 返回空
    let mut slots = empty_slots();
    slots[ApoSlot::Sfx.index() as usize] = SlotValue::NoValue;
    // LFX 默认是 NoKey（来自 empty_slots）
    assert_eq!(get_original_pre_mix(&slots, InstallMode::SfxEfx), "");
}

// ── 回退逻辑 — get_original_post_mix ───────────────────────

#[test]
fn postmix_primary_has_guid() {
    // EFX 有 GUID → 直接返回
    let mut slots = empty_slots();
    slots[ApoSlot::Efx.index() as usize] = SlotValue::Guid(test_guid(10));
    assert_eq!(get_original_post_mix(&slots, InstallMode::SfxEfx), guid_to_string(&test_guid(10)));
}

#[test]
fn postmix_primary_nokey_returns_empty() {
    // EFX 是 NoKey → 无法回退
    let slots = empty_slots();
    assert_eq!(get_original_post_mix(&slots, InstallMode::SfxEfx), "");
}

#[test]
fn postmix_sfxefx_novalue_fallback_to_mfx() {
    // EFX 是 NoValue → 尝试 MFX
    let mut slots = empty_slots();
    slots[ApoSlot::Efx.index() as usize] = SlotValue::NoValue;
    slots[ApoSlot::Mfx.index() as usize] = SlotValue::Guid(test_guid(11));
    assert_eq!(get_original_post_mix(&slots, InstallMode::SfxEfx), guid_to_string(&test_guid(11)));
}

#[test]
fn postmix_sfxefx_novalue_fallback_to_gfx() {
    // EFX 是 NoValue，MFX 是 NoValue → 尝试 GFX
    let mut slots = empty_slots();
    slots[ApoSlot::Efx.index() as usize] = SlotValue::NoValue;
    slots[ApoSlot::Mfx.index() as usize] = SlotValue::NoValue;
    slots[ApoSlot::Gfx.index() as usize] = SlotValue::Guid(test_guid(12));
    assert_eq!(get_original_post_mix(&slots, InstallMode::SfxEfx), guid_to_string(&test_guid(12)));
}

#[test]
fn postmix_sfxefx_all_empty_returns_empty() {
    // 所有 PostMix 槽位都是空的
    let mut slots = empty_slots();
    slots[ApoSlot::Efx.index() as usize] = SlotValue::NoValue;
    slots[ApoSlot::Mfx.index() as usize] = SlotValue::NoValue;
    slots[ApoSlot::Gfx.index() as usize] = SlotValue::NoValue;
    assert_eq!(get_original_post_mix(&slots, InstallMode::SfxEfx), "");
}

#[test]
fn postmix_sfxmfx_novalue_fallback_to_efx() {
    // SfxMfx 模式：MFX 是 NoValue → 尝试 EFX
    let mut slots = empty_slots();
    slots[ApoSlot::Mfx.index() as usize] = SlotValue::NoValue;
    slots[ApoSlot::Efx.index() as usize] = SlotValue::Guid(test_guid(13));
    assert_eq!(get_original_post_mix(&slots, InstallMode::SfxMfx), guid_to_string(&test_guid(13)));
}

#[test]
fn postmix_lfxgfx_novalue_fallback_to_efx() {
    // LfxGfx 模式：GFX 是 NoValue → 尝试 EFX
    let mut slots = empty_slots();
    slots[ApoSlot::Gfx.index() as usize] = SlotValue::NoValue;
    slots[ApoSlot::Efx.index() as usize] = SlotValue::Guid(test_guid(14));
    assert_eq!(get_original_post_mix(&slots, InstallMode::LfxGfx), guid_to_string(&test_guid(14)));
}

#[test]
fn postmix_lfxgfx_novalue_fallback_to_mfx() {
    // LfxGfx 模式：GFX 是 NoValue，EFX 是 NoValue → 尝试 MFX
    let mut slots = empty_slots();
    slots[ApoSlot::Gfx.index() as usize] = SlotValue::NoValue;
    slots[ApoSlot::Efx.index() as usize] = SlotValue::NoValue;
    slots[ApoSlot::Mfx.index() as usize] = SlotValue::Guid(test_guid(15));
    assert_eq!(get_original_post_mix(&slots, InstallMode::LfxGfx), guid_to_string(&test_guid(15)));
}

// ── 回退顺序验证 ─────────────────────────────────────────────────────

#[test]
fn postmix_fallback_order_sfxefx() {
    assert_eq!(postmix_fallback_order(InstallMode::SfxEfx), &[ApoSlot::Mfx, ApoSlot::Gfx]);
}

#[test]
fn postmix_fallback_order_sfxmfx() {
    assert_eq!(postmix_fallback_order(InstallMode::SfxMfx), &[ApoSlot::Efx, ApoSlot::Gfx]);
}

#[test]
fn postmix_fallback_order_lfxgfx() {
    assert_eq!(postmix_fallback_order(InstallMode::LfxGfx), &[ApoSlot::Efx, ApoSlot::Mfx]);
}

// ── other_premix_slot ─────────────────────────────────────────────────

#[test]
fn other_premix_for_lfxgfx_is_sfx() {
    assert_eq!(other_premix_slot(InstallMode::LfxGfx), ApoSlot::Sfx);
}

#[test]
fn other_premix_for_sfxefx_is_lfx() {
    assert_eq!(other_premix_slot(InstallMode::SfxEfx), ApoSlot::Lfx);
}

#[test]
fn other_premix_for_sfxmfx_is_lfx() {
    assert_eq!(other_premix_slot(InstallMode::SfxMfx), ApoSlot::Lfx);
}

// ── 常量验证 ──────────────────────────────────────────────────────────

// ── detect_install_mode（EAPO 三档探测） ──────────────────────────────

#[test]
fn detect_mode_old_windows_defaults_lfxgfx() {
    // Win < 8.1 → 不探测，Legacy 初始默认。
    let slots = [SlotValue::NoKey; 5];
    assert_eq!(detect_install_mode(false, &slots, false), InstallMode::LfxGfx);
    assert_eq!(detect_install_mode(false, &slots, true), InstallMode::LfxGfx);
}

#[test]
fn detect_mode_legacy_only_lfxgfx() {
    // Win8.1+ 且仅 LFX/GFX 有值、SFX/MFX/EFX 全空 → LfxGfx。
    let mut slots = [SlotValue::NoValue; 5];
    slots[ApoSlot::Lfx.index() as usize] = SlotValue::Guid(test_guid(1));
    slots[ApoSlot::Gfx.index() as usize] = SlotValue::Guid(test_guid(2));
    assert_eq!(detect_install_mode(true, &slots, false), InstallMode::LfxGfx);
}

#[test]
fn detect_mode_sfx_occupied_not_legacy() {
    // SFX 被占 → 非 Legacy 独占 → 继续探测。
    let mut slots = [SlotValue::NoValue; 5];
    slots[ApoSlot::Lfx.index() as usize] = SlotValue::Guid(test_guid(1));
    slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(test_guid(3));
    assert_eq!(detect_install_mode(true, &slots, false), InstallMode::SfxEfx);
}

#[test]
fn detect_mode_bluetooth_sfxmfx() {
    // 蓝牙容器 ID 存在 → SfxMfx。
    let slots = [SlotValue::NoValue; 5];
    assert_eq!(detect_install_mode(true, &slots, true), InstallMode::SfxMfx);
}

#[test]
fn detect_mode_bluetooth_beats_legacy() {
    // 蓝牙容器存在且 SFX 被占 → 仍 SfxMfx（C42 优先于 C43 默认）。
    let mut slots = [SlotValue::NoValue; 5];
    slots[ApoSlot::Sfx.index() as usize] = SlotValue::Guid(test_guid(3));
    assert_eq!(detect_install_mode(true, &slots, true), InstallMode::SfxMfx);
}

#[test]
fn detect_mode_default_sfxefx() {
    // 现代驱动、无蓝牙、SFX 等被占 → SfxEfx 默认。
    let slots = [SlotValue::NoValue; 5];
    assert_eq!(detect_install_mode(true, &slots, false), InstallMode::SfxEfx);
}

#[test]
fn install_version_constants() {
    assert_eq!(INSTALL_VERSION, "2");
    assert_eq!(INSTALL_VERSION_LEGACY, "1");
    assert_ne!(INSTALL_VERSION, INSTALL_VERSION_LEGACY);
}

#[test]
fn apo_fx_property_guid_format() {
    // GUID 格式应该是 32 字符的十六进制（无花括号）
    assert_eq!(APO_FX_PROPERTY_GUID.len(), 36); // xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx
    assert!(!APO_FX_PROPERTY_GUID.contains('{'));
    assert!(!APO_FX_PROPERTY_GUID.contains('}'));
}

// ── 辅助函数 ──────────────────────────────────────────────────────────

/// 创建全 NoKey 的空槽位数组。
fn empty_slots() -> [SlotValue; 5] {
    [SlotValue::NoKey; 5]
}

/// 创建测试用 GUID，不同编号产生不同的 GUID。
fn test_guid(n: u32) -> GUID {
    GUID {
        data1: 0xA000_0000 + n,
        data2: 0xB000 + n as u16,
        data3: 0xC000 + n as u16,
        data4: [0xD0, 0xE0, 0xF0, n as u8, (n >> 8) as u8, (n >> 16) as u8, (n >> 24) as u8, 0xFF],
    }
}
