//! Accessibility permission (TCC).
//!
//! # The grant attaches to the binary, not to the project
//!
//! macOS keys the Accessibility grant on the **bundle id plus the code
//! requirement** of whatever process actually calls the AX API. Under
//! `cargo run` (or `tauri dev`) the process is a bare Mach-O with no bundle,
//! so TCC attributes it to the **launching terminal**: granting
//! "Accessibility" to Terminal.app or iTerm is what makes development work,
//! and the toggle you want in System Settings is the terminal's, not Neo's.
//!
//! Ad-hoc signed builds are keyed on their cdhash, so **every rebuild
//! silently invalidates the grant while the toggle still shows ON**; the
//! symptom is [`trusted`] returning `false` with the switch enabled, and the
//! fix is `tccutil reset Accessibility com.starkbot.neo`. Developer ID builds
//! keep the grant across updates. Test the real grant only with a signed
//! `.app`.

#![allow(unsafe_code)]

use objc2_application_services::{
    AXIsProcessTrusted, AXIsProcessTrustedWithOptions, kAXTrustedCheckOptionPrompt,
};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString};
use objc2_core_graphics::{CGPreflightPostEventAccess, CGRequestPostEventAccess};

/// Deep link to the Accessibility pane, for onboarding and `neo doctor`.
pub const ACCESSIBILITY_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

/// Whether this binary is a trusted accessibility client. Never prompts.
///
/// Safe to poll: onboarding and Doctor call it once a second while their
/// screen is open.
#[must_use]
pub fn trusted() -> bool {
    // SAFETY: `AXIsProcessTrustedWithOptions` takes an optional CFDictionary;
    // passing `None` means "no options", which is exactly the no-prompt query.
    // The call has no other precondition and returns a plain bool.
    unsafe { AXIsProcessTrustedWithOptions(None) }
}

/// Whether this binary is trusted, asking the system to show the prompt when
/// it is not.
///
/// The prompt is asynchronous and does not affect the return value, so the
/// caller keeps polling [`trusted`] afterwards. Call this once, from
/// onboarding.
#[must_use]
pub fn request_trust() -> bool {
    // SAFETY: `kAXTrustedCheckOptionPrompt` is a `static` CFString constant
    // exported by HIServices and valid for the process lifetime; reading it
    // is unsafe only because it is an `extern "C"` static.
    let key: &CFString = unsafe { kAXTrustedCheckOptionPrompt };
    let options: CFRetained<CFDictionary<CFString, CFBoolean>> =
        CFDictionary::from_slices(&[key], &[CFBoolean::new(true)]);
    // SAFETY: the dictionary has the documented key type (CFString) and value
    // type (CFBoolean), which is the generics requirement in the binding's
    // safety contract. It outlives the call.
    unsafe { AXIsProcessTrustedWithOptions(Some(options.as_opaque())) }
}

/// Whether the plain (option-less) trust check passes.
///
/// Equivalent to [`trusted`]; kept because `AXIsProcessTrusted` is the call
/// Doctor quotes when it explains a stale ad-hoc grant.
#[must_use]
pub fn trusted_plain() -> bool {
    // SAFETY: nullary query with no preconditions.
    unsafe { AXIsProcessTrusted() }
}

/// Whether this binary may post synthetic events.
///
/// Posting rides on the same Accessibility grant; this is the preflight the
/// plan asks for at actor start.
#[must_use]
pub fn can_post_events() -> bool {
    CGPreflightPostEventAccess()
}

/// Ask for post-event access, showing the system prompt when needed.
#[must_use]
pub fn request_post_event_access() -> bool {
    CGRequestPostEventAccess()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The trust query must never prompt, never panic and never block, even
    /// on a machine with no grant at all (CI).
    #[test]
    fn trust_queries_are_pure_and_agree() {
        let a = trusted();
        let b = trusted_plain();
        assert_eq!(a, b, "the two no-prompt queries must agree");
        // Idempotent: polling it is what onboarding does.
        assert_eq!(a, trusted());
    }

    #[test]
    fn post_event_preflight_answers_without_prompting() {
        let _ = can_post_events();
    }
}
