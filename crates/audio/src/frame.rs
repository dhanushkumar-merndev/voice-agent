//! Fixed-size framing of a byte stream.

use bytes::{Bytes, BytesMut};

/// Splits an arbitrary stream of μ-law bytes into frames of `frame_len`
/// bytes (160 = 20 ms at 8 kHz). Leftover bytes wait for the next push.
#[derive(Debug)]
pub struct Framer {
    frame_len: usize,
    pending: BytesMut,
}

impl Framer {
    /// A framer producing `frame_len`-byte frames.
    pub fn new(frame_len: usize) -> Self {
        Self {
            frame_len,
            pending: BytesMut::with_capacity(frame_len * 4),
        }
    }

    /// Adds bytes and appends every complete frame to `out`.
    pub fn push(&mut self, input: &[u8], out: &mut Vec<Bytes>) {
        self.pending.extend_from_slice(input);
        while self.pending.len() >= self.frame_len {
            out.push(self.pending.split_to(self.frame_len).freeze());
        }
    }

    /// Returns the partial last frame padded with `fill`, if any.
    pub fn flush(&mut self, fill: u8) -> Option<Bytes> {
        if self.pending.is_empty() {
            return None;
        }
        self.pending.resize(self.frame_len, fill);
        Some(self.pending.split().freeze())
    }

    /// Drops any partial frame.
    pub fn clear(&mut self) {
        self.pending.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_and_flushes() {
        let mut f = Framer::new(160);
        let mut out = Vec::new();
        f.push(&[1u8; 100], &mut out);
        assert!(out.is_empty());
        f.push(&[2u8; 300], &mut out);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|fr| fr.len() == 160));
        let last = f.flush(0xFF).unwrap();
        assert_eq!(last.len(), 160);
        assert_eq!(last[79], 2);
        assert_eq!(last[80], 0xFF);
        assert!(f.flush(0xFF).is_none());
    }
}
