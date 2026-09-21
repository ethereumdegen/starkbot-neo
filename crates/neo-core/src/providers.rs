use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures_core::Stream;
use http::HeaderName;
use neo_keys::Secret;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{ProviderError, Settings, TimestampMs, Utterance};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderId(String);

impl ProviderId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelRef {
    pub provider: ProviderId,
    pub id: String,
}

impl ModelRef {
    pub fn new(provider: ProviderId, id: impl Into<String>) -> Self {
        Self {
            provider,
            id: id.into(),
        }
    }
}

#[derive(Clone)]
pub struct Endpoint {
    pub base_url: Url,
    pub auth: Auth,
}

#[derive(Clone)]
pub enum Auth {
    Bearer(Arc<Secret>),
    Header {
        name: HeaderName,
        prefix: &'static str,
        secret: Arc<Secret>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "usd", rename_all = "snake_case")]
pub enum Usd {
    Exact(f64),
    Estimated(f64),
    Unpriced,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TokenCounts {
    pub input: u64,
    pub output: u64,
    pub cached_input: u64,
    pub reasoning: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Units {
    pub tokens: TokenCounts,
    pub audio_seconds: f64,
    pub characters: u64,
    pub images: u32,
    pub video_seconds: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub usd: Usd,
    pub units: Units,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelUseCase {
    Inference,
    TextHelper,
    SpeechToText,
    TextToSpeech,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub reasoning: bool,
    pub tools: bool,
    pub image_input: bool,
    pub streaming: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelPrice {
    pub input_per_million: Option<f64>,
    pub output_per_million: Option<f64>,
    pub cached_input_per_million: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub reference: ModelRef,
    pub use_cases: Vec<ModelUseCase>,
    pub capabilities: ModelCapabilities,
    pub price: Option<ModelPrice>,
    pub deprecated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResolvedModel {
    pub requested: ModelRef,
    pub concrete_id: String,
    pub capabilities: ModelCapabilities,
    pub price: Option<ModelPrice>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyInfo {
    pub label: Option<String>,
    pub used_usd: Option<f64>,
    pub limit_usd: Option<f64>,
    pub remaining_usd: Option<f64>,
}

/// Presence of one Starkbot-owned credential, the account it lives under, and
/// where it was read from.
///
/// Defined in `neo-keys` (the crate that owns the Keychain and the env
/// fallback) and re-exported here so the whole workspace — including the
/// front ends, which never link `neo-keys` — speaks a single vocabulary.
pub use neo_keys::{KeySource, KeyState, KeyStatus};

/// What the last look at a subscription account found.
///
/// `Unavailable` is *"we could not find out"*, not *"it is gone"*. A transport
/// failure, a provider outage or an offline laptop all land here, because the
/// rule the API-key path already follows — reachability never condemns a
/// credential (`key_check.rs`) — applies to a subscription too. Only a refusal
/// the vendor actually issued is `SignedOut`. A front end must therefore
/// render it neutrally: it is not a failure the user can act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAccountStatus {
    SignedOut,
    Connected,
    RateLimited,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitKind {
    Primary,
    Secondary,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RateLimitWindow {
    pub limit_id: String,
    pub limit_name: Option<String>,
    pub used_percent: f64,
    pub kind: RateLimitKind,
    pub window_duration_minutes: Option<u64>,
    pub resets_at: Option<TimestampMs>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Allowance {
    pub limits: Vec<RateLimitWindow>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderAccount {
    pub provider: ProviderId,
    pub status: ProviderAccountStatus,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub workspace: Option<String>,
    pub allowance: Option<Allowance>,
    pub updated_at: TimestampMs,
}

/// Provider id of the OpenAI API-key inference runtime (K6 path a).
pub const PROVIDER_OPENAI: &str = "openai";
/// Provider id of the ChatGPT plan runtime driven through the Codex app-server (K6 path b).
pub const PROVIDER_CHATGPT_CODEX: &str = "chatgpt-codex";
/// Provider id of the Anthropic API-key inference runtime (K6 path c).
pub const PROVIDER_ANTHROPIC: &str = "anthropic";
/// Provider id of the Claude subscription runtime driven through the Claude Code CLI (K6 path d).
pub const PROVIDER_CLAUDE_SUBSCRIPTION: &str = "claude-subscription";
/// Provider id of the Claude Pro/Max runtime Starkbot drives itself, over an
/// OAuth token it owns (K7, A25 runtime 5). Preferred over
/// [`PROVIDER_CLAUDE_SUBSCRIPTION`], which needs the vendor's CLI installed.
pub const PROVIDER_ANTHROPIC_OAUTH: &str = "anthropic-oauth";
/// Provider id of the ChatGPT Plus/Pro runtime Starkbot drives itself, over an
/// OAuth token it owns (K7, A25 runtime 6). Preferred over
/// [`PROVIDER_CHATGPT_CODEX`], which needs the Codex app-server.
pub const PROVIDER_OPENAI_CODEX: &str = "openai-codex";

/// Which K6 inference connection is configured and usable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum InferenceConnection {
    None,
    OpenAiKey,
    ChatGptCodex,
    AnthropicKey,
    ClaudeSubscription,
    /// Claude Pro/Max over an OAuth token Starkbot holds (K7).
    AnthropicOauth,
    /// ChatGPT Plus/Pro over an OAuth token Starkbot holds (K7).
    OpenAiCodexOauth,
}

impl InferenceConnection {
    /// Resolve the selected inference runtime against the credentials that
    /// actually exist: an API-key path needs its key present, a subscription
    /// path needs that provider's account connected. Anything else is `None`.
    pub fn detect(
        settings: &Settings,
        keys: &[KeyStatus],
        account: Option<&ProviderAccount>,
    ) -> Self {
        match settings.models.inference.provider.as_str() {
            PROVIDER_OPENAI if key_present(keys, neo_keys::ACCOUNT_OPENAI) => Self::OpenAiKey,
            PROVIDER_ANTHROPIC if key_present(keys, neo_keys::ACCOUNT_ANTHROPIC) => {
                Self::AnthropicKey
            }
            PROVIDER_CHATGPT_CODEX if connected(account, PROVIDER_CHATGPT_CODEX) => {
                Self::ChatGptCodex
            }
            PROVIDER_CLAUDE_SUBSCRIPTION if connected(account, PROVIDER_CLAUDE_SUBSCRIPTION) => {
                Self::ClaudeSubscription
            }
            PROVIDER_ANTHROPIC_OAUTH if connected(account, PROVIDER_ANTHROPIC_OAUTH) => {
                Self::AnthropicOauth
            }
            PROVIDER_OPENAI_CODEX if connected(account, PROVIDER_OPENAI_CODEX) => {
                Self::OpenAiCodexOauth
            }
            _ => Self::None,
        }
    }
}

fn key_present(keys: &[KeyStatus], account: &str) -> bool {
    keys.iter()
        .any(|key| key.account == account && key.state == KeyState::Present)
}

fn connected(account: Option<&ProviderAccount>, provider: &str) -> bool {
    account.is_some_and(|account| {
        account.provider.as_str() == provider && account.status == ProviderAccountStatus::Connected
    })
}

#[async_trait]
pub trait InferenceProvider: Send + Sync {
    fn id(&self) -> &ProviderId;
    fn endpoint(&self) -> &Endpoint;
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError>;
    async fn resolve(&self, model: &ModelRef) -> Result<ResolvedModel, ProviderError>;
    fn usage(
        &self,
        model: &ResolvedModel,
        counts: &TokenCounts,
        reported_cost: Option<f64>,
    ) -> Usage;
    async fn key_info(&self) -> Result<Option<KeyInfo>, ProviderError>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeechCaps {
    pub batch_stt: bool,
    pub streaming_stt: bool,
    pub keywords: bool,
    pub tts_pcm_stream: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SttHints {
    pub prompt: Option<String>,
    pub keywords: Vec<String>,
    pub language: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VoiceRef(pub String);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptEvent {
    Partial { text: String },
    Final { text: String, usage: Usage },
}

#[derive(Clone, Debug, PartialEq)]
pub struct PcmChunk {
    pub samples: Vec<i16>,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub usage: Option<Usage>,
}

pub type TranscriptStream =
    Pin<Box<dyn Stream<Item = Result<TranscriptEvent, ProviderError>> + Send>>;
pub type PcmStream = Pin<Box<dyn Stream<Item = Result<PcmChunk, ProviderError>> + Send>>;

#[async_trait]
pub trait Transcriber: Send {
    async fn transcribe(&mut self, utterance: Utterance)
    -> Result<TranscriptStream, ProviderError>;
}

#[async_trait]
pub trait Speaker: Send {
    async fn speak(
        &mut self,
        text: &str,
        instructions: Option<&str>,
    ) -> Result<PcmStream, ProviderError>;
}

pub trait SpeechProvider: Send + Sync {
    fn id(&self) -> &ProviderId;
    fn capabilities(&self) -> SpeechCaps;
    fn transcriber(
        &self,
        model: &ResolvedModel,
        hints: SttHints,
    ) -> Result<Box<dyn Transcriber>, ProviderError>;
    fn speaker(
        &self,
        model: &ResolvedModel,
        voice: &VoiceRef,
    ) -> Result<Box<dyn Speaker>, ProviderError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_for(provider: &str) -> Settings {
        let mut settings = Settings::default();
        settings.models.inference = ModelRef::new(ProviderId::new(provider), "sol-latest");
        settings
    }

    fn key(account: &str, state: KeyState) -> Vec<KeyStatus> {
        vec![KeyStatus::new(account, state)]
    }

    fn account(provider: &str, status: ProviderAccountStatus) -> ProviderAccount {
        ProviderAccount {
            provider: ProviderId::new(provider),
            status,
            email: None,
            plan_type: None,
            workspace: None,
            allowance: None,
            updated_at: 0,
        }
    }

    #[test]
    fn each_path_needs_its_own_credential() {
        assert_eq!(
            InferenceConnection::detect(
                &settings_for(PROVIDER_OPENAI),
                &key(neo_keys::ACCOUNT_OPENAI, KeyState::Present),
                None
            ),
            InferenceConnection::OpenAiKey
        );
        assert_eq!(
            InferenceConnection::detect(
                &settings_for(PROVIDER_ANTHROPIC),
                &key(neo_keys::ACCOUNT_ANTHROPIC, KeyState::Present),
                None
            ),
            InferenceConnection::AnthropicKey
        );
        assert_eq!(
            InferenceConnection::detect(
                &settings_for(PROVIDER_CHATGPT_CODEX),
                &[],
                Some(&account(
                    PROVIDER_CHATGPT_CODEX,
                    ProviderAccountStatus::Connected
                ))
            ),
            InferenceConnection::ChatGptCodex
        );
        assert_eq!(
            InferenceConnection::detect(
                &settings_for(PROVIDER_CLAUDE_SUBSCRIPTION),
                &[],
                Some(&account(
                    PROVIDER_CLAUDE_SUBSCRIPTION,
                    ProviderAccountStatus::Connected
                ))
            ),
            InferenceConnection::ClaudeSubscription
        );
    }

    #[test]
    fn a_selected_path_without_its_credential_is_not_connected() {
        for (provider, keys, account) in [
            (
                PROVIDER_OPENAI,
                key(neo_keys::ACCOUNT_OPENAI, KeyState::Missing),
                None,
            ),
            (
                PROVIDER_OPENAI,
                key(neo_keys::ACCOUNT_OPENAI, KeyState::Invalid),
                None,
            ),
            (
                PROVIDER_ANTHROPIC,
                key(neo_keys::ACCOUNT_OPENAI, KeyState::Present),
                None,
            ),
            (
                PROVIDER_CHATGPT_CODEX,
                key(neo_keys::ACCOUNT_OPENAI, KeyState::Present),
                None,
            ),
            (
                PROVIDER_CHATGPT_CODEX,
                Vec::new(),
                Some(account(
                    PROVIDER_CHATGPT_CODEX,
                    ProviderAccountStatus::SignedOut,
                )),
            ),
            (
                PROVIDER_CLAUDE_SUBSCRIPTION,
                Vec::new(),
                Some(account(
                    PROVIDER_CHATGPT_CODEX,
                    ProviderAccountStatus::Connected,
                )),
            ),
            (PROVIDER_CLAUDE_SUBSCRIPTION, Vec::new(), None),
        ] {
            assert_eq!(
                InferenceConnection::detect(&settings_for(provider), &keys, account.as_ref()),
                InferenceConnection::None,
                "provider {provider} should not be connected"
            );
        }
    }

    #[test]
    fn an_unknown_runtime_is_never_connected() {
        assert_eq!(
            InferenceConnection::detect(
                &settings_for("starkrouter"),
                &key(neo_keys::ACCOUNT_OPENAI, KeyState::Present),
                Some(&account(
                    PROVIDER_CHATGPT_CODEX,
                    ProviderAccountStatus::Connected
                ))
            ),
            InferenceConnection::None
        );
    }
}
