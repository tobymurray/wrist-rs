use std::collections::HashMap;
use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use wrist_power::{gauge_reset, restore_gauge, GaugeBus, GaugeStep, GAUGE_RESTORE};

/// A MAX17262 after a reset: the register defaults, `POR` set, `Refresh` clearing on the
/// first read after it is written. `fail_after` writes succeed, then every transfer fails;
/// `refuse` fails one write alone.
struct Gauge {
    regs: HashMap<u8, u16>,
    writes: Vec<(u8, u16)>,
    fail_after: usize,
    refuse: Option<(u8, u16)>,
    stuck_refresh: bool,
}

impl Gauge {
    fn reset() -> Self {
        let regs = HashMap::from([(0x00, 0x8002), (0x3D, 0), (0xBA, 0x870C), (0xDB, 0x040C), (0x18, 0x0BB8)]);
        Self { regs, writes: Vec::new(), fail_after: usize::MAX, refuse: None, stuck_refresh: false }
    }
    fn failing(&self) -> bool {
        self.writes.len() >= self.fail_after
    }
}

#[allow(clippy::unused_async_trait_impl)]
impl GaugeBus for Gauge {
    async fn read(&mut self, reg: u8) -> Option<u16> {
        if self.failing() {
            return None;
        }
        let v = self.regs.get(&reg).copied().unwrap_or(0);
        if reg == 0xDB && !self.stuck_refresh {
            self.regs.insert(reg, v & !0x8000);
        }
        Some(v)
    }
    async fn write(&mut self, reg: u8, value: u16) -> bool {
        let failing = self.failing() || self.refuse.take_if(|r| *r == (reg, value)).is_some();
        self.writes.push((reg, value));
        if !failing {
            self.regs.insert(reg, value);
        }
        !failing
    }
    async fn wait_ms(&mut self, _ms: u32) {}
}

fn run(gauge: &mut Gauge) -> Result<(), &'static str> {
    match pin!(restore_gauge(gauge)).as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(r) => r,
        Poll::Pending => panic!("the restore awaited something that never completes"),
    }
}

fn assert_configured(gauge: &Gauge) {
    for (reg, v) in [(0x18, 0x0190), (0x1E, 0x0059), (0x3A, 0xAA63), (0xDB, 0x0400), (0xBA, 0x870F)] {
        assert_eq!(gauge.regs[&reg], v, "register {reg:#04x}");
    }
    assert!(!gauge_reset(gauge.regs[&0x00]));
}

#[test]
fn por_is_status_bit_1() {
    assert!(gauge_reset(0x0002));
    assert!(!gauge_reset(0x8080));
    assert!(!gauge_reset(0xFFFD));
}

#[test]
fn restore_waits_for_the_gauge_then_writes_the_stock_configuration() {
    assert_eq!(GAUGE_RESTORE[..2], [GaugeStep::WaitClear(0x3D, 1), GaugeStep::SaveHibCfg]);
    let writes: Vec<_> = GAUGE_RESTORE
        .iter()
        .filter_map(|s| match *s {
            GaugeStep::Write(r, v) => Some((r, v)),
            _ => None,
        })
        .collect();
    // Out of hibernate first: 0x90 to Command, HibCfg 0, Command cleared.
    assert_eq!(&writes[..3], &[(0x60, 0x0090), (0xBA, 0), (0x60, 0)]);
    // The stock ModelCfg, 0x0400, with Refresh.
    assert_eq!(writes.last(), Some(&(0xDB, 0x0400 | 0x8000)));
}

#[test]
fn hibernate_and_por_come_back_only_after_the_model_reloads() {
    let at = |step| GAUGE_RESTORE.iter().position(|s| *s == step).unwrap();
    let refresh = at(GaugeStep::WaitClear(0xDB, 0x8000));
    assert!(at(GaugeStep::Write(0xDB, 0x8400)) < refresh);
    assert_eq!(at(GaugeStep::RestoreHibCfg), refresh + 1);
    assert_eq!(at(GaugeStep::ClearPor), GAUGE_RESTORE.len() - 1);
}

#[test]
fn a_reset_gauge_ends_configured_hibernating_and_without_por() {
    let mut gauge = Gauge::reset();
    assert_eq!(run(&mut gauge), Ok(()));
    assert_configured(&gauge);
}

#[test]
fn a_failure_anywhere_leaves_por_set_and_hibernate_on_after_a_retry() {
    // Each failure point from the write of HibCfg = 0 to clearing POR.
    for fail_after in 2..=9 {
        let mut gauge = Gauge::reset();
        gauge.fail_after = fail_after;
        assert!(run(&mut gauge).is_err(), "failing after {fail_after} writes");
        // The bus died, so the write putting HibCfg back may have failed too.
        assert!([0, 0x870F].contains(&gauge.regs[&0xBA]), "failing after {fail_after} writes");
        gauge.fail_after = usize::MAX;
        if gauge_reset(gauge.regs[&0x00]) {
            assert_eq!(run(&mut gauge), Ok(()));
        }
        assert_configured(&gauge);
    }
}

#[test]
fn a_refused_hibernate_write_is_tried_again() {
    let mut gauge = Gauge::reset();
    gauge.refuse = Some((0xBA, 0x870F));
    assert_eq!(run(&mut gauge), Err("HibCfg unwritten"));
    assert_eq!(gauge.regs[&0xBA], 0x870F);
    assert!(gauge_reset(gauge.regs[&0x00]));
}

#[test]
fn a_reload_that_never_finishes_gives_up_and_puts_hibernate_back() {
    let mut gauge = Gauge::reset();
    gauge.stuck_refresh = true;
    assert_eq!(run(&mut gauge), Err("still busy after 1 s"));
    assert_eq!(gauge.regs[&0xBA], 0x870F);
    assert!(gauge_reset(gauge.regs[&0x00]));
}
