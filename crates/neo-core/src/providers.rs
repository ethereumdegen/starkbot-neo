use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures_core::Stream;
use http::HeaderName;
use neo_keys::Secret;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{ProviderError, Utterance};

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
