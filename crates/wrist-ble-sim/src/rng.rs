/// `SplitMix64`: small, fast, and the same sequence for a seed on every host.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `lo..=hi`.
    pub fn range(&mut self, lo: u32, hi: u32) -> u32 {
        if hi <= lo {
            return lo;
        }
        lo + u32::try_from(self.next_u64() % u64::from(hi - lo + 1)).unwrap_or(0)
    }

    /// True `permille` times in a thousand.
    pub fn chance(&mut self, permille: u16) -> bool {
        self.next_u64() % 1_000 < u64::from(permille)
    }
}
