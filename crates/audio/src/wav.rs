//! Streaming RIFF/WAV header stripper.
//!
//! TTS providers stream WAV as a header followed by sample data, split
//! across arbitrary message boundaries. [`WavStripper`] buffers until the
//! `data` chunk starts, reports the format, then passes sample bytes
//! through. Input that does not start with `RIFF` is treated as raw audio.

use bytes::{Buf, Bytes, BytesMut};

/// Sample encoding announced by the WAV `fmt ` chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WavEncoding {
    /// Linear PCM (format tag 1).
    Pcm,
    /// G.711 μ-law (format tag 7).
    Mulaw,
    /// Anything else.
    Other(u16),
}

/// Audio format from the `fmt ` chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WavFormat {
    /// Sample encoding.
    pub encoding: WavEncoding,
    /// Channel count.
    pub channels: u16,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Bits per sample.
    pub bits_per_sample: u16,
}

#[derive(Debug)]
enum State {
    /// Collecting bytes until we know whether this is a WAV stream.
    Header(BytesMut),
    /// Inside the `data` chunk (or raw audio): pass bytes through.
    Data,
}

/// Incremental WAV header stripper. Call [`WavStripper::reset`] between
/// utterances when each utterance carries its own header.
#[derive(Debug)]
pub struct WavStripper {
    state: State,
    format: Option<WavFormat>,
}

impl Default for WavStripper {
    fn default() -> Self {
        Self::new()
    }
}

/// Largest header we are willing to buffer before giving up and passing
/// bytes through as raw audio.
const MAX_HEADER: usize = 4096;

impl WavStripper {
    /// A stripper expecting a new stream.
    pub fn new() -> Self {
        Self {
            state: State::Header(BytesMut::new()),
            format: None,
        }
    }

    /// Prepares for the next stream, which may start with its own header.
    pub fn reset(&mut self) {
        self.state = State::Header(BytesMut::new());
    }

    /// Format of the current stream, once its header has been parsed.
    /// `None` for raw (headerless) streams.
    pub fn format(&self) -> Option<WavFormat> {
        self.format
    }

    /// Feeds bytes; returns sample bytes that follow the header (possibly empty).
    pub fn push(&mut self, input: Bytes) -> Bytes {
        let buf = match &mut self.state {
            State::Data => return input,
            State::Header(buf) => {
                buf.extend_from_slice(&input);
                buf
            }
        };
        if buf.len() < 4 {
            return Bytes::new();
        }
        if &buf[..4] != b"RIFF" {
            let raw = std::mem::take(buf).freeze();
            self.state = State::Data;
            return raw;
        }
        match parse_header(buf) {
            HeaderParse::NeedMore if buf.len() < MAX_HEADER => Bytes::new(),
            HeaderParse::NeedMore => {
                let raw = std::mem::take(buf).freeze();
                self.state = State::Data;
                raw
            }
            HeaderParse::Done {
                data_offset,
                format,
            } => {
                let mut all = std::mem::take(buf);
                all.advance(data_offset);
                self.format = format.or(self.format);
                self.state = State::Data;
                all.freeze()
            }
        }
    }
}

enum HeaderParse {
    NeedMore,
    Done {
        data_offset: usize,
        format: Option<WavFormat>,
    },
}

fn parse_header(buf: &[u8]) -> HeaderParse {
    // RIFF <size> WAVE, then chunks: <id:4> <size:u32 le> <payload>.
    if buf.len() < 12 {
        return HeaderParse::NeedMore;
    }
    let mut pos = 12;
    let mut format = None;
    loop {
        if buf.len() < pos + 8 {
            return HeaderParse::NeedMore;
        }
        let id = &buf[pos..pos + 4];
        let size =
            u32::from_le_bytes([buf[pos + 4], buf[pos + 5], buf[pos + 6], buf[pos + 7]]) as usize;
        let body = pos + 8;
        if id == b"data" {
            return HeaderParse::Done {
                data_offset: body,
                format,
            };
        }
        let padded = size + (size & 1);
        if buf.len() < body + padded {
            return HeaderParse::NeedMore;
        }
        if id == b"fmt " && size >= 16 {
            let f = &buf[body..body + 16];
            let tag = u16::from_le_bytes([f[0], f[1]]);
            format = Some(WavFormat {
                encoding: match tag {
                    1 => WavEncoding::Pcm,
                    7 => WavEncoding::Mulaw,
                    other => WavEncoding::Other(other),
                },
                channels: u16::from_le_bytes([f[2], f[3]]),
                sample_rate: u32::from_le_bytes([f[4], f[5], f[6], f[7]]),
                bits_per_sample: u16::from_le_bytes([f[14], f[15]]),
            });
        }
        pos = body + padded;
    }
}

/// Builds a minimal 44-byte header for 8 kHz mono 16-bit PCM (used by tests and the simulator).
pub fn pcm16_header(sample_rate: u32, data_len: u32) -> Vec<u8> {
    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(36 + data_len).to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes());
    h.extend_from_slice(&sample_rate.to_le_bytes());
    h.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    h.extend_from_slice(&2u16.to_le_bytes());
    h.extend_from_slice(&16u16.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data_len.to_le_bytes());
    h
}

/// Wraps 8 kHz mono μ-law samples in a WAV file (format tag 7, with the
/// `fact` chunk non-PCM formats carry), for uploading caller audio to a
/// file-based STT API.
pub fn mulaw_wav(samples: &[u8]) -> Vec<u8> {
    let len = u32::try_from(samples.len()).unwrap_or(u32::MAX);
    let mut w = Vec::with_capacity(58 + samples.len());
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(50u32.saturating_add(len)).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&18u32.to_le_bytes());
    w.extend_from_slice(&7u16.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&crate::SAMPLE_RATE.to_le_bytes());
    w.extend_from_slice(&crate::SAMPLE_RATE.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&8u16.to_le_bytes());
    w.extend_from_slice(&0u16.to_le_bytes());
    w.extend_from_slice(b"fact");
    w.extend_from_slice(&4u32.to_le_bytes());
    w.extend_from_slice(&len.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&len.to_le_bytes());
    w.extend_from_slice(samples);
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mulaw_wav_round_trips_through_the_stripper() {
        let samples = [0xffu8, 0x7f, 0x00, 0x80, 0x55];
        let file = mulaw_wav(&samples);
        assert_eq!(&file[..4], b"RIFF");
        assert_eq!(
            u32::from_le_bytes([file[4], file[5], file[6], file[7]]) as usize,
            file.len() - 8
        );
        let mut s = WavStripper::new();
        assert_eq!(&s.push(Bytes::from(file))[..], &samples);
        let f = s.format().unwrap();
        assert_eq!(f.encoding, WavEncoding::Mulaw);
        assert_eq!((f.channels, f.sample_rate, f.bits_per_sample), (1, 8000, 8));
    }

    fn wav(payload: &[u8]) -> Vec<u8> {
        let mut v = pcm16_header(8000, payload.len() as u32);
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn strips_header_split_across_chunks() {
        let data = wav(&[1, 2, 3, 4, 5, 6]);
        let mut s = WavStripper::new();
        let mut out = Vec::new();
        for chunk in data.chunks(5) {
            out.extend_from_slice(&s.push(Bytes::copy_from_slice(chunk)));
        }
        assert_eq!(out, vec![1, 2, 3, 4, 5, 6]);
        let fmt = s.format().unwrap();
        assert_eq!(fmt.encoding, WavEncoding::Pcm);
        assert_eq!(fmt.sample_rate, 8000);
        assert_eq!(fmt.bits_per_sample, 16);
    }

    #[test]
    fn skips_extra_chunks_before_data() {
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF\0\0\0\0WAVE");
        v.extend_from_slice(b"LIST");
        v.extend_from_slice(&3u32.to_le_bytes());
        v.extend_from_slice(&[9, 9, 9, 0]); // odd size, padded
        let h = pcm16_header(8000, 2);
        v.extend_from_slice(&h[12..]);
        v.extend_from_slice(&[7, 8]);
        let mut s = WavStripper::new();
        assert_eq!(s.push(Bytes::from(v)).as_ref(), &[7, 8]);
    }

    #[test]
    fn raw_audio_passes_through_and_reset_handles_next_header() {
        let mut s = WavStripper::new();
        assert_eq!(s.push(Bytes::from_static(&[0xFF; 10])).len(), 10);
        assert_eq!(s.push(Bytes::from_static(&[0xFF; 3])).len(), 3);
        s.reset();
        assert_eq!(s.push(Bytes::from(wav(&[42]))).as_ref(), &[42]);
    }
}
