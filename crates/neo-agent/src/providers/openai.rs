//! OpenAI API-key validation: `GET {base}/v1/models` with a bearer token
//! (05 §6). The credential travels in a header, never in the URL.

use async_trait::async_trait;
use neo_keys::{ACCOUNT_OPENAI, KeyState, KeyValidator, Secret, ValidationError};
use reqwest::Client;
use url::Url;

use super::key_check;

/// The hosted OpenAI API. `KeyBases::hosted` is the only caller; 05 §1 rule 3
/// greps for this host outside `providers/`.
pub(super) const HOSTED_BASE: &str = "https://api.openai.com";

pub struct OpenAiKeyValidator {
    base_url: Url,
    client: Client,
    required_model: Option<String>,
}

impl OpenAiKeyValidator {
    /// A validator against `base_url`, which a test points at `wiremock`.
    pub fn new(base_url: Url) -> Result<Self, ValidationError> {
        Ok(Self {
            base_url,
            client: key_check::client(ACCOUNT_OPENAI)?,
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
impl KeyValidator for OpenAiKeyValidator {
    fn account(&self) -> &str {
        ACCOUNT_OPENAI
    }

    async fn validate(&self, secret: &Secret) -> Result<KeyState, ValidationError> {
        let url = key_check::models_url(ACCOUNT_OPENAI, &self.base_url)?;
        // The audited credential boundary clippy.toml points at: the secret
        // becomes one `Authorization` header on one request to an injected
        // base URL, and is never logged, stored or returned.
        #[allow(clippy::disallowed_methods)]
        let request = self.client.get(url).bearer_auth(secret.expose());
        key_check::check(ACCOUNT_OPENAI, request, self.required_model.as_deref()).await
    }
}
