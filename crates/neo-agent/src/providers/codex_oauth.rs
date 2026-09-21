//! ChatGPT Plus/Pro (Codex) inference on the subscription OAuth credential,
//! through rig — reached by `metalcraft::rig`, the re-export metalcraft
//! publishes under its `rig` feature, so the version is the one the user's own
//! crate pins.
//!
//! rig 0.42 ships a first-class `chatgpt` provider for exactly this endpoint:
//! it posts `{base}/codex/responses`, sends `Authorization: Bearer`,
//! `ChatGPT-Account-Id`, `originator` and a fresh `session_id` per request,
//! and reassembles the SSE body the Codex backend answers with even for a
//! non-streaming call. Nothing here re-implements any of that; this module
//! injects the base URL and the per-call credential, and flattens rig's
//! response into a [`Turn`].
//!
//! # The metalcraft seam
//!
//! metalcraft accepts a *prepared* `rig::completion::CompletionModel` —
//! `ReactAgentNode::new(model, …)` and `create_react_agent(model, …)` — and
//! never builds one from an API key. [`CodexOauthInference::model`] hands back
//! exactly that type, so a plan-backed model drops straight into a metalcraft
//! graph. A one-shot turn does not need the react loop, so
//! [`CodexOauthInference::complete_text`] calls the model directly; both use
//! the same prepared model.
//!
//! The access token is passed in per call. This module never reads the
//! Keychain and never caches a token: `ChatGPTAuth::AccessToken` short-circuits
//! rig's authenticator, so rig never touches its own `auth.json` either.
//!
//! # Strict JSON
//!
//! The Codex backend's `/responses` does **not** give us `text.format`
//! structured outputs: rig's ChatGPT provider clears `text` on every request
//! (the backend rejects it). So strict JSON here is the same shape as on
//! Anthropic — one function tool whose `parameters` is the caller's schema,
//! demanded by `tool_choice`, and the answer is the tool call's arguments.

use std::time::Instant;

use metalcraft::rig::completion::message::{AssistantContent, ToolChoice};
use metalcraft::rig::completion::{
    CompletionError, CompletionModel as _, CompletionRequestBuilder, Message, ToolDefinition,
};
use metalcraft::rig::providers::chatgpt::{self, ChatGPTAuth, ResponsesCompletionModel};
use neo_core::ProviderError;
use neo_keys::Secret;
use reqwest::StatusCode;
use serde_json::Value;
use url::Url;

use super::Turn;

/// The ChatGPT backend root. `codex` is appended to it: rig's provider posts
/// `{base}/responses` against the Codex base it is given. 05 §1 rule 3 greps
/// for this host outside `providers/`.
pub const HOSTED_BASE: &str = "https://chatgpt.com/backend-api";

/// What we call ourselves to the Codex backend, on the `originator` header and
/// in the OAuth authorize request. Not a Codex CLI impersonation: this is
/// Starkbot asking on the user's plan.
pub const ORIGINATOR: &str = "starkbot-neo";

/// The instructions every Codex turn carries. Set explicitly so the turn does
/// not inherit rig's default, which reads `CHATGPT_DEFAULT_INSTRUCTIONS` from
/// the environment.
const INSTRUCTIONS: &str = "You are a coding and reasoning assistant answering on behalf of Starkbot.";

/// The single required tool that makes [`CodexOauthInference::complete_json`]
/// strict, mirroring the Anthropic provider's.
const JSON_TOOL: &str = "respond";

pub struct CodexOauthInference {
    /// Already `{base}/codex`: the root rig appends `/responses` to.
    codex_base: String,
    account_id: Option<String>,
}

impl CodexOauthInference {
    /// Inference against `base_url`, the ChatGPT backend root, which a test
    /// points at `wiremock` (08 rule 1: the base URL is always injected).
    pub fn new(base_url: Url) -> Result<Self, ProviderError> {
        if base_url.cannot_be_a_base() {
            return Err(ProviderError::Transport(
                "base URL cannot have a path".to_string(),
            ));
        }
        Ok(Self {
            codex_base: format!("{}/codex", base_url.as_str().trim_end_matches('/')),
            account_id: None,
        })
    }

    /// The hosted ChatGPT backend. The only way a caller outside `providers/`
    /// reaches production without naming the vendor's host.
    pub fn hosted() -> Result<Self, ProviderError> {
        let base = Url::parse(HOSTED_BASE)
            .map_err(|error| ProviderError::Transport(format!("hosted base URL: {error}")))?;
        Self::new(base)
    }

    /// The `ChatGPT-Account-Id` every request carries, which the login read out
    /// of the `id_token` and stored on the credential. Absent means the
    /// personal account the token itself names.
    #[must_use]
    pub fn with_account(mut self, account_id: impl Into<String>) -> Self {
        self.account_id = Some(account_id.into());
        self
    }

    /// A rig completion model bound to this token and model id — the value
    /// `metalcraft::ReactAgentNode::new` and `metalcraft::create_react_agent`
    /// take. Built per call because the token is per call.
    pub fn model(
        &self,
        token: &Secret,
        model: &str,
    ) -> Result<ResponsesCompletionModel, ProviderError> {
        // The audited credential boundary clippy.toml points at: the token
        // becomes the bearer of one rig client aimed at an injected base URL,
        // and is never logged, stored, put in the URL or returned. rig's own
        // `Debug` for this value prints `AccessToken(<redacted>)`.
        #[allow(clippy::disallowed_methods)]
        let auth = ChatGPTAuth::AccessToken {
            access_token: token.expose().to_string(),
            account_id: self.account_id.clone(),
        };

        let client = chatgpt::Client::builder()
            .api_key(auth)
            .base_url(&self.codex_base)
            .originator(ORIGINATOR)
            .default_instructions(INSTRUCTIONS)
            // A service must never drop into rig's interactive device-code
            // login: a credential we cannot use is an error to report, not a
            // prompt to print.
            .allow_device_flow(false)
            .build()
            .map_err(|error| ProviderError::Transport(error.to_string()))?;

        Ok(ResponsesCompletionModel::new(client, model))
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

    /// One turn that can only answer with JSON matching `schema`.
    pub async fn complete_json(
        &self,
        token: &Secret,
        model: &str,
        prompt: &str,
        schema: &Value,
    ) -> Result<(Value, Turn), ProviderError> {
        let (turn, answer) = self.send(token, model, prompt, Some(schema)).await?;
        let answer = answer.ok_or_else(|| {
            ProviderError::InvalidResponse(format!("the model answered without calling `{JSON_TOOL}`"))
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
        let completion = self.model(token, model)?;
        let mut request = CompletionRequestBuilder::new(completion.clone(), Message::user(prompt));
        if let Some(schema) = schema {
            request = request
                .tool(ToolDefinition {
                    name: JSON_TOOL.to_string(),
                    description:
                        "Answer with the object this schema describes. This is the only way to answer."
                            .to_string(),
                    parameters: schema.clone(),
                })
                .tool_choice(ToolChoice::Specific {
                    function_names: vec![JSON_TOOL.to_string()],
                });
        }

        let started = Instant::now();
        let response = completion
            .completion(request.build())
            .await
            .map_err(|error| provider_error(model, &error))?;
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

        let mut answer = None;
        let mut blocks: Vec<String> = Vec::new();
        for content in response.choice {
            match content {
                AssistantContent::Text(text) => blocks.push(text.text),
                AssistantContent::ToolCall(call) if call.function.name == JSON_TOOL => {
                    answer = Some(call.function.arguments);
                }
                AssistantContent::ToolCall(_)
                | AssistantContent::Reasoning(_)
                | AssistantContent::Image(_) => {}
            }
        }

        let turn = Turn {
            text: blocks.join("\n"),
            model: response.model.unwrap_or_else(|| model.to_string()),
            // The vendor's own `usage`, verbatim off the wire response rig
            // captured — not rig's normalized counts.
            usage: response
                .raw
                .get("usage")
                .cloned()
                .unwrap_or(Value::Null),
            duration_ms,
        };
        Ok((turn, answer))
    }
}

/// The plan-inference failure table, read off the status rig preserved on the
/// error. `401`/`403` is the one a user can act on: sign in again.
fn provider_error(model: &str, error: &CompletionError) -> ProviderError {
    match error.provider_response_status() {
        Some(StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) => ProviderError::Authentication,
        Some(StatusCode::TOO_MANY_REQUESTS) => ProviderError::RateLimited,
        Some(StatusCode::NOT_FOUND) => ProviderError::ModelUnavailable(model.to_string()),
        Some(status) if status.as_u16() == 529 => ProviderError::RateLimited,
        Some(status) if status.is_server_error() => {
            ProviderError::Transport(format!("codex responded {status}"))
        }
        Some(status) => ProviderError::InvalidResponse(format!("codex responded {status}")),
        // No status means rig never got a response, or got one it could not
        // read. rig's own errors quote the response, never the request, so no
        // credential can reach this string.
        None => match error {
            CompletionError::HttpError(_) | CompletionError::RequestError(_) => {
                ProviderError::Transport(error.to_string())
            }
            _ => ProviderError::InvalidResponse(error.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]

    use super::*;
    use metalcraft::{AgentState, Executor, RunOutcome, ToolRegistry, create_react_agent};
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    /// Never a real credential, and never asserted on — only its absence is.
    const TOKEN: &str = "test-access-token";
    const ACCOUNT: &str = "acct_test_0001";

    fn secret() -> Secret {
        Secret::new(TOKEN).expect("a non-empty secret")
    }

    fn inference(server: &MockServer) -> CodexOauthInference {
        let base = Url::parse(&server.uri()).expect("wiremock hands out a URL");
        CodexOauthInference::new(base)
            .expect("a client builds")
            .with_account(ACCOUNT)
    }

    /// `/responses` answers with an SSE body even for a non-streaming request,
    /// so the fixture is the terminal `response.completed` event.
    fn sse(output: Value) -> String {
        let event = json!({
            "type": "response.completed",
            "sequence_number": 1,
            "response": {
                "id": "resp_01",
                "object": "response",
                "created_at": 1_764_000_000_u64,
                "status": "completed",
                "model": "gpt-5.3-codex",
                "output": output,
                "usage": { "input_tokens": 12, "output_tokens": 5, "total_tokens": 17 },
                "tools": [],
            }
        });
        format!("event: response.completed\ndata: {event}\n\n")
    }

    fn text_output() -> Value {
        json!([{
            "id": "msg_01",
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": "hello from the plan", "annotations": [] }]
        }])
    }

    #[tokio::test]
    async fn a_turn_carries_the_bearer_the_account_and_the_originator() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/codex/responses"))
            .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
            .and(header("chatgpt-account-id", ACCOUNT))
            .and(header("originator", ORIGINATOR))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse(text_output())),
            )
            .mount(&server)
            .await;

        let turn = inference(&server)
            .complete_text(&secret(), "gpt-5.3-codex", "hi")
            .await
            .expect("a turn");

        assert_eq!(turn.text, "hello from the plan");
        assert_eq!(turn.model, "gpt-5.3-codex");
        assert_eq!(turn.usage["input_tokens"], json!(12));

        let requests = server.received_requests().await.expect("recorded requests");
        let request: &Request = requests.first().expect("one request");
        // The credential travels in a header, never in the URL or the body.
        assert!(!request.url.as_str().contains(TOKEN));
        assert!(!String::from_utf8_lossy(&request.body).contains(TOKEN));
        // Every turn is correlated by its own session id.
        assert!(request.headers.contains_key("session_id"));
    }

    #[tokio::test]
    async fn strict_json_forces_the_tool_and_returns_its_arguments() {
        let server = MockServer::start().await;
        let answer = json!({ "verdict": "safe", "score": 3 });
        Mock::given(method("POST"))
            .and(path("/codex/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse(json!([{
                        "id": "fc_01",
                        "type": "function_call",
                        "status": "completed",
                        "call_id": "call_01",
                        "name": JSON_TOOL,
                        "arguments": answer.to_string(),
                    }]))),
            )
            .mount(&server)
            .await;

        let schema = json!({
            "type": "object",
            "properties": { "verdict": { "type": "string" }, "score": { "type": "integer" } },
            "required": ["verdict", "score"],
        });
        let (value, turn) = inference(&server)
            .complete_json(&secret(), "gpt-5.3-codex", "judge this", &schema)
            .await
            .expect("a json turn");

        assert_eq!(value, answer);
        assert_eq!(turn.usage["total_tokens"], json!(17));

        let requests = server.received_requests().await.expect("recorded requests");
        let body: Value = requests
            .first()
            .expect("one request")
            .body_json()
            .expect("json body");
        assert_eq!(body["tools"][0]["name"], json!(JSON_TOOL));
        assert_eq!(body["tools"][0]["parameters"], schema);
    }

    #[tokio::test]
    async fn a_tool_less_answer_to_a_strict_call_is_a_protocol_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse(text_output())),
            )
            .mount(&server)
            .await;

        let error = inference(&server)
            .complete_json(&secret(), "gpt-5.3-codex", "judge", &json!({ "type": "object" }))
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
                    "error": { "type": "invalid_request_error", "message": "token expired" }
                })))
                .mount(&server)
                .await;

            let error = inference(&server)
                .complete_text(&secret(), "gpt-5.3-codex", "hi")
                .await
                .expect_err("a rejected credential");
            assert_eq!(error, ProviderError::Authentication, "status {status}");
        }
    }

    #[tokio::test]
    async fn a_throttled_plan_is_retryable() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).set_body_json(json!({
                "error": { "type": "rate_limit_error", "message": "slow down" }
            })))
            .mount(&server)
            .await;

        let error = inference(&server)
            .complete_text(&secret(), "gpt-5.3-codex", "hi")
            .await
            .expect_err("a throttle");
        assert_eq!(error, ProviderError::RateLimited);
    }

    #[tokio::test]
    async fn a_body_that_is_not_a_responses_stream_is_a_protocol_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string("data: {\"type\":\"response.created\"}\n\n"),
            )
            .mount(&server)
            .await;

        let error = inference(&server)
            .complete_text(&secret(), "gpt-5.3-codex", "hi")
            .await
            .expect_err("no terminal event");
        assert!(matches!(error, ProviderError::InvalidResponse(_)));
    }

    #[tokio::test]
    async fn an_unknown_model_names_the_model() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "error": { "type": "invalid_request_error", "message": "unknown model" }
            })))
            .mount(&server)
            .await;

        let error = inference(&server)
            .complete_text(&secret(), "gpt-nope", "hi")
            .await
            .expect_err("an unknown model");
        assert_eq!(error, ProviderError::ModelUnavailable("gpt-nope".to_string()));
    }

    /// The reason this provider goes through rig at all: the prepared model is
    /// the value metalcraft's ReAct agent takes, so a plan-backed subscription
    /// answers inside a metalcraft graph with no adapter in between.
    #[tokio::test]
    async fn the_prepared_model_answers_inside_a_metalcraft_graph() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/codex/responses"))
            .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
            .and(header("chatgpt-account-id", ACCOUNT))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse(text_output())),
            )
            .mount(&server)
            .await;

        let model = inference(&server)
            .model(&secret(), "gpt-5.3-codex")
            .expect("a prepared rig model");
        let graph = create_react_agent(model, ToolRegistry::new(), "Answer briefly.")
            .expect("a react agent graph");

        let outcome = Executor::new(graph)
            .run(AgentState::new("hi"), "test-thread")
            .await
            .expect("the graph runs");

        let RunOutcome::Completed(state) = outcome else {
            panic!("the graph did not complete");
        };
        assert_eq!(state.final_answer(), Some("hello from the plan"));
    }
}
