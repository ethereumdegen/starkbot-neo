//! The Claude Pro/Max subscription as a rig completion model, so the ReAct
//! graph in [`crate::agent::metal`] can run on it the way it runs on OpenAI.
//!
//! [`crate::providers::anthropic_oauth`] already speaks this credential, but
//! it speaks one shape: a single non-streaming `POST /v1/messages` whose whole
//! body is read with `response.text()`. A chat turn needs three things that
//! shape cannot give — token deltas as they are produced, `tool_use` blocks
//! Neo runs and answers with `tool_result`, and a multi-turn message array —
//! and metalcraft's seam for all three is `M: rig::completion::CompletionModel`.
//!
//! # The seam, and why it is this one
//!
//! rig 0.42 already implements the Messages API completely: the message
//! conversion (`tool_use`/`tool_result` content blocks both directions), the
//! tool definitions, the SSE reader that turns `content_block_delta` into
//! `RawStreamingChoice::Message` and `input_json_delta` into tool-argument
//! fragments, and the `message_delta` usage with its cache-token breakout.
//! Re-implementing that here would be a second Messages client in this
//! workspace that has to be kept in agreement with the first one, and the
//! streaming tool-argument assembly is the part nobody gets right twice.
//!
//! What rig cannot do is *authenticate* this credential.
//! `anthropic::AnthropicKey::into_header` is hardcoded to `x-api-key`, and
//! `AnthropicBuilder: ProviderBuilder` pins `type ApiKey = AnthropicKey`, so
//! the key type cannot be swapped for a bearer; `ClientBuilder::http_headers`
//! can add `Authorization` but cannot remove `x-api-key`, and the OAuth
//! endpoint takes exactly one of the two.
//!
//! So the difference is pushed to the one layer it actually lives in — the
//! transport. [`OauthTransport`] is a `rig::http_client::HttpClientExt`, the
//! backend rig accepts through `ClientBuilder::http_client` precisely so a
//! caller can own the wire, and it does one thing: drop `x-api-key`, add
//! `Authorization: Bearer`. Everything above it is rig's own Anthropic
//! provider, unmodified. metalcraft needs nothing; it was never the thing in
//! the way.
//!
//! The second difference is not a header. A subscription token is scoped to
//! Claude Code, and Anthropic rejects a turn whose first system block is not
//! Claude Code's own identity line — so [`ClaudeSubscription`] takes the
//! caller's preamble, moves it to a second system block, and puts the identity
//! first. That is the whole model wrapper.

use std::pin::Pin;
use std::sync::Arc;

use metalcraft::rig::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse, ProviderCapabilities,
};
use metalcraft::rig::http_client::{
    self, HeaderValue, LazyBody, MultipartForm, Request, Response, StreamingResponse,
};
use metalcraft::rig::message::Message;
use metalcraft::rig::providers::anthropic;
use metalcraft::rig::streaming::StreamingCompletionResponse;
use neo_core::ProviderError;
use neo_keys::Secret;
use url::Url;

use super::anthropic::{API_VERSION, HOSTED_BASE};
use super::anthropic_oauth::OAUTH_BETA;

/// One freshly refreshed subscription access token.
pub type TokenFuture = Pin<Box<dyn Future<Output = Result<Arc<Secret>, ProviderError>> + Send>>;

/// Where a request gets that token.
///
/// Asked **per request**, not once per model: a turn is many requests and a
/// plan access token expires on its own schedule, so a model that captured one
/// token at construction would keep sending the stale one for the rest of the
/// turn. The closure is what [`crate::runtime::Runtime::token_source`] hands
/// over, and it is the only thing here that can reach the Keychain.
pub type TokenSource = Arc<dyn Fn() -> TokenFuture + Send + Sync>;

/// A source that answers with one token it already holds — what a caller with
/// a credential in hand, and every test, needs.
#[must_use]
pub fn one_token(token: Secret) -> TokenSource {
    let token = Arc::new(token);
    Arc::new(move || {
        let token = Arc::clone(&token);
        Box::pin(async move { Ok(token) })
    })
}

/// The first system block a Claude Code OAuth session must send. Duplicated
/// from [`crate::providers::anthropic_oauth`] on purpose — the two paths send
/// it for the same reason but through different machinery, and a shared
/// constant would suggest one of them could change it alone.
const CLAUDE_CODE_IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

/// What one turn may write. Anthropic requires `max_tokens`, and rig's
/// per-model table answers `None` for any id it has not heard of — which is
/// every id a catalogue refresh could introduce — so the limit is pinned here
/// instead. 8192 is what a Claude Code session asks for.
const MAX_TOKENS: u64 = 8192;

/// The header rig's Anthropic client inserts for its API key, and the one this
/// transport removes: the OAuth endpoint accepts a bearer or a key, never both.
const API_KEY_HEADER: &str = "x-api-key";

/// The Claude subscription as a model the ReAct graph can drive.
#[derive(Clone)]
pub struct ClaudeSubscription {
    inner: anthropic::completion::CompletionModel<OauthTransport>,
}

impl ClaudeSubscription {
    /// A model against `base_url`, which a test points at `wiremock`
    /// (08 rule 1: the base URL is always injected).
    ///
    /// `tokens` is asked for a token on every request the model makes, so a
    /// refresh that lands between two steps of one turn is on the next step's
    /// wire. Nothing here caches what it is given.
    ///
    /// # Errors
    ///
    /// Fails when rig cannot build a client for `base_url`.
    pub fn new(base_url: &Url, tokens: TokenSource, model: &str) -> Result<Self, ProviderError> {
        let transport = OauthTransport::new(tokens);

        let client = anthropic::Client::builder()
            // rig types its builder on an API key it will turn into the
            // `x-api-key` header. The empty one below never reaches the wire:
            // `OauthTransport` removes that header and adds the bearer.
            .api_key(anthropic::client::AnthropicKey::from(String::new()))
            .base_url(base_url.as_str())
            .anthropic_version(API_VERSION)
            .anthropic_betas(&OAUTH_BETA.split(',').collect::<Vec<_>>())
            .http_client(transport)
            .build()
            .map_err(|error| ProviderError::Transport(error.to_string()))?;

        let mut inner = anthropic::completion::CompletionModel::new(client, model);
        inner.default_max_tokens = Some(MAX_TOKENS);
        Ok(Self { inner })
    }

    /// The hosted Anthropic API. The only way a caller outside `providers/`
    /// reaches production without naming the vendor's host (05 §1 rule 3).
    ///
    /// # Errors
    ///
    /// Fails for the same reasons as [`ClaudeSubscription::new`].
    pub fn hosted(tokens: TokenSource, model: &str) -> Result<Self, ProviderError> {
        let base = Url::parse(HOSTED_BASE)
            .map_err(|error| ProviderError::Transport(format!("hosted base URL: {error}")))?;
        Self::new(&base, tokens, model)
    }

    /// Put Claude Code's identity in front of the caller's system prompt.
    ///
    /// rig sends `preamble` as the first `system` block and any leading
    /// `Message::System` in the history after it, so demoting the preamble to
    /// a system *message* and claiming the preamble for the identity produces
    /// exactly `system: [identity, preamble]`. Concatenating the two into one
    /// block would also be two sentences, but the credential is scoped by the
    /// first block matching Claude Code's, and a block that merely starts with
    /// it is not that.
    fn identify(&self, mut request: CompletionRequest) -> CompletionRequest {
        let preamble = request
            .preamble
            .replace(CLAUDE_CODE_IDENTITY.to_owned())
            .filter(|preamble| !preamble.is_empty());
        if let Some(content) = preamble {
            request.chat_history.insert(0, Message::System { content });
        }
        request
    }
}

impl CompletionModel for ClaudeSubscription {
    fn completion(
        &self,
        request: CompletionRequest,
    ) -> impl Future<Output = Result<CompletionResponse, CompletionError>> + Send {
        self.inner.completion(self.identify(request))
    }

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> impl Future<Output = Result<StreamingCompletionResponse, CompletionError>> + Send {
        self.inner.stream(self.identify(request))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.inner.capabilities()
    }
}

/// rig's HTTP backend with the subscription's authorization on it.
///
/// Every request rig's Anthropic provider makes passes through here on its way
/// out, which is the one place both the unary and the SSE path share — so the
/// swap is written once and cannot be forgotten on the streaming path, which
/// is the path a chat turn actually uses.
#[derive(Clone, Default)]
pub struct OauthTransport {
    inner: http_client::ReqwestClient,
    /// `None` only for [`Default`], which exists because rig's Anthropic model
    /// bounds its backend on it and never calls it. A request sent through a
    /// default transport is refused here rather than sent unauthenticated.
    tokens: Option<TokenSource>,
}

/// Hand-written because a [`TokenSource`] is a closure, and because the point
/// of the type is to hold something that must never be printed.
impl std::fmt::Debug for OauthTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OauthTransport")
            .field("authorized", &self.tokens.is_some())
            .finish()
    }
}

impl OauthTransport {
    /// A transport that asks `tokens` for its bearer on every request.
    fn new(tokens: TokenSource) -> Self {
        Self {
            inner: http_client::ReqwestClient::default(),
            tokens: Some(tokens),
        }
    }
}

/// Replace rig's API-key header with a bearer read now.
///
/// The removal happens whether or not a token arrives: a request that went out
/// with rig's placeholder `x-api-key` and no bearer would be authenticated as
/// an empty API key, and Anthropic's answer to that is a 401 that reads like a
/// revoked subscription.
async fn authorize<B>(
    request: &mut Request<B>,
    tokens: Option<&TokenSource>,
) -> http_client::Result<()> {
    request.headers_mut().remove(API_KEY_HEADER);
    let Some(tokens) = tokens else {
        return Err(instance_error(
            "this Claude subscription model was built without a token source",
        ));
    };
    let token = tokens().await.map_err(instance_error)?;

    // The audited credential boundary clippy.toml points at: the token becomes
    // one `Authorization` header on one request to an injected base URL, and
    // is never logged, stored, put in the URL or returned. The header is
    // marked sensitive so nothing downstream can print it either.
    #[allow(clippy::disallowed_methods)]
    let mut bearer = HeaderValue::from_str(&format!("Bearer {}", token.expose()))
        .map_err(|_| instance_error("the access token is not a usable header value"))?;
    bearer.set_sensitive(true);
    request
        .headers_mut()
        .insert(http::header::AUTHORIZATION, bearer);
    Ok(())
}

/// A local failure in rig's transport error shape. The message never quotes
/// the request, so no credential can reach it.
fn instance_error(reason: impl std::fmt::Display) -> http_client::Error {
    http_client::Error::Instance(reason.to_string().into())
}

impl http_client::HttpClientExt for OauthTransport {
    fn send<T, U>(
        &self,
        request: Request<T>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + Send + 'static
    where
        T: Into<bytes::Bytes> + Send,
        U: From<bytes::Bytes> + Send + 'static,
    {
        let inner = self.inner.clone();
        let tokens = self.tokens.clone();
        // The body becomes bytes here rather than inside the future, because
        // the trait promises a `'static` future and `T` is not.
        let mut request: Request<bytes::Bytes> = request.map(Into::into);
        async move {
            authorize(&mut request, tokens.as_ref()).await?;
            inner.send(request).await
        }
    }

    fn send_multipart<U>(
        &self,
        mut request: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + Send + 'static
    where
        U: From<bytes::Bytes> + Send + 'static,
    {
        let inner = self.inner.clone();
        let tokens = self.tokens.clone();
        async move {
            authorize(&mut request, tokens.as_ref()).await?;
            inner.send_multipart(request).await
        }
    }

    fn send_streaming<T>(
        &self,
        request: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + Send
    where
        T: Into<bytes::Bytes> + Send,
    {
        let inner = self.inner.clone();
        let tokens = self.tokens.clone();
        let mut request: Request<bytes::Bytes> = request.map(Into::into);
        async move {
            authorize(&mut request, tokens.as_ref()).await?;
            inner.send_streaming(request).await
        }
    }
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]

    use metalcraft::rig::completion::ToolDefinition;
    use metalcraft::rig::message::AssistantContent;
    use serde_json::{Value, json};
    use wiremock::matchers::{header, headers, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    /// Never a real credential, and never asserted on except for its absence
    /// from anywhere it must not be.
    const TOKEN: &str = "test-access-token";

    fn model(server: &MockServer) -> ClaudeSubscription {
        let base = Url::parse(&server.uri()).expect("wiremock hands out a URL");
        let token = Secret::new(TOKEN).expect("a non-empty secret");
        ClaudeSubscription::new(&base, one_token(token), "claude-sonnet-4-5").expect("a model")
    }

    fn ask(prompt: &str) -> CompletionRequest {
        CompletionRequest {
            model: None,
            preamble: Some("You are Starkbot.".to_owned()),
            chat_history: vec![Message::user(prompt)],
            documents: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            tool_choice: None,
            additional_params: None,
            output_schema: None,
            record_telemetry_content: false,
        }
    }

    async fn sent_body(server: &MockServer) -> Value {
        let requests = server.received_requests().await.expect("recorded requests");
        requests
            .first()
            .expect("one request")
            .body_json()
            .expect("a JSON body")
    }

    /// The whole reason this module exists. Anthropic's OAuth endpoint accepts
    /// a bearer or an API key and never both, the plan is gated on the beta
    /// list, and the credential is scoped to Claude Code — so a turn that gets
    /// any one of the three wrong is refused before the model sees it.
    #[tokio::test]
    async fn a_turn_goes_out_as_a_claude_code_oauth_session() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
            .and(header("anthropic-version", API_VERSION))
            // `header` splits on commas, so the beta list is matched as the
            // ordered set of betas it is.
            .and(headers("anthropic-beta", OAUTH_BETA.split(',').collect()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg_01",
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-4-5-20260101",
                "content": [{ "type": "text", "text": "hello from the plan" }],
                "stop_reason": "end_turn",
                "usage": { "input_tokens": 11, "output_tokens": 4 }
            })))
            .mount(&server)
            .await;

        let response = model(&server)
            .completion(ask("hi"))
            .await
            .expect("a completion");

        assert!(matches!(
            response.choice.first(),
            Some(AssistantContent::Text(text)) if text.text == "hello from the plan"
        ));
        // What `Tally` counts.
        assert_eq!(response.usage.input_tokens, 11);
        assert_eq!(response.usage.output_tokens, 4);

        let requests = server.received_requests().await.expect("recorded requests");
        let request = requests.first().expect("one request");
        assert!(
            !request.headers.contains_key(API_KEY_HEADER),
            "an `x-api-key` beside the bearer makes Anthropic pick the wrong one",
        );
        // The credential travels in a header, never in the URL or the body.
        assert!(!request.url.as_str().contains(TOKEN));
        assert!(!String::from_utf8_lossy(&request.body).contains(TOKEN));

        let body = sent_body(&server).await;
        assert_eq!(body["system"][0]["text"], json!(CLAUDE_CODE_IDENTITY));
        assert_eq!(body["system"][1]["text"], json!("You are Starkbot."));
    }

    /// Without this a turn cannot drive an application: the graph offers tools
    /// and acts on the calls that come back, so the definition has to reach
    /// Anthropic as a `tools` entry and a `tool_use` block has to return as a
    /// rig tool call with its arguments intact.
    #[tokio::test]
    async fn a_tool_goes_out_and_a_tool_use_block_comes_back_as_a_call() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg_02",
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-4-5-20260101",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_01",
                    "name": "open_page",
                    "input": { "url": "https://example.com" },
                }],
                "stop_reason": "tool_use",
                "usage": { "input_tokens": 20, "output_tokens": 9 }
            })))
            .mount(&server)
            .await;

        let mut request = ask("open example.com");
        request.tools = vec![ToolDefinition {
            name: "open_page".to_owned(),
            description: "Open a page in the browser.".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": { "url": { "type": "string" } },
                "required": ["url"],
            }),
        }];

        let response = model(&server)
            .completion(request)
            .await
            .expect("a completion");

        let body = sent_body(&server).await;
        assert_eq!(body["tools"][0]["name"], json!("open_page"));
        assert_eq!(
            body["tools"][0]["input_schema"]["properties"]["url"]["type"],
            json!("string"),
        );

        let Some(AssistantContent::ToolCall(call)) = response.choice.first() else {
            panic!(
                "the tool_use block did not come back as a call: {:?}",
                response.choice
            );
        };
        assert_eq!(call.function.name, "open_page");
        assert_eq!(
            call.function.arguments,
            json!({ "url": "https://example.com" })
        );
    }

    /// Anthropic scopes a subscription token to Claude Code and refuses a turn
    /// whose first system block is something else, so the caller's preamble has
    /// to become the *second* block rather than replace it or absorb it.
    #[test]
    fn the_claude_code_identity_leads_the_system_prompt() {
        let token = Secret::new("tok").expect("a secret");
        let base = Url::parse("http://127.0.0.1:9/").expect("a base URL");
        let model =
            ClaudeSubscription::new(&base, one_token(token), "claude-sonnet-4-5").expect("a model");

        let identified = model.identify(ask("hello"));

        assert_eq!(identified.preamble.as_deref(), Some(CLAUDE_CODE_IDENTITY));
        assert!(
            matches!(
                identified.chat_history.first(),
                Some(Message::System { content }) if content == "You are Starkbot."
            ),
            "the caller's preamble is the second system block: {:?}",
            identified.chat_history.first()
        );
        assert_eq!(identified.chat_history.len(), 2, "the user turn survives");
    }
}
