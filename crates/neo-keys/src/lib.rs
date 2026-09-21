#![forbid(unsafe_code)]

mod accounts;
mod keychain;

use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

pub use accounts::{
    ACCOUNT_ANTHROPIC, ACCOUNT_OPENAI, ACCOUNT_TYPESAFE, CORE_ACCOUNTS, env_var, from_env,
};
pub use keychain::{
    DEFAULT_SERVICE, KEYCHAIN_BACKEND_ENV, KEYCHAIN_FILE_ENV, Keychain, KeychainError,
    NO_KEYRING_WARNING, os_keyring_available, wanted_file_backend,
};

/// State of one Starkbot-owned credential.
///
/// `Missing`, `Present` and `Invalid` describe what is in the Keychain;
/// `Unchecked` and `Limited` describe what a vendor validator later learned
/// about a key that is present.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum KeyState {
    Missing,
    Present,
    Invalid,
    Unchecked,
    Limited,
}

/// Where a secret came from.
///
/// The Keychain is authoritative; the environment is a development fallback
/// only (05 §6). Reporting the source lets the UI say "this key is coming from
/// your shell, not from the Keychain" without ever naming the value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum KeySource {
    Keychain,
    Environment,
}

/// One key account and the state of the credential stored under it.
///
/// `source` is absent whenever there is nothing to source — a missing key, or
/// a state reported before the secret was read — and is omitted from the wire
/// payload in that case.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyStatus {
    pub account: String,
    pub state: KeyState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<KeySource>,
}

impl KeyStatus {
    /// A status with no known source.
    pub fn new(account: impl Into<String>, state: KeyState) -> Self {
        Self::with_source(account, state, None)
    }

    pub fn with_source(
        account: impl Into<String>,
        state: KeyState,
        source: Option<KeySource>,
    ) -> Self {
        Self {
            account: account.into(),
            state,
            source,
        }
    }
}

/// Why a validator could not turn a secret into a [`KeyState`].
///
/// Every field is a short diagnostic the validator wrote itself: an account
/// name and a message about the endpoint or the response shape. No variant can
/// hold a [`Secret`], a request header or a response body, so no error on this
/// type can carry key material into a log or a UI.
///
/// A transport failure is normally *not* an error at all — a validator maps it
/// to [`KeyState::Unchecked`], because an offline user must not be told their
/// key is bad (05 §6).
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("the `{account}` validator was given an endpoint it cannot use: {detail}")]
    Endpoint { account: String, detail: String },
    #[error("could not reach the `{account}` validation endpoint: {detail}")]
    Transport { account: String, detail: String },
    #[error("the `{account}` validation endpoint answered in an unreadable shape: {detail}")]
    Protocol { account: String, detail: String },
    #[error("no validator is registered for account `{0}`")]
    Unsupported(String),
}

impl ValidationError {
    pub fn endpoint(account: impl Into<String>, detail: impl fmt::Display) -> Self {
        Self::Endpoint {
            account: account.into(),
            detail: detail.to_string(),
        }
    }

    pub fn transport(account: impl Into<String>, detail: impl fmt::Display) -> Self {
        Self::Transport {
            account: account.into(),
            detail: detail.to_string(),
        }
    }

    pub fn protocol(account: impl Into<String>, detail: impl fmt::Display) -> Self {
        Self::Protocol {
            account: account.into(),
            detail: detail.to_string(),
        }
    }
}

/// One authenticated call that decides what a stored secret is worth.
///
/// The trait lives here; every implementation lives in the crate that owns the
/// vendor's endpoint (`neo-agent::providers`, `neo-judge`, `neo-packs`), which
/// is what keeps `neo-keys` free of HTTP and of vendor URLs (05 §1, §6). An
/// implementation is constructed with its `base_url` injected, so tests point
/// it at `wiremock` (08).
#[async_trait]
pub trait KeyValidator: Send + Sync {
    /// The account this validator checks, e.g. [`ACCOUNT_OPENAI`].
    fn account(&self) -> &str;

    /// Check one secret. Reachability problems are reported as
    /// [`KeyState::Unchecked`], not as an error and never as
    /// [`KeyState::Invalid`].
    async fn validate(&self, secret: &Secret) -> Result<KeyState, ValidationError>;
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SecretError {
    #[error("a secret cannot be empty")]
    Empty,
}

/// Process-local secret material.
///
/// The inner value is intentionally private, is never serializable, and is
/// zeroized on drop. Share a secret with `Arc<Secret>` rather than cloning it.
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Result<Self, SecretError> {
        let value = value.into();
        if value.is_empty() {
            return Err(SecretError::Empty);
        }
        Ok(Self(value))
    }

    /// Expose secret bytes only at an audited provider credential boundary.
    pub fn expose(&self) -> &str {
        &self.0
    }

    fn last_four(&self) -> String {
        let mut chars = self.0.chars().rev().take(4).collect::<Vec<_>>();
        chars.reverse();
        chars.into_iter().collect()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Secret(•••• {})", self.last_four())
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_secrets() {
        assert!(matches!(Secret::new(""), Err(SecretError::Empty)));
    }

    #[test]
    fn formatting_is_redacted() {
        let secret = Secret::new("sk-example-123456").unwrap_or_else(|error| panic!("{error}"));
        let debug = format!("{secret:?}");
        let display = secret.to_string();

        assert_eq!(debug, "Secret(•••• 3456)");
        assert_eq!(display, debug);
        assert!(!debug.contains("sk-example"));
    }

    #[test]
    fn key_status_without_a_source_keeps_its_wire_shape() {
        let status = KeyStatus::new(ACCOUNT_OPENAI, KeyState::Present);
        let json = serde_json::to_value(&status).unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(
            json,
            serde_json::json!({ "account": "openai", "state": "present" })
        );

        let decoded: KeyStatus =
            serde_json::from_value(json).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(decoded, status);
        assert_eq!(decoded.source, None);
    }

    #[test]
    fn key_status_round_trips_its_source() {
        let status = KeyStatus::with_source(
            ACCOUNT_ANTHROPIC,
            KeyState::Limited,
            Some(KeySource::Environment),
        );
        let json = serde_json::to_value(&status).unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(
            json,
            serde_json::json!({
                "account": "anthropic",
                "state": "limited",
                "source": "environment",
            })
        );

        let decoded: KeyStatus =
            serde_json::from_value(json).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(decoded, status);
    }

    #[test]
    fn validation_errors_name_only_the_account() {
        let error = ValidationError::protocol(ACCOUNT_OPENAI, "missing `data` array");
        assert_eq!(
            error.to_string(),
            "the `openai` validation endpoint answered in an unreadable shape: missing `data` array"
        );
        assert_eq!(
            ValidationError::endpoint(ACCOUNT_ANTHROPIC, "cannot be a base").to_string(),
            "the `anthropic` validator was given an endpoint it cannot use: cannot be a base"
        );
    }
}
