//! In-memory WAV encoding.
//!
//! The utterance becomes a RIFF file only to satisfy the multipart upload,
//! and it never touches the filesystem: the plan's privacy stance is that
//! audio is not written to disk.

use std::io::Cursor;

use crate::error::VoiceError;

/// Encode 16-bit mono PCM as a WAV file in memory.
pub(crate) fn encode(pcm16: &[i16], sample_rate: u32) -> Result<Vec<u8>, VoiceError> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let encode = |detail: String| VoiceError::Encode { detail };
    // 44-byte header plus the samples: exactly one allocation.
    let mut cursor = Cursor::new(Vec::with_capacity(44 + pcm16.len() * 2));
    let mut writer = hound::WavWriter::new(&mut cursor, spec).map_err(|e| encode(e.to_string()))?;
    for sample in pcm16 {
        writer
            .write_sample(*sample)
            .map_err(|e| encode(e.to_string()))?;
    }
    writer.finalize().map_err(|e| encode(e.to_string()))?;
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]
    use super::*;

    #[test]
    fn the_bytes_are_a_16k_mono_riff_wave_the_provider_will_accept() {
        let pcm = vec![0i16, 1_000, -1_000, 32_767];
        let bytes = encode(&pcm, 16_000).expect("encode");

        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        // fmt chunk: 1 = PCM, 1 channel, 16 000 Hz, 16 bits.
        assert_eq!(u16::from_le_bytes([bytes[20], bytes[21]]), 1);
        assert_eq!(u16::from_le_bytes([bytes[22], bytes[23]]), 1);
        assert_eq!(
            u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]),
            16_000
        );
        assert_eq!(u16::from_le_bytes([bytes[34], bytes[35]]), 16);
        // Header plus two bytes per sample, and the RIFF size agrees.
        assert_eq!(bytes.len(), 44 + pcm.len() * 2);
        assert_eq!(
            u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize,
            bytes.len() - 8
        );
    }

    #[test]
    fn samples_survive_the_round_trip_little_endian() {
        let bytes = encode(&[-1, 256], 16_000).expect("encode");
        assert_eq!(&bytes[44..48], &[0xFF, 0xFF, 0x00, 0x01]);
    }
}
