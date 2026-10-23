use wrist_power::external_power;

#[test]
fn external_input_detection_uses_pmic_when_pd_is_unpowered() {
    assert_eq!(external_power(None, Some(0)), Some(false));
    for status in [0x40, 0x80, 0xC0] {
        assert_eq!(external_power(None, Some(status)), Some(true));
        assert_eq!(external_power(Some(false), Some(status)), Some(true));
    }
    assert_eq!(external_power(Some(true), Some(0)), Some(true));
    assert_eq!(external_power(None, None), None);
    assert_eq!(external_power(Some(false), None), Some(false));
}
