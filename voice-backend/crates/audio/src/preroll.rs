//! Pre-roll buffer.
//!
//! STT only receives audio while the VAD reports speech (Sarvam bills per
//! second). VAD needs a few windows to confirm speech, so the last few
//! hundred milliseconds are kept here and sent to STT first; otherwise
//! word onsets would be clipped.

use std::collections::VecDeque;

use bytes::Bytes;

/// Ring buffer of the most recent audio frames.
#[derive(Debug)]
pub struct PreRoll {
    frames: VecDeque<Bytes>,
    capacity: usize,
}

impl PreRoll {
    /// Keeps at most `capacity` frames.
    pub fn new(capacity: usize) -> Self {
        Self {
            frames: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Adds a frame, evicting the oldest when full.
    pub fn push(&mut self, frame: Bytes) {
        if self.frames.len() == self.capacity {
            self.frames.pop_front();
        }
        self.frames.push_back(frame);
    }

    /// Removes and returns all buffered frames, oldest first.
    pub fn drain(&mut self) -> impl Iterator<Item = Bytes> + '_ {
        self.frames.drain(..)
    }

    /// Number of buffered frames.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// `true` when no frames are buffered.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_most_recent() {
        let mut p = PreRoll::new(3);
        for i in 0..5u8 {
            p.push(Bytes::from(vec![i]));
        }
        let got: Vec<u8> = p.drain().map(|b| b[0]).collect();
        assert_eq!(got, vec![2, 3, 4]);
        assert!(p.is_empty());
    }
}
