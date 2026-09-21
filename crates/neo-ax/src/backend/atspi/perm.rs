//! Accessibility permission on Linux: there is none.
//!
//! macOS gates the AX API behind TCC, and every caller asks these questions
//! before it does anything. AT-SPI2 has no per-client grant: a process that
//! can reach the session bus can reach the accessibility bus, and the only
//! switch in the system — `org.a11y.Status.IsEnabled` — is a hint *to
//! applications* that an assistive client is listening, not a permission
//! held against this binary. Reporting *not trusted* here would send a Linux
//! user to a System Settings pane that does not exist and hide the real
//! answer, which is whatever [`crate::AxHandle::table`] reports about the
//! app in front of them.
//!
//! The Doctor's Linux rows (17 §3.4) are facts it can check instead: the
//! a11y bus answers, `org.a11y.Status.IsEnabled` is true, the compositor
//! offers the two virtual-input protocols, and a Chromium-family binary
//! exists. None of them is a grant, so none of them belongs here.

/// Deep link to the Accessibility pane. Empty: no pane awards anything, so
/// there is nothing to link to and a front end renders no button.
pub const ACCESSIBILITY_SETTINGS_URL: &str = "";

/// `true`: no grant stands between this binary and AT-SPI2.
#[must_use]
pub fn trusted() -> bool {
    true
}

/// `true`, having asked for nothing: there is no prompt to raise.
#[must_use]
pub fn request_trust() -> bool {
    true
}

/// `true`: posting synthetic input is not permission-gated.
///
/// It can still be *unavailable* — a compositor that implements neither
/// `zwp_virtual_keyboard_v1` nor `zwlr_virtual_pointer_v1` cannot be typed
/// into — but that is a missing protocol reported by the action that needed
/// it, not a permission the user can grant.
#[must_use]
pub fn can_post_events() -> bool {
    true
}
