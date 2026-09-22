//! Optional, bounded FFT analysis on the capture thread, never the audio callback.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

/// Number of low-to-high, logarithmically spaced microphone frequency bands.
pub const SPECTRUM_BINS: usize = 32;
const FFT_SIZE: usize = 1024;
const INTERVAL: Duration = Duration::from_millis(40);

#[derive(Default)]
pub(crate) struct Spectrum {
    enabled: AtomicBool,
    bins: [AtomicU32; SPECTRUM_BINS],
}

impl Spectrum {
    pub(crate) fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub(crate) fn read(&self) -> [f32; SPECTRUM_BINS] {
        self.enabled.store(true, Ordering::Relaxed);
        std::array::from_fn(|i| f32::from_bits(self.bins[i].load(Ordering::Relaxed)))
    }

    pub(crate) fn clear(&self) {
        for bin in &self.bins {
            bin.store(0, Ordering::Relaxed);
        }
    }
}

pub(crate) struct Analyzer {
    fft: Arc<dyn Fft<f32>>,
    buffer: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    window: [f32; FFT_SIZE],
    bands: [(usize, usize); SPECTRUM_BINS],
    last: Instant,
    samples_seen: usize,
}

impl Analyzer {
    pub(crate) fn new(sample_rate: u32) -> Self {
        let fft = FftPlanner::new().plan_fft_forward(FFT_SIZE);
        let scratch = vec![Complex::default(); fft.get_inplace_scratch_len()];
        let resolution = sample_rate as f32 / FFT_SIZE as f32;
        let low = resolution.max(60.0);
        let high = (sample_rate as f32 / 2.0).min(8000.0).max(low);
        let edge = |i: usize| {
            ((low * (high / low).powf(i as f32 / SPECTRUM_BINS as f32) / resolution) as usize)
                .clamp(1, FFT_SIZE / 2)
        };
        Self {
            fft,
            buffer: vec![Complex::default(); FFT_SIZE],
            scratch,
            window: std::array::from_fn(|i| {
                0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / FFT_SIZE as f32).cos()
            }),
            bands: std::array::from_fn(|i| (edge(i), edge(i + 1).max(edge(i) + 1))),
            last: Instant::now() - INTERVAL,
            samples_seen: 0,
        }
    }

    pub(crate) fn update(&mut self, samples: &[f32], spectrum: &Spectrum) {
        if samples.len() < FFT_SIZE || self.last.elapsed() < INTERVAL {
            return;
        }
        self.last = Instant::now();
        if samples.len() == self.samples_seen {
            // A stalled input must not display a frozen, apparently live frame.
            spectrum.clear();
            return;
        }
        self.samples_seen = samples.len();
        let recent = &samples[samples.len() - FFT_SIZE..];
        for (i, sample) in recent.iter().enumerate() {
            self.buffer[i] = Complex::new(sample * self.window[i], 0.0);
        }
        self.fft
            .process_with_scratch(&mut self.buffer, &mut self.scratch);
        for (i, &(start, end)) in self.bands.iter().enumerate() {
            let power = self.buffer[start..end]
                .iter()
                .map(Complex::norm_sqr)
                .fold(0.0f32, f32::max);
            // Hann coherent gain is 1/2; account for the omitted negative half.
            let amplitude = power.sqrt() * 4.0 / FFT_SIZE as f32;
            let normalized = ((20.0 * amplitude.max(1e-6).log10() + 60.0) / 60.0).clamp(0.0, 1.0);
            spectrum.bins[i].store(normalized.to_bits(), Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Analyzer, SPECTRUM_BINS, Spectrum};

    #[test]
    fn spectrum_distinguishes_silence_from_a_one_kilohertz_tone() {
        let silence = Spectrum::default();
        Analyzer::new(16_000).update(&[0.0; 2048], &silence);
        assert_eq!(silence.read(), [0.0; SPECTRUM_BINS]);

        let samples: Vec<f32> = (0..2048)
            .map(|i| (std::f32::consts::TAU * 1000.0 * i as f32 / 16_000.0).sin())
            .collect();
        let tone = Spectrum::default();
        Analyzer::new(16_000).update(&samples, &tone);
        let bins = tone.read();
        assert!(
            bins.iter()
                .all(|bin| bin.is_finite() && (0.0..=1.0).contains(bin))
        );
        // Band 18 spans approximately 941–1097 Hz at this sample rate.
        assert!(
            bins[18] > 0.95,
            "a full-scale 1 kHz tone must occupy its frequency band"
        );
        assert!(
            bins.iter()
                .enumerate()
                .all(|(i, bin)| i == 18 || *bin < 0.05),
            "frequency analysis must not paint the tone across unrelated bands: {bins:?}"
        );
    }
}
