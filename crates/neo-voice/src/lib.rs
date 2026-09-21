//! `neo-voice` — push-to-talk microphone capture and speech-to-text.
//!
//! Two interchangeable backends sit behind one `Transcriber` trait:
//!
//! * `AppleTranscriber` — macOS `Speech.framework` with
//!   `requiresOnDeviceRecognition`, so the audio never leaves the machine.
//!   Free, offline, no credential. This is the default.
//! * `OpenAiTranscriber` — `gpt-transcribe` over the audio transcriptions
//!   endpoint, used when an OpenAI key exists.
//!
//! Capture and the two backends land in this module tree over the next few
//! commits; the DSP chain and the error surface are here already.

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
pub use stt::{OpenAiTranscriber, Transcriber, Transcript, transcriber};

#[cfg(target_os = "macos")]
pub use stt::AppleTranscriber;
