//! A chip's answer beside its model's: each of the five ways they can stand.

use wrist_ble::conformance::{classify, failure_kind, Classified};
use wrist_ble::{Admitted, Exclusion, Failure, FailureKind, SchedKind};

const EXCLUDED: SchedKind = SchedKind::Excluded(Exclusion::NoAdvertiseWhileInitiating);

fn failure(kind: FailureKind) -> Failure {
    Failure { kind, native: 0x99 }
}

#[test]
fn each_way_the_chip_and_the_model_can_stand() {
    let refusal = failure(FailureKind::ModeExclusion);
    assert_eq!(classify(Ok(Admitted::AS_ASKED), Ok(())), Classified::Admitted);
    assert_eq!(classify(Err(EXCLUDED), Err(refusal)), Classified::Predicted(EXCLUDED));
    assert_eq!(classify(Ok(Admitted::AS_ASKED), Err(refusal)), Classified::Unpredicted(refusal));
    assert_eq!(classify(Err(EXCLUDED), Ok(())), Classified::Missed(EXCLUDED));
    let busy = failure(FailureKind::Busy);
    assert_eq!(classify(Err(EXCLUDED), Err(busy)), Classified::Otherwise { model: EXCLUDED, chip: busy });
}

#[test]
fn only_the_chip_doing_as_the_model_said_agrees() {
    let f = failure(FailureKind::Unknown);
    assert!(Classified::Admitted.agrees());
    assert!(Classified::Predicted(SchedKind::NoSlot).agrees());
    assert!(!Classified::Unpredicted(f).agrees());
    assert!(!Classified::Missed(SchedKind::NoSlot).agrees());
    assert!(!Classified::Otherwise { model: SchedKind::NoSlot, chip: f }.agrees());
}

#[test]
fn a_scheduler_refusal_is_reported_as_its_own_kind_and_an_exclusion_as_the_modes() {
    for k in [SchedKind::NewIntervalFailed, SchedKind::IntervalTooLarge, SchedKind::LengthFailed, SchedKind::NoSlot] {
        assert_eq!(failure_kind(k), FailureKind::Schedule(k));
        assert_eq!(classify(Err(k), Err(failure(FailureKind::Schedule(k)))), Classified::Predicted(k));
        assert!(!classify(Err(k), Err(failure(FailureKind::ModeExclusion))).agrees());
    }
    for e in [Exclusion::NoAdvertiseWhileInitiating, Exclusion::NoScanWhileInitiating, Exclusion::NoInitiateWhileInitiating] {
        assert_eq!(failure_kind(SchedKind::Excluded(e)), FailureKind::ModeExclusion);
    }
}
