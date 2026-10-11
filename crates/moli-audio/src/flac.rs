//! FLAC without compression (« verbatim » subframes): every ESPHome media
//! player decodes it, even a firmware built without WAV (the HA Voice PE's
//! is). Written as the samples come, so a voice can stream while it is
//! made; the samples stay exact, the file as large as a WAV.

/// Samples per frame (the last one may be shorter).
const BLOCK: usize = 1152;

/// 16-bit mono samples in, FLAC bytes out.
#[derive(Debug)]
pub struct Writer {
    rate: u32,
    pending: Vec<i16>,
    frame: u64,
}

impl Writer {
    #[must_use]
    pub fn new(rate: u32) -> Self {
        Self {
            rate,
            pending: Vec::with_capacity(BLOCK),
            frame: 0,
        }
    }

    /// The stream's start: the marker and its STREAMINFO (sizes and length
    /// unknown, as for any stream).
    #[must_use]
    pub fn header(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(42);
        out.extend_from_slice(b"fLaC");
        // Last metadata block, STREAMINFO, 34 bytes.
        out.extend_from_slice(&[0x80, 0, 0, 34]);
        #[allow(clippy::cast_possible_truncation)]
        let block = BLOCK as u16;
        out.extend_from_slice(&block.to_be_bytes()); // shortest block
        out.extend_from_slice(&block.to_be_bytes()); // longest block
        out.extend_from_slice(&[0; 6]); // frame sizes: unknown
        // 20 bits of rate, 3 of channels - 1, 5 of bits - 1, 36 of length.
        let packed = (u64::from(self.rate) << 44) | (15 << 36);
        out.extend_from_slice(&packed.to_be_bytes());
        out.extend_from_slice(&[0; 16]); // MD5: unknown
        out
    }

    /// More samples: the whole frames they complete.
    pub fn push(&mut self, samples: &[i16]) -> Vec<u8> {
        let mut out = Vec::new();
        for s in samples {
            self.pending.push(*s);
            if self.pending.len() == BLOCK {
                out.extend(self.frame());
            }
        }
        out
    }

    /// What is left, as a last, shorter frame.
    pub fn finish(&mut self) -> Vec<u8> {
        if self.pending.is_empty() {
            Vec::new()
        } else {
            self.frame()
        }
    }

    fn frame(&mut self) -> Vec<u8> {
        let mut f = Vec::with_capacity(16 + 1 + self.pending.len() * 2 + 2);
        f.extend_from_slice(&[0xFF, 0xF8]); // sync, fixed block size
        let (rate_code, rate_bytes) = rate_code(self.rate);
        f.push(0x70 | rate_code); // block size in 16 bits at the end
        f.push(0x08); // mono, 16 bits
        f.extend(utf8_number(self.frame));
        #[allow(clippy::cast_possible_truncation)]
        let size = (self.pending.len() - 1) as u16;
        f.extend_from_slice(&size.to_be_bytes());
        f.extend(rate_bytes);
        f.push(crc8(&f));
        f.push(0x02); // a verbatim subframe, no wasted bits
        for s in self.pending.drain(..) {
            f.extend_from_slice(&s.to_be_bytes());
        }
        let crc = crc16(&f);
        f.extend_from_slice(&crc.to_be_bytes());
        self.frame += 1;
        f
    }
}

/// A whole FLAC file of `samples`.
#[must_use]
pub fn encode(samples: &[i16], rate: u32) -> Vec<u8> {
    let mut w = Writer::new(rate);
    let mut out = w.header();
    out.extend(w.push(samples));
    out.extend(w.finish());
    out
}

/// The frame header's sample-rate code, and the bytes it calls for.
fn rate_code(rate: u32) -> (u8, Vec<u8>) {
    match rate {
        8_000 => (0b0100, Vec::new()),
        16_000 => (0b0101, Vec::new()),
        22_050 => (0b0110, Vec::new()),
        24_000 => (0b0111, Vec::new()),
        32_000 => (0b1000, Vec::new()),
        44_100 => (0b1001, Vec::new()),
        48_000 => (0b1010, Vec::new()),
        r if r % 1000 == 0 && r / 1000 <= 255 => {
            #[allow(clippy::cast_possible_truncation)]
            let k = (r / 1000) as u8;
            (0b1100, vec![k])
        }
        r if r <= 65_535 => {
            #[allow(clippy::cast_possible_truncation)]
            let hz = r as u16;
            (0b1101, hz.to_be_bytes().to_vec())
        }
        r => {
            #[allow(clippy::cast_possible_truncation)]
            let tens = (r / 10).min(65_535) as u16;
            (0b1110, tens.to_be_bytes().to_vec())
        }
    }
}

/// A frame number as FLAC writes it (UTF-8's scheme, up to 36 bits).
fn utf8_number(n: u64) -> Vec<u8> {
    if n < 0x80 {
        #[allow(clippy::cast_possible_truncation)]
        return vec![n as u8];
    }
    let extra = match n {
        0..0x800 => 1,
        0x800..0x1_0000 => 2,
        0x1_0000..0x20_0000 => 3,
        0x20_0000..0x400_0000 => 4,
        0x400_0000..0x8000_0000 => 5,
        _ => 6,
    };
    let lead_mask: u8 = !(0xFF >> (extra + 1));
    let mut out = Vec::with_capacity(extra + 1);
    #[allow(clippy::cast_possible_truncation)]
    out.push(lead_mask | (n >> (6 * extra)) as u8);
    for i in (0..extra).rev() {
        #[allow(clippy::cast_possible_truncation)]
        out.push(0x80 | ((n >> (6 * i)) & 0x3F) as u8);
    }
    out
}

/// CRC-8, polynomial 0x07 (the frame header's).
fn crc8(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |mut crc, b| {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 == 0 {
                crc << 1
            } else {
                (crc << 1) ^ 0x07
            };
        }
        crc
    })
}

/// CRC-16, polynomial 0x8005 (the whole frame's).
fn crc16(bytes: &[u8]) -> u16 {
    bytes.iter().fold(0u16, |mut crc, b| {
        crc ^= u16::from(*b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 == 0 {
                crc << 1
            } else {
                (crc << 1) ^ 0x8005
            };
        }
        crc
    })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use symphonia_bundle_flac::{FlacDecoder, FlacReader};
    use symphonia_core::codecs::audio::{AudioDecoder as _, AudioDecoderOptions};
    use symphonia_core::formats::{FormatOptions, FormatReader as _, TrackType};
    use symphonia_core::io::{MediaSourceStream, MediaSourceStreamOptions};

    use super::*;

    /// What a real FLAC decoder reads back.
    fn decode(bytes: Vec<u8>) -> (u32, Vec<i16>) {
        let source = MediaSourceStream::new(
            Box::new(Cursor::new(bytes)),
            MediaSourceStreamOptions::default(),
        );
        let mut reader = FlacReader::try_new(source, FormatOptions::default()).unwrap();
        let params = reader
            .default_track(TrackType::Audio)
            .unwrap()
            .codec_params
            .as_ref()
            .unwrap()
            .audio()
            .unwrap()
            .clone();
        let rate = params.sample_rate.unwrap();
        let mut decoder = FlacDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap();
        let mut out = Vec::new();
        while let Some(packet) = reader.next_packet().unwrap() {
            let mut samples: Vec<i16> = Vec::new();
            decoder
                .decode(&packet)
                .unwrap()
                .copy_to_vec_interleaved(&mut samples);
            out.extend(samples);
        }
        (rate, out)
    }

    #[test]
    fn a_decoder_reads_back_every_sample() {
        // Three whole frames and a short one, a voice's range of values.
        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
        let samples: Vec<i16> = (0..3 * BLOCK + 500)
            .map(|i| ((i as f64 * 0.05).sin() * 20_000.0) as i16)
            .collect();
        assert_eq!(decode(encode(&samples, 24_000)), (24_000, samples.clone()));
        // Streamed in odd pieces: the same bytes.
        let mut w = Writer::new(24_000);
        let mut streamed = w.header();
        for piece in samples.chunks(333) {
            streamed.extend(w.push(piece));
        }
        streamed.extend(w.finish());
        assert_eq!(streamed, encode(&samples, 24_000));
        // A rate without a code of its own.
        assert_eq!(decode(encode(&samples[..100], 22_000)).0, 22_000);
    }

    #[test]
    fn numbers_and_checks_are_flac_s() {
        assert_eq!(utf8_number(0x7F), [0x7F]);
        assert_eq!(utf8_number(0x80), [0xC2, 0x80]);
        assert_eq!(utf8_number(0x800), [0xE0, 0xA0, 0x80]);
        // CRC-8/SMBUS and CRC-16/UMTS (BUYPASS) of "123456789".
        assert_eq!(crc8(b"123456789"), 0xF4);
        assert_eq!(crc16(b"123456789"), 0xFEE8);
    }
}
