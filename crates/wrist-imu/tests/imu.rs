use wrist_imu::{chunks, engine_ok, enable_steps, reg, steps_enabled, Activity, Found, Outputs, CHUNK, CONFIG, PAGE_LEN};

#[test]
fn the_image_is_bosch_s() {
    assert_eq!(CONFIG.len(), 8192);
    assert_eq!(wrist_flash::plan::crc32(CONFIG), 0x229A_1189);
}

#[test]
fn chunks_cover_the_image_in_order_at_word_addresses() {
    let all: Vec<_> = chunks().collect();
    assert_eq!(all.len(), CONFIG.len() / CHUNK);
    let mut joined = Vec::new();
    for (i, c) in all.iter().enumerate() {
        assert_eq!(c.addr[0], reg::INIT_ADDR_0);
        assert_eq!(c.data[0], reg::INIT_DATA);
        let word = usize::from(c.addr[1]) | usize::from(c.addr[2]) << 4;
        assert!(c.addr[1] < 0x10);
        assert_eq!(word * 2, i * CHUNK);
        joined.extend_from_slice(&c.data[1..]);
    }
    assert_eq!(&joined[..], &CONFIG[..]);
}

#[test]
fn the_last_chunk_s_address_fits_its_registers() {
    let last = chunks().last().unwrap();
    assert_eq!(usize::from(last.addr[1]) | usize::from(last.addr[2]) << 4, (CONFIG.len() - CHUNK) / 2);
}

#[test]
fn the_engine_is_up_only_on_message_one() {
    assert!(engine_ok(0x01));
    assert!(engine_ok(0x41));
    for s in [0x00, 0x02, 0x03, 0x04, 0x05, 0x0F] {
        assert!(!engine_ok(s));
    }
}

#[test]
fn enabling_steps_keeps_the_rest_of_the_page() {
    let mut page = [0xA5u8; PAGE_LEN];
    page[3] = 0x02;
    enable_steps(&mut page);
    assert!(steps_enabled(&page));
    let mut expected = [0xA5u8; PAGE_LEN];
    expected[3] = 0x32;
    assert_eq!(page, expected);
    enable_steps(&mut page);
    assert_eq!(page, expected);
    let mut half = [0u8; PAGE_LEN];
    half[3] = 0x10;
    assert!(!steps_enabled(&half));
}

fn counting() -> Found {
    let mut step_page = [0; PAGE_LEN];
    enable_steps(&mut step_page);
    Found { internal_status: 0x01, pwr_ctrl: 0x04, acc_conf: 0x17, pwr_conf: 0x03, step_page }
}

#[test]
fn a_chip_counting_as_set_up_is_adopted() {
    assert!(counting().adoptable());
}

#[test]
fn any_difference_means_loading_again() {
    let base = counting();
    let mut cases = vec![
        Found { internal_status: 0x00, ..base },
        Found { pwr_ctrl: 0x00, ..base },
        Found { pwr_ctrl: 0x0C, ..base },
        Found { acc_conf: 0xA8, ..base },
        Found { pwr_conf: 0x00, ..base },
        Found { step_page: [0; PAGE_LEN], ..base },
    ];
    // The power-on state.
    cases.push(Found { internal_status: 0x00, pwr_ctrl: 0x00, acc_conf: 0xA8, pwr_conf: 0x03, step_page: [0; PAGE_LEN] });
    for f in cases {
        assert!(!f.adoptable(), "{f:?}");
    }
}

#[test]
fn outputs_decode_the_count_and_activity() {
    assert_eq!(Outputs::parse(&[0x39, 0x30, 0x00, 0x00, 0x01]), Outputs { steps: 12345, activity: Activity::Walking });
    assert_eq!(Outputs::parse(&[0xFF, 0xFF, 0xFF, 0xFF, 0x02]).steps, u32::MAX);
    let names: Vec<_> = (0..4).map(|a| Outputs::parse(&[0, 0, 0, 0, a]).activity.name()).collect();
    assert_eq!(names, ["still", "walking", "running", "unknown"]);
    assert_eq!(Outputs::parse(&[0, 0, 0, 0, 0xFC]).activity, Activity::Still);
}
