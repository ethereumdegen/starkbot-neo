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

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, FromSample, Sample, SampleFormat, SizedSample, StreamConfig};

use crate::error::VoiceError;
use crate::resample::{self, TARGET_RATE};
use crate::spectrum::{Analyzer, SPECTRUM_BINS, Spectrum};

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

/// How long [`Microphone::drop`] waits for the device to be released.
///
/// Much shorter than [`STOP_TIMEOUT`], because nobody is waiting for the
/// samples: the only reason to wait at all is that macOS keeps the orange
/// microphone indicator lit until the stream is dropped. A quit path that
/// blocks is worse than an indicator that lingers a moment.
const DROP_TIMEOUT: Duration = Duration::from_millis(500);

/// What cpal's error callback leaves behind when the host tears the stream
/// down mid-recording — a device unplugged with the key still held, which is
/// the realistic way this fires.
///
/// A `Mutex<Option<String>>` and not an atomic flag because the host's own
/// message is the only thing that says *which* device went away, and it is
/// what [`VoiceError::Device`] carries. Legal here in a way it would not be
/// two functions down: this is the error callback, not the realtime one. It
/// fires on cpal's thread, at most a handful of times, and nothing in the
/// audio path ever waits on this lock.
type StreamError = Arc<Mutex<Option<String>>>;

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
    spectrum: Arc<Spectrum>,
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
        let devices = host.input_devices().map_err(|e| VoiceError::Device {
            call: "input_devices",
            detail: e.to_string(),
        })?;
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
                let device = host
                    .default_input_device()
                    .ok_or(VoiceError::NoInputDevice)?;
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
        let spectrum = Arc::new(Spectrum::default());
        let (ready_tx, ready_rx) = sync_channel::<Result<(), VoiceError>>(1);
        let (done_tx, done_rx) = sync_channel::<Result<Captured, VoiceError>>(1);

        let device_id = self.device_id.clone();
        let thread_stop = Arc::clone(&stop);
        let thread_level = Arc::clone(&level);
        let thread_spectrum = Arc::clone(&spectrum);
        let thread = std::thread::Builder::new()
            .name("neo-voice-capture".into())
            .spawn(move || {
                let outcome = capture(
                    device_id,
                    &thread_stop,
                    &thread_level,
                    &thread_spectrum,
                    &ready_tx,
                );
                // The receiver is gone only if the caller dropped the
                // microphone mid-recording; the samples die with it.
                let _ = done_tx.send(outcome);
            })
            .map_err(|e| VoiceError::CaptureThread {
                detail: e.to_string(),
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => {
                self.session = Some(Session {
                    stop,
                    level,
                    spectrum,
                    done: done_rx,
                    thread,
                });
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
    /// The device is closed before this returns whenever the capture thread
    /// is still answering, and this returns either way. That distinction is
    /// the fix for a real wedge: the timeout below used to be followed by an
    /// unconditional `join()`, so a thread stuck inside a CoreAudio call for
    /// a device that had just vanished blocked the caller *forever*, after
    /// the code above had already decided it had timed out. The caller is
    /// the TUI's event loop.
    pub fn stop(&mut self) -> Result<Utterance, VoiceError> {
        let session = self.session.take().ok_or(VoiceError::NotRecording)?;
        session.stop.store(true, Ordering::Relaxed);
        let captured = match session.done.recv_timeout(STOP_TIMEOUT) {
            // Reporting on `done` is the thread's last statement, so all
            // that is left to join on is its own teardown — and the report
            // comes after the stream has been dropped, which is what makes
            // "the device is closed before this returns" true.
            Ok(result) => {
                let _ = session.thread.join();
                result
            }
            // Detached on purpose, and the one case where the device may
            // still be open when this returns. The thread keeps its stop
            // flag and releases the device if the host call ever comes back;
            // dropping the handle is what detaches it.
            Err(RecvTimeoutError::Timeout) => {
                drop(session.thread);
                Err(VoiceError::CaptureThread {
                    detail: format!(
                        "the thread did not release the device within {}s",
                        STOP_TIMEOUT.as_secs()
                    ),
                })
            }
            // The sender was dropped without a report, so the thread has
            // already panicked: this join cannot block.
            Err(RecvTimeoutError::Disconnected) => {
                let _ = session.thread.join();
                Err(VoiceError::CaptureThread {
                    detail: "the thread panicked".into(),
                })
            }
        };

        let captured = captured?;
        if captured.overflows > 0 {
            tracing::warn!(
                dropped = captured.overflows,
                "the capture ring overflowed; samples were dropped"
            );
        }
        if captured.overflowed {
            return Err(VoiceError::TooLong {
                limit: MAX_UTTERANCE,
            });
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

    /// Whether the capture thread has ended (device loss or the duration cap).
    ///
    /// Call [`Self::stop`] to retrieve the recording or its terminal error.
    #[must_use]
    pub fn capture_finished(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.thread.is_finished())
    }

    /// Log-spaced FFT magnitudes from actual input, normalized to `0.0..=1.0`.
    ///
    /// The first read enables analysis on the capture thread; clients using
    /// only the peak meter pay no FFT cost. At most 25 frames are computed
    /// per second. Silence and a stopped microphone read as zero.
    #[must_use]
    pub fn spectrum(&self) -> [f32; SPECTRUM_BINS] {
        self.session
            .as_ref()
            .map_or([0.0; SPECTRUM_BINS], |session| session.spectrum.read())
    }
}

impl Drop for Microphone {
    /// Ask the thread to stop and give it [`DROP_TIMEOUT`] to say it has.
    ///
    /// Bounded for the same reason as [`Microphone::stop`], and more
    /// tightly: `drop` runs on whatever thread let the handle go — for the
    /// TUI, its event loop — and a wedged capture thread used to block it
    /// here with no deadline at all. Waiting for the report rather than
    /// joining blind is what makes the wait bounded: the report is the
    /// thread's last statement, so a thread that has sent it is a thread
    /// whose `join` returns immediately.
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            session.stop.store(true, Ordering::Relaxed);
            match session.done.recv_timeout(DROP_TIMEOUT) {
                // Either the samples arrived or the thread panicked; in both
                // cases it is done and this join returns at once.
                Ok(_) | Err(RecvTimeoutError::Disconnected) => {
                    let _ = session.thread.join();
                }
                Err(RecvTimeoutError::Timeout) => {
                    tracing::warn!(
                        "the capture thread did not release the microphone in time; detaching it"
                    );
                    drop(session.thread);
                }
            }
        }
    }
}

/// The capture thread body: own the device, drain the ring, report once.
fn capture(
    device_id: Option<String>,
    stop: &AtomicBool,
    level: &AtomicU32,
    spectrum: &Spectrum,
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
    let Started {
        stream,
        mut consumer,
        sample_rate,
        overflows,
        stream_error,
    } = started;

    let mut accumulator = Accumulator::new(sample_rate, MAX_UTTERANCE);
    let mut peak = 0.0f32;
    let mut analyzer = None;
    let failed = loop {
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
        if spectrum.enabled() {
            analyzer
                .get_or_insert_with(|| Analyzer::new(sample_rate))
                .update(&accumulator.samples, spectrum);
        }
        // Checked every tick rather than only at the end: once the host has
        // torn the stream down no more samples are coming, so draining on is
        // a busy loop over a dead device.
        if let Some(detail) = take_stream_error(&stream_error) {
            break Some(detail);
        }
        if finishing || accumulator.overflowed {
            break None;
        }
        std::thread::sleep(DRAIN_INTERVAL);
    };

    // Drop the stream before returning: the orange indicator goes out here.
    drop(stream);
    level.store(0.0f32.to_bits(), Ordering::Relaxed);
    spectrum.clear();

    // Read once more after the stream is gone, for the unplug that lands in
    // the same tick the key was released: without this the caller gets an
    // `Ok` utterance holding whatever prefix arrived, which is a confidently
    // truncated transcript and the worst of the available outcomes.
    if let Some(detail) = failed.or_else(|| take_stream_error(&stream_error)) {
        return Err(VoiceError::Device {
            call: "input_stream",
            detail,
        });
    }

    Ok(Captured {
        samples: accumulator.samples,
        sample_rate,
        overflowed: accumulator.overflowed,
        overflows: overflows.load(Ordering::Relaxed),
    })
}

/// The host's message, if the stream failed. A poisoned lock means the
/// callback panicked while holding it, which is itself a stream that cannot
/// be trusted, so it reads as a failure rather than as silence.
fn take_stream_error(stream_error: &StreamError) -> Option<String> {
    match stream_error.lock() {
        Ok(mut held) => held.take(),
        Err(_) => Some("the stream error callback panicked".to_owned()),
    }
}

struct Started {
    stream: cpal::Stream,
    consumer: rtrb::Consumer<f32>,
    sample_rate: u32,
    overflows: Arc<AtomicU64>,
    stream_error: StreamError,
}

fn open_stream(device_id: Option<&str>) -> Result<Started, VoiceError> {
    let host = cpal::default_host();
    let device = match device_id {
        None => host
            .default_input_device()
            .ok_or(VoiceError::NoInputDevice)?,
        Some(wanted) => host
            .input_devices()
            .map_err(|e| VoiceError::Device {
                call: "input_devices",
                detail: e.to_string(),
            })?
            .find(|device| device.id().ok().is_some_and(|id| id.id() == wanted))
            .ok_or_else(|| VoiceError::UnknownDevice {
                requested: wanted.to_owned(),
                available: Vec::new(),
            })?,
    };
    let supported = device
        .default_input_config()
        .map_err(|e| VoiceError::Device {
            call: "default_input_config",
            detail: e.to_string(),
        })?;
    let sample_rate = supported.sample_rate();
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();

    let (producer, consumer) = rtrb::RingBuffer::<f32>::new(sample_rate as usize * RING_SECONDS);
    let overflows = Arc::new(AtomicU64::new(0));
    let stream_error: StreamError = Arc::new(Mutex::new(None));

    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, &config, producer, &overflows, &stream_error),
        SampleFormat::I16 => build::<i16>(&device, &config, producer, &overflows, &stream_error),
        SampleFormat::I32 => build::<i32>(&device, &config, producer, &overflows, &stream_error),
        SampleFormat::U16 => build::<u16>(&device, &config, producer, &overflows, &stream_error),
        other => Err(VoiceError::UnsupportedFormat {
            format: other.to_string(),
        }),
    }?;
    stream.play().map_err(|e| VoiceError::Device {
        call: "stream.play",
        detail: e.to_string(),
    })?;

    Ok(Started {
        stream,
        consumer,
        sample_rate,
        overflows,
        stream_error,
    })
}

fn build<T>(
    device: &Device,
    config: &StreamConfig,
    mut producer: rtrb::Producer<f32>,
    overflows: &Arc<AtomicU64>,
    stream_error: &StreamError,
) -> Result<cpal::Stream, VoiceError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels).max(1);
    let overflows = Arc::clone(overflows);
    let stream_error = Arc::clone(stream_error);
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
            // cpal's *error* callback, not the realtime one: allocating and
            // taking a lock here is fine, and warning and moving on is not.
            // A stream the host has torn down delivers no more samples, and
            // a `stop` that reported `Ok` on the prefix that did arrive was
            // a confidently truncated transcript — the user dictates a
            // sentence, sees half of it, and has no way to know why.
            move |error| {
                let detail = error.to_string();
                tracing::warn!(%error, "the input stream failed; the recording will be refused");
                if let Ok(mut held) = stream_error.lock()
                    && held.is_none()
                {
                    // First report wins: the useful one is the failure that
                    // ended the stream, not whatever followed it.
                    *held = Some(detail);
                }
            },
            None,
        )
        .map_err(|e| VoiceError::Device {
            call: "build_input_stream",
            detail: e.to_string(),
        })
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
        assert_eq!(
            accumulator.samples.len(),
            1_000,
            "it kept growing past the cap"
        );

        // Still refusing, still not growing.
        for _ in 0..10_000 {
            assert!(!accumulator.push(1.0));
        }
        assert_eq!(accumulator.samples.len(), 1_000);
    }

    /// A stream failure that does not reach `stop` is a truncated
    /// transcript the user is given no reason to doubt, so the one path
    /// that could swallow it — a callback that panicked while holding the
    /// lock — reports a failure rather than silence.
    #[test]
    fn a_stream_failure_survives_a_poisoned_lock_rather_than_reading_as_silence() {
        let reported: StreamError = Arc::new(Mutex::new(Some("device disappeared".to_owned())));
        assert_eq!(
            take_stream_error(&reported).as_deref(),
            Some("device disappeared")
        );
        assert_eq!(
            take_stream_error(&reported),
            None,
            "the report was not consumed"
        );

        let poisoned: StreamError = Arc::new(Mutex::new(None));
        let held = Arc::clone(&poisoned);
        let panicked = std::thread::spawn(move || {
            let _guard = held.lock().expect("an uncontended lock");
            panic!("the callback panicked with the lock held");
        })
        .join();
        assert!(panicked.is_err(), "the thread was supposed to panic");
        assert!(
            take_stream_error(&poisoned).is_some(),
            "a poisoned lock read as a healthy stream"
        );
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
            eprintln!(
                "{}{} [{}]",
                if device.is_default { "* " } else { "  " },
                device.name,
                device.id
            );
        }
        assert!(!devices.is_empty());
    }
}
