//! Push-to-talk microphone capture.
//!
//! [`Microphone`] is a handle, not a stream: `cpal`'s `Stream` is neither
//! `Send` nor `Clone` and must be dropped on the thread that built it, so
//! every recording owns a short-lived capture thread that opens the device,
//! drains a lock-free ring and hands back the samples. The handle itself is
//! `Send`, which is what lets the TUI keep one in its app state.
//!
//! The real-time callback does no allocation, no locking and no logging: it
//! collapses each interleaved frame to mono ([`resample::mono`]) and pushes it
//! into an [`rtrb`] ring. Everything else — rate conversion, PCM packing —
//! happens once, in [`Microphone::stop`].
//!
//! Dropping the stream at the end of a recording is deliberate: macOS turns
//! the orange microphone indicator off only when nothing holds the device, so
//! "not recording" is visibly true.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, FromSample, Sample, SampleFormat, SizedSample, StreamConfig};

use crate::error::VoiceError;
use crate::resample::{self, TARGET_RATE};

/// The longest single utterance push-to-talk will keep.
///
/// Past this the recording stops growing and [`Microphone::stop`] reports
/// [`VoiceError::TooLong`] rather than letting a stuck key eat memory.
pub const MAX_UTTERANCE: Duration = Duration::from_secs(120);

/// How much audio the ring holds before the callback has to drop a sample.
const RING_SECONDS: usize = 2;

/// How often the capture thread drains the ring.
const DRAIN_INTERVAL: Duration = Duration::from_millis(5);

/// Peak decay per drain tick, so the meter falls in ~200 ms.
const LEVEL_DECAY: f32 = 0.85;

/// How long `stop` waits for the capture thread to hand the samples over.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// One audio input, as offered to the device picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Backend identifier, stable across runs; what [`Microphone::open`] wants.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Whether this is the system default input.
    pub is_default: bool,
}

/// A finished push-to-talk recording: 16 kHz mono signed 16-bit PCM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Utterance {
    /// The samples, mono, at [`Utterance::sample_rate`].
    pub pcm16: Vec<i16>,
    /// Always [`resample::TARGET_RATE`]; carried so callers need not assume.
    pub sample_rate: u32,
    /// Wall length of the speech, derived from the sample count.
    pub duration: Duration,
}

impl Utterance {
    /// Build an utterance from native-rate mono audio.
    pub(crate) fn from_mono(samples: &[f32], sample_rate: u32) -> Result<Self, VoiceError> {
        let resampled = resample::resample_to_target(samples, sample_rate)?;
        let pcm16 = resample::to_pcm16(&resampled);
        if pcm16.is_empty() {
            return Err(VoiceError::NoAudio);
        }
        Ok(Self {
            duration: Duration::from_secs_f64(pcm16.len() as f64 / f64::from(TARGET_RATE)),
            pcm16,
            sample_rate: TARGET_RATE,
        })
    }
}

/// Bounded sample accumulator: the 120 s cap, without a microphone attached.
struct Accumulator {
    samples: Vec<f32>,
    max: usize,
    overflowed: bool,
}

impl Accumulator {
    fn new(sample_rate: u32, limit: Duration) -> Self {
        let max = (limit.as_secs_f64() * f64::from(sample_rate)) as usize;
        Self {
            samples: Vec::with_capacity(max.min(sample_rate as usize * 8)),
            max,
            overflowed: false,
        }
    }

    /// Accept one sample. Returns `false` once the cap is reached, after
    /// which nothing more is stored.
    fn push(&mut self, sample: f32) -> bool {
        if self.samples.len() >= self.max {
            self.overflowed = true;
            return false;
        }
        self.samples.push(sample);
        true
    }
}

/// What the capture thread hands back when the key is released.
struct Captured {
    samples: Vec<f32>,
    sample_rate: u32,
    overflowed: bool,
    overflows: u64,
}

/// A live recording: the thread, its stop flag and its shared level.
struct Session {
    stop: Arc<AtomicBool>,
    level: Arc<AtomicU32>,
    done: Receiver<Result<Captured, VoiceError>>,
    thread: JoinHandle<()>,
}

/// One audio input, opened on demand.
///
/// ```no_run
/// # fn demo() -> Result<(), neo_voice::VoiceError> {
/// let mut mic = neo_voice::Microphone::open(None)?;
/// mic.start()?;
/// // …hold the key…
/// let utterance = mic.stop()?;
/// # let _ = utterance; Ok(()) }
/// ```
pub struct Microphone {
    /// `None` means "follow the system default input".
    device_id: Option<String>,
    name: String,
    session: Option<Session>,
}

impl Microphone {
    /// Every input the host offers, default first in the flag, not the order.
    pub fn devices() -> Result<Vec<DeviceInfo>, VoiceError> {
        let host = cpal::default_host();
        let default_id = host
            .default_input_device()
            .and_then(|device| device.id().ok())
            .map(|id| id.id().to_owned());
        let devices = host
            .input_devices()
            .map_err(|e| VoiceError::Device { call: "input_devices", detail: e.to_string() })?;
        Ok(devices
            .filter_map(|device| {
                let id = device.id().ok()?.id().to_owned();
                Some(DeviceInfo {
                    is_default: Some(&id) == default_id.as_ref(),
                    name: device.to_string(),
                    id,
                })
            })
            .collect())
    }

    /// Open the default input, or the one whose id or name is `device`.
    ///
    /// This resolves and validates the device but holds nothing open: the
    /// orange indicator stays off until [`start`](Self::start).
    pub fn open(device: Option<&str>) -> Result<Self, VoiceError> {
        let host = cpal::default_host();
        match device {
            None => {
                let device = host.default_input_device().ok_or(VoiceError::NoInputDevice)?;
                Ok(Self {
                    device_id: None,
                    name: device.to_string(),
                    session: None,
                })
            }
            Some(wanted) => {
                let inputs = Self::devices()?;
                let found = inputs
                    .iter()
                    .find(|info| info.id == wanted || info.name == wanted);
                match found {
                    Some(info) => Ok(Self {
                        device_id: Some(info.id.clone()),
                        name: info.name.clone(),
                        session: None,
                    }),
                    None => Err(VoiceError::UnknownDevice {
                        requested: wanted.to_owned(),
                        available: inputs.into_iter().map(|info| info.name).collect(),
                    }),
                }
            }
        }
    }

    /// The name of the device this handle records from.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether a recording is in progress.
    #[must_use]
    pub fn is_recording(&self) -> bool {
        self.session.is_some()
    }

    /// Open the device and start recording. Push-to-talk key-down.
    ///
    /// Returns once the stream is actually running, so a device or
    /// permission failure surfaces here and not at [`stop`](Self::stop).
    pub fn start(&mut self) -> Result<(), VoiceError> {
        if self.session.is_some() {
            return Err(VoiceError::AlreadyRecording);
        }
        crate::permission::ensure_microphone()?;

        let stop = Arc::new(AtomicBool::new(false));
        let level = Arc::new(AtomicU32::new(0));
        let (ready_tx, ready_rx) = sync_channel::<Result<(), VoiceError>>(1);
        let (done_tx, done_rx) = sync_channel::<Result<Captured, VoiceError>>(1);

        let device_id = self.device_id.clone();
        let thread_stop = Arc::clone(&stop);
        let thread_level = Arc::clone(&level);
        let thread = std::thread::Builder::new()
            .name("neo-voice-capture".into())
            .spawn(move || {
                let outcome = capture(device_id, &thread_stop, &thread_level, &ready_tx);
                // The receiver is gone only if the caller dropped the
                // microphone mid-recording; the samples die with it.
                let _ = done_tx.send(outcome);
            })
            .map_err(|e| VoiceError::CaptureThread { detail: e.to_string() })?;

        match ready_rx.recv() {
            Ok(Ok(())) => {
                self.session = Some(Session { stop, level, done: done_rx, thread });
                Ok(())
            }
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                let _ = thread.join();
                Err(VoiceError::CaptureThread {
                    detail: "the thread exited before the stream was running".into(),
                })
            }
        }
    }

    /// Stop recording and return the utterance. Push-to-talk key-up.
    ///
    /// The device is closed before this returns, whatever the outcome.
    pub fn stop(&mut self) -> Result<Utterance, VoiceError> {
        let session = self.session.take().ok_or(VoiceError::NotRecording)?;
        session.stop.store(true, Ordering::Relaxed);
        let captured = match session.done.recv_timeout(STOP_TIMEOUT) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(VoiceError::CaptureThread {
                detail: "the thread did not release the device within 5s".into(),
            }),
            Err(RecvTimeoutError::Disconnected) => Err(VoiceError::CaptureThread {
                detail: "the thread panicked".into(),
            }),
        };
        let _ = session.thread.join();

        let captured = captured?;
        if captured.overflows > 0 {
            tracing::warn!(
                dropped = captured.overflows,
                "the capture ring overflowed; samples were dropped"
            );
        }
        if captured.overflowed {
            return Err(VoiceError::TooLong { limit: MAX_UTTERANCE });
        }
        Utterance::from_mono(&captured.samples, captured.sample_rate)
    }

    /// Peak level of the last few milliseconds, `0.0..=1.0`.
    ///
    /// One relaxed atomic load: safe to call every frame from a render loop.
    /// Reads `0.0` when nothing is recording.
    #[must_use]
    pub fn level(&self) -> f32 {
        match &self.session {
            Some(session) => f32::from_bits(session.level.load(Ordering::Relaxed)),
            None => 0.0,
        }
    }
}

impl Drop for Microphone {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            session.stop.store(true, Ordering::Relaxed);
            let _ = session.thread.join();
        }
    }
}

/// The capture thread body: own the device, drain the ring, report once.
fn capture(
    device_id: Option<String>,
    stop: &AtomicBool,
    level: &AtomicU32,
    ready: &SyncSender<Result<(), VoiceError>>,
) -> Result<Captured, VoiceError> {
    let started = match open_stream(device_id.as_deref()) {
        Ok(started) => {
            let _ = ready.send(Ok(()));
            started
        }
        Err(error) => {
            let _ = ready.send(Err(error.clone()));
            return Err(error);
        }
    };
    let Started { stream, mut consumer, sample_rate, overflows } = started;

    let mut accumulator = Accumulator::new(sample_rate, MAX_UTTERANCE);
    let mut peak = 0.0f32;
    loop {
        let finishing = stop.load(Ordering::Relaxed);
        let mut batch_peak = 0.0f32;
        while let Ok(sample) = consumer.pop() {
            batch_peak = batch_peak.max(sample.abs());
            if !accumulator.push(sample) {
                break;
            }
        }
        peak = batch_peak.max(peak * LEVEL_DECAY);
        level.store(peak.min(1.0).to_bits(), Ordering::Relaxed);
        if finishing || accumulator.overflowed {
            break;
        }
        std::thread::sleep(DRAIN_INTERVAL);
    }

    // Drop the stream before returning: the orange indicator goes out here.
    drop(stream);
    level.store(0.0f32.to_bits(), Ordering::Relaxed);

    Ok(Captured {
        samples: accumulator.samples,
        sample_rate,
        overflowed: accumulator.overflowed,
        overflows: overflows.load(Ordering::Relaxed),
    })
}

struct Started {
    stream: cpal::Stream,
    consumer: rtrb::Consumer<f32>,
    sample_rate: u32,
    overflows: Arc<AtomicU64>,
}

fn open_stream(device_id: Option<&str>) -> Result<Started, VoiceError> {
    let host = cpal::default_host();
    let device = match device_id {
        None => host.default_input_device().ok_or(VoiceError::NoInputDevice)?,
        Some(wanted) => host
            .input_devices()
            .map_err(|e| VoiceError::Device { call: "input_devices", detail: e.to_string() })?
            .find(|device| device.id().ok().is_some_and(|id| id.id() == wanted))
            .ok_or_else(|| VoiceError::UnknownDevice {
                requested: wanted.to_owned(),
                available: Vec::new(),
            })?,
    };
    let supported = device.default_input_config().map_err(|e| VoiceError::Device {
        call: "default_input_config",
        detail: e.to_string(),
    })?;
    let sample_rate = supported.sample_rate();
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();

    let (producer, consumer) = rtrb::RingBuffer::<f32>::new(sample_rate as usize * RING_SECONDS);
    let overflows = Arc::new(AtomicU64::new(0));

    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, &config, producer, &overflows),
        SampleFormat::I16 => build::<i16>(&device, &config, producer, &overflows),
        SampleFormat::I32 => build::<i32>(&device, &config, producer, &overflows),
        SampleFormat::U16 => build::<u16>(&device, &config, producer, &overflows),
        other => Err(VoiceError::UnsupportedFormat { format: other.to_string() }),
    }?;
    stream
        .play()
        .map_err(|e| VoiceError::Device { call: "stream.play", detail: e.to_string() })?;

    Ok(Started { stream, consumer, sample_rate, overflows })
}

fn build<T>(
    device: &Device,
    config: &StreamConfig,
    mut producer: rtrb::Producer<f32>,
    overflows: &Arc<AtomicU64>,
) -> Result<cpal::Stream, VoiceError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels).max(1);
    let overflows = Arc::clone(overflows);
    device
        .build_input_stream(
            *config,
            move |data: &[T], _| {
                // Real-time thread: no alloc, no lock, no log.
                for chunk in data.chunks_exact(channels) {
                    let sample =
                        resample::mono_of(chunk.iter().map(|s| f32::from_sample(*s)), channels);
                    if producer.push(sample).is_err() {
                        overflows.fetch_add(1, Ordering::Relaxed);
                    }
                }
            },
            move |error| {
                tracing::warn!(%error, "input stream error");
            },
            None,
        )
        .map_err(|e| VoiceError::Device { call: "build_input_stream", detail: e.to_string() })
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn the_accumulator_stops_at_the_cap_and_says_so() {
        // One second of headroom at a rate that makes the maths obvious.
        let mut accumulator = Accumulator::new(1_000, Duration::from_secs(1));
        for n in 0..1_000 {
            assert!(accumulator.push(n as f32), "sample {n} was refused early");
        }
        assert!(!accumulator.overflowed);

        assert!(!accumulator.push(1.0), "the cap did not hold");
        assert!(accumulator.overflowed);
        assert_eq!(accumulator.samples.len(), 1_000, "it kept growing past the cap");

        // Still refusing, still not growing.
        for _ in 0..10_000 {
            assert!(!accumulator.push(1.0));
        }
        assert_eq!(accumulator.samples.len(), 1_000);
    }

    #[test]
    fn the_cap_is_two_minutes_of_the_devices_own_rate() {
        let accumulator = Accumulator::new(48_000, MAX_UTTERANCE);
        assert_eq!(accumulator.max, 48_000 * 120);
    }

    #[test]
    fn an_utterance_is_16k_mono_with_a_duration_that_matches_its_samples() {
        let samples = vec![0.25f32; 48_000 * 3];
        let utterance = Utterance::from_mono(&samples, 48_000).expect("utterance");
        assert_eq!(utterance.sample_rate, TARGET_RATE);
        assert!(
            utterance.pcm16.len().abs_diff(16_000 * 3) < 1_024,
            "got {} samples",
            utterance.pcm16.len()
        );
        let seconds = utterance.duration.as_secs_f64();
        assert!((seconds - 3.0).abs() < 0.1, "duration was {seconds}s");
    }

    #[test]
    fn an_empty_recording_is_an_error_not_an_empty_utterance() {
        assert_eq!(Utterance::from_mono(&[], 48_000), Err(VoiceError::NoAudio));
    }

    #[test]
    fn a_handle_can_move_between_threads() {
        fn assert_send<T: Send>() {}
        assert_send::<Microphone>();
        assert_send::<Utterance>();
    }

    #[test]
    #[ignore = "needs a real microphone and Privacy › Microphone granted"]
    fn records_three_seconds_from_the_real_microphone() {
        let mut mic = Microphone::open(None).expect("open the default input");
        eprintln!("recording 3s from `{}` — speak now", mic.name());
        mic.start().expect("start");
        let mut peak = 0.0f32;
        for _ in 0..60 {
            std::thread::sleep(Duration::from_millis(50));
            peak = peak.max(mic.level());
        }
        let utterance = mic.stop().expect("stop");
        eprintln!(
            "captured {} samples ({:.2}s) at {} Hz, peak level {peak:.3}",
            utterance.pcm16.len(),
            utterance.duration.as_secs_f32(),
            utterance.sample_rate
        );
        assert_eq!(utterance.sample_rate, TARGET_RATE);
        assert!(utterance.duration.as_secs_f32() > 2.5);
        assert!(peak > 0.0, "the level meter never moved: is the mic muted?");
    }

    #[test]
    #[ignore = "needs a real microphone"]
    fn lists_the_real_input_devices() {
        let devices = Microphone::devices().expect("enumerate");
        for device in &devices {
            eprintln!("{}{} [{}]", if device.is_default { "* " } else { "  " }, device.name, device.id);
        }
        assert!(!devices.is_empty());
    }
}
