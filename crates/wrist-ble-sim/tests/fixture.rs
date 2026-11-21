//! A capture cut to a fixture.

use std::path::Path;

use wrist_ble_sim::capture::{command, scrub_addresses, stamped, Command, Profile};
use wrist_ble_sim::fixture::{fixture, keep};
use wrist_ble_sim::replay::{replay, Report};

fn read(path: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures").join(path))
        .unwrap()
}

/// A capture from the watch, every byte that could be an address overwritten, and its fixture.
#[test]
fn a_capture_is_cut_as_before() {
    let capture = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/captures/cable.log");
    let out =
        std::process::Command::new(env!("CARGO_BIN_EXE_fixture")).arg(capture).output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8(out.stdout).unwrap(), read("captures/cable-fixture.log"));
}

/// The same operations, though a console command falls a little apart from where it did: its
/// time on the watch is read from the trace lines around it.
#[test]
fn a_fixture_replays_as_its_capture() {
    let (capture, cut) =
        (replay(&read("captures/cable.log")), replay(&read("captures/cable-fixture.log")));
    let ops = |r: &Report| {
        format!(
            "{:?} {:?}",
            r.core_ops.iter().map(|o| o.1).collect::<Vec<_>>(),
            r.driver_ops.iter().map(|o| o.1).collect::<Vec<_>>()
        )
    };
    assert_eq!(ops(&capture), ops(&cut));
    assert_eq!(capture.tally, cut.tally);
    assert_eq!(capture.disagreements(), cut.disagreements());
}

#[test]
fn a_fixture_is_its_own_fixture() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let traces = std::fs::read_dir(fixtures.join("traces")).unwrap().map(|e| e.unwrap().path());
    for path in traces.chain([fixtures.join("captures/cable-fixture.log")]) {
        let trace = std::fs::read_to_string(&path).unwrap();
        assert_eq!(fixture(&trace), trace, "{}", path.display());
    }
}

/// The cut leaves an address beside a hex digit, so no committed mark or `[update]` line holds one.
#[test]
fn no_mark_holds_an_address() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let traces = std::fs::read_dir(fixtures.join("traces")).unwrap().map(|e| e.unwrap().path());
    let captures = std::fs::read_dir(fixtures.join("captures")).unwrap().map(|e| e.unwrap().path());
    for path in traces.chain(captures) {
        let trace = std::fs::read_to_string(&path).unwrap();
        for text in trace.lines().filter_map(stamped).map(|s| s.text) {
            if text.starts_with("== ") || text.starts_with("[update] ") {
                let joined: String =
                    text.chars().filter(|c| !matches!(c, ':' | '-' | '_')).collect();
                let longest = joined.split(|c: char| !c.is_ascii_hexdigit()).map(str::len).max();
                assert!(longest < Some(12), "{}: an address in {text}", path.display());
            }
        }
    }
}

#[test]
fn an_address_in_a_mark_is_replaced() {
    for written in ["C2:3D:4E:5F:60:71", "c2-3d-4e-5f-60-71", "C23D4E5F6071", "C2_3D_4E_5F_60_71"] {
        let line = format!("12.345 == the phone {written} connects");
        assert_eq!(
            keep(&line).as_deref(),
            Some("12.345 == the phone <address> connects"),
            "{written}"
        );
    }
}

#[test]
fn a_mark_without_an_address_is_kept_as_written() {
    let mark = "12.345 == P1 try 2: phone B connect at 235.269, reason 13, 0x3E";
    assert_eq!(keep(mark).as_deref(), Some(mark));
}

#[test]
fn twelve_hex_digits_are_taken_for_an_address() {
    // A build id can't be told from an address written bare.
    assert_eq!(keep("1.000 == build 77df27bcf274").as_deref(), Some("1.000 == build <address>"));
}

#[test]
fn only_a_run_of_whole_bytes_is_an_address() {
    for mark in [
        "build 77df27bcf274a",
        "C2:3D:4E:5F:60",
        "C2:3D:4E:5F:60:7",
        "C2::3D:4E:5F:60:71",
        "C2 3D 4E 5F 60 71",
    ] {
        let line = format!("1.000 == {mark}");
        assert_eq!(keep(&line).as_deref(), Some(&*line), "{mark}");
    }
    for (mark, scrubbed) in [
        ("x C2:3D:4E:5F:60:71:82y", "x <address>y"),
        ("C2:3D:4E:5F:60:71 then 0102030405060708", "<address> then <address>"),
        ("é0102030405060708é", "é<address>é"),
    ] {
        assert_eq!(
            keep(&format!("1.000 == {mark}")),
            Some(format!("1.000 == {scrubbed}")),
            "{mark}"
        );
    }
    assert_eq!(scrub_addresses("C23D4E5F6071"), "<address>");
    assert_eq!(scrub_addresses("C23D4E5F607"), "C23D4E5F607");
}

#[test]
fn the_operators_disconnect_is_kept() {
    assert_eq!(
        keep("  85.804 >> usb ble disconnect").as_deref(),
        Some("85.804 >> usb ble disconnect")
    );
    assert_eq!(
        keep("85.804 >> usb ble profile auto").as_deref(),
        Some("85.804 >> usb ble profile auto")
    );
    assert_eq!(keep("85.804 >> usb ble bonds"), None);
    assert_eq!(keep("85.804 >> usb ble forget C2:3D:4E:5F:60:71"), None);
}

#[test]
fn the_consoles_interval_ask_is_kept() {
    assert_eq!(
        keep("85.804 >> usb ble interval now 24 40").as_deref(),
        Some("85.804 >> usb ble interval now 24 40")
    );
    assert_eq!(keep("85.804 >> usb ble interval now 24"), None);
}

/// As the console takes it: the first two numbers, and only an interval it would ask for.
#[test]
fn only_an_interval_ask_the_console_takes_is_kept() {
    let ask = "85.804 >> usb ble interval now 24 40";
    for written in [
        "ble interval now 24 40 ; ble interval now 12 24",
        "ble interval now  +24\t40",
        "ble interval now24 40",
    ] {
        assert_eq!(keep(&format!("85.804 >> usb {written}")).as_deref(), Some(ask), "{written}");
    }
    for refused in ["5 5", "24 12", "2000 2000", "6 x 6", "70000 70000"] {
        assert_eq!(keep(&format!("85.804 >> usb ble interval now {refused}")), None, "{refused}");
    }
}

#[test]
fn every_command_kept_reads_back_as_itself() {
    for cmd in [
        "ble on",
        "ble off",
        "ble yes",
        "ble no",
        "ble disconnect",
        "ble profile auto",
        "ble profile cable",
        "ble profile battery",
        "ble interval now 6 1999",
        "update 345624 FE7A7220",
    ] {
        let c = command(cmd).unwrap_or_else(|| panic!("{cmd}"));
        assert_eq!(c.to_string(), cmd);
        assert_eq!(command(&c.to_string()), Some(c));
    }
    assert_eq!(command("ble profile"), None);
    assert_eq!(command("ble on now"), None);
    assert_eq!(command("update 5 1234"), Some(Command::Update(5, 0x1234)));
}

#[test]
fn a_command_is_trimmed_as_the_console_trims_it() {
    assert_eq!(command("ble on "), Some(Command::Radio(true)));
    assert_eq!(command("ble profile battery "), Some(Command::Profile(Profile::Battery)));
    assert_eq!(command("ble on  "), None);
    assert_eq!(command(" ble on"), None);
}

#[test]
fn a_transfers_bounds_are_kept() {
    for line in [
        "2524.936 [update] send 345624 bytes",
        "2529.546 [update] received 16492",
        "2625.693 [update] ok 345624 bytes, crc 0xFE7A7220, swap marked; reset to apply",
    ] {
        assert_eq!(keep(line).as_deref(), Some(line));
    }
    assert_eq!(
        keep("1.000 >> usb update 345624 FE7A7220").as_deref(),
        Some("1.000 >> usb update 345624 FE7A7220")
    );
    assert_eq!(keep("1.000 >> usb update now"), None);
}

/// As `parse_transfer` in the firmware's console.
#[test]
fn only_an_update_the_console_takes_is_kept() {
    for refused in [
        "update 5 0x1234",
        "update 4294967296 1234",
        "update\t5 1234",
        "update 5 123456789",
        "update -5 1234",
    ] {
        assert_eq!(keep(&format!("1.000 >> usb {refused}")), None, "{refused}");
    }
    assert_eq!(
        keep("1.000 >> usb update +5 +00001234 more").as_deref(),
        Some("1.000 >> usb update 5 1234")
    );
    assert_eq!(
        keep("1.000 >> usb update 345624 fe7a7220").as_deref(),
        Some("1.000 >> usb update 345624 FE7A7220")
    );
}

#[test]
fn only_the_drivers_operations_are_kept() {
    assert_eq!(
        keep("1.000 [trace] 5 c 0193FC03010813").as_deref(),
        Some("1.000 [trace] 5 c 0193FC03010813")
    );
    assert_eq!(keep("1.000 [trace] 5 c 01030C00"), None);
    assert_eq!(keep("1.000 [trace] 5 c 0193"), None);
}

#[test]
fn only_a_reply_to_an_operation_is_kept_and_only_to_its_status() {
    let e = |p: &str| {
        keep(&format!("1.000 [trace] 5 e {p}")).map(|l| l["1.000 [trace] 5 e ".len()..].to_string())
    };
    assert_eq!(e("040F0440018DFC").as_deref(), Some("040F0440018DFC"));
    assert_eq!(e("040E050181FC0012").as_deref(), Some("040E040181FC00"));
    assert_eq!(e("040E0A01030C00010203040506"), None);
    assert_eq!(e("040F040001030C"), None);
    assert_eq!(e("040E0401"), None);
}

#[test]
fn a_connections_addresses_are_zeroed_and_reports_and_console_writes_dropped() {
    let e = |p: &str| {
        keep(&format!("1.000 [trace] 5 e {p}")).map(|l| l["1.000 [trace] 5 e ".len()..].to_string())
    };
    assert_eq!(
        e("043E13010001000100C23D4E5F607124000000480100").as_deref(),
        Some("043E1301000100010000000000000024000000480100"),
    );
    assert_eq!(
        e("043E1F0A0001000100C23D4E5F6071C23D4E5F6071C23D4E5F607124000000480100").as_deref(),
        Some("043E1F0A000100010000000000000000000000000000000000000024000000480100"),
    );
    assert_eq!(e("043E0B0A0001000100C23D").as_deref(), Some("043E0B0A00010001000000"));
    assert_eq!(e("043E0C02010001C23D4E5F607100"), None);
    assert_eq!(e("04FF06010C0100AB"), None);
    assert_eq!(e("04FF06030C0108DC00").as_deref(), Some("04FF06030C0108DC00"));
    assert_eq!(e("04050400010813").as_deref(), Some("04050400010813"));
    assert_eq!(e("02050400010813"), None);
    assert_eq!(e("0405"), None);
    assert_eq!(e("040500").as_deref(), Some("040500"));
    assert_eq!(e("043E00").as_deref(), Some("043E00"));
}

#[test]
fn the_drivers_lost_reads_and_snapshots_are_kept() {
    for line in [
        "1.000 [trace] 5 l",
        "1.000 [trace] 5 s",
        "1.000 [trace] 5 s 0001080102",
        "1.000 [trace] 5 s 00FF0E0100",
    ] {
        assert_eq!(keep(line).as_deref(), Some(line));
    }
    for packet in ["00010801", "0801080102", "0001080103", "00000F0100"] {
        assert_eq!(keep(&format!("1.000 [trace] 5 s {packet}")), None, "{packet}");
    }
    assert_eq!(keep("1.000 [trace] 5 m 01").as_deref(), Some("1.000 [trace] 5 m 01"));
    assert_eq!(keep("1.000 [trace] 5 m 00").as_deref(), Some("1.000 [trace] 5 m 00"));
    assert_eq!(keep("1.000 [trace] 5 m 02"), None);
    assert_eq!(keep("1.000 [trace] 5 m"), None);
}

#[test]
fn a_cut_packet_keeps_what_the_trace_kept() {
    assert_eq!(
        keep("1.000 [trace] 5 e 04050400010813 +7").as_deref(),
        Some("1.000 [trace] 5 e 04050400010813")
    );
    assert_eq!(keep("1.000 [trace] 5 r").as_deref(), Some("1.000 [trace] 5 r"));
}

/// A line the replay can't read is not one the fixture keeps.
#[test]
fn a_trace_line_the_replay_cannot_read_is_dropped() {
    for line in [
        "1.000 [trace] x r",
        "1.000 [trace] 5 e 04050",
        "1.000 [trace] 5 e 0405GG",
        "1.000 [trace] 5 x 0102",
        "1.000 [trace]  5",
    ] {
        assert_eq!(keep(line), None, "{line}");
    }
    assert_eq!(keep("1.000 [ble] connected"), None);
}

/// A `ble dump` line has no host stamp: it is kept, stamped with the watch's own milliseconds,
/// and so are the recorder's goal records; other unstamped text is not.
#[test]
fn a_dumped_line_is_stamped_with_the_watchs_time() {
    assert_eq!(keep("[trace] 5 r").as_deref(), Some("0.005 [trace] 5 r"));
    assert_eq!(keep("[trace] 12345 G 07").as_deref(), Some("12.345 [trace] 12345 G 07"));
    assert_eq!(keep("[trace] 12345 g 0100").as_deref(), Some("12.345 [trace] 12345 g 0100"));
    assert_eq!(keep("[ble] dump next 4973"), None);
}

#[test]
fn wall_clock_times_are_read_as_seconds_are() {
    assert_eq!(keep("09:27:15.813 >> usb ble off").as_deref(), Some("09:27:15.813 >> usb ble off"));
    assert_eq!(
        keep("09:27:15.870 [trace] 946160 r").as_deref(),
        Some("09:27:15.870 [trace] 946160 r")
    );
    assert_eq!(keep("9:27:15.813 >> usb ble off"), None);
}

#[test]
fn a_stamp_is_read_as_milliseconds() {
    let ms = |line: &str| stamped(line).map(|s| s.ms);
    assert_eq!(ms("09:27:16.421 x"), Some(((9 * 60 + 27) * 60 + 16) * 1_000 + 421));
    assert_eq!(ms("  85.804 x"), Some(85_804));
    for line in [
        "9:27:16.421 x",
        "09:27.421 x",
        "85.80 x",
        "85.8040 x",
        "85 x",
        ".804 x",
        "85.804",
        "1:02:03:04.000 x",
        "+85.804 x",
    ] {
        assert_eq!(ms(line), None, "{line}");
    }
    assert_eq!(ms("99999999999999999999999.000 x"), Some(u64::MAX));
}
