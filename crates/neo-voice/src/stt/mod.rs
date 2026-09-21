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

/// A transcription backend, named the way [`Transcriber::name`] names it.
///
/// The enum itself is platform-independent so logs, the settings screen and
/// `neo doctor` can *talk about* a backend this build cannot run. What varies
/// is [`BACKENDS`], the one place a platform's answer to "what can transcribe
/// here?" lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// OpenAI `gpt-transcribe` over the audio transcriptions endpoint.
    /// Portable, needs a key, sends the audio to the vendor.
    OpenAi,
    /// macOS `Speech.framework` with `requiresOnDeviceRecognition`. Needs no
    /// key and no network; macOS only.
    AppleOnDevice,
}

impl Backend {
    /// The stable identifier, identical to the [`Transcriber::name`] the
    /// built backend reports — a test holds the two together, because Health
    /// rows and settings are written against these strings.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::AppleOnDevice => "apple-on-device",
        }
    }
}

/// Every backend this build can construct, in the order [`transcriber`]
/// prefers them.
///
/// macOS has both. Every other platform has OpenAI only: `Speech.framework`
/// is the single on-device recogniser Neo implements, and Q5's L1 scope
/// (plan 16 §8.2) ships Linux the browser product plus OpenAI STT rather
/// than a second, half-tested local recogniser. `neo doctor` reads this
/// table instead of assuming the macOS answer.
#[cfg(target_os = "macos")]
pub const BACKENDS: &[Backend] = &[Backend::OpenAi, Backend::AppleOnDevice];

/// See the macOS definition: off macOS the table has one entry.
#[cfg(not(target_os = "macos"))]
pub const BACKENDS: &[Backend] = &[Backend::OpenAi];

/// Pick the backend this machine can actually use.
///
/// An OpenAI key wins when one exists — it is the accurate, keyword-aware
/// path the plan budgets for. Without a key we fall back to macOS on-device
/// dictation, which costs nothing, needs no network and keeps the audio
/// local; decision K6 said speech was OpenAI-key-only, and that would leave a
/// keyless user with no voice at all. Where the table is OpenAI-only, a
/// keyless machine gets the error below rather than a fallback that cannot
/// exist.
///
/// # Errors
///
/// [`VoiceError::NoTranscriber`], naming both fixes, when there is no key
/// *and* the on-device path is unusable or absent.
pub fn transcriber(
    openai_key: Option<&Secret>,
    base_url: Option<&Url>,
) -> Result<Box<dyn Transcriber>, VoiceError> {
    match openai_key {
        Some(key) => Ok(Box::new(OpenAiTranscriber::new(key, base_url)?)),
        None => on_device(),
    }
}

/// Build one *named* backend, with no fallback.
///
/// [`transcriber`] chooses; this honours a choice already made. A caller that
/// asked for on-device recognition must not silently get a vendor call
/// instead — that is the difference between the audio staying on the machine
/// and leaving it — so every refusal here names the backend that was asked
/// for and what this platform does offer.
///
/// # Errors
///
/// [`VoiceError::BackendUnavailable`] when `backend` is not in [`BACKENDS`]
/// (asking for on-device recognition off macOS),
/// [`VoiceError::MissingOpenAiKey`] when the OpenAI backend is named without
/// a key, and whatever the backend's own constructor refuses with otherwise.
pub fn transcriber_for(
    backend: Backend,
    openai_key: Option<&Secret>,
    base_url: Option<&Url>,
) -> Result<Box<dyn Transcriber>, VoiceError> {
    match backend {
        Backend::OpenAi => match openai_key {
            Some(key) => Ok(Box::new(OpenAiTranscriber::new(key, base_url)?)),
            None => Err(VoiceError::MissingOpenAiKey),
        },
        #[cfg(target_os = "macos")]
        Backend::AppleOnDevice => Ok(Box::new(AppleTranscriber::new()?)),
        // Off macOS the variant exists but the framework does not, so the
        // only honest answer is a named refusal that points at `openai`.
        #[cfg(not(target_os = "macos"))]
        Backend::AppleOnDevice => Err(unavailable(backend)),
    }
}

/// The refusal for a backend missing from this platform's table, carrying the
/// table itself so the message tells the caller what it *can* have.
#[cfg(not(target_os = "macos"))]
fn unavailable(backend: Backend) -> VoiceError {
    VoiceError::BackendUnavailable {
        backend: backend.name(),
        available: BACKENDS
            .iter()
            .map(|entry| format!("`{}`", entry.name()))
            .collect::<Vec<_>>()
            .join(", "),
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
        assert_eq!(picked.name(), Backend::OpenAi.name());
    }

    #[test]
    fn no_key_never_selects_openai() {
        // Either on-device dictation is available, or the error names both
        // ways out. What must never happen is silently reaching for a vendor
        // with no credential.
        match transcriber(None, None) {
            Ok(picked) => assert_eq!(picked.name(), Backend::AppleOnDevice.name()),
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
        assert_eq!(picked.name(), Backend::OpenAi.name());
    }

    #[test]
    fn every_entry_in_the_table_is_reachable_by_name() {
        // The table is what doctor and settings enumerate: an entry that no
        // caller can build, or one whose name does not match the transcriber
        // it builds, would make those screens lie.
        for backend in BACKENDS {
            match transcriber_for(*backend, Some(&key()), None) {
                Ok(built) => assert_eq!(built.name(), backend.name()),
                // On-device construction depends on the machine's TCC state
                // and Dictation switch; a refusal is a valid answer, an
                // "unavailable backend" from its own table is not.
                Err(VoiceError::BackendUnavailable {
                    backend: named,
                    available,
                }) => {
                    panic!("{named} is in the table but refused as unavailable ({available})")
                }
                Err(_) => assert_eq!(*backend, Backend::AppleOnDevice),
            }
        }
    }

    #[test]
    fn naming_openai_without_a_key_refuses_instead_of_falling_back() {
        // The one thing a named request must never do is quietly become the
        // other backend — on macOS that would be a silent switch of where
        // the audio goes.
        let error = transcriber_for(Backend::OpenAi, None, None)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("needs an OpenAI key"), "{error}");
        assert!(error.contains("OPENAI_API_KEY"), "{error}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_keeps_both_backends_in_preference_order() {
        // Guards against the Linux table being applied to macOS: on-device
        // dictation is the keyless default here (A-Q6) and must stay listed.
        assert_eq!(BACKENDS, &[Backend::OpenAi, Backend::AppleOnDevice]);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn off_macos_the_only_backend_is_openai() {
        assert_eq!(BACKENDS, &[Backend::OpenAi]);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn off_macos_asking_for_on_device_names_the_remedy() {
        // Not a panic, not a silent fallback: a named refusal that tells the
        // user the one backend this platform has and how to enable it.
        let error = transcriber_for(Backend::AppleOnDevice, None, None)
            .err()
            .expect("on-device recognition cannot exist off macOS");
        assert!(
            matches!(error, VoiceError::BackendUnavailable { backend, .. } if backend == "apple-on-device"),
            "{error:?}"
        );
        let message = error.to_string();
        assert!(message.contains("`openai`"), "{message}");
        assert!(message.contains("OPENAI_API_KEY"), "{message}");
    }
}
