//! Claude Pro/Max inference on the subscription OAuth credential: one
//! `POST {base}/v1/messages` carrying `Authorization: Bearer`, the pinned
//! `anthropic-version` and the Claude Code `anthropic-beta` list.
//!
//! The access token is passed in per call and used exactly once, as one
//! header. This module never reads the Keychain and never caches a token; the
//! caller (`OauthStore::access_token`) owns refresh.
//!
//! # Why this is not the chat path
//!
//! It used to be the only Claude Pro/Max client here, and the module comment
//! used to argue that rig 0.42 could not authenticate this credential at all.
//! Half of that argument still holds and half of it was wrong.
//!
//! Still true: `rig::providers::anthropic::AnthropicKey`'s
//! `ApiKey::into_header` is hardcoded to `x-api-key`, and
//! `AnthropicBuilder: ProviderBuilder` pins `type ApiKey = AnthropicKey`, so
//! the key type cannot be swapped for a bearer; `ClientBuilder::http_headers`
//! can *add* `Authorization` but cannot remove `x-api-key`, and the OAuth
//! endpoint takes exactly one of the two.
//!
//! Wrong: that the remaining escape hatch — a custom `H: HttpClientExt`
//! backend — was out of reach because rig states its bounds in terms of
//! `bytes::Bytes` and re-exports neither the crate nor an alias. Naming
//! `bytes = "1"` as a dependency resolves to rig's own, and
//! [`super::anthropic_oauth_model`] is that backend: one header swapped, and
//! the whole of rig's streaming Anthropic provider above it. The agent loop
//! runs there.
//!
//! What is left here is the shape that module does not serve and should not:
//! one round trip, no stream, and [`AnthropicOauthInference::complete_json`]'s
//! strict answer through a forced `respond` tool call — which is what
//! `Runtime::ask_json` and every one-shot `neo ask` need.
//!
//! [`Turn::usage`] is the vendor's `usage` object verbatim.

use std::time::{Duration, Instant};

use neo_core::ProviderError;
use neo_keys::Secret;
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;

use super::Turn;
use super::anthropic::{API_VERSION, HOSTED_BASE};

/// The `anthropic-beta` list a Claude Code OAuth session sends, byte for byte
/// what OMP sends. Anthropic gates plan inference on `oauth-2025-04-20` and
/// `claude-code-20250219`; the rest are the feature betas that session opts
/// into.
pub const OAUTH_BETA: &str = "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,redact-thinking-2026-02-12,context-management-2025-06-27,prompt-caching-scope-2026-01-05,mid-conversation-system-2026-04-07,advanced-tool-use-2025-11-20,effort-2025-11-24,extended-cache-ttl-2025-04-11";

/// The first system block a Claude Code OAuth session must send. The
/// subscription credential is scoped to `user:inference` for Claude Code, and
/// Anthropic rejects a plan turn that does not identify itself this way.
const CLAUDE_CODE_IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

/// How long one turn may take. Plan models think for a while; this is the same
/// ceiling the Claude Code bridge uses.
const TIMEOUT: Duration = Duration::from_secs(300);

/// Anthropic requires `max_tokens`. 8192 is what a Claude Code session asks
/// for by default.
const DEFAULT_MAX_TOKENS: u32 = 8192;

/// The single required tool that makes [`AnthropicOauthInference::complete_json`]
/// strict: its input schema *is* the caller's schema, so the model can only
/// answer by filling it in.
const JSON_TOOL: &str = "respond";

pub struct AnthropicOauthInference {
    base_url: Url,
    client: Client,
    max_tokens: u32,
}

impl AnthropicOauthInference {
    /// Inference against `base_url`, which a test points at `wiremock`
    /// (08 rule 1: the base URL is always injected).
    pub fn new(base_url: Url) -> Result<Self, ProviderError> {
        Ok(Self {
            base_url,
            client: Client::builder()
                .timeout(TIMEOUT)
                .build()
                .map_err(|error| ProviderError::Transport(error.to_string()))?,
            max_tokens: DEFAULT_MAX_TOKENS,
        })
    }

    /// The hosted Anthropic API. The only way a caller outside `providers/`
    /// reaches production without naming the vendor's host (05 §1 rule 3).
    pub fn hosted() -> Result<Self, ProviderError> {
        let base = Url::parse(HOSTED_BASE)
            .map_err(|error| ProviderError::Transport(format!("hosted base URL: {error}")))?;
        Self::new(base)
    }

    #[must_use]
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// One text turn on the connected subscription.
    pub async fn complete_text(
        &self,
        token: &Secret,
        model: &str,
        prompt: &str,
    ) -> Result<Turn, ProviderError> {
        let (turn, _) = self.send(token, model, prompt, None).await?;
        Ok(turn)
    }

    /// One turn that can only answer with JSON matching `schema`: `schema`
    /// becomes the input schema of a single tool the model is forced to call,
    /// so the answer is validated by Anthropic rather than re-parsed out of
    /// prose.
    pub async fn complete_json(
        &self,
        token: &Secret,
        model: &str,
        prompt: &str,
        schema: &Value,
    ) -> Result<(Value, Turn), ProviderError> {
        let (turn, answer) = self.send(token, model, prompt, Some(schema)).await?;
        let answer = answer.ok_or_else(|| {
            ProviderError::InvalidResponse(format!(
                "the model answered without calling `{JSON_TOOL}`"
            ))
        })?;
        Ok((answer, turn))
    }

    async fn send(
        &self,
        token: &Secret,
        model: &str,
        prompt: &str,
        schema: Option<&Value>,
    ) -> Result<(Turn, Option<Value>), ProviderError> {
        let url = self.messages_url()?;
        let body = self.body(model, prompt, schema);

        // The audited credential boundary clippy.toml points at: the token
        // becomes one `Authorization` header on one request to an injected
        // base URL, and is never logged, stored, put in the URL or returned.
        #[allow(clippy::disallowed_methods)]
        let request = self
            .client
            .post(url)
            .bearer_auth(token.expose())
            .header("anthropic-version", API_VERSION)
            .header("anthropic-beta", OAUTH_BETA)
            .json(&body);

        let started = Instant::now();
        let response = request
            .send()
            .await
            .map_err(|error| ProviderError::Transport(error.to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| ProviderError::Transport(error.to_string()))?;
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

        if !status.is_success() {
            return Err(status_error(status, model, &text));
        }
        parse(&text, model, duration_ms)
    }

    fn messages_url(&self) -> Result<Url, ProviderError> {
        // `join` replaces the last path segment unless the base ends in `/`,
        // which would silently drop a base like `http://127.0.0.1:1234/proxy`.
        let mut url = self.base_url.clone();
        {
            let mut segments = url.path_segments_mut().map_err(|()| {
                ProviderError::Transport("base URL cannot have a path".to_string())
            })?;
            segments.pop_if_empty().extend(["v1", "messages"]);
        }
        Ok(url)
    }

    fn body(&self, model: &str, prompt: &str, schema: Option<&Value>) -> Value {
        let mut body = json!({
            "model": model,
            "max_tokens": self.max_tokens,
            "system": [{ "type": "text", "text": CLAUDE_CODE_IDENTITY }],
            "messages": [{ "role": "user", "content": prompt }],
        });
        if let Some(schema) = schema {
            body["tools"] = json!([{
                "name": JSON_TOOL,
                "description": "Answer with the object this schema describes. This is the only way to answer.",
                "input_schema": schema,
            }]);
            body["tool_choice"] = json!({ "type": "tool", "name": JSON_TOOL });
        }
        body
    }
}

/// The Messages response, reduced to what a turn needs. Unknown fields are
/// ignored on purpose: the envelope gains fields.
#[derive(Deserialize)]
struct MessageResponse {
    model: Option<String>,
    #[serde(default)]
    content: Vec<ContentBlock>,
    usage: Option<Value>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        name: String,
        input: Value,
    },
    /// Thinking, redacted thinking, and whatever a beta adds next: a turn
    /// reads text and the one tool call, and ignores the rest.
    #[serde(other)]
    Other,
}

fn parse(
    text: &str,
    model: &str,
    duration_ms: u64,
) -> Result<(Turn, Option<Value>), ProviderError> {
    let parsed: MessageResponse = serde_json::from_str(text).map_err(|error| {
        ProviderError::InvalidResponse(format!("the Messages response did not parse: {error}"))
    })?;

    let mut answer = None;
    let mut blocks: Vec<String> = Vec::new();
    for block in parsed.content {
        match block {
            ContentBlock::Text { text } => blocks.push(text),
            ContentBlock::ToolUse { name, input } if name == JSON_TOOL => answer = Some(input),
            ContentBlock::ToolUse { .. } | ContentBlock::Other => {}
        }
    }

    let turn = Turn {
        text: blocks.join("\n"),
        model: parsed.model.unwrap_or_else(|| model.to_string()),
        usage: parsed.usage.unwrap_or(Value::Null),
        duration_ms,
    };
    Ok((turn, answer))
}

/// The plan-inference failure table. `401`/`403` is the one a user can act on:
/// the OAuth credential is gone or was revoked, so sign in again. `429` and
/// `529` are the plan's own throttle, which a caller may retry.
fn status_error(status: StatusCode, model: &str, body: &str) -> ProviderError {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ProviderError::Authentication,
        StatusCode::TOO_MANY_REQUESTS => ProviderError::RateLimited,
        StatusCode::NOT_FOUND => ProviderError::ModelUnavailable(model.to_string()),
        // Anthropic's 529 "overloaded" is a retry, not a fault.
        _ if status.as_u16() == 529 => ProviderError::RateLimited,
        _ if status.is_server_error() => ProviderError::Transport(detail(status, body)),
        _ => ProviderError::InvalidResponse(detail(status, body)),
    }
}

/// The vendor's own message for a failed call, never the request. A Messages
/// error body carries `{"error":{"type":…,"message":…}}` and no credential.
fn detail(status: StatusCode, body: &str) -> String {
    match serde_json::from_str::<Value>(body)
        .ok()
        .as_ref()
        .and_then(|value| value.pointer("/error/message"))
        .and_then(Value::as_str)
    {
        Some(message) => format!("{status}: {message}"),
        None => status.to_string(),
    }
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]

    use super::*;
    use wiremock::matchers::{header, headers, method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    /// Never a real credential, and never asserted on — only its absence is.
    const TOKEN: &str = "test-access-token";

    fn secret() -> Secret {
        Secret::new(TOKEN).expect("a non-empty secret")
    }

    async fn inference(server: &MockServer) -> AnthropicOauthInference {
        let base = Url::parse(&server.uri()).expect("wiremock hands out a URL");
        AnthropicOauthInference::new(base).expect("a client builds")
    }

    fn text_body() -> Value {
        json!({
            "id": "msg_01",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-5-20260101",
            "content": [
                { "type": "thinking", "thinking": "…", "signature": "sig" },
                { "type": "text", "text": "hello from the plan" }
            ],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 11, "output_tokens": 4, "cache_read_input_tokens": 0 }
        })
    }

    #[tokio::test]
    async fn a_turn_carries_the_bearer_and_the_version_and_beta_headers() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
            .and(header("anthropic-version", API_VERSION))
            // `header` splits on commas, so the beta list is matched as the
            // ordered set of betas it is.
            .and(headers("anthropic-beta", OAUTH_BETA.split(',').collect()))
            .respond_with(ResponseTemplate::new(200).set_body_json(text_body()))
            .mount(&server)
            .await;

        let turn = inference(&server)
            .await
            .complete_text(&secret(), "claude-sonnet-5", "hi")
            .await
            .expect("a turn");

        assert_eq!(turn.text, "hello from the plan");
        assert_eq!(turn.model, "claude-sonnet-5-20260101");
        assert_eq!(turn.usage["input_tokens"], json!(11));

        // The credential travels in a header, never in the URL or the body.
        let requests = server.received_requests().await.expect("recorded requests");
        let request: &Request = requests.first().expect("one request");
        assert!(!request.url.as_str().contains(TOKEN));
        assert!(!String::from_utf8_lossy(&request.body).contains(TOKEN));
    }

    #[tokio::test]
    async fn the_plan_turn_identifies_itself_as_claude_code() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(text_body()))
            .mount(&server)
            .await;

        inference(&server)
            .await
            .complete_text(&secret(), "claude-sonnet-5", "hi")
            .await
            .expect("a turn");

        let requests = server.received_requests().await.expect("recorded requests");
        let body: Value = requests
            .first()
            .expect("one request")
            .body_json()
            .expect("json body");
        assert_eq!(body["system"][0]["text"], json!(CLAUDE_CODE_IDENTITY));
    }

    #[tokio::test]
    async fn strict_json_forces_the_tool_and_returns_its_input() {
        let server = MockServer::start().await;
        let answer = json!({ "verdict": "safe", "score": 3 });
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg_02",
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-5-20260101",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_01",
                    "name": JSON_TOOL,
                    "input": answer,
                }],
                "stop_reason": "tool_use",
                "usage": { "input_tokens": 20, "output_tokens": 9 }
            })))
            .mount(&server)
            .await;

        let schema = json!({
            "type": "object",
            "properties": { "verdict": { "type": "string" }, "score": { "type": "integer" } },
            "required": ["verdict", "score"],
        });
        let (value, turn) = inference(&server)
            .await
            .complete_json(&secret(), "claude-sonnet-5", "judge this", &schema)
            .await
            .expect("a json turn");

        assert_eq!(value, answer);
        assert_eq!(turn.usage["output_tokens"], json!(9));

        let requests = server.received_requests().await.expect("recorded requests");
        let body: Value = requests
            .first()
            .expect("one request")
            .body_json()
            .expect("json body");
        assert_eq!(body["tools"][0]["input_schema"], schema);
        assert_eq!(
            body["tool_choice"],
            json!({ "type": "tool", "name": JSON_TOOL })
        );
    }

    #[tokio::test]
    async fn a_tool_less_answer_to_a_strict_call_is_a_protocol_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(text_body()))
            .mount(&server)
            .await;

        let error = inference(&server)
            .await
            .complete_json(
                &secret(),
                "claude-sonnet-5",
                "judge",
                &json!({ "type": "object" }),
            )
            .await
            .expect_err("no tool call");
        assert!(matches!(error, ProviderError::InvalidResponse(_)));
    }

    #[tokio::test]
    async fn an_expired_credential_asks_the_user_to_sign_in_again() {
        for status in [401_u16, 403] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                    "type": "error",
                    "error": { "type": "authentication_error", "message": "OAuth token has expired" }
                })))
                .mount(&server)
                .await;

            let error = inference(&server)
                .await
                .complete_text(&secret(), "claude-sonnet-5", "hi")
                .await
                .expect_err("a rejected credential");
            assert_eq!(error, ProviderError::Authentication, "status {status}");
        }
    }

    #[tokio::test]
    async fn a_throttled_plan_is_retryable() {
        for status in [429_u16, 529] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                    "type": "error",
                    "error": { "type": "rate_limit_error", "message": "slow down" }
                })))
                .mount(&server)
                .await;

            let error = inference(&server)
                .await
                .complete_text(&secret(), "claude-sonnet-5", "hi")
                .await
                .expect_err("a throttle");
            assert_eq!(error, ProviderError::RateLimited, "status {status}");
        }
    }

    #[tokio::test]
    async fn a_body_that_is_not_a_message_is_a_protocol_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{\"content\":"))
            .mount(&server)
            .await;

        let error = inference(&server)
            .await
            .complete_text(&secret(), "claude-sonnet-5", "hi")
            .await
            .expect_err("a truncated body");
        assert!(matches!(error, ProviderError::InvalidResponse(_)));
    }

    #[tokio::test]
    async fn an_unknown_model_names_the_model() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "type": "error",
                "error": { "type": "not_found_error", "message": "model not found" }
            })))
            .mount(&server)
            .await;

        let error = inference(&server)
            .await
            .complete_text(&secret(), "claude-nope", "hi")
            .await
            .expect_err("an unknown model");
        assert_eq!(
            error,
            ProviderError::ModelUnavailable("claude-nope".to_string())
        );
    }

    #[test]
    fn an_error_detail_quotes_the_vendor_and_nothing_else() {
        let body = r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        let detail = detail(StatusCode::INTERNAL_SERVER_ERROR, body);
        assert!(detail.contains("Overloaded"));
        assert!(!detail.contains(TOKEN));
    }
}
