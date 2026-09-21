//! Reading a runtime's model catalogue and running it through the registry
//! (05 §7): one authenticated `GET {base}/v1/models`, every id classified,
//! hidden and unclassified ids dropped.
//!
//! The vendors' catalogues carry no prices, so `price` stays `None` here and a
//! call is recorded with units only until the price table lands (05 §7 source
//! 3). Nothing is invented.

use neo_core::{ModelCapabilities, ModelInfo, ModelRef, ModelUseCase, ProviderId, registry};
use neo_keys::{ACCOUNT_ANTHROPIC, ACCOUNT_OPENAI, Secret, ValidationError};

use super::anthropic;
use super::key_check::{self, KeyBases};

/// One runtime's catalogue as Starkbot understands it.
#[derive(Clone, Debug, PartialEq)]
pub struct Catalog {
    pub provider: ProviderId,
    /// Every offerable model, in catalogue order.
    pub models: Vec<ModelInfo>,
    /// Ids the vendor listed that Starkbot never offers (hide-list or
    /// unclassified, 05 §7). Kept so `neo models` can say why an id is absent.
    pub skipped: Vec<String>,
}

impl KeyBases {
    /// Fetch and classify one runtime's catalogue.
    ///
    /// Only the two API-key runtimes come through here; a subscription
    /// runtime's catalogue belongs to its helper (05 §7).
    pub async fn catalog(
        &self,
        account: &str,
        secret: &Secret,
    ) -> Result<Catalog, ValidationError> {
        let (provider, ids) = match account {
            ACCOUNT_OPENAI => (neo_core::PROVIDER_OPENAI, self.openai_ids(secret).await?),
            ACCOUNT_ANTHROPIC => (
                neo_core::PROVIDER_ANTHROPIC,
                self.anthropic_ids(secret).await?,
            ),
            other => return Err(ValidationError::Unsupported(other.to_owned())),
        };
        Ok(classify_all(provider, ids))
    }

    async fn openai_ids(&self, secret: &Secret) -> Result<Vec<String>, ValidationError> {
        let url = key_check::models_url(ACCOUNT_OPENAI, &self.openai)?;
        // The audited credential boundary clippy.toml points at: one bearer
        // header on one request to an injected base URL.
        #[allow(clippy::disallowed_methods)]
        let request = key_check::client(ACCOUNT_OPENAI)?
            .get(url)
            .bearer_auth(secret.expose());
        key_check::model_ids(ACCOUNT_OPENAI, request).await
    }

    async fn anthropic_ids(&self, secret: &Secret) -> Result<Vec<String>, ValidationError> {
        let url = key_check::models_url(ACCOUNT_ANTHROPIC, &self.anthropic)?;
        // The audited credential boundary clippy.toml points at: one
        // `x-api-key` header on one request to an injected base URL.
        #[allow(clippy::disallowed_methods)]
        let request = key_check::client(ACCOUNT_ANTHROPIC)?
            .get(url)
            .header("x-api-key", secret.expose())
            .header("anthropic-version", anthropic::API_VERSION);
        key_check::model_ids(ACCOUNT_ANTHROPIC, request).await
    }
}

/// Turn raw ids into models, dropping everything Starkbot never offers.
#[must_use]
pub fn classify_all(provider: &str, ids: Vec<String>) -> Catalog {
    let mut models = Vec::new();
    let mut skipped = Vec::new();
    for id in ids {
        match registry::classify(provider, &id) {
            Some(classified) => {
                let capabilities = capabilities(&classified.use_cases);
                models.push(ModelInfo {
                    reference: ModelRef::new(ProviderId::new(provider), classified.id),
                    use_cases: classified.use_cases,
                    capabilities,
                    price: None,
                    deprecated: false,
                });
            }
            None => skipped.push(id),
        }
    }
    Catalog {
        provider: ProviderId::new(provider),
        models,
        skipped,
    }
}

/// What a use case implies about a model, until a runtime reports capabilities
/// of its own (05 §7): an inference-family model reasons, calls tools, takes
/// images and streams; a speech model only streams.
fn capabilities(use_cases: &[ModelUseCase]) -> ModelCapabilities {
    let inference = use_cases
        .iter()
        .any(|use_case| matches!(use_case, ModelUseCase::Inference | ModelUseCase::TextHelper));
    ModelCapabilities {
        reasoning: inference,
        tools: inference,
        image_input: inference,
        streaming: true,
    }
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn a_catalog_drops_what_starkbot_never_offers() {
        let catalog = classify_all(
            neo_core::PROVIDER_OPENAI,
            vec![
                "gpt-5.6-sol".to_owned(),
                "whisper-1".to_owned(),
                "gpt-4.1".to_owned(),
                "gpt-transcribe".to_owned(),
            ],
        );
        let offered: Vec<&str> = catalog
            .models
            .iter()
            .map(|model| model.reference.id.as_str())
            .collect();
        assert_eq!(offered, vec!["gpt-5.6-sol", "gpt-transcribe"]);
        assert_eq!(catalog.skipped, vec!["whisper-1", "gpt-4.1"]);
    }

    #[test]
    fn a_speech_model_claims_no_reasoning() {
        let catalog = classify_all(
            neo_core::PROVIDER_OPENAI,
            vec!["gpt-transcribe".to_owned(), "gpt-5.6-sol".to_owned()],
        );
        let speech = &catalog.models[0];
        assert!(!speech.capabilities.reasoning);
        assert!(!speech.capabilities.tools);
        let inference = &catalog.models[1];
        assert!(inference.capabilities.reasoning);
    }
}
