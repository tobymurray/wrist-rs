//! Consistent extension of an asynchronous 16-bit hardware counter.

/// `None` when the two snapshots differ, so read again; the caller services
/// the overflow interrupt at least once a counter cycle.
#[must_use]
pub const fn extend(
    epochs: (u32, u32), pending: (bool, bool), counters: (u16, u16),
) -> Option<u64> {
    if epochs.0 != epochs.1 || pending.0 != pending.1 || counters.0 != counters.1 { return None; }
    Some(((epochs.0 as u64 + pending.0 as u64) << 16) | counters.0 as u64)
}
