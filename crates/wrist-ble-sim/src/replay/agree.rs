//! The replay's verdict.

use wrist_ble::Op;

use super::{DriverOp, Report};

/// How far apart the two sides may act and still have done the same thing.
const NEAR: u64 = 300;

impl Report {
    /// Where the core and the driver acted differently, letting pass the driver's ask for timing at
    /// every connection and the advertising interval, which the core takes from its menu.
    #[must_use]
    pub fn disagreements(&self) -> Vec<String> {
        let near = |a: u64, b: u64| a.abs_diff(b) <= NEAR;
        let mut out = Vec::new();
        for &(at, op) in &self.core_ops {
            match op {
                Op::Advertise(Some(_)) if !self.driver_ops.iter().any(|&(t, d)| near(t, at) && matches!(d, DriverOp::Advertise(..))) => {
                    out.push(format!("{at}: the core advertised and the driver did not"));
                }
                Op::UpdateParams(_, p) => {
                    let asked = DriverOp::AskParams(p.interval_min.as_u16(), p.interval_max.as_u16(), p.latency);
                    if !self.driver_ops.iter().any(|&(t, d)| near(t, at) && d == asked) {
                        out.push(format!("{at}: the core asked for {asked:?} and the driver did not"));
                    }
                }
                _ => {}
            }
        }
        for &(at, d) in &self.driver_ops {
            if matches!(d, DriverOp::Advertise(..)) && !self.core_ops.iter().any(|&(t, op)| near(t, at) && matches!(op, Op::Advertise(Some(_)))) {
                out.push(format!("{at}: the driver advertised and the core did not"));
            }
        }
        if self.unknown_events > 0 {
            out.push(format!("{} events the core could not read", self.unknown_events));
        }
        out
    }
}
