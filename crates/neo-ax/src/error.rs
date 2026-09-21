//! Errors returned by the AX actor.
//!
//! Nothing in here carries a CoreFoundation handle: every variant is plain
//! data so it can cross the actor boundary and be logged or serialised.

use std::fmt;

/// Everything the accessibility layer can refuse or fail to do.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AxError {
    /// The process is not a trusted accessibility client.
    #[error("accessibility permission is not granted for this binary")]
    NotTrusted,

    /// The actor thread is gone (it panicked hard, or was shut down).
    #[error("the accessibility actor is not running")]
    ActorDead,

    /// A command body panicked; the generation was bumped and every ref died.
    #[error("the accessibility actor panicked handling `{command}`")]
    Panic {
        /// Name of the command that panicked.
        command: &'static str,
    },

    /// The app is on the deny list (P3/A9) and is never read or driven.
    #[error("`{app}` is on the deny list and is never read or driven")]
    Denied {
        /// Bundle id or name of the refused app.
        app: String,
    },

    /// No running application matched the selector.
    #[error("no running application matches {selector}")]
    NoApp {
        /// The selector that matched nothing.
        selector: String,
    },

    /// The app has no focused window (or no window at all).
    #[error("`{app}` has no focused window")]
    NoWindow {
        /// Name of the app.
        app: String,
    },

    /// A ref from an evicted generation was used.
    #[error("stale reference: take a new observation")]
    StaleRef,

    /// A secure text field's value was requested. It is never returned.
    #[error("the value of a secure text field is never readable")]
    SecureField,

    /// Another process holds secure input, so no key event may be posted.
    #[error("secure input is enabled by another app; no keystroke can be sent")]
    SecureInput,

    /// The session is locked; no event is ever posted into a lock screen.
    #[error("the screen is locked")]
    ScreenLocked,

    /// The app did not answer within the messaging timeout, twice.
    #[error("`{app}` is not responding to accessibility requests")]
    Unresponsive {
        /// Name or pid of the app.
        app: String,
    },

    /// An Objective-C exception was caught around an AX call cluster.
    #[error("`{app}` raised an Objective-C exception during {call}")]
    Exception {
        /// Name or pid of the app.
        app: String,
        /// The call cluster that raised.
        call: &'static str,
    },

    /// A raw `AXError` code came back from the API.
    #[error("{call} failed with AXError {code}")]
    Ax {
        /// The call that failed.
        call: &'static str,
        /// The raw `AXError` value.
        code: i32,
    },

    /// The app exposes no usable accessibility tree.
    #[error("`{app}` exposes no usable accessibility tree")]
    OpaqueApp {
        /// Name of the app.
        app: String,
    },

    /// A Chrome-family browser: the web path owns this app, not `neo-ax`.
    #[error("`{app}` is a Chrome-family browser; use the CDP observer")]
    UseCdp {
        /// Name of the app.
        app: String,
    },

    /// The requested operation is not offered on that element.
    #[error("{0}")]
    Unsupported(&'static str),

    /// A wall deadline elapsed.
    #[error("{what} timed out")]
    Timeout {
        /// What timed out.
        what: &'static str,
    },

    /// [`crate::AxHandle::stop`] was called. Every later command fails this
    /// way: a stopped actor refuses work rather than half-doing it.
    #[error("the accessibility actor was stopped")]
    Stopped,

    /// This build has no accessibility backend for the platform it is
    /// running on. Nothing is denied and no permission is missing: the code
    /// that would read a native app does not exist here yet.
    #[error(
        "no accessibility backend for {platform}: driving native apps is macOS-only in this build"
    )]
    NoBackend {
        /// `std::env::consts::OS` of the build that refused.
        platform: &'static str,
    },

    /// The session publishes no accessibility bus, so no app can be read.
    ///
    /// Distinct from [`AxError::NoBackend`]: the backend exists, the
    /// operating system's accessibility service does not. On Linux this is
    /// a missing or dead `at-spi2-core`.
    #[error("no accessibility bus on this session: {detail}")]
    NoBus {
        /// What the lookup of `org.a11y.Bus` reported.
        detail: String,
    },

    /// No window manager this build can drive.
    ///
    /// Enumerating windows and moving focus is compositor-specific and
    /// there is no portable protocol for it. A session whose compositor has
    /// no implementation here is told so by name (17 §3.2), never silently
    /// degraded into "the app has no focused window".
    #[error(
        "no supported window manager on this session: {detail}; native-app control needs one to \
         enumerate and focus windows"
    )]
    NoWindowManager {
        /// Which compositor was detected, and what was missing.
        detail: String,
    },

    /// The compositor offers no way to synthesise input.
    ///
    /// The accessibility API is always tried first; this is the refusal
    /// when an element advertises no usable action *and* the fallback is
    /// unavailable, so that "nothing happened" is never reported as
    /// success.
    #[error("the compositor offers no virtual-input protocol: {detail}")]
    NoVirtualInput {
        /// Which protocol was missing.
        detail: String,
    },
}

/// Why an observation is no longer the surface a decision was made on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaleReason {
    /// The observation's generation has been evicted.
    Generation,
    /// The element is gone and could not be relocated.
    ElementGone,
    /// The element's role changed under us.
    RoleChanged {
        /// Role at observation time.
        was: String,
        /// Role now.
        now: String,
    },
    /// The element's label changed under us.
    LabelChanged {
        /// Label at observation time.
        was: String,
        /// Label now.
        now: String,
    },
    /// The element is now disabled.
    Disabled,
    /// The element moved by more than its own size.
    Moved,
    /// Another app is frontmost.
    NotFrontmost {
        /// Pid that is frontmost now.
        pid: i32,
    },
    /// The focused window is not the observed one.
    WindowChanged,
    /// A sheet or dialog appeared that was not in the observation.
    SheetAppeared,
    /// Something else is on top of the element's centre point.
    Occluded {
        /// What the hit-test returned.
        by: String,
    },
    /// The session locked.
    ScreenLocked,
    /// The app went on the deny list between observation and execution.
    Denied {
        /// Bundle id or name of the refused app.
        app: String,
    },
}

impl fmt::Display for StaleReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Generation => f.write_str("generation evicted"),
            Self::ElementGone => f.write_str("element gone"),
            Self::RoleChanged { was, now } => write!(f, "role changed {was} -> {now}"),
            Self::LabelChanged { was, now } => write!(f, "label changed {was:?} -> {now:?}"),
            Self::Disabled => f.write_str("element disabled"),
            Self::Moved => f.write_str("element moved"),
            Self::NotFrontmost { pid } => write!(f, "app not frontmost (pid {pid} is)"),
            Self::WindowChanged => f.write_str("focused window changed"),
            Self::SheetAppeared => f.write_str("a sheet appeared"),
            Self::Occluded { by } => write!(f, "occluded by {by}"),
            Self::ScreenLocked => f.write_str("screen locked"),
            Self::Denied { app } => write!(f, "{app} is denied"),
        }
    }
}

/// The verdict of a guard check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Freshness {
    /// Every check passed; the action may be executed.
    Fresh,
    /// A check failed; re-observe and re-decide.
    Stale(StaleReason),
}

impl Freshness {
    /// True when every guard check passed.
    #[must_use]
    pub fn is_fresh(&self) -> bool {
        matches!(self, Self::Fresh)
    }

    /// The failure reason rendered for a trace line, if stale.
    #[must_use]
    pub fn reason(&self) -> Option<String> {
        match self {
            Self::Fresh => None,
            Self::Stale(r) => Some(r.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_reasons_render_one_line() {
        let cases = [
            (StaleReason::Generation, "generation evicted"),
            (StaleReason::ElementGone, "element gone"),
            (
                StaleReason::RoleChanged {
                    was: "AXButton".into(),
                    now: "AXGroup".into(),
                },
                "role changed AXButton -> AXGroup",
            ),
            (
                StaleReason::LabelChanged {
                    was: "Send".into(),
                    now: "Sending".into(),
                },
                "label changed \"Send\" -> \"Sending\"",
            ),
            (StaleReason::Disabled, "element disabled"),
            (StaleReason::Moved, "element moved"),
            (
                StaleReason::NotFrontmost { pid: 812 },
                "app not frontmost (pid 812 is)",
            ),
            (StaleReason::WindowChanged, "focused window changed"),
            (StaleReason::SheetAppeared, "a sheet appeared"),
            (
                StaleReason::Occluded {
                    by: "Finder".into(),
                },
                "occluded by Finder",
            ),
            (StaleReason::ScreenLocked, "screen locked"),
            (
                StaleReason::Denied {
                    app: "Terminal".into(),
                },
                "Terminal is denied",
            ),
        ];
        for (reason, want) in cases {
            let rendered = reason.to_string();
            assert_eq!(rendered, want);
            assert!(
                !rendered.contains('\n'),
                "reason must be one line: {rendered}"
            );
        }
    }

    #[test]
    fn freshness_exposes_reason_only_when_stale() {
        assert!(Freshness::Fresh.is_fresh());
        assert_eq!(Freshness::Fresh.reason(), None);
        let stale = Freshness::Stale(StaleReason::Disabled);
        assert!(!stale.is_fresh());
        assert_eq!(stale.reason().as_deref(), Some("element disabled"));
    }
}
