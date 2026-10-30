//! A recording of the accelerometer's FIFO in a borrowed buffer: samples from the start up,
//! and from the end down a mark at each drain with the chip's count and activity, so a walk
//! can be scored against what the chip counted.

/// One headerless FIFO frame: x, y and z, little-endian.
pub const FRAME: usize = 6;
pub const MARK: usize = 8;

/// The most FIFO bytes one read takes: whole frames, about 3 ms at 400 kHz.
pub const READ: usize = 21 * FRAME;

/// Whole frames in a FIFO fill level.
#[must_use]
pub const fn frames(fill: u16) -> usize {
    fill as usize / FRAME
}

/// The 14-bit fill level from `FIFO_LENGTH_0` and `FIFO_LENGTH_1`.
#[must_use]
pub const fn fill(length: [u8; 2]) -> u16 {
    u16::from_le_bytes([length[0], length[1] & 0x3F])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    /// Samples recorded before it.
    pub at: u32,
    pub steps: u32,
    pub activity: u8,
}

pub struct Recording<'a> {
    buf: &'a mut [u8],
    samples: usize,
    marks: usize,
}

impl<'a> Recording<'a> {
    /// An empty recording over `buf`.
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, samples: 0, marks: 0 }
    }

    #[must_use]
    pub const fn samples(&self) -> usize {
        self.samples
    }

    #[must_use]
    pub const fn marks(&self) -> usize {
        self.marks
    }

    const fn free(&self) -> usize {
        self.buf.len() - self.samples * FRAME - self.marks * MARK
    }

    /// Room for another drain's worth: `frames` samples and their mark.
    #[must_use]
    pub const fn has_room(&self, frames: usize) -> bool {
        self.free() >= frames * FRAME + MARK
    }

    /// Appends whole frames, keeping room for a mark; false, taking none, if they do not fit.
    pub fn push_frames(&mut self, bytes: &[u8]) -> bool {
        if !bytes.len().is_multiple_of(FRAME) || !self.has_room(bytes.len() / FRAME) {
            return false;
        }
        let at = self.samples * FRAME;
        self.buf[at..at + bytes.len()].copy_from_slice(bytes);
        self.samples += bytes.len() / FRAME;
        true
    }

    /// False if there is no room.
    pub fn push_mark(&mut self, steps: u32, activity: u8) -> bool {
        if self.free() < MARK {
            return false;
        }
        self.marks += 1;
        let at = self.buf.len() - self.marks * MARK;
        let index = u32::try_from(self.samples).unwrap_or(u32::MAX) & 0x00FF_FFFF;
        self.buf[at..at + 4].copy_from_slice(&(index | u32::from(activity) << 24).to_le_bytes());
        self.buf[at + 4..at + 8].copy_from_slice(&steps.to_le_bytes());
        true
    }

    /// The raw bytes of samples `range`, as the FIFO gave them.
    #[must_use]
    pub fn sample_bytes(&self, range: core::ops::Range<usize>) -> &[u8] {
        let end = range.end.min(self.samples);
        &self.buf[range.start.min(end) * FRAME..end * FRAME]
    }

    #[must_use]
    pub fn sample(&self, i: usize) -> Option<[i16; 3]> {
        let b = self.sample_bytes(i..i + 1);
        (b.len() == FRAME).then(|| core::array::from_fn(|a| i16::from_le_bytes([b[2 * a], b[2 * a + 1]])))
    }

    #[must_use]
    pub fn mark(&self, i: usize) -> Option<Mark> {
        if i >= self.marks {
            return None;
        }
        let at = self.buf.len() - (i + 1) * MARK;
        let word = |o: usize| u32::from_le_bytes([self.buf[at + o], self.buf[at + o + 1], self.buf[at + o + 2], self.buf[at + o + 3]]);
        let head = word(0);
        #[allow(clippy::cast_possible_truncation)]
        Some(Mark { at: head & 0x00FF_FFFF, steps: word(4), activity: (head >> 24) as u8 })
    }
}
