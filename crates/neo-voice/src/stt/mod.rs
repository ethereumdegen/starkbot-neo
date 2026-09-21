//! Speech-to-text: one trait, two backends, and the rule for choosing.

use async_trait::async_trait;
use neo_keys::Secret;
use url::Url;

use crate::capture::Utterance;
use crate::error::VoiceError;

#[cfg(target_os = "macos")]
mod apple;
mod openai;
mod wav;

#[cfg(target_os = "macos")]
pub use apple::AppleTranscriber;
pub use openai::OpenAiTranscriber;

/// What a backend heard.
#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    /// The recognised text, as the backend formatted it.
    pub text: String,
    /// Backend confidence in `0.0..=1.0`, when it reports one.
    pub confidence: Option<f32>,
    /// How long recognition took, measured by the caller-visible boundary.
    pub duration_ms: u64,
}

/// Turns an [`Utterance`] into text.
///
/// Implementations are interchangeable: nothing above this trait knows
/// whether the audio stayed on the machine or went to a vendor.
#[async_trait]
pub trait Transcriber: Send + Sync {
    /// Recognise one complete utterance.
    async fn transcribe(&self, utterance: &Utterance) -> Result<Transcript, VoiceError>;

    /// Stable identifier for logs, Health and the settings screen.
    fn name(&self) -> &'static str;
}

/// Pick the backend this machine can actually use.
///
/// An OpenAI key wins when one exists — it is the accurate, keyword-aware
/// path the plan budgets for. Without a key we fall back to macOS on-device
/// dictation, which costs nothing, needs no network and keeps the audio
/// local; decision K6 said speech was OpenAI-key-only, and that would leave a
/// keyless user with no voice at all.
///
/// # Errors
///
/// [`VoiceError::NoTranscriber`], naming both fixes, when there is no key
/// *and* the on-device path is unusable.
pub fn transcriber(
    openai_key: Option<&Secret>,
    base_url: Option<&Url>,
) -> Result<Box<dyn Transcriber>, VoiceError> {
    match openai_key {
        Some(key) => Ok(Box::new(OpenAiTranscriber::new(key, base_url)?)),
        None => on_device(),
    }
}

#[cfg(target_os = "macos")]
fn on_device() -> Result<Box<dyn Transcriber>, VoiceError> {
    match AppleTranscriber::new() {
        Ok(transcriber) => Ok(Box::new(transcriber)),
        Err(error) => Err(VoiceError::NoTranscriber {
            on_device: error.to_string(),
        }),
    }
}

#[cfg(not(target_os = "macos"))]
fn on_device() -> Result<Box<dyn Transcriber>, VoiceError> {
    Err(VoiceError::NoTranscriber {
        on_device: VoiceError::Unsupported("on-device dictation").to_string(),
    })
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]
    use super::*;

    fn key() -> Secret {
        Secret::new("sk-test-not-a-real-key").expect("secret")
    }

    #[test]
    fn a_key_selects_the_openai_backend() {
        let base = Url::parse("http://127.0.0.1:1/").expect("url");
        let picked = transcriber(Some(&key()), Some(&base)).expect("a key always selects OpenAI");
        assert_eq!(picked.name(), "openai");
    }

    #[test]
    fn no_key_never_selects_openai() {
        // Either on-device dictation is available, or the error names both
        // ways out. What must never happen is silently reaching for a vendor
        // with no credential.
        match transcriber(None, None) {
            Ok(picked) => assert_eq!(picked.name(), "apple-on-device"),
            Err(VoiceError::NoTranscriber { on_device }) => {
                let message = VoiceError::NoTranscriber { on_device }.to_string();
                assert!(message.contains("on-device"), "{message}");
                assert!(message.contains("OpenAI key"), "{message}");
            }
            Err(other) => panic!("expected a NoTranscriber verdict, got {other}"),
        }
    }

    #[test]
    fn a_base_url_is_not_required_to_pick_openai() {
        let picked = transcriber(Some(&key()), None).expect("hosted default");
        assert_eq!(picked.name(), "openai");
    }
}
