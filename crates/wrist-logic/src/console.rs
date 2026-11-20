//! A console command's output, held while it runs and let out a line at a time.

/// Room a line let out into a slow sink leaves for output that cannot wait.
pub const HEADROOM: usize = 2048;

/// Whether a line of `len` bytes may go into a sink of `capacity` holding `free` now: it
/// leaves [`HEADROOM`], or the sink is empty.
#[must_use]
pub const fn room(free: usize, capacity: usize, len: usize) -> bool {
    free >= len + HEADROOM || free == capacity
}

pub struct Held<const N: usize> {
    buf: [u8; N],
    len: usize,
    out: usize,
}

impl<const N: usize> Default for Held<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Held<N> {
    #[must_use]
    pub const fn new() -> Self {
        Self { buf: [0; N], len: 0, out: 0 }
    }

    /// Keeps all of `bytes`, or none if they do not fit.
    pub fn hold(&mut self, bytes: &[u8]) -> bool {
        let end = self.len + bytes.len();
        if end > N {
            return false;
        }
        self.buf[self.len..end].copy_from_slice(bytes);
        self.len = end;
        true
    }

    /// A whole line is held.
    #[must_use]
    pub fn whole_line(&self) -> bool {
        self.buf[self.out..self.len].contains(&b'\n')
    }

    /// What [`Held::take`] lets out with a large enough piece: up to and including the next
    /// `\n`, else all that is held.
    #[must_use]
    pub fn next_len(&self) -> usize {
        let rest = &self.buf[self.out..self.len];
        rest.iter().position(|&b| b == b'\n').map_or(rest.len(), |i| i + 1)
    }

    /// Copies out what is held up to and including its next `\n`, at most `piece.len()` bytes;
    /// how many, and whether more is held.
    pub fn take(&mut self, piece: &mut [u8]) -> (usize, bool) {
        let rest = &self.buf[self.out..self.len];
        let rest = &rest[..rest.len().min(piece.len())];
        let n = rest.iter().position(|&b| b == b'\n').map_or(rest.len(), |i| i + 1);
        piece[..n].copy_from_slice(&rest[..n]);
        self.out += n;
        if self.out == self.len {
            (self.out, self.len) = (0, 0);
        }
        (n, self.len > 0)
    }
}

/// The furthest [`Log::read`] looks for the end of a line it lost the start of.
pub const SKIP_MAX: u64 = 512;

/// The console's output, each line stamped with the uptime it began at; positions count every
/// byte ever kept.
pub struct Log<const N: usize> {
    buf: [u8; N],
    end: u64,
    fresh: bool,
}

/// What [`Log::read`] copied: `len` bytes from position `from`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Read {
    pub from: u64,
    pub len: usize,
}

impl<const N: usize> Default for Log<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Log<N> {
    #[must_use]
    pub const fn new() -> Self {
        Self { buf: [0; N], end: 0, fresh: true }
    }

    #[must_use]
    pub const fn end(&self) -> u64 {
        self.end
    }

    /// The oldest position still kept.
    #[must_use]
    pub fn start(&self) -> u64 {
        self.end.saturating_sub(N as u64)
    }

    fn push(&mut self, b: u8) {
        let at = usize::try_from(self.end % N as u64).unwrap_or(0);
        self.buf[at] = b;
        self.end += 1;
    }

    fn push_decimal(&mut self, n: u64, width: usize) {
        let mut digits = [b'0'; 20];
        let (mut i, mut n) = (digits.len(), n);
        while n > 0 || digits.len() - i < width.max(1) {
            i -= 1;
            digits[i] = b'0' + u8::try_from(n % 10).unwrap_or(0);
            n /= 10;
        }
        for &d in &digits[i..] {
            self.push(d);
        }
    }

    /// Keeps `bytes`, dropping `\r`, with `now_ms` as `seconds.millis ` before each line.
    pub fn record(&mut self, now_ms: u64, bytes: &[u8]) {
        for &b in bytes {
            if b == b'\r' {
                continue;
            }
            if self.fresh {
                self.push_decimal(now_ms / 1000, 1);
                self.push(b'.');
                self.push_decimal(now_ms % 1000, 3);
                self.push(b' ');
                self.fresh = false;
            }
            self.push(b);
            self.fresh = b == b'\n';
        }
    }

    fn at(&self, pos: u64) -> u8 {
        self.buf[usize::try_from(pos % N as u64).unwrap_or(0)]
    }

    /// Copies from `at` towards `until` the whole lines that fit in `out`, or a piece of a line
    /// longer than `out`. From before [`Log::start`] it begins at the first whole line kept, looking
    /// no further than [`SKIP_MAX`] bytes for it.
    pub fn read(&self, at: u64, until: u64, out: &mut [u8]) -> Read {
        let until = until.min(self.end);
        let mut from = at;
        if at < self.start() {
            from = self.start();
            let limit = until.min(from + SKIP_MAX);
            if let Some(nl) = (from..limit).find(|&p| self.at(p) == b'\n') {
                from = nl + 1;
            }
        }
        if from >= until {
            return Read { from: until.max(at), len: 0 };
        }
        let n = usize::try_from(until - from).unwrap_or(usize::MAX).min(out.len());
        for (i, b) in out[..n].iter_mut().enumerate() {
            *b = self.at(from + i as u64);
        }
        let mut len = n;
        if from + (n as u64) < until {
            if let Some(i) = out[..n].iter().rposition(|&b| b == b'\n') {
                len = i + 1;
            }
        }
        Read { from, len }
    }
}
