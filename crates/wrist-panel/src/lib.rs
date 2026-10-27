#![no_std]

//! The Sharp LS012B7DD06A's pixel bus as this board wires it, and the timing the driver keeps
//! (LCP-2619063C, `HARDWARE.md`).

pub const LINES: usize = 240;
/// Pixel pairs a line shifts in, top pair first.
pub const PAIRS: usize = 120;

/// Port E: the odd pixel of a pair on bits 0, 2 and 4 (R, G, B), the even one 8 bits up.
pub const DATA_MASK: u32 = 0x1515;
/// Port E's `BSRR` word for a black pair.
pub const BLACK: u32 = DATA_MASK << 16;

/// Port E's `BSRR` word for the pair `odd` above `even`, both ABGR2222, on `plane` (0 the LSB).
#[must_use]
pub const fn word(odd: u8, even: u8, plane: u32) -> u32 {
    let set = ((odd as u32 >> plane) & 0x15) | (((even as u32 >> plane) & 0x15) << 8);
    set | ((!set & DATA_MASK) << 16)
}

/// The gate line that writes screen column `x`.
#[must_use]
pub const fn line(x: usize) -> usize {
    LINES - 1 - x
}

/// A cycle counter, and code run with interrupts masked.
pub trait Clock {
    fn cycles(&mut self) -> u32;
    fn masked<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R;
}

/// Waits from the last write: [`Pace::after`]'s final counter read and its write run masked, so an
/// interrupt only lengthens a wait.
pub struct Pace<C> {
    clock: C,
    mark: u32,
}

impl<C: Clock> Pace<C> {
    pub fn new(mut clock: C) -> Self {
        let mark = clock.cycles();
        Self { clock, mark }
    }

    pub fn wait(&mut self, cycles: u32) {
        while self.clock.cycles().wrapping_sub(self.mark) < cycles {}
    }

    /// `write`, once `cycles` have passed since the last write.
    pub fn after(&mut self, cycles: u32, write: impl FnOnce()) {
        self.wait(cycles.saturating_sub(MASKED_SPIN));
        let mark = self.mark;
        self.mark = self.clock.masked(|clock| {
            let now = loop {
                let now = clock.cycles();
                if now.wrapping_sub(mark) >= cycles {
                    break now;
                }
            };
            write();
            now
        });
    }
}

/// The end of a wait [`Pace::after`] spins masked, so a short wait ends on time.
const MASKED_SPIN: u32 = 48;

/// Nanoseconds, against the minimums of Tables 6-3-2 and 6-3-3.
pub mod timing {
    /// Data held after a `BCK` edge, then set up before the next: a half-period is both.
    pub const DATA_HOLD: u32 = 300;
    pub const DATA_SETUP: u32 = 300;
    /// `BSP` high before `BCK`'s first edge.
    pub const BSP_SETUP: u32 = 1_000;
    /// `BCK` parked low, then `GEN` high.
    pub const GEN_SETUP: u32 = 1_000;
    pub const GEN_HIGH: u32 = 11_000;
    /// `GEN` low before the next `GCK` edge.
    pub const GEN_HOLD: u32 = 5_000;
    /// A `GCK` half-period with no line written, and `GCK` 483 to `INTB` low.
    pub const GCK_HALF: u32 = 70_000;
    /// A `GCK` half-period over a line a partial frame keeps.
    pub const GCK_FAST: u32 = 2_000;
    /// `INTB` to `GSP`, and `GSP` either side of `GCK`'s first edges.
    pub const GSP_SETUP: u32 = 12_000;
    pub const INTB_HIGH_MS: u32 = 31;
    pub const INTB_LOW: u32 = 12_000;
    /// What one plane of a written line holds `GCK` for, at the least.
    pub const WRITTEN_HALF: u32 = BSP_SETUP + 123 * (DATA_HOLD + DATA_SETUP) + GEN_SETUP + GEN_HIGH + GEN_HOLD;

    const _: () = {
        assert!(DATA_HOLD >= 200 && DATA_SETUP >= 200, "thRGB, tsRGB");
        assert!(DATA_HOLD + DATA_SETUP >= 400, "thwBCK, tlwBCK");
        assert!(BSP_SETUP >= 200 && DATA_HOLD >= 200, "thsBSP, tlsBSP");
        assert!(GEN_HIGH >= 9_670, "thwGEN");
        assert!(GEN_HOLD >= 4_000, "thhGCK");
        assert!(GCK_HALF >= 62_670 && WRITTEN_HALF >= 62_670, "thwGCK, tlwGCK, thhINTB");
        assert!(GCK_FAST >= 1_000, "Note 6-3-1");
        assert!(GSP_SETUP >= 10_000, "thsGSP, tlsGSP");
        assert!(INTB_HIGH_MS * 1_000_000 >= 30_300_000, "thwINTB");
        assert!(INTB_LOW >= 10_000, "tlwINTB");
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bus as `HARDWARE.md` maps it, one channel at a time.
    fn by_channel(odd: u8, even: u8, plane: u32) -> u32 {
        let bit = |p: u8, channel: u32| (u32::from(p) >> (2 * channel + plane)) & 1;
        let mut set = 0;
        for (channel, pin) in [(0, 0), (1, 2), (2, 4)] {
            set |= bit(odd, channel) << pin;
            set |= bit(even, channel) << (pin + 8);
        }
        set | ((!set & DATA_MASK) << 16)
    }

    #[test]
    fn every_pair_on_both_planes() {
        for odd in 0..=u8::MAX {
            for even in 0..=u8::MAX {
                for plane in 0..2 {
                    assert_eq!(word(odd, even, plane), by_channel(odd, even, plane), "{odd:#04x} {even:#04x} plane {plane}");
                }
            }
        }
    }

    #[test]
    fn a_word_sets_or_clears_every_data_line_and_nothing_else() {
        for odd in 0..=u8::MAX {
            let w = word(odd, !odd, 1);
            assert_eq!((w | (w >> 16)) & 0xFFFF, DATA_MASK);
            assert_eq!(w & (w >> 16), 0);
            assert_eq!(w & !(DATA_MASK | (DATA_MASK << 16)), 0);
        }
        assert_eq!(word(0, 0, 0), BLACK);
        assert_eq!(word(0xC0, 0xC0, 1), BLACK, "alpha is not on the bus");
    }

    /// One tick a counter read or a write; an interrupt of `x` ticks before step `at`, or at
    /// the unmask after it when it falls in masked code.
    struct Model {
        now: u32,
        step: usize,
        at: usize,
        x: u32,
        masked: bool,
        pending: bool,
    }

    impl Model {
        fn tick(&mut self) -> u32 {
            if self.step == self.at {
                if self.masked {
                    self.pending = true;
                } else {
                    self.now += self.x;
                }
            }
            self.step += 1;
            self.now += 1;
            self.now
        }
    }

    struct Shared<'a>(&'a core::cell::RefCell<Model>);

    impl Clock for Shared<'_> {
        fn cycles(&mut self) -> u32 {
            self.0.borrow_mut().tick()
        }

        fn masked<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
            self.0.borrow_mut().masked = true;
            let r = f(self);
            let mut m = self.0.borrow_mut();
            m.masked = false;
            if m.pending {
                m.pending = false;
                m.now += m.x;
            }
            r
        }
    }

    #[test]
    fn an_interrupt_anywhere_only_lengthens_a_wait() {
        extern crate std;
        use core::cell::RefCell;
        use std::vec::Vec;
        let gaps = [3, 50, 1, 200, 7];
        for x in [1, 2, 9, 60, 10_000] {
            for at in 0..400 {
                let model = RefCell::new(Model { now: 0, step: 0, at, x, masked: false, pending: false });
                let mut writes = Vec::new();
                let mut pace = Pace::new(Shared(&model));
                let start = model.borrow().now;
                for gap in gaps {
                    pace.after(gap, || writes.push(model.borrow_mut().tick()));
                }
                for (i, gap) in gaps.into_iter().enumerate() {
                    let from = if i == 0 { start } else { writes[i - 1] };
                    let took = writes[i] - from;
                    assert!(took >= gap, "interrupt of {x} at step {at}: write {i} {took} after the last, wanted {gap}");
                }
            }
        }
    }

    #[test]
    fn line_zero_is_the_right_edge() {
        assert_eq!(line(LINES - 1), 0);
        assert_eq!(line(0), LINES - 1);
    }
}
