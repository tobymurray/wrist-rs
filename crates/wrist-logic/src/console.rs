//! A console command's output, held while it runs and let out a line at a time.

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
