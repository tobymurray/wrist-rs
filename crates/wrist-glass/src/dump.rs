//! A frame as text, for the console's `page dump`: each row as run-length pairs in hex,
//! which `tools/wrist-frame.py` turns back into a PNG.

use crate::WIDTH;

/// The longest code a row of [`WIDTH`] pixels can need: one run per pixel.
pub const MAX_ROW_CODE: usize = 2 * WIDTH as usize;
/// The longest hex line a row can need.
pub const MAX_ROW_HEX: usize = 2 * MAX_ROW_CODE;

/// `row` as (count, value) pairs, each count 1 to 255, into `out`; the bytes used, or
/// `None` when `out` is too short for them.
#[must_use]
pub fn encode_row(row: &[u8], out: &mut [u8]) -> Option<usize> {
    let mut n = 0;
    let mut rest = row;
    while let Some(&value) = rest.first() {
        let run = rest.iter().take(255).take_while(|&&b| b == value).count();
        let pair = out.get_mut(n..n + 2)?;
        pair[0] = u8::try_from(run).unwrap_or(u8::MAX);
        pair[1] = value;
        n += 2;
        rest = &rest[run..];
    }
    Some(n)
}

/// `bytes` as upper-case hex into `out`; the bytes used, or `None` when `out` is too short.
#[must_use]
pub fn hex(bytes: &[u8], out: &mut [u8]) -> Option<usize> {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    let out = out.get_mut(..2 * bytes.len())?;
    for (pair, &b) in out.as_chunks_mut::<2>().0.iter_mut().zip(bytes) {
        pair[0] = DIGITS[usize::from(b >> 4)];
        pair[1] = DIGITS[usize::from(b & 0xF)];
    }
    Some(out.len())
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    fn decode(code: &[u8]) -> Vec<u8> {
        code.as_chunks::<2>().0.iter().flat_map(|p| core::iter::repeat_n(p[1], usize::from(p[0]))).collect()
    }

    #[test]
    fn a_black_row_is_one_pair() {
        let mut out = [0u8; MAX_ROW_CODE];
        let n = encode_row(&[0u8; WIDTH as usize], &mut out).unwrap();
        assert_eq!(&out[..n], &[240, 0]);
    }

    #[test]
    fn rows_round_trip_and_fit() {
        let alternating: Vec<u8> = (0..WIDTH).map(|i| u8::from(i % 2 == 0)).collect();
        let mixed: Vec<u8> = (0..WIDTH).map(|i| (i / 7 % 3) as u8 * 0x55).collect();
        let long: Vec<u8> = core::iter::repeat_n(9, 600).collect();
        for row in [alternating.as_slice(), mixed.as_slice()] {
            let mut out = [0u8; MAX_ROW_CODE];
            let n = encode_row(row, &mut out).unwrap();
            assert_eq!(decode(&out[..n]), row);
        }
        let mut out = [0u8; 8];
        let n = encode_row(&long, &mut out).unwrap();
        assert_eq!(&out[..n], &[255, 9, 255, 9, 90, 9]);
    }

    #[test]
    fn short_buffers_are_refused() {
        let mut out = [0u8; 3];
        assert_eq!(encode_row(&[1, 2], &mut out), None);
        assert_eq!(hex(&[1, 2], &mut out), None);
    }

    #[test]
    fn hex_is_upper_case_pairs() {
        let mut out = [0u8; 6];
        assert_eq!(hex(&[0x00, 0xAB, 0x7F], &mut out), Some(6));
        assert_eq!(&out, b"00AB7F");
    }
}
