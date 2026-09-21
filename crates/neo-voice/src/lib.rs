//! `neo-voice` — push-to-talk microphone capture and speech-to-text.
//!
//! Two interchangeable backends sit behind one `Transcriber` trait:
//!
//! * `AppleTranscriber` — macOS `Speech.framework` with
//!   `requiresOnDeviceRecognition`, so the audio never leaves the machine.
//!   Free, offline, no credential. This is the default on macOS.
//! * `OpenAiTranscriber` — `gpt-transcribe` over the audio transcriptions
//!   endpoint, used when an OpenAI key exists.
//!
//! # Platforms
//!
//! Capture is portable: `cpal` speaks CoreAudio on macOS and ALSA/PulseAudio
//! on Linux, and the DSP chain is plain Rust. Only the Apple attachments are
//! macOS-only — `Speech.framework`, the two TCC gates in `permission`, and
//! the `Info.plist` that `build.rs` embeds into test binaries. Off macOS
//! [`BACKENDS`] therefore has exactly one entry, and naming the on-device
//! backend is a [`VoiceError::BackendUnavailable`] pointing at `openai` —
//! not a panic, and not a silent switch of where the audio goes (plan 16
//! §8.2, seam 3).

#![deny(missing_docs)]

mod capture;
mod error;
mod permission;
mod resample;
mod stt;

pub use capture::{DeviceInfo, MAX_UTTERANCE, Microphone, Utterance};
pub use error::VoiceError;
pub use permission::{
    DICTATION_SETTINGS_URL, MICROPHONE_SETTINGS_URL, MicrophoneAuth, SPEECH_SETTINGS_URL,
    SpeechAuth, dictation_enabled, microphone_status, speech_status,
};
pub use resample::TARGET_RATE;
pub use stt::{
    BACKENDS, Backend, OpenAiTranscriber, Transcriber, Transcript, transcriber, transcriber_for,
};

#[cfg(target_os = "macos")]
pub use stt::AppleTranscriber;
