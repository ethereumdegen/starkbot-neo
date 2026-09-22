//! The one error type that crosses the bridge (04 §14).
//!
//! `RuntimeError`'s `Display` is the redaction boundary: every variant that
//! could have held a credential (`Secret`, `Keychain`, `Oauth`) is written to
//! name the account or the failure, never the value. Nothing here formats a
//! source with `{:?}`, and nothing here reads a secret.

use neo_agent::agent::{AgentError, ToolError};
use neo_agent::ax::AxError;
use neo_agent::screen::ScreenBusy;
use neo_agent::{ProjectError, RuntimeError};
use neo_core::CoreError;
use neo_eval::EvalError;
use neo_store::StoreError;
use serde::Serialize;
use ts_rs::TS;

use crate::view::Fix;

/// The Keychain account the navigator cannot run without (K6).
const JEV_ACCOUNT: &str = "typesafe";

/// What every "stopped on purpose" failure reports, so a front end can style
/// a stopped run as stopped rather than broken without matching on prose.
pub const CANCELLED: &str = "cancelled";

#[derive(Clone, Debug, Serialize, TS)]
pub struct UiError {
    pub code: String,
    pub message: String,
    /// What the user can press to get out of this, when the screen has a
    /// control for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fix: Option<Fix>,
}

impl UiError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
            fix: None,
        }
    }

    #[must_use]
    pub fn with_fix(mut self, fix: Fix) -> Self {
        self.fix = Some(fix);
        self
    }
}

impl From<RuntimeError> for UiError {
    fn from(error: RuntimeError) -> Self {
        let (code, fix) = match &error {
            RuntimeError::Oauth(_) => ("oauth", None),
            RuntimeError::Keychain(_) => ("keychain", None),
            RuntimeError::Secret(_) => ("secret", None),
            RuntimeError::Validation(_) => ("validation", None),
            RuntimeError::MissingKey(account) => (
                "missing_key",
                Some(Fix::SetKey {
                    account: account.clone(),
                }),
            ),
            RuntimeError::RuntimeUnavailable(_) => {
                ("runtime_unavailable", Some(Fix::ChooseRuntime))
            }
            // Another Starkbot on this laptop holds the keyboard, an app or
            // the managed profile. Its own code because it is not a failure
            // of the request: the screen says who has it and the user either
            // waits or stops the other one, and no control here can.
            RuntimeError::Busy { .. } => ("busy", None),
            // A settings pane has to tell "that value is not allowed" from
            // "the database is broken": the first belongs on the field the
            // user typed into, the second is not their problem at all. The
            // store validates the whole of `Settings` before committing, so
            // a rejected patch arrives wrapped in a `Store` error and would
            // otherwise read as a database failure.
            RuntimeError::Store(
                StoreError::InvalidSettings(_) | StoreError::InvalidSettingsShape,
            ) => ("validation", None),
            RuntimeError::Store(StoreError::UnknownSettingsSection(_)) => ("unknown_section", None),
            RuntimeError::Store(_) => ("store", None),
            _ => ("runtime", None),
        };
        Self {
            code: code.to_owned(),
            message: error.to_string(),
            fix,
        }
    }
}
impl From<ProjectError> for UiError {
    fn from(error: ProjectError) -> Self {
        if let ProjectError::Runtime(inner) = error {
            return Self::from(inner);
        }
        let code = match &error {
            ProjectError::Busy => "heartbeat_busy",
            ProjectError::InvalidName
            | ProjectError::InvalidRoot(_)
            | ProjectError::InvalidDocument => "validation",
            ProjectError::Store(_) => "store",
            ProjectError::Agent(_) => "agent",
            ProjectError::Io { .. } => "io",
            ProjectError::Runtime(_) => unreachable!("handled above"),
        };
        Self::new(code, error.to_string())
    }
}

/// A command ran on a blocking thread and the thread itself failed.
impl From<tokio::task::JoinError> for UiError {
    fn from(error: tokio::task::JoinError) -> Self {
        Self::new("join", format!("a background task failed: {error}"))
    }
}

/// A run the user stopped, a missing key, or a grant macOS has not given.
///
/// Cancellation gets its own code because the window must not paint a red
/// banner over work the user stopped themselves — and the accessibility
/// grant gets a `Manual` fix because no control in this app can award it:
/// only System Settings can, and the deep link is the most help there is.
impl From<ToolError> for UiError {
    fn from(error: ToolError) -> Self {
        match error {
            ToolError::Cancelled(CoreError::Cancelled) => Self::new(CANCELLED, error.to_string()),
            ToolError::MissingJevKey => {
                Self::new("missing_key", error.to_string()).with_fix(Fix::SetKey {
                    account: JEV_ACCOUNT.to_owned(),
                })
            }
            ToolError::NotTrusted => {
                Self::new("not_trusted", error.to_string()).with_fix(Fix::Manual {
                    detail: neo_ax::ACCESSIBILITY_SETTINGS_URL.to_owned(),
                })
            }
            ToolError::ScreenBusy(ref busy) => screen_busy(busy),
            ToolError::Runtime(inner) => Self::from(*inner),
            // The user has no runtime that can write a field value, which is
            // a settings choice they can make from this window: point at the
            // setting rather than at a shell command the desktop app has no
            // terminal for.
            ToolError::NoTextHelper => {
                Self::new("no_text_helper", error.to_string()).with_fix(Fix::Manual {
                    detail: "Settings → Models → inference runtime".to_owned(),
                })
            }
            ToolError::Browser(_)
            | ToolError::App { .. }
            | ToolError::Shell(_)
            | ToolError::Cancelled(_) => Self::new("tool", error.to_string()),
        }
    }
}

impl From<AgentError> for UiError {
    fn from(error: AgentError) -> Self {
        if error.is_cancelled() {
            return Self::new(CANCELLED, error.to_string());
        }
        match error {
            AgentError::Runtime(inner) => Self::from(*inner),
            AgentError::Tool(inner) => Self::from(inner),
            // The one agent failure a window can act on: the key the
            // selected path needs is not stored, and the Connections screen
            // is one press away. Everything else is the model or the graph
            // failing, which no control here can repair.
            AgentError::NoKey(account) => {
                Self::new("missing_key", error.to_string()).with_fix(Fix::SetKey {
                    account: (*account).to_owned(),
                })
            }
            AgentError::Request(_) | AgentError::Graph(_) | AgentError::Core(_) => {
                Self::new("agent", error.to_string())
            }
        }
    }
}

impl From<AxError> for UiError {
    fn from(error: AxError) -> Self {
        let code = match &error {
            AxError::Cancelled(CoreError::Cancelled) => CANCELLED,
            AxError::NotTrusted => {
                return Self::new("not_trusted", error.to_string()).with_fix(Fix::Manual {
                    detail: neo_ax::ACCESSIBILITY_SETTINGS_URL.to_owned(),
                });
            }
            // The surface moved under an index the caller took from an older
            // table. Its own code, because the fix is "observe again", not
            // "something is broken".
            AxError::ScreenBusy(busy) => return screen_busy(busy),
            AxError::Stale(_) => "stale",
            AxError::NoRow(_) | AxError::UnknownKey(_) => "bad_request",
            AxError::Spawn(_) | AxError::Ax(_) | AxError::Cancelled(_) => "ax",
        };
        Self::new(code, error.to_string())
    }
}

/// A suite that could not be measured. A *failing* case is not this: it comes
/// back inside the report.
impl From<EvalError> for UiError {
    fn from(error: EvalError) -> Self {
        match error {
            EvalError::Cancelled { .. } => Self::new(CANCELLED, error.to_string()),
            EvalError::NoInference { .. } => {
                Self::new("no_inference", error.to_string()).with_fix(Fix::ChooseRuntime)
            }
            // The same repair as any other missing Jev key: the window has a
            // field for it, so the suite's refusal points at the control
            // rather than at the `neo keys set typesafe` the message names.
            EvalError::NoJudge { .. } => {
                Self::new("missing_key", error.to_string()).with_fix(Fix::SetKey {
                    account: JEV_ACCOUNT.to_owned(),
                })
            }
            EvalError::NoCases => Self::new("no_cases", error.to_string()),
            EvalError::ScreenBusy(ref busy) => screen_busy(busy),
            EvalError::Runtime(inner) => Self::from(inner),
        }
    }
}

/// One code for "somebody else has the keyboard", wherever it surfaces from.
///
/// Its own code rather than a generic failure because the window should offer
/// to stop the other run, not paint an error: nothing is broken. The message
/// already names the holder, so the front end needs no second lookup.
fn screen_busy(busy: &ScreenBusy) -> UiError {
    UiError::new("screen_busy", busy.to_string())
}
