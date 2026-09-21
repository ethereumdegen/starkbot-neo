//! What the two API-key validators share: how a model-list response becomes a
//! [`KeyState`], and where each one calls.

use std::sync::Arc;
use std::time::Duration;

use neo_keys::{ACCOUNT_ANTHROPIC, ACCOUNT_OPENAI, KeyState, KeyValidator, ValidationError};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use url::Url;

use neo_judge::TypeSafeKeyValidator;

use super::anthropic::{self, AnthropicKeyValidator};
use super::openai::{self, OpenAiKeyValidator};

/// How long a validation call may take before the key is reported
/// [`KeyState::Unchecked`].
pub(super) const TIMEOUT: Duration = Duration::from_secs(10);

/// The `{ "data": [{ "id": … }] }` envelope both vendors return for a model
/// list. Unknown fields are ignored on purpose: a catalogue gains fields.
#[derive(Deserialize)]
struct ModelList {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

pub(super) fn client(account: &str) -> Result<Client, ValidationError> {
    Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|error| ValidationError::transport(account, error))
}

pub(super) fn models_url(account: &str, base_url: &Url) -> Result<Url, ValidationError> {
    // `join` replaces the last path segment unless the base ends in `/`, which
    // would silently drop a base like `http://127.0.0.1:1234/proxy`.
    let mut url = base_url.clone();
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|()| ValidationError::endpoint(account, "base URL cannot have a path"))?;
        segments.pop_if_empty().extend(["v1", "models"]);
    }
    Ok(url)
}

/// The validator table from 05 §6, shared by both API-key vendors: `200` is a
/// working key, `200` without the model the configured use case needs is a key
/// that cannot do the job, `401` (and `403`, which both vendors use for a
/// revoked or unauthorised key) is a bad key, and anything else — including
/// every transport failure — leaves the key unjudged.
pub fn model_list_state(
    account: &str,
    status: StatusCode,
    body: &str,
    required_model: Option<&str>,
) -> Result<KeyState, ValidationError> {
    match status {
        StatusCode::OK => {
            let Some(required) = required_model else {
                return Ok(KeyState::Present);
            };
            let list: ModelList = serde_json::from_str(body)
                .map_err(|error| ValidationError::protocol(account, error))?;
            if list.data.iter().any(|entry| entry.id == required) {
                Ok(KeyState::Present)
            } else {
                Ok(KeyState::Limited)
            }
        }
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Ok(KeyState::Invalid),
        _ => Ok(KeyState::Unchecked),
    }
}

/// Runs one authenticated `GET`, with every reachability failure folded into
/// [`KeyState::Unchecked`] so an offline user is never told their key is bad.
pub(super) async fn check(
    account: &str,
    request: reqwest::RequestBuilder,
    required_model: Option<&str>,
) -> Result<KeyState, ValidationError> {
    let response = match request.send().await {
        Ok(response) => response,
        Err(_) => return Ok(KeyState::Unchecked),
    };
    let status = response.status();
    let body = match response.text().await {
        Ok(body) => body,
        Err(_) => return Ok(KeyState::Unchecked),
    };
    model_list_state(account, status, &body, required_model)
}

/// Every id one runtime's catalogue lists, in catalogue order.
///
/// Unlike a key check, a catalogue read reports its failures: a caller asking
/// "what models are there" must not be told "none" because the network was
/// down (05 §7 — the cached `models` table serves until a refresh lands).
pub(super) async fn model_ids(
    account: &str,
    request: reqwest::RequestBuilder,
) -> Result<Vec<String>, ValidationError> {
    let response = request
        .send()
        .await
        .map_err(|error| ValidationError::transport(account, error))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| ValidationError::transport(account, error))?;
    if status != StatusCode::OK {
        return Err(ValidationError::protocol(
            account,
            format_args!("the model list answered HTTP {}", status.as_u16()),
        ));
    }
    let list: ModelList =
        serde_json::from_str(&body).map_err(|error| ValidationError::protocol(account, error))?;
    Ok(list.data.into_iter().map(|entry| entry.id).collect())
}

/// Where each API-key validator calls. The default is the hosted vendor; a
/// test swaps in a `wiremock` base so no check ever leaves the machine (08
/// rule 1: `base_url` is always injected).
///
/// `typesafe` is absent on purpose: 05 §1 gives the TypeSafe validator to
/// `neo-judge`, which owns that endpoint.
#[derive(Clone, Debug)]
pub struct KeyBases {
    pub openai: Url,
    pub anthropic: Url,
    /// Where a TypeSafe check pings (A6). `jev-nav` owns the request; the
    /// validator lives in `neo-judge` (05 §1).
    pub typesafe: Url,
}

impl KeyBases {
    /// The hosted vendor endpoints, unless `NEO_OPENAI_BASE` names somewhere
    /// else.
    ///
    /// The override exists for the development loop: the whole chat path —
    /// streamed tokens, tool calls, steering, an interrupt mid-answer — can
    /// then be exercised against a local stand-in without a vendor key and
    /// without spending anything. A shipped Starkbot does not set it and
    /// talks to OpenAI. An unparseable value is ignored rather than fatal:
    /// a typo in a shell profile must not stop the app from starting.
    #[allow(clippy::expect_used)]
    pub fn hosted() -> Self {
        let openai = std::env::var("NEO_OPENAI_BASE")
            .ok()
            .and_then(|base| Url::parse(&base).ok())
            .unwrap_or_else(|| Url::parse(openai::HOSTED_BASE).expect("a valid literal base URL"));
        Self {
            openai,
            anthropic: Url::parse(anthropic::HOSTED_BASE).expect("a valid literal base URL"),
            typesafe: Url::parse(crate::nav::TYPESAFE_ENDPOINT).expect("a valid literal base URL"),
        }
    }

    /// Every base pointed at one server, for tests.
    pub fn all(base: Url) -> Self {
        Self {
            openai: base.clone(),
            anthropic: base.clone(),
            typesafe: base,
        }
    }

    /// The validator for `account`, requiring `required_model` of its
    /// catalogue when the user's configuration names one. `None` means no
    /// validator in this crate can judge that account.
    pub fn validator(
        &self,
        account: &str,
        required_model: Option<String>,
    ) -> Result<Option<Arc<dyn KeyValidator>>, ValidationError> {
        let validator: Arc<dyn KeyValidator> = match account {
            ACCOUNT_OPENAI => {
                let validator = OpenAiKeyValidator::new(self.openai.clone())?;
                Arc::new(require(
                    validator,
                    required_model,
                    OpenAiKeyValidator::requiring,
                ))
            }
            ACCOUNT_ANTHROPIC => {
                let validator = AnthropicKeyValidator::new(self.anthropic.clone())?;
                Arc::new(require(
                    validator,
                    required_model,
                    AnthropicKeyValidator::requiring,
                ))
            }
            neo_keys::ACCOUNT_TYPESAFE => Arc::new(TypeSafeKeyValidator::new(
                self.typesafe.as_str(),
                crate::nav::TYPESAFE_MODEL,
            )),
            _ => return Ok(None),
        };
        Ok(Some(validator))
    }
}

impl Default for KeyBases {
    fn default() -> Self {
        Self::hosted()
    }
}

fn require<V>(validator: V, required_model: Option<String>, with: fn(V, String) -> V) -> V {
    match required_model {
        Some(model) => with(validator, model),
        None => validator,
    }
}

/// The model id a check should require of `account`, given the user's
/// configuration: only the inference use case the account actually powers.
pub(crate) fn required_model(settings: &neo_core::Settings, account: &str) -> Option<String> {
    let inference = &settings.models.inference;
    let provider = match account {
        ACCOUNT_OPENAI => neo_core::PROVIDER_OPENAI,
        ACCOUNT_ANTHROPIC => neo_core::PROVIDER_ANTHROPIC,
        _ => return None,
    };
    if inference.provider.as_str() == provider {
        Some(inference.id.clone())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn a_catalog_without_the_required_model_is_limited() {
        let body = r#"{"data":[{"id":"gpt-4.1"},{"id":"gpt-5.6-luna"}]}"#;
        let state = model_list_state(ACCOUNT_OPENAI, StatusCode::OK, body, Some("sol-latest"))
            .expect("readable body");
        assert_eq!(state, KeyState::Limited);
    }

    #[test]
    fn the_required_model_makes_the_key_present() {
        let body = r#"{"data":[{"id":"sol-latest"}]}"#;
        let state = model_list_state(ACCOUNT_OPENAI, StatusCode::OK, body, Some("sol-latest"))
            .expect("readable body");
        assert_eq!(state, KeyState::Present);
    }

    #[test]
    fn with_nothing_required_any_catalog_is_present() {
        let state = model_list_state(ACCOUNT_OPENAI, StatusCode::OK, "not json", None)
            .expect("body is not read");
        assert_eq!(state, KeyState::Present);
    }

    #[test]
    fn unauthorised_and_forbidden_are_invalid() {
        for status in [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN] {
            let state = model_list_state(ACCOUNT_ANTHROPIC, status, "", None).expect("no body");
            assert_eq!(state, KeyState::Invalid);
        }
    }

    #[test]
    fn a_server_fault_leaves_the_key_unjudged() {
        let state = model_list_state(
            ACCOUNT_ANTHROPIC,
            StatusCode::INTERNAL_SERVER_ERROR,
            "",
            None,
        )
        .expect("no body");
        assert_eq!(state, KeyState::Unchecked);
    }

    #[test]
    fn an_unreadable_catalog_is_a_protocol_error() {
        let error = model_list_state(ACCOUNT_OPENAI, StatusCode::OK, "{", Some("sol-latest"))
            .expect_err("truncated JSON");
        assert!(matches!(error, ValidationError::Protocol { .. }));
    }

    #[test]
    fn every_core_account_has_a_validator_and_nothing_else_does() {
        let bases = KeyBases::hosted();
        for account in [
            ACCOUNT_OPENAI,
            ACCOUNT_ANTHROPIC,
            neo_keys::ACCOUNT_TYPESAFE,
        ] {
            assert!(
                bases
                    .validator(account, None)
                    .expect("a validator builds")
                    .is_some(),
                "{account} must be validatable"
            );
        }
        // A pack's own credential is validated by the pack's own declaration
        // (05 §6), not by anything in this crate.
        assert!(
            bases
                .validator("some-pack-key", None)
                .expect("no validator is not an error")
                .is_none()
        );
    }
}
