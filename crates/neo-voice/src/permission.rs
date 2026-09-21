//! The two TCC gates voice needs: Microphone and Speech Recognition.
//!
//! # The grant attaches to the binary, not to the project
//!
//! Like Accessibility (see `neo-ax`'s `perm.rs`), macOS keys these grants on
//! the *responsible* process. Under `cargo run` or `cargo test` the process is
//! a bare Mach-O with no bundle, so TCC attributes it to the launching
//! terminal: what you grant is Terminal.app's or iTerm's Microphone toggle,
//! not Neo's. IDE-integrated terminals can fail the prompt silently, which is
//! why the dev instructions say to run from a real terminal.
//!
//! # Usage descriptions
//!
//! A prompt only appears if the responsible bundle carries the matching
//! `Info.plist` string. The shipped app needs both
//! `NSMicrophoneUsageDescription` and `NSSpeechRecognitionUsageDescription`;
//! `build.rs` embeds the same pair into this crate's test binaries via
//! `-sectcreate __TEXT __info_plist`, so the ignored hardware tests can be
//! granted rather than killed.

use crate::error::VoiceError;

/// Deep link to the Microphone pane, for onboarding and `neo doctor`.
pub const MICROPHONE_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone";

/// Deep link to the Speech Recognition pane.
pub const SPEECH_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_SpeechRecognition";

/// Deep link to Keyboard settings, where the Dictation switch lives.
pub const DICTATION_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.Keyboard-Settings.extension";

/// How long a first-run prompt is given to be answered.
#[cfg(target_os = "macos")]
const PROMPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// What TCC says about microphone access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicrophoneAuth {
    /// No answer yet; asking will show the system prompt.
    NotDetermined,
    /// The user said no, or a profile forbids it. macOS will not re-prompt.
    Denied,
    /// Allowed.
    Authorized,
}

/// What TCC says about speech recognition, or that the question does not
/// apply here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeechAuth {
    /// No answer yet; asking will show the system prompt.
    NotDetermined,
    /// The user said no, or a profile forbids it.
    Denied,
    /// Allowed.
    Authorized,
    /// This platform has no on-device speech recogniser at all, so there is
    /// nothing to authorise. Distinct from [`SpeechAuth::Denied`], which
    /// says a recogniser exists and the user refused it: a caller that
    /// conflates the two tells a Linux user to go and un-deny a permission
    /// that was never asked for.
    Unsupported,
}

/// Current microphone authorisation. Never prompts.
#[must_use]
pub fn microphone_status() -> MicrophoneAuth {
    #[cfg(target_os = "macos")]
    {
        mac::microphone_status()
    }
    #[cfg(not(target_os = "macos"))]
    {
        MicrophoneAuth::Authorized
    }
}

/// Current speech-recognition authorisation. Never prompts.
#[must_use]
pub fn speech_status() -> SpeechAuth {
    #[cfg(target_os = "macos")]
    {
        mac::speech_status()
    }
    #[cfg(not(target_os = "macos"))]
    {
        SpeechAuth::Unsupported
    }
}

/// Whether *Siri & Dictation* is switched on.
///
/// On-device recognition refuses to run while it is off — the framework
/// answers "Siri and Dictation are disabled" — so Doctor reads this and
/// points the user at [`DICTATION_SETTINGS_URL`]. It is a read-only probe on
/// purpose: Starkbot never flips a System Settings switch for the user.
///
/// Always `false` where there is no such switch; a caller asks
/// [`speech_status`] first and stops at [`SpeechAuth::Unsupported`].
#[must_use]
pub fn dictation_enabled() -> bool {
    #[cfg(target_os = "macos")]
    {
        mac::dictation_enabled()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Make sure the microphone may be opened, prompting once if TCC has never
/// asked. Blocks on the prompt, because push-to-talk has nothing to do until
/// it is answered.
pub(crate) fn ensure_microphone() -> Result<(), VoiceError> {
    #[cfg(target_os = "macos")]
    {
        match mac::microphone_status() {
            MicrophoneAuth::Authorized => Ok(()),
            MicrophoneAuth::Denied => Err(VoiceError::MicrophoneDenied),
            MicrophoneAuth::NotDetermined => match mac::request_microphone(PROMPT_TIMEOUT) {
                true => Ok(()),
                false => Err(VoiceError::MicrophoneDenied),
            },
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}

/// Make sure speech recognition may run, prompting once if TCC has never
/// asked.
#[cfg(target_os = "macos")]
pub(crate) fn ensure_speech() -> Result<(), VoiceError> {
    let settled = match mac::speech_status() {
        SpeechAuth::NotDetermined => mac::request_speech(PROMPT_TIMEOUT),
        answered => answered,
    };
    match settled {
        SpeechAuth::Authorized => Ok(()),
        SpeechAuth::Denied => Err(VoiceError::SpeechDenied),
        // `mac` answers only the three TCC states — a recogniser exists
        // here, which is the one thing `Unsupported` denies.
        SpeechAuth::NotDetermined | SpeechAuth::Unsupported => Err(VoiceError::SpeechNotDetermined),
    }
}

#[cfg(target_os = "macos")]
mod mac {
    //! `unsafe` here is the price of asking TCC anything: both status calls
    //! and both request calls are Objective-C class methods whose completion
    //! handlers are blocks, and `objc2` cannot express "this selector is sound
    //! to send" without an unsafe call. Nothing in this module dereferences a
    //! raw pointer we did not get from the runtime, and no handle escapes.

    #![allow(unsafe_code)]

    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::time::{Duration, Instant};

    use block2::RcBlock;
    use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
    use objc2_core_foundation::{
        CFPreferencesGetAppBooleanValue, CFRunLoop, CFString, kCFRunLoopDefaultMode,
    };
    use objc2_speech::{SFSpeechRecognizer, SFSpeechRecognizerAuthorizationStatus};

    use super::{MicrophoneAuth, SpeechAuth};

    /// Pump this thread's run loop for `slice`, so a completion handler that
    /// is scheduled on the current thread still gets to run.
    ///
    /// AVFoundation and Speech both deliver on private queues in practice,
    /// but a run loop turn costs nothing and removes the "it hangs on some
    /// machines" failure mode entirely.
    pub(crate) fn pump(slice: Duration) {
        // SAFETY: `CFRunLoopRunInMode` on the current thread's run loop with a
        // finite timeout and `returnAfterSourceHandled: false`. It creates the
        // run loop if this thread has none, and borrows nothing of ours.
        unsafe {
            CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, slice.as_secs_f64(), false);
        }
    }

    /// Block until `poll` yields a value, or `timeout` elapses, pumping the
    /// run loop meanwhile.
    fn await_answer<T>(timeout: Duration, poll: impl Fn() -> Option<T>) -> Option<T> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(value) = poll() {
                return Some(value);
            }
            if Instant::now() >= deadline {
                return None;
            }
            pump(Duration::from_millis(50));
        }
    }

    /// The `AVMediaTypeAudio` constant, or `None` if AVFoundation did not
    /// export it — which would mean the framework is not loaded at all.
    fn audio_media_type() -> Option<&'static objc2_foundation::NSString> {
        // SAFETY: reading an immutable `NSString *` constant exported by
        // AVFoundation. It is initialised before any of our code runs and is
        // never written, so there is no race and no invalid data.
        unsafe { AVMediaTypeAudio }
    }

    pub(super) fn microphone_status() -> MicrophoneAuth {
        let Some(audio) = audio_media_type() else {
            return MicrophoneAuth::Denied;
        };
        // SAFETY: a class method that reads TCC state for a framework media
        // type constant and returns a plain enum.
        let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(audio) };
        match status {
            AVAuthorizationStatus::Authorized => MicrophoneAuth::Authorized,
            AVAuthorizationStatus::NotDetermined => MicrophoneAuth::NotDetermined,
            _ => MicrophoneAuth::Denied,
        }
    }

    pub(super) fn request_microphone(timeout: Duration) -> bool {
        let Some(audio) = audio_media_type() else {
            return false;
        };
        // -1 = no answer yet, 0 = denied, 1 = granted.
        let answer = Arc::new(AtomicI64::new(-1));
        let sink = Arc::clone(&answer);
        let handler = RcBlock::new(move |granted: objc2::runtime::Bool| {
            sink.store(i64::from(granted.as_bool()), Ordering::SeqCst);
        });
        // SAFETY: the block outlives the call through its `RcBlock`, which we
        // hold until `await_answer` returns; the media type is a framework
        // constant. The callback only touches an atomic.
        unsafe {
            AVCaptureDevice::requestAccessForMediaType_completionHandler(audio, &handler);
        }
        await_answer(timeout, || match answer.load(Ordering::SeqCst) {
            -1 => None,
            other => Some(other == 1),
        })
        .unwrap_or(false)
    }

    fn map_speech(status: SFSpeechRecognizerAuthorizationStatus) -> SpeechAuth {
        match status {
            SFSpeechRecognizerAuthorizationStatus::Authorized => SpeechAuth::Authorized,
            SFSpeechRecognizerAuthorizationStatus::NotDetermined => SpeechAuth::NotDetermined,
            _ => SpeechAuth::Denied,
        }
    }

    /// The *Siri & Dictation* master switch, read straight out of the
    /// preference domain System Settings writes.
    pub(super) fn dictation_enabled() -> bool {
        let key = CFString::from_static_str("Dictation Enabled");
        let domain = CFString::from_static_str("com.apple.assistant.support");
        let mut exists: u8 = 0;
        // SAFETY: both arguments are live `CFString`s, and the out-parameter
        // is a stack byte we own for the whole call. The function reads a
        // preference and writes one `Boolean` through that pointer.
        let value = unsafe { CFPreferencesGetAppBooleanValue(&key, &domain, &mut exists) };
        exists != 0 && value
    }

    pub(super) fn speech_status() -> SpeechAuth {
        // SAFETY: a class method with no arguments that reads TCC state.
        map_speech(unsafe { SFSpeechRecognizer::authorizationStatus() })
    }

    pub(super) fn request_speech(timeout: Duration) -> SpeechAuth {
        let answer = Arc::new(AtomicI64::new(-1));
        let sink = Arc::clone(&answer);
        let handler = RcBlock::new(move |status: SFSpeechRecognizerAuthorizationStatus| {
            sink.store(status.0 as i64, Ordering::SeqCst);
        });
        // SAFETY: as above — the `RcBlock` lives across the call, and the
        // handler only stores an integer.
        unsafe {
            SFSpeechRecognizer::requestAuthorization(&handler);
        }
        match await_answer(timeout, || match answer.load(Ordering::SeqCst) {
            -1 => None,
            other => Some(other),
        }) {
            Some(raw) => map_speech(SFSpeechRecognizerAuthorizationStatus(raw as isize)),
            None => SpeechAuth::NotDetermined,
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) use mac::pump as pump_run_loop;

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]
    use super::*;

    #[test]
    fn reading_authorisation_never_blocks_and_never_prompts() {
        // All three are pure reads; a denied or switched-off machine is a
        // valid answer, a hang, a prompt or a crash is not. This is the
        // guard against accidentally wiring the *request* variants — or a
        // settings write — into the status path.
        let _ = microphone_status();
        let _ = speech_status();
        let _ = dictation_enabled();
    }

    #[test]
    fn the_settings_links_point_at_the_panes_a_user_has_to_open() {
        assert!(MICROPHONE_SETTINGS_URL.ends_with("Privacy_Microphone"));
        assert!(SPEECH_SETTINGS_URL.ends_with("Privacy_SpeechRecognition"));
        assert!(DICTATION_SETTINGS_URL.contains("Keyboard-Settings"));
    }
}
