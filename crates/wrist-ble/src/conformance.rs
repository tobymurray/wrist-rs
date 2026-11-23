//! A chip's answer set beside its driver's model: what the conformance suite, and the watch's
//! `ble selftest`, make of each change the radio was asked for.

use crate::{Admitted, Failure, FailureKind, SchedKind};

/// A chip's answer to one change, judged against what [`crate::Schedule::admit`] said of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Classified {
    /// Both took it.
    Admitted,
    /// Both refused it, the chip for the reason the model gave.
    Predicted(SchedKind),
    /// The chip refused what the model admits: the model is wrong, or the chip is not in the
    /// state the model was given.
    Unpredicted(Failure),
    /// The chip took what the model refuses.
    Missed(SchedKind),
    /// Both refused it, for different reasons.
    Otherwise { model: SchedKind, chip: Failure },
}

impl Classified {
    /// Whether the chip did what the model said it would.
    #[must_use]
    pub fn agrees(&self) -> bool {
        matches!(self, Self::Admitted | Self::Predicted(_))
    }
}

/// How a driver reports a refusal of this kind.
#[must_use]
pub const fn failure_kind(k: SchedKind) -> FailureKind {
    match k {
        SchedKind::Excluded(_) => FailureKind::ModeExclusion,
        k => FailureKind::Schedule(k),
    }
}

/// The chip's answer, `chip`, beside the model's, `model`.
#[must_use]
pub fn classify(model: Result<Admitted, SchedKind>, chip: Result<(), Failure>) -> Classified {
    match (model, chip) {
        (Ok(_), Ok(())) => Classified::Admitted,
        (Ok(_), Err(f)) => Classified::Unpredicted(f),
        (Err(k), Ok(())) => Classified::Missed(k),
        (Err(k), Err(f)) if f.kind == failure_kind(k) => Classified::Predicted(k),
        (Err(k), Err(f)) => Classified::Otherwise { model: k, chip: f },
    }
}
