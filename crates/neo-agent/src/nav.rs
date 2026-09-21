//! Wiring the navigator to the façade: where the TypeSafe credential comes
//! from, and who types field values.
//!
//! `jev-nav` depends on no `neo-*` crate (05 §1), so the adapters live here:
//! `Runtime` supplies the Jev client from the `typesafe` key and a
//! [`jev_nav::text::TextHelper`] backed by whichever inference runtime the user
//! selected (K6) — which is what lets a Claude-subscription-only user fill in a
//! form.

use std::sync::Arc;

use jev_nav::text::{TextError, TextHelper, TextValue};
use jev_nav::wire::TypeSafe;
use neo_core::Settings;
use neo_core::registry::ModelTier;
use serde_json::{Value, json};

use crate::runtime::{Runtime, RuntimeError};

/// The TypeSafe endpoint Jev answers on (A6). Overridable for a fixture
/// server; the default is the vendor's.
pub const TYPESAFE_ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
/// The model id every Jev request asks for until settings carry one.
pub const TYPESAFE_MODEL: &str = "jev-latest";

/// The schema a text-helper answer must match: exactly one `text` field.
///
/// Same contract the OpenAI helper enforces by hand, expressed once so a
/// strict-JSON runtime can enforce it for us.
fn text_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "text": { "type": "string" } },
        "required": ["text"],
        "additionalProperties": false
    })
}

/// A text helper that asks the selected inference runtime for the value.
pub struct RuntimeTextHelper {
    runtime: Arc<Runtime>,
    model: Option<String>,
}

impl RuntimeTextHelper {
    /// `model` is the text-helper model from settings; `None` leaves the
    /// runtime's own default.
    pub fn new(runtime: Arc<Runtime>, model: Option<String>) -> Self {
        Self { runtime, model }
    }
}

#[async_trait::async_trait]
impl TextHelper for RuntimeTextHelper {
    async fn value(&self, context: &Value) -> Result<TextValue, TextError> {
        let started = std::time::Instant::now();
        let prompt = format!(
            "{}\n\nField context:\n{}",
            jev_nav::rules::TEXT_VALUE,
            context
        );
        let (answer, turn) = self
            .runtime
            .ask_json(&prompt, &text_schema(), self.model.as_deref())
            .await
            .map_err(|error| match error {
                RuntimeError::RuntimeUnavailable(_) => TextError::Unconfigured,
                other => TextError::Request(other.to_string()),
            })?;
        let text = answer
            .get("text")
            .and_then(Value::as_str)
            .ok_or(TextError::Invalid)?;
        if text.trim().is_empty() || text.chars().count() > 2000 {
            return Err(TextError::Invalid);
        }
        Ok(TextValue {
            text: text.to_owned(),
            latency: started.elapsed(),
            usage: turn.usage,
        })
    }
}

impl Runtime {
    /// The Jev client, built from the stored `typesafe` key (05 §6, A6).
    ///
    /// The credential is read here and handed to the client; it never reaches
    /// settings, an event or a log.
    pub fn jev(&self) -> Result<TypeSafe, RuntimeError> {
        let secret = self
            .typesafe_secret()?
            .ok_or_else(|| RuntimeError::MissingKey(neo_keys::ACCOUNT_TYPESAFE.to_owned()))?;
        let endpoint =
            std::env::var("TYPESAFE_ENDPOINT").unwrap_or_else(|_| TYPESAFE_ENDPOINT.to_owned());
        let model = std::env::var("TYPESAFE_MODEL").unwrap_or_else(|_| TYPESAFE_MODEL.to_owned());
        // The audited credential boundary clippy.toml points at: the key goes
        // into one client that sends it as a bearer header.
        #[allow(clippy::disallowed_methods)]
        Ok(TypeSafe::new(secret.expose(), endpoint, model))
    }

    /// A text helper on the selected runtime, or `None` when no runtime can
    /// answer — in which case a run refuses to start rather than dying at
    /// the first `TYPE_TEXT` (16 §4, B5).
    pub fn text_helper(self: &Arc<Self>, settings: &Settings) -> Option<Box<dyn TextHelper>> {
        let selected = settings.models.inference.provider.as_str();
        if !crate::runtime::routes_inference(selected) {
            return None;
        }
        // A catalogue this runtime has never fetched is not a failure: it is
        // the fresh install, and `helper_model` answers `None` for it.
        let catalogue: Vec<String> = self
            .models(selected)
            .unwrap_or_default()
            .into_iter()
            .map(|model| model.info.reference.id)
            .collect();
        let model = helper_model(selected, &settings.models.text_helper, &catalogue);
        Some(Box::new(RuntimeTextHelper::new(Arc::clone(self), model)))
    }
}

/// Which vendor's id vocabulary a runtime speaks.
///
/// The registry classifies ids per vendor (05 §7), and all three runtimes
/// that can answer a text-helper request are fronts for one of the two
/// vendors' families: the Claude subscription and the Claude OAuth runtime
/// both serve `claude-*` ids, the ChatGPT runtime serves `gpt-*`. Without
/// this mapping the tier lookup answers `None` for every runtime that can
/// actually type.
fn vendor_of(selected: &str) -> Option<&'static str> {
    match selected {
        neo_core::PROVIDER_ANTHROPIC_OAUTH | neo_core::PROVIDER_CLAUDE_SUBSCRIPTION => {
            Some(neo_core::PROVIDER_ANTHROPIC)
        }
        neo_core::PROVIDER_OPENAI_CODEX => Some(neo_core::PROVIDER_OPENAI),
        _ => None,
    }
}

/// The model that fills a field in, given what the user saved and what the
/// selected runtime's catalogue offers.
///
/// A field value is a sentence, not a plan, and the helper is called once
/// per fill: riding the Sol-class model the conversation uses costs a
/// multiple of the Luna-class tier for the same answer (10 §5, 16 §4). So an
/// explicitly saved `models.text_helper` wins, but only when it names the
/// runtime that will serve it — an id from the other vendor's catalogue is
/// rejected by the API, not silently translated. Otherwise the newest
/// evergreen Luna-tier id the runtime offers.
///
/// `None` means "let the runtime choose", which is the Sol-class default: a
/// subscription runtime has no catalogue until one is fetched, and refusing
/// to type until then would be worse than one expensive fill.
fn helper_model(
    selected: &str,
    saved: &neo_core::ModelRef,
    catalogue: &[String],
) -> Option<String> {
    if saved.provider.as_str() == selected {
        return Some(saved.id.clone());
    }
    neo_core::registry::resolve_latest(vendor_of(selected)?, ModelTier::Luna, catalogue)
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn the_answer_schema_allows_exactly_one_field() {
        let schema = text_schema();
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(schema["required"], json!(["text"]));
    }

    #[test]
    fn no_helper_exists_until_a_runtime_can_answer() {
        let runtime = Arc::new(
            Runtime::open(
                tempfile::tempdir()
                    .expect("a temporary data directory")
                    .path(),
            )
            .expect("the store opens"),
        );
        let mut settings = Settings::default();
        assert!(
            runtime.text_helper(&settings).is_none(),
            "the default runtime cannot type yet"
        );

        settings.models.inference.provider =
            neo_core::ProviderId::new(neo_core::PROVIDER_CLAUDE_SUBSCRIPTION);
        assert!(runtime.text_helper(&settings).is_some());

        // Both subscription-OAuth runtimes answer `ask_json` (A25), so both
        // can type a field value. A helper offered for a runtime that then
        // refuses would stop a run at the first `TYPE_TEXT` for no reason.
        for provider in [
            neo_core::PROVIDER_ANTHROPIC_OAUTH,
            neo_core::PROVIDER_OPENAI_CODEX,
        ] {
            settings.models.inference.provider = neo_core::ProviderId::new(provider);
            assert!(
                runtime.text_helper(&settings).is_some(),
                "{provider} can answer, so it can type"
            );
        }

        // An API-key runtime has no `ask_json` path yet, so it must not
        // pretend to have one.
        settings.models.inference.provider =
            neo_core::ProviderId::new(neo_core::PROVIDER_ANTHROPIC);
        assert!(runtime.text_helper(&settings).is_none());
    }

    /// The helper is called once per field fill, and a field value is a
    /// sentence: riding the Sol-class model the conversation uses costs a
    /// multiple of the fast tier for the same answer (16 §4). The Luna tier
    /// is spelled differently by each vendor, so the lookup has to know
    /// which vendor's family a subscription runtime serves — that mapping is
    /// the part that silently answered "no fast model" before.
    #[test]
    fn the_helper_takes_the_fast_tier_the_runtime_offers() {
        let default = Settings::default().models.text_helper;
        let catalogue = [
            "claude-opus-5".to_owned(),
            "claude-haiku-4-5".to_owned(),
            "claude-haiku-5".to_owned(),
            "claude-haiku-5-20260101".to_owned(),
        ];
        assert_eq!(
            helper_model(neo_core::PROVIDER_ANTHROPIC_OAUTH, &default, &catalogue).as_deref(),
            Some("claude-haiku-5"),
            "the newest evergreen fast id, never a dated snapshot"
        );

        // Nothing fetched yet, or nothing fast in what was: the runtime's own
        // default answers rather than the run refusing to type.
        assert_eq!(
            helper_model(neo_core::PROVIDER_ANTHROPIC_OAUTH, &default, &[]),
            None
        );
        assert_eq!(
            helper_model(
                neo_core::PROVIDER_OPENAI_CODEX,
                &default,
                &["gpt-5.6-sol".to_owned()]
            ),
            None
        );

        // A saved id counts only for the runtime that will serve it: the
        // default names OpenAI's `gpt-5.6-luna`, which Anthropic would
        // reject, and that is why the two cases above ignored it.
        let saved = neo_core::ModelRef {
            provider: neo_core::ProviderId::new(neo_core::PROVIDER_OPENAI_CODEX),
            id: "gpt-5.6-luna".to_owned(),
        };
        assert_eq!(
            helper_model(neo_core::PROVIDER_OPENAI_CODEX, &saved, &[]).as_deref(),
            Some("gpt-5.6-luna")
        );
    }
}
