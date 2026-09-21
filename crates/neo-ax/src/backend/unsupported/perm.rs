//! Accessibility permission on a platform that has none.
//!
//! macOS gates the AX API behind TCC, and every caller asks these questions
//! before it does anything. Nowhere else does: AT-SPI has no per-client
//! grant, and neither has Windows UIA. Reporting *not trusted* here would
//! send a Linux user to a System Settings pane that does not exist and hide
//! the real answer, which is [`crate::AxError::NoBackend`] from
//! `AxHandle::spawn`. So the gates answer *open* and the spawn answers
//! honestly.

/// Deep link to the Accessibility pane. Empty here: no pane awards anything,
/// so there is nothing to link to, and a front end renders no button.
pub const ACCESSIBILITY_SETTINGS_URL: &str = "";

/// `true`: no grant stands between this binary and the accessibility API.
#[must_use]
pub fn trusted() -> bool {
    true
}

/// `true`, having asked for nothing: there is no prompt to raise.
#[must_use]
pub fn request_trust() -> bool {
    true
}

/// `true`: posting synthetic input is not permission-gated here.
#[must_use]
pub fn can_post_events() -> bool {
    true
}
