//! `gpt-transcribe` over `POST {base_url}/v1/audio/transcriptions`.
//!
//! Raw `reqwest` rather than a typed client: the model and its parameters are
//! weeks old and typed clients lag. The vendor hostname exists only in this
//! file — everything else takes an injected `base_url`, which is also how the
//! tests point it at `wiremock`.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use neo_keys::Secret;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use reqwest::multipart::{Form, Part};
use reqwest::{Client, StatusCode};
use url::Url;

use super::{Transcriber, Transcript, wav};
use crate::capture::Utterance;
use crate::error::VoiceError;

/// The hosted OpenAI API. The only vendor hostname in `neo-voice`.
pub const HOSTED_BASE: &str = "https://api.openai.com";

/// The only transcription model offered (K3 hides the deprecated ids).
pub const MODEL: &str = "gpt-transcribe";

/// Per the plan's latency budget: 8 s, one retry.
const TIMEOUT: Duration = Duration::from_secs(8);
const RETRY_DELAY: Duration = Duration::from_millis(250);

/// How much of an error body is worth keeping in a message.
const MAX_ERROR_BODY: usize = 400;

/// Per-utterance transcription through the OpenAI audio API.
pub struct OpenAiTranscriber {
    endpoint: Url,
    client: Client,
}

impl OpenAiTranscriber {
    /// A transcriber for `base_url`, defaulting to the hosted API.
    ///
    /// The credential is consumed here and only here: it becomes one
    /// sensitive `Authorization` header on this client and is never stored,
    /// logged or returned.
    pub fn new(key: &Secret, base_url: Option<&Url>) -> Result<Self, VoiceError> {
        let base = match base_url {
            Some(url) => url.clone(),
            None => Url::parse(HOSTED_BASE).map_err(|e| VoiceError::Transport {
                detail: format!("the built-in base URL is invalid: {e}"),
            })?,
        };
        let endpoint =
            base.join("/v1/audio/transcriptions")
                .map_err(|e| VoiceError::Transport {
                    detail: e.to_string(),
                })?;

        // The audited credential boundary clippy.toml points at: the secret
        // becomes one `Authorization` header, marked sensitive so it is
        // redacted from any header dump, and nothing else.
        #[allow(clippy::disallowed_methods)]
        let mut header =
            HeaderValue::from_str(&format!("Bearer {}", key.expose())).map_err(|_| {
                VoiceError::Transport {
                    detail: "the OpenAI key is not a valid header value".into(),
                }
            })?;
        header.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, header);

        let client = Client::builder()
            .default_headers(headers)
            .timeout(TIMEOUT)
            .build()
            .map_err(|e| VoiceError::Transport {
                detail: e.to_string(),
            })?;
        Ok(Self { endpoint, client })
    }

    fn form(wav: Vec<u8>) -> Result<Form, VoiceError> {
        let audio = Part::bytes(wav)
            .file_name("utterance.wav")
            .mime_str("audio/wav")
            .map_err(|e| VoiceError::Transport {
                detail: e.to_string(),
            })?;
        Ok(Form::new()
            .part("file", audio)
            .text("model", MODEL)
            .text("response_format", "json"))
    }
}

#[async_trait]
impl Transcriber for OpenAiTranscriber {
    async fn transcribe(&self, utterance: &Utterance) -> Result<Transcript, VoiceError> {
        let wav = wav::encode(&utterance.pcm16, utterance.sample_rate)?;
        let started = Instant::now();

        let mut last: Option<VoiceError> = None;
        for attempt in 0..2u8 {
            if attempt > 0 {
                tokio::time::sleep(RETRY_DELAY).await;
            }
            let response = self
                .client
                .post(self.endpoint.clone())
                .multipart(Self::form(wav.clone())?)
                .send()
                .await;
            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    // No URL, no header: `reqwest`'s Display can carry the
                    // request URL, and that is all we ever want of it.
                    last = Some(VoiceError::Transport {
                        detail: error.to_string(),
                    });
                    continue;
                }
            };
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            if status.is_success() {
                return parse(&body, started.elapsed());
            }
            let mut detail = body;
            detail.truncate(MAX_ERROR_BODY);
            let error = VoiceError::Provider {
                provider: "openai",
                status: status.as_u16(),
                detail,
            };
            if !retryable(status) {
                return Err(error);
            }
            last = Some(error);
        }
        Err(last.unwrap_or(VoiceError::Transport {
            detail: "no attempt was made".into(),
        }))
    }

    fn name(&self) -> &'static str {
        "openai"
    }
}

fn retryable(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn parse(body: &str, elapsed: Duration) -> Result<Transcript, VoiceError> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| VoiceError::BadResponse {
            provider: "openai",
            detail: e.to_string(),
        })?;
    let text = value
        .get("text")
        .and_then(serde_json::Value::as_str)
        .ok_or(VoiceError::BadResponse {
            provider: "openai",
            detail: "no `text` field".into(),
        })?;
    Ok(Transcript {
        text: text.to_owned(),
        // The endpoint reports logprobs, not a confidence; claiming one would
        // be a number the caller cannot trust.
        confidence: None,
        duration_ms: elapsed.as_millis().min(u128::from(u64::MAX)) as u64,
    })
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]
    use super::*;
    use wiremock::matchers::{header_exists, method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    fn utterance() -> Utterance {
        Utterance {
            pcm16: vec![0i16, 512, -512, 1_024],
            sample_rate: 16_000,
            duration: Duration::from_millis(250),
        }
    }

    fn transcriber(base: &str) -> OpenAiTranscriber {
        let key = Secret::new("sk-test").expect("secret");
        let base = Url::parse(base).expect("base url");
        OpenAiTranscriber::new(&key, Some(&base)).expect("transcriber")
    }

    #[tokio::test]
    async fn the_upload_is_multipart_with_a_wav_part_and_the_model_field() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .and(header_exists("authorization"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "text": "open Hypercanvas"
            })))
            .mount(&server)
            .await;

        let transcript = transcriber(&server.uri())
            .transcribe(&utterance())
            .await
            .expect("transcribe");
        assert_eq!(transcript.text, "open Hypercanvas");
        assert_eq!(transcript.confidence, None);

        let requests = server.received_requests().await.unwrap_or_default();
        let request: &Request = requests.first().expect("one request");

        let content_type = request
            .headers
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert!(
            content_type.starts_with("multipart/form-data; boundary="),
            "content-type was {content_type}"
        );

        let body = String::from_utf8_lossy(&request.body);
        assert!(
            body.contains(r#"name="file"; filename="utterance.wav""#),
            "no named WAV part in the body"
        );
        assert!(
            body.contains("Content-Type: audio/wav"),
            "the part is not typed as WAV"
        );
        assert!(
            body.contains("RIFF") && body.contains("WAVE"),
            "the part is not a WAV file"
        );
        assert!(body.contains(r#"name="model""#), "no model field");
        assert!(body.contains(MODEL), "the model field is not `{MODEL}`");
        assert!(
            body.contains(r#"name="response_format""#),
            "no response_format field"
        );
    }

    #[tokio::test]
    async fn the_credential_travels_in_the_header_and_never_in_the_url_or_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"text": "ok"})),
            )
            .mount(&server)
            .await;
        transcriber(&server.uri())
            .transcribe(&utterance())
            .await
            .expect("transcribe");

        let requests = server.received_requests().await.unwrap_or_default();
        let request = requests.first().expect("one request");
        assert_eq!(
            request
                .headers
                .get("authorization")
                .and_then(|v| v.to_str().ok()),
            Some("Bearer sk-test")
        );
        assert!(!request.url.as_str().contains("sk-test"));
        assert!(!String::from_utf8_lossy(&request.body).contains("sk-test"));
    }

    #[tokio::test]
    async fn a_rate_limit_is_retried_once_then_reported() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).set_body_string("slow down"))
            .expect(2)
            .mount(&server)
            .await;

        let error = transcriber(&server.uri())
            .transcribe(&utterance())
            .await
            .expect_err("429 should surface");
        assert!(matches!(
            error,
            VoiceError::Provider {
                provider: "openai",
                status: 429,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn an_invalid_key_is_not_retried() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
            .expect(1)
            .mount(&server)
            .await;

        let error = transcriber(&server.uri())
            .transcribe(&utterance())
            .await
            .expect_err("401 should surface");
        match error {
            VoiceError::Provider {
                status: 401,
                detail,
                ..
            } => assert!(detail.contains("bad key")),
            other => panic!("expected a 401 verdict, got {other}"),
        }
    }

    #[tokio::test]
    async fn a_body_without_text_is_a_bad_response_not_an_empty_transcript() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"oops": 1})))
            .mount(&server)
            .await;

        let error = transcriber(&server.uri())
            .transcribe(&utterance())
            .await
            .expect_err("a shapeless body should surface");
        assert!(matches!(
            error,
            VoiceError::BadResponse {
                provider: "openai",
                ..
            }
        ));
    }

    #[test]
    fn the_endpoint_is_built_under_the_injected_base() {
        let base = Url::parse("http://127.0.0.1:9/some/prefix").expect("base");
        let key = Secret::new("sk-test").expect("secret");
        let transcriber = OpenAiTranscriber::new(&key, Some(&base)).expect("transcriber");
        assert_eq!(
            transcriber.endpoint.as_str(),
            "http://127.0.0.1:9/v1/audio/transcriptions"
        );
    }

    #[test]
    fn the_default_base_is_the_hosted_api() {
        let key = Secret::new("sk-test").expect("secret");
        let transcriber = OpenAiTranscriber::new(&key, None).expect("transcriber");
        assert_eq!(
            transcriber.endpoint.as_str(),
            "https://api.openai.com/v1/audio/transcriptions"
        );
    }
}
