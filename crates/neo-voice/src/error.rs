//! Errors returned by capture and transcription.
//!
//! Every variant names the fix, and none of them carries audio, a transcript
//! or a credential: they are plain data, safe to log and to put on an event.

use std::time::Duration;

/// Everything the voice layer can refuse or fail to do.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VoiceError {
    /// The machine has no audio input at all.
    #[error("no audio input device is available; plug in or enable a microphone")]
    NoInputDevice,

    /// The named device is not among the inputs.
    #[error("no input device named `{requested}`; available: {}", available.join(", "))]
    UnknownDevice {
        /// What the caller asked for.
        requested: String,
        /// The input device names that do exist, for the message.
        available: Vec<String>,
    },

    /// CoreAudio refused a call.
    #[error("{call} failed: {detail}")]
    Device {
        /// The cpal call that failed.
        call: &'static str,
        /// The host's message.
        detail: String,
    },

    /// The device's default input format is not one we can read.
    #[error("input format {format} is not supported; pick a device that offers f32 or i16")]
    UnsupportedFormat {
        /// The `cpal::SampleFormat` name.
        format: String,
    },

    /// `start` was called on a microphone that is already recording.
    #[error("this microphone is already recording; call stop() first")]
    AlreadyRecording,

    /// `stop` (or `level`) was called before `start`.
    #[error("this microphone is not recording; call start() first")]
    NotRecording,

    /// The stream opened but produced nothing.
    #[error("no audio was captured; hold the key long enough to speak, and check the input level")]
    NoAudio,

    /// The push-to-talk key was held past the cap.
    #[error("the recording hit the {}s limit; release the key and send a shorter utterance", limit.as_secs())]
    TooLong {
        /// The cap that was hit.
        limit: Duration,
    },

    /// The capture thread died without reporting a result.
    #[error("the capture thread stopped unexpectedly: {detail}")]
    CaptureThread {
        /// What we know about why.
        detail: String,
    },

    /// Rubato refused the rate conversion.
    #[error("resampling {from} Hz to 16 kHz failed: {detail}")]
    Resample {
        /// The device rate we tried to convert from.
        from: u32,
        /// The resampler's message.
        detail: String,
    },

    /// `hound` refused to write the WAV.
    #[error("encoding the utterance as WAV failed: {detail}")]
    Encode {
        /// The encoder's message.
        detail: String,
    },

    /// TCC has denied microphone access to this binary.
    #[error(
        "microphone access is denied; grant it in System Settings › Privacy & Security › Microphone (under `cargo run` the grant belongs to the launching terminal)"
    )]
    MicrophoneDenied,

    /// TCC has denied speech recognition to this binary.
    #[error(
        "speech recognition is denied; grant it in System Settings › Privacy & Security › Speech Recognition (under `cargo run` the grant belongs to the launching terminal)"
    )]
    SpeechDenied,

    /// The user has not answered the Speech prompt yet.
    #[error("speech recognition has not been authorised yet; accept the system prompt and retry")]
    SpeechNotDetermined,

    /// On-device recognition is not installed for this locale.
    #[error(
        "on-device dictation is unavailable for `{locale}`; add the language in System Settings › Keyboard › Dictation, or set an OpenAI key"
    )]
    OnDeviceUnavailable {
        /// The locale we asked `SFSpeechRecognizer` for.
        locale: String,
    },

    /// *Siri & Dictation* is switched off, so no recogniser will run.
    #[error(
        "Siri & Dictation is turned off, so on-device dictation cannot run; turn it on in System Settings › Keyboard › Dictation"
    )]
    DictationDisabled,

    /// `SFSpeechRecognizer` reported a failure.
    #[error("on-device dictation failed: {detail}")]
    Speech {
        /// The `NSError` description.
        detail: String,
    },

    /// The request never reached the provider.
    #[error("the transcription request failed: {detail}")]
    Transport {
        /// The reqwest message, with no URL credential in it.
        detail: String,
    },

    /// The provider answered with a non-2xx status.
    #[error("{provider} refused the transcription ({status}): {detail}")]
    Provider {
        /// Which backend answered.
        provider: &'static str,
        /// The HTTP status.
        status: u16,
        /// The body, truncated.
        detail: String,
    },

    /// The provider's body was not the shape we parse.
    #[error("{provider} returned an unreadable transcription body: {detail}")]
    BadResponse {
        /// Which backend answered.
        provider: &'static str,
        /// What was wrong.
        detail: String,
    },

    /// Neither backend can run, with the reason for each.
    #[error(
        "no transcriber is available: on-device dictation is unusable ({on_device}), and no OpenAI key is set (add one in Connections, or export OPENAI_API_KEY)"
    )]
    NoTranscriber {
        /// Why the on-device path is out.
        on_device: String,
    },

    /// A capability that only exists on macOS.
    #[error("{0} is only available on macOS")]
    Unsupported(&'static str),
}
