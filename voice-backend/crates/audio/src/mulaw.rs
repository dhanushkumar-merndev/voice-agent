//! G.711 μ-law codec.
//!
//! Decoding uses a 256-entry table built at compile time. Encoding uses the
//! standard biased-segment algorithm with an 8-bit exponent table, which is
//! a handful of integer ops per sample.

const BIAS: i32 = 0x84;
const CLIP: i32 = 32635;

/// μ-law byte → linear 16-bit PCM, for all 256 codes.
static DECODE_TABLE: [i16; 256] = build_decode_table();

/// Exponent (segment) for the top 8 bits of a biased magnitude.
static EXP_TABLE: [u8; 256] = build_exp_table();

const fn decode_one(code: u8) -> i16 {
    let u = !code;
    let sign = u & 0x80;
    let exponent = ((u >> 4) & 0x07) as i32;
    let mantissa = (u & 0x0F) as i32;
    let magnitude = (((mantissa << 3) + BIAS) << exponent) - BIAS;
    if sign != 0 {
        -magnitude as i16
    } else {
        magnitude as i16
    }
}

const fn build_decode_table() -> [i16; 256] {
    let mut table = [0i16; 256];
    let mut i = 0;
    while i < 256 {
        table[i] = decode_one(i as u8);
        i += 1;
    }
    table
}

const fn build_exp_table() -> [u8; 256] {
    let mut table = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let mut exp = 7u8;
        let mut mask = 0x80usize;
        while exp > 0 && (i & mask) == 0 {
            exp -= 1;
            mask >>= 1;
        }
        table[i] = exp;
        i += 1;
    }
    table
}

/// Decodes one μ-law byte to 16-bit PCM.
#[inline]
pub fn decode(code: u8) -> i16 {
    DECODE_TABLE[code as usize]
}

/// Encodes one 16-bit PCM sample to μ-law.
#[inline]
pub fn encode(sample: i16) -> u8 {
    let mut pcm = i32::from(sample);
    let sign = if pcm < 0 {
        pcm = -pcm;
        0x80
    } else {
        0
    };
    pcm = pcm.min(CLIP) + BIAS;
    let exponent = EXP_TABLE[((pcm >> 7) & 0xFF) as usize];
    let mantissa = ((pcm >> (exponent + 3)) & 0x0F) as u8;
    !(sign | (exponent << 4) | mantissa)
}

/// Decodes μ-law bytes to `f32` samples in `[-1, 1)`, appending to `out`.
pub fn decode_to_f32(input: &[u8], out: &mut Vec<f32>) {
    out.reserve(input.len());
    out.extend(input.iter().map(|&b| f32::from(decode(b)) / 32768.0));
}

/// Encodes little-endian 16-bit PCM bytes to μ-law, appending to `out`.
/// A trailing odd byte is ignored.
pub fn encode_pcm16le(input: &[u8], out: &mut Vec<u8>) {
    out.reserve(input.len() / 2);
    out.extend(
        input
            .chunks_exact(2)
            .map(|pair| encode(i16::from_le_bytes([pair[0], pair[1]]))),
    );
}

/// The μ-law code for silence.
pub const SILENCE: u8 = 0xFF;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values() {
        assert_eq!(decode(0xFF), 0);
        assert_eq!(decode(0x7F), 0);
        assert_eq!(decode(0x00), -32124);
        assert_eq!(decode(0x80), 32124);
        assert_eq!(encode(0), 0xFF);
    }

    #[test]
    fn every_code_round_trips() {
        for code in 0u8..=255 {
            let pcm = decode(code);
            let again = encode(pcm);
            // 0x7F and 0xFF both mean zero; everything else is exact.
            assert_eq!(decode(again), pcm, "code {code:#04x}");
        }
    }

    #[test]
    fn pcm_round_trip_error_is_small() {
        for sample in (-32768i32..=32767).step_by(7) {
            let s = sample as i16;
            let back = i32::from(decode(encode(s)));
            let err = (back - sample).abs();
            // μ-law quantization step grows with magnitude (max ~1/16 of value + 1 step).
            let allowed = (sample.abs() / 16).max(8) + 132;
            assert!(err <= allowed, "sample {sample} -> {back} (err {err})");
        }
    }

    #[test]
    fn pcm16le_and_f32_helpers() {
        let pcm: Vec<u8> = [0i16, 1000, -1000, 32767]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let mut mu = Vec::new();
        encode_pcm16le(&pcm, &mut mu);
        assert_eq!(mu.len(), 4);
        let mut f = Vec::new();
        decode_to_f32(&mu, &mut f);
        assert!(f[0].abs() < 1e-6);
        assert!((f[1] - 1000.0 / 32768.0).abs() < 0.01);
        assert!(f[3] > 0.95);
    }
}
