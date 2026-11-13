//! The verdict, told each side's actions as they come.

use proptest::prelude::*;
use wrist_ble::{AdvParams, ConnHandle, ConnParams, DisconnectReason, Interval, Op, OpKind, Slots, Timeout};
use wrist_ble_shadow::verdict::{Act, Disagreement, Verdict, NEAR, OPERATOR};
use wrist_ble_shadow::DriverOp;

#[derive(Clone, Copy, Debug)]
#[allow(clippy::large_enum_variant, reason = "a test's record of actions")]
enum Side {
    Core(Op),
    Driver(DriverOp),
    Operator,
}

/// Every action told in time order, ties in the order given, then the end of the record.
fn judge(actions: &[(u64, Side)]) -> Vec<Disagreement> {
    let mut actions = actions.to_vec();
    actions.sort_by_key(|(at, _)| *at);
    let mut v = Verdict::new();
    let mut found = Vec::new();
    let mut tell = |d| found.push(d);
    for (at, side) in actions {
        match side {
            Side::Core(op) => match Act::of_core(&op) {
                Some(act) => v.core(at, act, &mut tell),
                None => v.advance(at, &mut tell),
            },
            Side::Driver(d) => match Act::of_driver(d) {
                Some(act) => v.driver(at, act, &mut tell),
                None => v.advance(at, &mut tell),
            },
            Side::Operator => v.operator(at, &mut tell),
        }
    }
    v.finish(&mut tell);
    found
}

fn sides(core: &[(u64, Op)], driver: &[(u64, DriverOp)]) -> Vec<(u64, Side)> {
    core.iter().map(|&(t, op)| (t, Side::Core(op))).chain(driver.iter().map(|&(t, d)| (t, Side::Driver(d)))).collect()
}

fn count(core: &[(u64, Op)], driver: &[(u64, DriverOp)]) -> usize {
    judge(&sides(core, driver)).len()
}

fn ask(min: u16, max: u16, latency: u16) -> Op {
    let params = ConnParams {
        interval_min: Interval::from_u16(min),
        interval_max: Interval::from_u16(max),
        latency,
        timeout: Timeout::from_u16(600),
        ce_len: Slots::from_u16(0),
    };
    Op::UpdateParams(ConnHandle::new(1), params)
}

fn advertise() -> Op {
    Op::Advertise(Some(AdvParams { interval_min: Slots::from_u16(160), interval_max: Slots::from_u16(160), connectable: true }))
}

#[test]
fn an_answer_on_one_side_alone_or_unlike_the_others_is_a_disagreement() {
    let yes = Op::ConfirmNumeric(ConnHandle::new(1), true);
    assert_eq!(count(&[(1_000, yes)], &[(1_200, DriverOp::Confirm(true))]), 0);
    assert_eq!(count(&[(1_000, yes)], &[(1_000, DriverOp::Confirm(false))]), 2, "the other answer");
    assert_eq!(count(&[(1_000, yes)], &[]), 1, "the core answered unprompted");
    assert_eq!(count(&[], &[(1_000, DriverOp::Confirm(false))]), 1, "the core never answered");
    assert_eq!(count(&[(1_000, yes)], &[(1_301, DriverOp::Confirm(true))]), 2, "too far apart");
}

#[test]
fn security_rebonds_and_disconnects_on_one_side_alone_are_disagreements() {
    let link = ConnHandle::new(1);
    let drop = Op::Disconnect(link, DisconnectReason::RemoteUserTerminatedConn);
    for (op, kind) in [(Op::RequestSecurity(link), OpKind::RequestSecurity), (Op::AllowRebond(link), OpKind::AllowRebond), (drop, OpKind::Disconnect)] {
        assert_eq!(count(&[(1_000, op)], &[(1_100, DriverOp::Other(kind))]), 0, "{kind:?}");
        assert_eq!(count(&[(1_000, op)], &[]), 1, "{kind:?}");
        assert_eq!(count(&[], &[(1_000, DriverOp::Other(kind))]), 1, "{kind:?}");
    }
}

#[test]
fn only_a_disconnect_the_operator_asked_for_is_the_drivers_alone() {
    let asked = |at: u64, disconnect: u64| judge(&[(at, Side::Operator), (disconnect, Side::Driver(DriverOp::Other(OpKind::Disconnect)))]).len();
    assert_eq!(asked(1_000, 1_000), 0);
    assert_eq!(asked(1_000, 2_000), 0);
    assert_eq!(asked(1_000, 2_001), 1, "too long after");
    assert_eq!(asked(1_000, 999), 1, "before it");
    let disconnect_first = [(1_000, Side::Driver(DriverOp::Other(OpKind::Disconnect))), (1_000, Side::Operator)];
    assert_eq!(judge(&disconnect_first).len(), 0, "the same millisecond, the driver's line first");
}

#[test]
fn the_same_ask_within_the_window_agrees_and_any_other_does_not() {
    let core = [(1_000, ask(80, 160, 4))];
    assert_eq!(count(&core, &[(1_300, DriverOp::AskParams(80, 160, 4))]), 0);
    assert_eq!(count(&core, &[(700, DriverOp::AskParams(80, 160, 4))]), 0);
    for driver in [DriverOp::AskParams(80, 160, 0), DriverOp::AskParams(12, 160, 4), DriverOp::AskParams(80, 24, 4)] {
        assert_eq!(count(&core, &[(1_000, driver)]), 1, "{driver:?}");
    }
    assert_eq!(count(&core, &[(1_301, DriverOp::AskParams(80, 160, 4))]), 1);
    assert_eq!(count(&core, &[(699, DriverOp::AskParams(80, 160, 4))]), 1);
    assert_eq!(count(&[], &[(1_000, DriverOp::AskParams(80, 160, 4))]), 0, "the driver asks at every connection");
}

#[test]
fn advertising_on_one_side_alone_is_a_disagreement() {
    assert_eq!(count(&[(1_000, advertise())], &[(1_000, DriverOp::Advertise(160, 160))]), 0);
    assert_eq!(count(&[(1_000, advertise())], &[(1_301, DriverOp::Advertise(160, 160))]), 2);
    assert_eq!(count(&[(1_000, advertise())], &[(1_000, DriverOp::StopAdvertising)]), 1);
    assert_eq!(count(&[(1_000, advertise())], &[(1_000, DriverOp::AskParams(80, 160, 4))]), 1);
    assert_eq!(count(&[(1_000, Op::Advertise(None))], &[(1_000, DriverOp::Advertise(160, 160))]), 1);
    assert_eq!(count(&[], &[(1_000, DriverOp::Advertise(160, 160))]), 1);
}

#[test]
fn a_disagreement_names_the_side_that_acted_and_what_it_did() {
    let found = judge(&sides(&[(1_000, Op::ConfirmNumeric(ConnHandle::new(1), false))], &[(5_000, DriverOp::AskParams(80, 160, 4)), (6_000, DriverOp::Advertise(160, 160))]));
    let lines: Vec<String> = found.iter().map(ToString::to_string).collect();
    assert_eq!(lines, ["1000: the core refused the comparison and the driver did not", "6000: the driver advertised and the core did not"]);
}

#[test]
fn the_end_of_the_record_judges_every_action_however_late() {
    assert_eq!(judge(&[(u64::MAX - NEAR, Side::Driver(DriverOp::Other(OpKind::Disconnect)))]).len(), 1);
}

/// Past the window's room, the oldest is judged before the other side could answer it.
#[test]
fn more_actions_within_near_than_the_window_holds_judge_the_oldest_early() {
    let many: Vec<(u64, Side)> = (0..17).map(|t| (t, Side::Core(advertise()))).chain([(17, Side::Driver(DriverOp::Advertise(160, 160)))]).collect();
    let found = judge(&many);
    assert_eq!(found.iter().map(|d| d.at).collect::<Vec<_>>(), [0]);
    assert_eq!(judge(&many[1..]).len(), 0);
}

/// The verdict over a whole record at once, the reference for [`Verdict`].
fn whole(actions: &[(u64, Side)]) -> Vec<(u64, bool, Act)> {
    let core: Vec<(u64, Op)> = actions.iter().filter_map(|&(t, s)| if let Side::Core(op) = s { Some((t, op)) } else { None }).collect();
    let driver: Vec<(u64, DriverOp)> = actions.iter().filter_map(|&(t, s)| if let Side::Driver(d) = s { Some((t, d)) } else { None }).collect();
    let operator: Vec<u64> = actions.iter().filter_map(|&(t, s)| matches!(s, Side::Operator).then_some(t)).collect();
    let near = |a: u64, b: u64| a.abs_diff(b) <= NEAR;
    let same = |op: &Op, d: DriverOp| match (op, d) {
        (Op::ConfirmNumeric(_, yes), DriverOp::Confirm(answer)) => *yes == answer,
        (Op::RequestSecurity(_), DriverOp::Other(OpKind::RequestSecurity))
        | (Op::AllowRebond(_), DriverOp::Other(OpKind::AllowRebond))
        | (Op::Disconnect(..), DriverOp::Other(OpKind::Disconnect)) => true,
        _ => false,
    };
    let compared = |op: &Op| matches!(op, Op::ConfirmNumeric(..) | Op::RequestSecurity(_) | Op::AllowRebond(_) | Op::Disconnect(..));
    let compared_driver = |d: DriverOp| matches!(d, DriverOp::Confirm(_) | DriverOp::Other(OpKind::RequestSecurity | OpKind::AllowRebond | OpKind::Disconnect));
    let mut out = Vec::new();
    for &(at, op) in &core {
        let alone = match op {
            Op::Advertise(Some(_)) => !driver.iter().any(|&(t, d)| near(t, at) && matches!(d, DriverOp::Advertise(..))),
            Op::UpdateParams(_, p) => {
                let asked = DriverOp::AskParams(p.interval_min.as_u16(), p.interval_max.as_u16(), p.latency);
                !driver.iter().any(|&(t, d)| near(t, at) && d == asked)
            }
            op if compared(&op) => !driver.iter().any(|&(t, d)| near(t, at) && same(&op, d)),
            _ => false,
        };
        if alone {
            out.push((at, true, Act::of_core(&op).unwrap()));
        }
    }
    for &(at, d) in &driver {
        if matches!(d, DriverOp::Advertise(..)) && !core.iter().any(|&(t, op)| near(t, at) && matches!(op, Op::Advertise(Some(_)))) {
            out.push((at, false, Act::Advertise));
        }
        let asked = operator.iter().any(|&t| t <= at && at - t <= OPERATOR);
        let operators = d == DriverOp::Other(OpKind::Disconnect) && asked;
        if compared_driver(d) && !operators && !core.iter().any(|&(t, op)| near(t, at) && same(&op, d)) {
            out.push((at, false, Act::of_driver(d).unwrap()));
        }
    }
    out
}

fn side() -> impl Strategy<Value = Side> {
    let link = ConnHandle::new(1);
    let asks = [(80, 160, 4), (12, 24, 0), (6, 6, 0)];
    prop_oneof![
        Just(Side::Core(advertise())),
        Just(Side::Core(Op::Advertise(None))),
        (0..asks.len()).prop_map(move |i| Side::Core(ask(asks[i].0, asks[i].1, asks[i].2))),
        any::<bool>().prop_map(move |yes| Side::Core(Op::ConfirmNumeric(link, yes))),
        Just(Side::Core(Op::RequestSecurity(link))),
        Just(Side::Core(Op::AllowRebond(link))),
        Just(Side::Core(Op::Disconnect(link, DisconnectReason::RemoteUserTerminatedConn))),
        Just(Side::Core(Op::Passkey(link, 0))),
        Just(Side::Driver(DriverOp::Advertise(160, 160))),
        Just(Side::Driver(DriverOp::StopAdvertising)),
        (0..asks.len() + 1).prop_map(move |i| Side::Driver(asks.get(i).map_or(DriverOp::AskParams(80, 160, 0), |a| DriverOp::AskParams(a.0, a.1, a.2)))),
        any::<bool>().prop_map(|yes| Side::Driver(DriverOp::Confirm(yes))),
        prop::sample::select(vec![OpKind::RequestSecurity, OpKind::AllowRebond, OpKind::Disconnect, OpKind::Scan, OpKind::Passkey])
            .prop_map(|k| Side::Driver(DriverOp::Other(k))),
        Just(Side::Operator),
    ]
}

proptest! {
    /// Told as they come, within the window's room, the verdict is the one over whole records.
    #[test]
    fn as_they_come_is_as_over_the_whole_record(steps in prop::collection::vec((prop_oneof![Just(0u64), 0..400u64, 0..1_500u64], side()), 0..=16)) {
        let mut at = 0;
        let actions: Vec<(u64, Side)> = steps.into_iter().map(|(gap, s)| { at += gap; (at, s) }).collect();
        let key = |v: Vec<(u64, bool, Act)>| { let mut v: Vec<String> = v.into_iter().map(|t| format!("{t:?}")).collect(); v.sort(); v };
        let streamed = judge(&actions).into_iter().map(|d| (d.at, d.core, d.act)).collect();
        prop_assert_eq!(key(streamed), key(whole(&actions)));
    }
}
