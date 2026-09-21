//! Downmix, rate conversion and PCM packing.
//!
//! Both transcribers want the same thing — 16 kHz mono signed 16-bit — so the
//! whole chain lives here and the capture thread only ever hands over native
//! rate mono `f32`. The functions are pure so a synthetic sine can prove them
//! without a microphone.

use audioadapter_buffers::owned::InterleavedOwned;
use rubato::{Fft, FixedSync, Resampler};

use crate::error::VoiceError;

/// The rate every `Utterance` is delivered at.
pub const TARGET_RATE: u32 = 16_000;

/// One interleaved frame collapsed to mono.
///
/// This is the exact arithmetic the real-time callback runs per frame — it
/// takes an iterator so the callback can convert `i16`/`i32` samples on the
/// fly without a scratch buffer — so [`downmix`] tests it for free.
#[inline]
#[must_use]
pub(crate) fn mono_of(frame: impl IntoIterator<Item = f32>, channels: usize) -> f32 {
    match channels {
        0 | 1 => frame.into_iter().sum(),
        n => frame.into_iter().sum::<f32>() / n as f32,
    }
}

/// Convert mono `from` Hz audio to [`TARGET_RATE`].
///
/// Returns the input untouched when it is already at the target rate, which
/// is the common case for USB interfaces pinned to 16 kHz.
pub(crate) fn resample_to_target(samples: &[f32], from: u32) -> Result<Vec<f32>, VoiceError> {
    if from == TARGET_RATE {
        return Ok(samples.to_vec());
    }
    if samples.is_empty() {
        return Ok(Vec::new());
    }
    let error = |detail: String| VoiceError::Resample { from, detail };
    let input = InterleavedOwned::new_from(samples.to_vec(), 1, samples.len())
        .map_err(|e| error(e.to_string()))?;
    let mut resampler = Fft::<f32>::new(
        from as usize,
        TARGET_RATE as usize,
        1_024,
        1,
        FixedSync::Both,
    )
    .map_err(|e| error(e.to_string()))?;
    Ok(resampler
        .process_all(&input, samples.len(), None)
        .map_err(|e| error(e.to_string()))?
        .take_data())
}

/// Pack normalised floats as signed 16-bit PCM, clipping rather than wrapping.
#[must_use]
pub(crate) fn to_pcm16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|sample| (sample.clamp(-1.0, 1.0) * 32_767.0) as i16)
        .collect()
}

/// Unpack signed 16-bit PCM back to normalised floats.
///
/// Used by the on-device backend, which hands AVFoundation an `f32` buffer.
#[must_use]
pub(crate) fn from_pcm16(pcm16: &[i16]) -> Vec<f32> {
    pcm16
        .iter()
        .map(|sample| f32::from(*sample) / 32_768.0)
        .collect()
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]
    use super::*;
    use std::f32::consts::TAU;

    /// Collapse interleaved `channels`-channel audio to mono, frame by
    /// frame, exactly as the real-time capture callback does. A trailing
    /// partial frame is dropped: half a frame is not a sample.
    fn downmix(interleaved: &[f32], channels: u16) -> Vec<f32> {
        let channels = usize::from(channels).max(1);
        interleaved
            .chunks_exact(channels)
            .map(|frame| mono_of(frame.iter().copied(), channels))
            .collect()
    }

    /// Energy of `samples` at `freq` Hz, by the Goertzel algorithm.
    fn tone_power(samples: &[f32], rate: u32, freq: f32) -> f32 {
        let omega = TAU * freq / rate as f32;
        let coeff = 2.0 * omega.cos();
        let (mut s1, mut s2) = (0.0f32, 0.0f32);
        for sample in samples {
            let s0 = sample + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        (s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0).sqrt() / samples.len() as f32
    }

    fn sine(freq: f32, rate: u32, seconds: f32, amplitude: f32) -> Vec<f32> {
        let count = (rate as f32 * seconds) as usize;
        (0..count)
            .map(|n| amplitude * (TAU * freq * n as f32 / rate as f32).sin())
            .collect()
    }

    #[test]
    fn downmix_averages_channels_and_cancels_opposite_phase() {
        // L/R in antiphase sum to silence; identical channels keep their level.
        let antiphase = [1.0, -1.0, 0.5, -0.5, -0.25, 0.25];
        assert_eq!(downmix(&antiphase, 2), vec![0.0, 0.0, 0.0]);

        let identical = [0.4, 0.4, -0.8, -0.8];
        assert_eq!(downmix(&identical, 2), vec![0.4, -0.8]);

        // Four channels average; a trailing partial frame is dropped.
        let quad = [1.0, 0.0, 1.0, 0.0, 0.5, 0.5];
        assert_eq!(downmix(&quad, 4), vec![0.5]);

        // Mono passes through untouched.
        assert_eq!(downmix(&[0.1, -0.2], 1), vec![0.1, -0.2]);
    }

    #[test]
    fn resampling_48k_to_16k_keeps_the_tone_and_thirds_the_length() {
        let input = sine(1_000.0, 48_000, 1.0, 0.5);
        let output = resample_to_target(&input, 48_000).expect("resample");

        // One second in, one second out, to within a resampler chunk.
        let expected = TARGET_RATE as usize;
        assert!(
            output.len().abs_diff(expected) < 1_024,
            "expected ~{expected} samples, got {}",
            output.len()
        );

        // The 1 kHz tone survives at its original amplitude, and no energy
        // has folded down to a lower bin.
        let settled = output.get(2_000..14_000).unwrap_or_default();
        let at_1k = tone_power(settled, TARGET_RATE, 1_000.0);
        let at_300 = tone_power(settled, TARGET_RATE, 300.0);
        assert!((at_1k - 0.25).abs() < 0.03, "1 kHz power was {at_1k}");
        assert!(at_300 < 0.01, "300 Hz leakage was {at_300}");
    }

    #[test]
    fn resampling_drops_content_above_the_new_nyquist_instead_of_aliasing() {
        // 10 kHz cannot exist at 16 kHz; it must not reappear at 6 kHz.
        let input = sine(10_000.0, 48_000, 0.5, 0.5);
        let output = resample_to_target(&input, 48_000).expect("resample");
        let settled = output.get(2_000..6_000).unwrap_or_default();
        assert!(
            tone_power(settled, TARGET_RATE, 6_000.0) < 0.02,
            "10 kHz aliased into the 16 kHz band"
        );
    }

    #[test]
    fn matching_rate_is_a_passthrough() {
        let input = sine(440.0, TARGET_RATE, 0.1, 0.3);
        assert_eq!(resample_to_target(&input, TARGET_RATE).expect("resample"), input);
    }

    #[test]
    fn pcm16_clips_rather_than_wrapping() {
        let packed = to_pcm16(&[0.0, 1.0, -1.0, 2.5, -2.5, 0.5]);
        assert_eq!(packed, vec![0, 32_767, -32_767, 32_767, -32_767, 16_383]);
    }

    #[test]
    fn pcm16_round_trips_within_one_step() {
        let input = sine(440.0, TARGET_RATE, 0.05, 0.9);
        let round_tripped = from_pcm16(&to_pcm16(&input));
        for (before, after) in input.iter().zip(&round_tripped) {
            assert!((before - after).abs() < 1.0 / 16_384.0);
        }
    }
}
