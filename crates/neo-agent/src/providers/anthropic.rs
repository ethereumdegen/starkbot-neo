//! Anthropic API-key validation: `GET {base}/v1/models` with `x-api-key` and
//! the pinned `anthropic-version` (05 §6). The credential travels in a header,
//! never in the URL.

use async_trait::async_trait;
use neo_keys::{ACCOUNT_ANTHROPIC, KeyState, KeyValidator, Secret, ValidationError};
use reqwest::Client;
use url::Url;

use super::key_check;

/// The hosted Anthropic API. `KeyBases::hosted` is the only caller; 05 §1
/// rule 3 greps for this host outside `providers/`.
pub(super) const HOSTED_BASE: &str = "https://api.anthropic.com";

/// The Messages API version Anthropic requires on every request.
pub(super) const API_VERSION: &str = "2023-06-01";

pub struct AnthropicKeyValidator {
    base_url: Url,
    client: Client,
    required_model: Option<String>,
}

impl AnthropicKeyValidator {
    /// A validator against `base_url`, which a test points at `wiremock`.
    pub fn new(base_url: Url) -> Result<Self, ValidationError> {
        Ok(Self {
            base_url,
            client: key_check::client(ACCOUNT_ANTHROPIC)?,
            required_model: None,
        })
    }

    /// Report [`KeyState::Limited`] unless the catalogue offers this model id.
    #[must_use]
    pub fn requiring(mut self, model_id: impl Into<String>) -> Self {
        self.required_model = Some(model_id.into());
        self
    }
}

#[async_trait]
impl KeyValidator for AnthropicKeyValidator {
    fn account(&self) -> &str {
        ACCOUNT_ANTHROPIC
    }

    async fn validate(&self, secret: &Secret) -> Result<KeyState, ValidationError> {
        let url = key_check::models_url(ACCOUNT_ANTHROPIC, &self.base_url)?;
        // The audited credential boundary clippy.toml points at: the secret
        // becomes one `x-api-key` header on one request to an injected base
        // URL, and is never logged, stored or returned.
        #[allow(clippy::disallowed_methods)]
        let request = self
            .client
            .get(url)
            .header("x-api-key", secret.expose())
            .header("anthropic-version", API_VERSION);
        key_check::check(ACCOUNT_ANTHROPIC, request, self.required_model.as_deref()).await
    }
}
