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

/// How much of `bytes` is whole lines: up to and including its last `\n`, or all of it when it
/// has none.
#[must_use]
pub fn whole_lines(bytes: &[u8]) -> usize {
    bytes.iter().rposition(|&b| b == b'\n').map_or(bytes.len(), |i| i + 1)
}

/// Where [`Cut`] puts output: a pipe to a host.
pub trait Sink {
    /// Queues a prefix of `bytes`, which may be short of what fits; how many.
    fn write(&mut self, bytes: &[u8]) -> usize;
}

fn write_all(sink: &mut impl Sink, mut bytes: &[u8]) -> usize {
    let len = bytes.len();
    while !bytes.is_empty() {
        match sink.write(bytes) {
            0 => break,
            n => bytes = &bytes[n..],
        }
    }
    len - bytes.len()
}

/// Output into a sink that drops what does not fit: the rest of a line cut short goes with it,
/// and the next line to go starts on a new one.
#[derive(Default)]
pub struct Cut {
    /// The sink ends inside a line.
    mid: bool,
    /// The current line is being dropped up to its `\n`.
    dropping: bool,
    /// A line was cut short, and the sink ends inside it.
    open: bool,
}

impl Cut {
    #[must_use]
    pub const fn new() -> Self {
        Self { mid: false, dropping: false, open: false }
    }

    /// Writes what fits of `bytes`; how many it dropped.
    pub fn feed(&mut self, bytes: &[u8], sink: &mut impl Sink) -> usize {
        let mut dropped = 0;
        for piece in bytes.split_inclusive(|&b| b == b'\n') {
            let ends = piece.last() == Some(&b'\n');
            if self.open {
                if write_all(sink, b"\r\n") == 2 {
                    (self.open, self.mid) = (false, false);
                } else {
                    self.dropping = true;
                }
            }
            if self.dropping {
                dropped += piece.len();
                self.dropping = !ends;
                continue;
            }
            let n = write_all(sink, piece);
            if n > 0 {
                self.mid = piece[n - 1] != b'\n';
            }
            if n < piece.len() {
                dropped += piece.len() - n;
                self.dropping = !ends;
                self.open = self.mid;
            }
        }
        dropped
    }
}

/// The furthest [`Log::read`] looks for the end of a line it lost the start of.
pub const SKIP_MAX: u64 = 512;

/// `seconds.millis `, its digits in 32-bit arithmetic: the watch has no 64-bit divide.
fn stamp(now_ms: u64, out: &mut [u8; 16]) -> &[u8] {
    let digit = |v: u32| b'0' + u8::try_from(v % 10).unwrap_or(0);
    let (mut s, mut ms) = (u32::try_from(now_ms / 1000).unwrap_or(u32::MAX), u32::try_from(now_ms % 1000).unwrap_or(0));
    let mut i = 15;
    out[i] = b' ';
    for _ in 0..3 {
        i -= 1;
        out[i] = digit(ms);
        ms /= 10;
    }
    i -= 1;
    out[i] = b'.';
    loop {
        i -= 1;
        out[i] = digit(s);
        s /= 10;
        if s == 0 {
            return &out[i..];
        }
    }
}

/// The console's output, each line stamped with the uptime it began at; positions count every
/// byte ever kept.
pub struct Log<const N: usize> {
    buf: [u8; N],
    end: u64,
    line_end: u64,
    mid_line: bool,
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
        Self { buf: [0; N], end: 0, line_end: 0, mid_line: false }
    }

    #[must_use]
    pub const fn end(&self) -> u64 {
        self.end
    }

    /// The position after the last whole line.
    #[must_use]
    pub const fn line_end(&self) -> u64 {
        self.line_end
    }

    /// The oldest position still kept.
    #[must_use]
    pub fn start(&self) -> u64 {
        self.end.saturating_sub(N as u64)
    }

    fn push_all(&mut self, bytes: &[u8]) {
        let skip = bytes.len().saturating_sub(N);
        self.end += skip as u64;
        let bytes = &bytes[skip..];
        let at = usize::try_from(self.end % N as u64).unwrap_or(0);
        let (first, wrapped) = bytes.split_at(bytes.len().min(N - at));
        self.buf[at..at + first.len()].copy_from_slice(first);
        self.buf[..wrapped.len()].copy_from_slice(wrapped);
        self.end += bytes.len() as u64;
    }

    /// Keeps `bytes`, dropping `\r`, with `now_ms` as `seconds.millis ` before each line.
    pub fn record(&mut self, now_ms: u64, bytes: &[u8]) {
        for run in bytes.split_inclusive(|&b| b == b'\n') {
            for part in run.split(|&b| b == b'\r').filter(|p| !p.is_empty()) {
                if !self.mid_line {
                    self.push_all(stamp(now_ms, &mut [0; 16]));
                }
                self.push_all(part);
                self.mid_line = part.last() != Some(&b'\n');
                if !self.mid_line {
                    self.line_end = self.end;
                }
            }
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
        let len = if from + (n as u64) < until { whole_lines(&out[..n]) } else { n };
        Read { from, len }
    }
}
