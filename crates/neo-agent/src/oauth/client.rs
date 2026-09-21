//! The token endpoint: authorization-code exchange and refresh.
//!
//! The endpoint URL is injected (08 rule 1), so every test here talks to
//! `wiremock` and the vendor hostnames only appear as provider constants.
//!
//! Two vendor shapes are covered by one code path: Anthropic posts JSON and
//! wants the `state` repeated in the exchange body, OpenAI posts a form. Both
//! are described by [`OauthProvider`] rather than branched on by id.

use std::fmt;
use std::time::Duration;

use neo_keys::Secret;
use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use url::{Url, form_urlencoded};

use super::{AuthCode, OauthCredential, OauthError, OauthProvider, TokenBody, Verifier, now_ms};

/// A token request is a single small round trip; anything slower than this is
/// a network problem, not a slow vendor.
const TIMEOUT: Duration = Duration::from_secs(30);

/// When a vendor answers without `expires_in`, assume the hour both of them
/// actually issue. The five-minute refresh window makes a wrong guess cheap.
const DEFAULT_EXPIRES_IN: i64 = 3600;

/// Which stage of the flow an error came from. A `&'static str`, so no
/// response content can ride along.
const EXCHANGE: &str = "exchange";
const REFRESH: &str = "refresh";

pub struct OauthClient {
    provider: &'static OauthProvider,
    token_url: Url,
    http: Client,
}

impl fmt::Debug for OauthClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OauthClient")
            .field("provider", &self.provider.id)
            .field("token_url", &self.token_url.as_str())
            .finish()
    }
}

impl OauthClient {
    /// A client against `token_url`, which a test points at `wiremock`.
    pub fn new(provider: &'static OauthProvider, token_url: Url) -> Result<Self, OauthError> {
        let http = Client::builder()
            .timeout(TIMEOUT)
            .build()
            .map_err(|error| OauthError::Transport {
                stage: "client",
                detail: error.to_string(),
            })?;
        Ok(Self {
            provider,
            token_url,
            http,
        })
    }

    /// A client against the provider's real token endpoint.
    pub fn hosted(provider: &'static OauthProvider) -> Result<Self, OauthError> {
        let token_url = Url::parse(provider.token_url).map_err(|error| OauthError::Url {
            detail: error.to_string(),
        })?;
        Self::new(provider, token_url)
    }

    pub fn provider(&self) -> &'static OauthProvider {
        self.provider
    }

    pub async fn exchange(
        &self,
        code: &AuthCode,
        verifier: &Verifier,
    ) -> Result<OauthCredential, OauthError> {
        self.exchange_at(code, verifier, now_ms()).await
    }

    /// [`OauthClient::exchange`] against an injected clock.
    pub async fn exchange_at(
        &self,
        code: &AuthCode,
        verifier: &Verifier,
        now_ms: i64,
    ) -> Result<OauthCredential, OauthError> {
        let mut form = vec![
            ("grant_type", "authorization_code".to_owned()),
            ("code", code.code().to_owned()),
            ("redirect_uri", self.provider.redirect_uri()),
            ("client_id", self.provider.client_id.to_owned()),
            ("code_verifier", verifier.as_str().to_owned()),
        ];
        if self.provider.exchange_echoes_state {
            form.push(("state", code.state().to_owned()));
        }
        let response = self.post(EXCHANGE, form).await?;
        Ok(response.into_credential(now_ms, None))
    }

    pub async fn refresh(&self, refresh_token: &Secret) -> Result<OauthCredential, OauthError> {
        self.refresh_at(refresh_token, now_ms()).await
    }

    /// [`OauthClient::refresh`] against an injected clock.
    pub async fn refresh_at(
        &self,
        refresh_token: &Secret,
        now_ms: i64,
    ) -> Result<OauthCredential, OauthError> {
        // The audited credential boundary clippy.toml points at: the stored
        // refresh token becomes one field of one POST body to the injected
        // token endpoint, and is never logged or returned in an error.
        #[allow(clippy::disallowed_methods)]
        let exposed = refresh_token.expose().to_owned();
        let form = vec![
            ("grant_type", "refresh_token".to_owned()),
            ("refresh_token", exposed.clone()),
            ("client_id", self.provider.client_id.to_owned()),
        ];
        let response = self.post(REFRESH, form).await?;
        // A vendor that rotates refresh tokens sends a new one; one that does
        // not omits the field, and the old token stays valid.
        Ok(response.into_credential(now_ms, Some(exposed)))
    }

    async fn post(
        &self,
        stage: &'static str,
        form: Vec<(&'static str, String)>,
    ) -> Result<TokenResponse, OauthError> {
        let mut request = self.http.post(self.token_url.clone());
        for (name, value) in self.provider.token_headers {
            request = request.header(*name, *value);
        }
        request = match self.provider.token_body {
            TokenBody::Json => {
                let body = form
                    .iter()
                    .map(|(name, value)| {
                        ((*name).to_owned(), serde_json::Value::String(value.clone()))
                    })
                    .collect::<serde_json::Map<_, _>>();
                request.json(&body)
            }
            TokenBody::Form => {
                // `reqwest`'s own `.form()` is behind a feature the
                // workspace does not enable; `url` already carries the
                // encoder.
                let mut body = form_urlencoded::Serializer::new(String::new());
                for (name, value) in &form {
                    body.append_pair(name, value);
                }
                request
                    .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(body.finish())
            }
        };
        let response = request.send().await.map_err(|error| OauthError::Transport {
            stage,
            // `reqwest`'s message names the endpoint and the failure kind; a
            // request body never appears in it.
            detail: error.without_url().to_string(),
        })?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| OauthError::Transport {
                stage,
                detail: error.without_url().to_string(),
            })?;
        if !status.is_success() {
            // The body is inspected for exactly two OAuth error codes and is
            // then dropped. It is never logged, stored or attached to an
            // error, because an error body from a token endpoint can quote
            // the request that produced it.
            return Err(if is_dead_grant(status, &body) {
                OauthError::SignedOut {
                    provider: self.provider.id,
                }
            } else {
                OauthError::Endpoint {
                    stage,
                    status: status.as_u16(),
                }
            });
        }
        serde_json::from_str::<TokenResponse>(&body).map_err(|_| OauthError::Protocol {
            stage,
            // Deliberately our own words: a parser message can quote input.
            detail: "the token endpoint answered in an unexpected shape".to_owned(),
        })
    }
}

/// A refusal the user can only fix by signing in again.
fn is_dead_grant(status: StatusCode, body: &str) -> bool {
    matches!(status, StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED)
        && (body.contains("invalid_grant") || body.contains("invalid_token"))
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    id_token: Option<String>,
    /// Anthropic returns the signed-in identity alongside the tokens.
    #[serde(default)]
    account: Option<AccountClaims>,
}

#[derive(Deserialize)]
struct AccountClaims {
    #[serde(default)]
    uuid: Option<String>,
    #[serde(default)]
    email_address: Option<String>,
}

impl TokenResponse {
    fn into_credential(self, now_ms: i64, previous_refresh: Option<String>) -> OauthCredential {
        let identity = self.id_token.as_deref().map(identity_from_id_token);
        let (mut account_id, mut email, plan) = identity.unwrap_or_default();
        if let Some(account) = &self.account {
            account_id = account_id.or_else(|| account.uuid.clone());
            email = email.or_else(|| account.email_address.clone());
        }
        let expires_in = self.expires_in.unwrap_or(DEFAULT_EXPIRES_IN);
        OauthCredential {
            access_token: self.access_token,
            refresh_token: self
                .refresh_token
                .or(previous_refresh)
                .unwrap_or_default(),
            expires_at_ms: now_ms.saturating_add(expires_in.saturating_mul(1000)),
            account_id,
            email,
            plan,
        }
    }
}

/// OpenAI's namespaced `id_token` claims.
const AUTH_CLAIM: &str = "https://api.openai.com/auth";
const PROFILE_CLAIM: &str = "https://api.openai.com/profile";

/// Read `account_id`, `email` and `plan` out of an `id_token`.
///
/// **The signature is not verified, and nothing read here is trusted for
/// authorization.** The token was just handed to us over TLS by the endpoint
/// we asked, and the only use for these claims is addressing and display: the
/// `ChatGPT-Account-Id` header, the account row's email and the plan label.
/// Authorization is the access token's job, decided by the vendor.
fn identity_from_id_token(id_token: &str) -> (Option<String>, Option<String>, Option<String>) {
    let Some(payload) = jwt_payload(id_token) else {
        return (None, None, None);
    };
    let auth = payload.get(AUTH_CLAIM);
    let profile = payload.get(PROFILE_CLAIM);
    let account_id = auth
        .and_then(|claims| claims.get("chatgpt_account_id"))
        .and_then(|value| value.as_str())
        .or_else(|| payload.get("account_id").and_then(|value| value.as_str()))
        .map(str::to_owned);
    let email = profile
        .and_then(|claims| claims.get("email"))
        .and_then(|value| value.as_str())
        .or_else(|| payload.get("email").and_then(|value| value.as_str()))
        .map(|value| value.trim().to_lowercase());
    let plan = auth
        .and_then(|claims| claims.get("chatgpt_plan_type"))
        .and_then(|value| value.as_str())
        .map(|value| value.trim().to_lowercase());
    (account_id, non_empty(email), non_empty(plan))
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

/// The middle segment of a JWT, base64url-decoded and parsed as JSON.
fn jwt_payload(id_token: &str) -> Option<serde_json::Value> {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    let mut segments = id_token.split('.');
    let _header = segments.next()?;
    let payload = segments.next()?;
    segments.next()?;
    let decoded = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&decoded).ok()
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use serde_json::json;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::oauth::{ANTHROPIC_OAUTH, OPENAI_CODEX, OauthFlow};

    fn id_token(account: &str, email: &str, plan: &str) -> String {
        let payload = json!({
            AUTH_CLAIM: { "chatgpt_account_id": account, "chatgpt_plan_type": plan },
            PROFILE_CLAIM: { "email": email },
        });
        #[allow(clippy::expect_used)]
        let payload = serde_json::to_vec(&payload).expect("claims serialise");
        format!(
            "{}.{}.{}",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#),
            URL_SAFE_NO_PAD.encode(payload),
            URL_SAFE_NO_PAD.encode(b"not-a-signature")
        )
    }

    fn client(provider: &'static OauthProvider, server: &MockServer) -> OauthClient {
        #[allow(clippy::expect_used)]
        let url = Url::parse(&format!("{}/oauth/token", server.uri())).expect("mock URL");
        #[allow(clippy::expect_used)]
        OauthClient::new(provider, url).expect("client builds")
    }

    #[tokio::test]
    async fn anthropic_exchange_posts_json_with_the_verifier_and_state() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .and(header("anthropic-beta", "oauth-2025-04-20"))
            .and(header("content-type", "application/json"))
            .and(body_string_contains("\"grant_type\":\"authorization_code\""))
            .and(body_string_contains("\"code_verifier\""))
            .and(body_string_contains("\"state\""))
            .and(body_string_contains(
                "\"redirect_uri\":\"http://localhost:54545/callback\"",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "at_anthropic",
                "refresh_token": "rt_anthropic",
                "expires_in": 3600,
                "account": { "uuid": "acct_9", "email_address": "user@example.com" },
            })))
            .mount(&server)
            .await;

        #[allow(clippy::expect_used)]
        let login = OauthFlow::start(&ANTHROPIC_OAUTH).expect("flow starts");
        #[allow(clippy::expect_used)]
        let code = login
            .code_from_redirect(&format!("ac_1#{}", login.state()))
            .expect("code parses");
        #[allow(clippy::expect_used)]
        let credential = client(&ANTHROPIC_OAUTH, &server)
            .exchange_at(&code, login.verifier(), 1_000)
            .await
            .expect("exchange succeeds");

        assert_eq!(credential.access_token, "at_anthropic");
        assert_eq!(credential.refresh_token, "rt_anthropic");
        assert_eq!(credential.expires_at_ms, 1_000 + 3_600_000);
        assert_eq!(credential.account_id.as_deref(), Some("acct_9"));
        assert_eq!(credential.email.as_deref(), Some("user@example.com"));
    }

    #[tokio::test]
    async fn openai_exchange_posts_a_form_and_reads_the_id_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .and(header(
                "content-type",
                "application/x-www-form-urlencoded",
            ))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code_verifier="))
            .and(body_string_contains(
                "redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "at_openai",
                "refresh_token": "rt_openai",
                "expires_in": 600,
                "id_token": id_token("acct_chatgpt", "Someone@Example.com ", "Pro"),
            })))
            .mount(&server)
            .await;

        #[allow(clippy::expect_used)]
        let login = OauthFlow::start(&OPENAI_CODEX).expect("flow starts");
        #[allow(clippy::expect_used)]
        let code = login
            .code_from_redirect(&format!(
                "http://localhost:1455/auth/callback?code=ac_2&state={}",
                login.state()
            ))
            .expect("code parses");
        #[allow(clippy::expect_used)]
        let credential = client(&OPENAI_CODEX, &server)
            .exchange_at(&code, login.verifier(), 0)
            .await
            .expect("exchange succeeds");

        assert_eq!(credential.access_token, "at_openai");
        assert_eq!(credential.expires_at_ms, 600_000);
        assert_eq!(credential.account_id.as_deref(), Some("acct_chatgpt"));
        assert_eq!(credential.email.as_deref(), Some("someone@example.com"));
        assert_eq!(credential.plan.as_deref(), Some("pro"));
    }

    #[tokio::test]
    async fn refresh_keeps_the_old_token_when_the_vendor_omits_one() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .and(body_string_contains("\"grant_type\":\"refresh_token\""))
            .and(body_string_contains("rt_old"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "at_new",
                "expires_in": 60,
            })))
            .mount(&server)
            .await;

        #[allow(clippy::expect_used)]
        let stored = Secret::new("rt_old").expect("secret");
        #[allow(clippy::expect_used)]
        let credential = client(&ANTHROPIC_OAUTH, &server)
            .refresh_at(&stored, 5_000)
            .await
            .expect("refresh succeeds");

        assert_eq!(credential.access_token, "at_new");
        assert_eq!(credential.refresh_token, "rt_old");
        assert_eq!(credential.expires_at_ms, 5_000 + 60_000);
    }

    #[tokio::test]
    async fn a_rotated_refresh_token_replaces_the_old_one() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "at_new",
                "refresh_token": "rt_new",
                "expires_in": 60,
            })))
            .mount(&server)
            .await;

        #[allow(clippy::expect_used)]
        let stored = Secret::new("rt_old").expect("secret");
        #[allow(clippy::expect_used)]
        let credential = client(&OPENAI_CODEX, &server)
            .refresh_at(&stored, 0)
            .await
            .expect("refresh succeeds");
        assert_eq!(credential.refresh_token, "rt_new");
    }

    #[tokio::test]
    async fn a_dead_grant_is_told_apart_from_a_server_fault() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": "invalid_grant",
                "error_description": "refresh token rt_old is revoked",
            })))
            .mount(&server)
            .await;

        #[allow(clippy::expect_used)]
        let stored = Secret::new("rt_old").expect("secret");
        let error = client(&OPENAI_CODEX, &server)
            .refresh_at(&stored, 0)
            .await
            .err();
        match error {
            Some(error @ OauthError::SignedOut { .. }) => {
                let rendered = format!("{error} {error:?}");
                assert!(!rendered.contains("rt_old"), "error quoted the token");
                assert!(!rendered.contains("revoked"), "error quoted the body");
            }
            other => panic!("expected a signed-out error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_server_fault_is_not_a_sign_out() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503).set_body_string("upstream down"))
            .mount(&server)
            .await;

        #[allow(clippy::expect_used)]
        let stored = Secret::new("rt_old").expect("secret");
        let error = client(&ANTHROPIC_OAUTH, &server)
            .refresh_at(&stored, 0)
            .await
            .err();
        match error {
            Some(OauthError::Endpoint { stage, status }) => {
                assert_eq!(stage, "refresh");
                assert_eq!(status, 503);
            }
            other => panic!("expected an endpoint error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_unreadable_answer_never_quotes_the_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(r#"{"token":"at_leaked"}"#),
            )
            .mount(&server)
            .await;

        #[allow(clippy::expect_used)]
        let stored = Secret::new("rt_old").expect("secret");
        let error = client(&ANTHROPIC_OAUTH, &server)
            .refresh_at(&stored, 0)
            .await
            .err();
        match error {
            Some(error @ OauthError::Protocol { .. }) => {
                let rendered = format!("{error} {error:?}");
                assert!(!rendered.contains("at_leaked"));
            }
            other => panic!("expected a protocol error, got {other:?}"),
        }
    }

    #[test]
    fn a_malformed_id_token_yields_no_identity() {
        assert_eq!(identity_from_id_token("not-a-jwt"), (None, None, None));
        assert_eq!(identity_from_id_token("a.!!!.c"), (None, None, None));
    }
}
