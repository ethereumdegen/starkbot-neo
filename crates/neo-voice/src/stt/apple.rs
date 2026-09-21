//! On-device dictation through macOS `Speech.framework`.
//!
//! `requiresOnDeviceRecognition = true` is the whole point: the audio is
//! recognised by the same model that backs keyboard dictation, on this Mac,
//! with no network call, no credential and no per-minute charge. Decision K6
//! made speech OpenAI-key-only; a user with no OpenAI key would then have no
//! voice at all, so this is the default backend and OpenAI is the upgrade.
//!
//! The audio is handed over as an in-memory `AVAudioPCMBuffer`, not a temp
//! file, so the plan's "audio is never written to disk" stance holds.
//!
//! # `unsafe`
//!
//! Every `Speech.framework` and `AVFAudio` entry point is an Objective-C
//! message send, which `objc2` exposes as `unsafe fn`: the compiler cannot
//! know the selector exists, or that the argument types match. On top of
//! that, filling an `AVAudioPCMBuffer` means writing through the raw
//! `float *` the framework hands back. Both are contained in this module —
//! nothing unsafe crosses its boundary, and no Objective-C object escapes
//! the blocking thread that made it. This mirrors `crates/neo-ax/src`.
//!
//! # Limits Apple imposes
//!
//! Speech stops a recognition task after roughly a minute of audio, which is
//! below this crate's 120 s capture cap; a long utterance comes back as a
//! [`VoiceError::Speech`] naming the framework's own message rather than a
//! silently truncated transcript.

#![allow(unsafe_code)]

use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use block2::RcBlock;
use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2_avf_audio::{AVAudioCommonFormat, AVAudioFormat, AVAudioPCMBuffer};
use objc2_foundation::{NSError, NSOperationQueue};
use objc2_speech::{
    SFSpeechAudioBufferRecognitionRequest, SFSpeechRecognitionResult, SFSpeechRecognitionTaskHint,
    SFSpeechRecognizer,
};

use super::{Transcriber, Transcript};
use crate::capture::Utterance;
use crate::error::VoiceError;
use crate::permission;
use crate::resample;

/// How long to wait for the recogniser before giving up.
const RECOGNITION_TIMEOUT: Duration = Duration::from_secs(60);

/// On-device speech recognition. Holds no Objective-C object, so it is
/// `Send + Sync` and can live in the agent's shared state; the recogniser is
/// built and torn down inside each blocking call.
pub struct AppleTranscriber {
    locale: String,
}

impl AppleTranscriber {
    /// Check that on-device dictation can run here, and remember the locale.
    ///
    /// # Errors
    ///
    /// [`VoiceError::SpeechDenied`] when TCC has refused,
    /// [`VoiceError::DictationDisabled`] when the *Siri & Dictation* switch
    /// is off, and [`VoiceError::OnDeviceUnavailable`] when the system has
    /// no recogniser for the current locale or has not downloaded its
    /// on-device assets.
    pub fn new() -> Result<Self, VoiceError> {
        if permission::speech_status() == permission::SpeechAuth::Denied {
            return Err(VoiceError::SpeechDenied);
        }
        if !permission::dictation_enabled() {
            return Err(VoiceError::DictationDisabled);
        }
        let recognizer = recognizer()?;
        // SAFETY: plain property reads on a live recogniser.
        let (on_device, locale) = unsafe {
            (
                recognizer.supportsOnDeviceRecognition(),
                recognizer.locale().localeIdentifier().to_string(),
            )
        };
        if !on_device {
            return Err(VoiceError::OnDeviceUnavailable { locale });
        }
        Ok(Self { locale })
    }

    /// The locale the recogniser will use, e.g. `en_US`.
    #[must_use]
    pub fn locale(&self) -> &str {
        &self.locale
    }
}

#[async_trait]
impl Transcriber for AppleTranscriber {
    async fn transcribe(&self, utterance: &Utterance) -> Result<Transcript, VoiceError> {
        if utterance.pcm16.is_empty() {
            return Err(VoiceError::NoAudio);
        }
        let samples = resample::from_pcm16(&utterance.pcm16);
        let rate = f64::from(utterance.sample_rate);
        let started = Instant::now();

        let recognised = tokio::task::spawn_blocking(move || {
            // Inside the closure, not on the line above it. On
            // `NotDetermined` this shows the system prompt and pumps the run
            // loop for up to `permission::PROMPT_TIMEOUT`, and doing that
            // before the `spawn_blocking` parked the whole executor for a
            // minute — undoing, one line early, exactly what this
            // `spawn_blocking` exists to do.
            permission::ensure_speech()?;
            recognise(&samples, rate)
        })
        .await
        .map_err(|e| VoiceError::Speech {
            detail: format!("the recognition thread died: {e}"),
        })??;

        Ok(Transcript {
            text: recognised.text,
            confidence: recognised.confidence,
            duration_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        })
    }

    fn name(&self) -> &'static str {
        "apple-on-device"
    }
}

/// A recogniser for the user's current locale.
fn recognizer() -> Result<Retained<SFSpeechRecognizer>, VoiceError> {
    // SAFETY: `SFSpeechRecognizer` is not main-thread-only, so allocating and
    // initialising it here is sound. `init` is nil-returning when the locale
    // has no recogniser, which the `Option` models correctly.
    let recognizer = unsafe { SFSpeechRecognizer::init(SFSpeechRecognizer::alloc()) };
    let recognizer = recognizer.ok_or_else(|| VoiceError::OnDeviceUnavailable {
        locale: "the system default".into(),
    })?;
    // SAFETY: a property read on the object we just made.
    if !unsafe { recognizer.isAvailable() } {
        // SAFETY: as above.
        let locale = unsafe { recognizer.locale().localeIdentifier() }.to_string();
        return Err(VoiceError::OnDeviceUnavailable { locale });
    }
    Ok(recognizer)
}

struct Recognised {
    text: String,
    confidence: Option<f32>,
}

/// What the result handler reports back to the waiting thread.
enum Outcome {
    Done(Recognised),
    Failed(String),
}

/// Run one blocking on-device recognition over `samples` (mono, `rate` Hz).
fn recognise(samples: &[f32], rate: f64) -> Result<Recognised, VoiceError> {
    let recognizer = recognizer()?;
    let buffer = pcm_buffer(samples, rate)?;

    // `SFSpeechRecognizer` delivers its result handler on `queue`, which
    // defaults to the **main** operation queue. Nothing here owns the main
    // thread — not `cargo test`, not the TUI's render loop, not a Tokio
    // worker — so on the default queue the handler simply never runs and the
    // recognition looks like a timeout. Giving the recogniser its own queue
    // is what makes this work off the main thread at all.
    let queue = NSOperationQueue::new();
    // SAFETY: handing the recogniser a queue we just created and keep alive
    // for the whole call; `setQueue:` retains it anyway.
    unsafe { recognizer.setQueue(&queue) };

    // SAFETY: `new` on a class with no main-thread requirement.
    let request = unsafe { SFSpeechAudioBufferRecognitionRequest::new() };
    // SAFETY: property writes on the request we just made.
    unsafe {
        request.setRequiresOnDeviceRecognition(true);
        request.setShouldReportPartialResults(false);
        request.setAddsPunctuation(true);
        request.setTaskHint(SFSpeechRecognitionTaskHint::Dictation);
        request.appendAudioPCMBuffer(&buffer);
        request.endAudio();
    }

    let slot: Arc<Mutex<Option<Outcome>>> = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&slot);
    let handler = RcBlock::new(
        move |result: *mut SFSpeechRecognitionResult, error: *mut NSError| {
            let outcome = read_callback(result, error);
            if let Some(outcome) = outcome {
                let mut guard = sink.lock().unwrap_or_else(|poison| poison.into_inner());
                guard.get_or_insert(outcome);
            }
        },
    );

    // SAFETY: the request outlives the call, the block is kept alive by the
    // `RcBlock` we hold until the wait below returns, and the returned task
    // is retained for the same span. The handler only touches an `Arc<Mutex>`
    // and framework objects it is handed.
    let task = unsafe { recognizer.recognitionTaskWithRequest_resultHandler(&request, &handler) };

    let deadline = Instant::now() + RECOGNITION_TIMEOUT;
    let outcome = loop {
        if let Some(outcome) = slot
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
        {
            break Some(outcome);
        }
        if Instant::now() >= deadline {
            break None;
        }
        permission::pump_run_loop(Duration::from_millis(20));
    };

    // SAFETY: cancelling a task we still hold; a no-op once it has finished.
    unsafe { task.cancel() };
    drop(handler);

    match outcome {
        Some(Outcome::Done(recognised)) => Ok(recognised),
        // The switch can be flipped between construction and use, and the
        // framework only says so here.
        Some(Outcome::Failed(detail)) if detail.contains("Dictation are disabled") => {
            Err(VoiceError::DictationDisabled)
        }
        Some(Outcome::Failed(detail)) => Err(VoiceError::Speech { detail }),
        None => Err(VoiceError::Speech {
            detail: format!(
                "the recogniser did not answer within {}s",
                RECOGNITION_TIMEOUT.as_secs()
            ),
        }),
    }
}

/// Turn one `(result, error)` callback into an outcome, or `None` for a
/// non-final partial that carries no verdict.
fn read_callback(result: *mut SFSpeechRecognitionResult, error: *mut NSError) -> Option<Outcome> {
    if let Some(error) = std::ptr::NonNull::new(error) {
        // SAFETY: a non-null `NSError *` owned by the caller for the duration
        // of the callback; we only read its description and copy it out.
        let detail = unsafe { error.as_ref().localizedDescription() }.to_string();
        return Some(Outcome::Failed(detail));
    }
    let result = std::ptr::NonNull::new(result)?;
    // SAFETY: a non-null `SFSpeechRecognitionResult *` valid for this call.
    let result = unsafe { result.as_ref() };
    // SAFETY: property reads on that result.
    if !unsafe { result.isFinal() } {
        return None;
    }
    // SAFETY: as above; `formattedString` and `segments` return retained
    // objects that `objc2` manages.
    let (text, confidence) = unsafe {
        let transcription = result.bestTranscription();
        let segments = transcription.segments();
        let scored: Vec<f32> = segments
            .iter()
            .map(|segment| segment.confidence())
            .filter(|score| *score > 0.0)
            .collect();
        let confidence = match scored.len() {
            0 => None,
            n => Some(scored.iter().sum::<f32>() / n as f32),
        };
        (transcription.formattedString().to_string(), confidence)
    };
    Some(Outcome::Done(Recognised { text, confidence }))
}

/// Wrap mono float samples in an `AVAudioPCMBuffer` the request can take.
fn pcm_buffer(samples: &[f32], rate: f64) -> Result<Retained<AVAudioPCMBuffer>, VoiceError> {
    let frames = u32::try_from(samples.len()).map_err(|_| VoiceError::Speech {
        detail: "the utterance is too long to hand to the recogniser".into(),
    })?;

    // SAFETY: a documented initialiser; mono non-interleaved float32 at the
    // utterance's own rate is a format AVFAudio always supports.
    let format = unsafe {
        AVAudioFormat::initWithCommonFormat_sampleRate_channels_interleaved(
            AVAudioFormat::alloc(),
            AVAudioCommonFormat::PCMFormatFloat32,
            rate,
            1,
            false,
        )
    }
    .ok_or_else(|| VoiceError::Speech {
        detail: format!("AVFAudio rejected a mono float32 format at {rate} Hz"),
    })?;

    // SAFETY: allocating a buffer with the format above; nil means the
    // capacity or format was refused, which the `Option` models.
    let buffer = unsafe {
        AVAudioPCMBuffer::initWithPCMFormat_frameCapacity(
            AVAudioPCMBuffer::alloc(),
            &format,
            frames,
        )
    }
    .ok_or_else(|| VoiceError::Speech {
        detail: "AVFAudio refused a PCM buffer for this utterance".into(),
    })?;

    // SAFETY: `floatChannelData` is non-null for a float32 PCM buffer, and
    // points at an array of one channel pointer because the format above
    // declares one channel. That channel has `frames` writable samples
    // because we asked for exactly `frames` of capacity. `frameLength` is set
    // to the number of samples actually written, as AVFAudio requires.
    unsafe {
        let channels = buffer.floatChannelData();
        if channels.is_null() {
            return Err(VoiceError::Speech {
                detail: "the PCM buffer exposed no float channel data".into(),
            });
        }
        let channel = (*channels).as_ptr();
        std::ptr::copy_nonoverlapping(samples.as_ptr(), channel, samples.len());
        buffer.setFrameLength(frames);
    }
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]
    use super::*;

    #[test]
    fn the_backend_is_shareable_across_the_agent() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AppleTranscriber>();
    }

    #[test]
    fn an_empty_utterance_is_refused_before_any_framework_call() {
        let Ok(transcriber) = AppleTranscriber::new() else {
            // No on-device dictation on this machine; nothing to assert.
            return;
        };
        let utterance = Utterance {
            pcm16: Vec::new(),
            sample_rate: 16_000,
            duration: Duration::ZERO,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let error = runtime
            .block_on(transcriber.transcribe(&utterance))
            .expect_err("silence is not a transcript");
        assert_eq!(error, VoiceError::NoAudio);
    }

    #[test]
    fn a_float_buffer_carries_every_sample_at_the_requested_rate() {
        let samples: Vec<f32> = (0..1_024).map(|n| (n as f32 / 1_024.0) - 0.5).collect();
        let buffer = pcm_buffer(&samples, 16_000.0).expect("buffer");
        // SAFETY: reading back what we just wrote, through the same
        // one-channel float32 layout.
        unsafe {
            assert_eq!(buffer.frameLength(), 1_024);
            assert_eq!(buffer.format().sampleRate(), 16_000.0);
            let channel = (*buffer.floatChannelData()).as_ptr();
            let read_back = std::slice::from_raw_parts(channel, samples.len());
            assert_eq!(read_back, samples.as_slice());
        }
    }

    #[test]
    #[ignore = "needs a real Mac with Speech Recognition granted"]
    fn reports_what_the_system_says_about_on_device_dictation() {
        eprintln!("speech authorisation: {:?}", permission::speech_status());
        match AppleTranscriber::new() {
            Ok(transcriber) => {
                eprintln!("on-device dictation ready, locale {}", transcriber.locale())
            }
            Err(error) => eprintln!("on-device dictation unavailable: {error}"),
        }
    }

    #[test]
    #[ignore = "needs a real microphone and a person to speak"]
    fn records_and_transcribes_on_device_with_no_openai_key() {
        let transcriber = match AppleTranscriber::new() {
            Ok(transcriber) => transcriber,
            Err(error) => panic!("on-device dictation is unavailable: {error}"),
        };
        let mut mic = crate::Microphone::open(None).expect("open the default input");
        eprintln!("recording 6s from `{}` — speak now", mic.name());
        mic.start().expect("start");
        let mut peak = 0.0f32;
        for tick in 0..120 {
            std::thread::sleep(Duration::from_millis(50));
            peak = peak.max(mic.level());
            if tick % 20 == 19 {
                eprintln!("  {}s, peak so far {peak:.3}", (tick + 1) / 20);
            }
        }
        let utterance = mic.stop().expect("stop");
        eprintln!(
            "captured {:.2}s, peak {peak:.3}",
            utterance.duration.as_secs_f32()
        );

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let transcript = runtime
            .block_on(transcriber.transcribe(&utterance))
            .expect("transcribe on device");
        eprintln!(
            "heard: {:?} (confidence {:?}, {} ms)",
            transcript.text, transcript.confidence, transcript.duration_ms
        );
        assert!(
            !transcript.text.trim().is_empty(),
            "the recogniser returned nothing"
        );
    }

    /// The recogniser proof that needs neither a person nor a quiet room:
    /// `say` writes a 16 kHz mono utterance, and the on-device recogniser
    /// has to read it back. Ignored because it depends on Speech being
    /// authorised and *Siri & Dictation* being on.
    #[test]
    #[ignore = "needs a real Mac with Speech authorised and Dictation on"]
    fn transcribes_a_synthesised_utterance_entirely_on_device() {
        const PHRASE: &str = "open Hypercanvas and make a bright blue launch advertisement";

        let transcriber = match AppleTranscriber::new() {
            Ok(transcriber) => transcriber,
            Err(error) => panic!("on-device dictation is unavailable: {error}"),
        };
        let path = std::env::temp_dir().join("neo-voice-on-device-proof.wav");
        let spoken = std::process::Command::new("/usr/bin/say")
            .args(["-r", "155", "--data-format=LEI16@16000", "-o"])
            .arg(&path)
            .arg(PHRASE)
            .status()
            .expect("run /usr/bin/say");
        assert!(spoken.success(), "`say` could not synthesise the fixture");

        let mut reader = hound::WavReader::open(&path).expect("open the fixture");
        let spec = reader.spec();
        let pcm16: Vec<i16> = reader
            .samples::<i16>()
            .map(|s| s.expect("sample"))
            .collect();
        let utterance = Utterance {
            duration: Duration::from_secs_f64(pcm16.len() as f64 / f64::from(spec.sample_rate)),
            sample_rate: spec.sample_rate,
            pcm16,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let transcript = runtime
            .block_on(transcriber.transcribe(&utterance))
            .expect("transcribe on device");
        let _ = std::fs::remove_file(&path);

        eprintln!(
            "spoke: {PHRASE:?}\nheard: {:?} (confidence {:?}, {} ms)",
            transcript.text, transcript.confidence, transcript.duration_ms
        );
        let heard = transcript.text.to_lowercase();
        for word in ["make", "bright", "blue", "launch", "advertisement"] {
            assert!(heard.contains(word), "`{word}` is missing from {heard:?}");
        }
    }
}
