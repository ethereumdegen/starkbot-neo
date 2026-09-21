//! Subscription OAuth: Starkbot holds the credential and calls the vendor's
//! inference API itself, instead of driving the vendor's CLI.
//!
//! Two flows live here, and they differ only in constants:
//!
//! * **Claude Pro/Max** — authorization code + PKCE at `claude.ai`, tokens at
//!   `api.anthropic.com`, JSON token bodies, `state` repeated in the
//!   exchange.
//! * **ChatGPT Plus/Pro (Codex)** — authorization code + PKCE at
//!   `auth.openai.com`, form token bodies, identity read out of the
//!   `id_token`.
//!
//! Everything the vendors differ on is data on [`OauthProvider`]; the code
//! path is shared. The endpoints are the only vendor hostnames in this module
//! and they are injected into [`OauthClient`], so every test here runs
//! against `wiremock` (08 rule 1).
//!
//! Tokens live in the macOS Keychain through [`OauthStore`] and nowhere else:
//! no variant of [`OauthError`] can carry a token or a token-endpoint body,
//! and every type that holds login material redacts its own formatting.

mod callback;
mod client;
mod pkce;
mod store;

use std::time::{SystemTime, UNIX_EPOCH};

use std::time::Duration;

use url::Url;

pub use callback::AuthCode;
pub use client::OauthClient;
pub use pkce::Verifier;
pub use store::{OauthCredential, OauthStore, REFRESH_WINDOW};

/// How a vendor wants its token-endpoint parameters encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenBody {
    /// `application/json` — Anthropic.
    Json,
    /// `application/x-www-form-urlencoded` — OpenAI.
    Form,
}

/// One vendor's OAuth shape. Constants only — no I/O.
#[derive(Debug)]
pub struct OauthProvider {
    pub id: &'static str,
    pub client_id: &'static str,
    pub authorize_url: &'static str,
    pub token_url: &'static str,
    pub scopes: &'static [&'static str],
    pub callback_port: u16,
    pub callback_path: &'static str,
    pub extra_authorize: &'static [(&'static str, &'static str)],
    pub token_headers: &'static [(&'static str, &'static str)],
    /// How the token endpoint wants its body. Not in the original sketch of
    /// this seam: Anthropic takes JSON and OpenAI takes a form, and sending
    /// the wrong one is a flat `400`.
    pub token_body: TokenBody,
    /// Whether the exchange body repeats the `state`. Anthropic's token
    /// endpoint requires it; OpenAI's ignores it.
    pub exchange_echoes_state: bool,
}

impl OauthProvider {
    /// The redirect the vendor has registered for this client. Both vendors
    /// register `localhost` (not `127.0.0.1`), and the string has to match
    /// byte for byte in the authorize request and again in the exchange.
    pub fn redirect_uri(&self) -> String {
        format!(
            "http://localhost:{}{}",
            self.callback_port, self.callback_path
        )
    }
}

/// How Starkbot Neo identifies itself to the Codex backend, in the authorize
/// request and later on every inference call.
pub const ORIGINATOR: &str = "starkbot-neo";

/// Claude Pro/Max.
pub const ANTHROPIC_OAUTH: OauthProvider = OauthProvider {
    id: "anthropic-oauth",
    client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
    authorize_url: "https://claude.ai/oauth/authorize",
    token_url: "https://api.anthropic.com/v1/oauth/token",
    scopes: &[
        "org:create_api_key",
        "user:profile",
        "user:inference",
        "user:sessions:claude_code",
        "user:mcp_servers",
        "user:file_upload",
    ],
    callback_port: 54545,
    callback_path: "/callback",
    extra_authorize: &[("code", "true")],
    token_headers: &[("anthropic-beta", "oauth-2025-04-20")],
    token_body: TokenBody::Json,
    exchange_echoes_state: true,
};

/// ChatGPT Plus/Pro, through the Codex client.
pub const OPENAI_CODEX: OauthProvider = OauthProvider {
    id: "openai-codex",
    client_id: "app_EMoamEEZ73f0CkXaXp7hrann",
    authorize_url: "https://auth.openai.com/oauth/authorize",
    token_url: "https://auth.openai.com/oauth/token",
    scopes: &[
        "openid",
        "profile",
        "email",
        "offline_access",
        "api.connectors.read",
        "api.connectors.invoke",
    ],
    callback_port: 1455,
    callback_path: "/auth/callback",
    extra_authorize: &[
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("originator", ORIGINATOR),
    ],
    token_headers: &[],
    token_body: TokenBody::Form,
    exchange_echoes_state: false,
};

/// Everything that can go wrong in a subscription login.
///
/// No variant holds a token, an authorization code, a PKCE verifier or a
/// token-endpoint response body. `stage` and `provider` are `&'static str`s
/// this module wrote, `status` is a number, and `detail` is either our own
/// prose or a transport message with the URL stripped off.
#[derive(Debug, thiserror::Error)]
pub enum OauthError {
    #[error("the OAuth endpoint `{detail}` is not a usable URL")]
    Url { detail: String },
    #[error("could not listen on 127.0.0.1:{port} for the sign-in callback: {detail}")]
    Listener { port: u16, detail: String },
    #[error("the sign-in callback did not arrive within {seconds}s")]
    Timeout { seconds: u64 },
    #[error("the sign-in callback did not match this login attempt")]
    StateMismatch,
    #[error("the sign-in was refused: {code}")]
    Denied { code: String },
    #[error("the sign-in callback carried no authorization code")]
    MissingCode,
    #[error("could not reach the token endpoint during {stage}: {detail}")]
    Transport { stage: &'static str, detail: String },
    #[error("the token endpoint answered {status} during {stage}")]
    Endpoint { stage: &'static str, status: u16 },
    #[error("{detail} ({stage})")]
    Protocol { stage: &'static str, detail: String },
    #[error("the `{provider}` sign-in is no longer valid; sign in again")]
    SignedOut { provider: &'static str },
    #[error("the stored `{provider}` credential is not usable")]
    StoredCredential { provider: &'static str },
    #[error(transparent)]
    Keychain(#[from] neo_keys::KeychainError),
}

/// PKCE + loopback + exchange.
pub struct OauthFlow;

impl OauthFlow {
    /// Begin a login. Pure: it mints PKCE material and builds the authorize
    /// URL, and touches neither the network nor the Keychain.
    pub fn start(provider: &'static OauthProvider) -> Result<PendingLogin, OauthError> {
        let verifier = Verifier::generate();
        let state = pkce::state_hex();
        let redirect_uri = provider.redirect_uri();
        let mut authorize_url =
            Url::parse(provider.authorize_url).map_err(|_| OauthError::Url {
                detail: provider.authorize_url.to_owned(),
            })?;
        {
            let mut query = authorize_url.query_pairs_mut();
            query
                .append_pair("response_type", "code")
                .append_pair("client_id", provider.client_id)
                .append_pair("redirect_uri", &redirect_uri)
                .append_pair("scope", &provider.scopes.join(" "))
                .append_pair("code_challenge", &verifier.challenge())
                .append_pair("code_challenge_method", "S256")
                .append_pair("state", &state);
            for (name, value) in provider.extra_authorize {
                query.append_pair(name, value);
            }
        }
        Ok(PendingLogin {
            provider,
            verifier,
            state,
            redirect_uri,
            authorize_url,
        })
    }
}

/// A login that has been started and is waiting for the user's browser.
pub struct PendingLogin {
    provider: &'static OauthProvider,
    verifier: Verifier,
    state: String,
    redirect_uri: String,
    authorize_url: Url,
}

impl PendingLogin {
    pub fn provider(&self) -> &'static OauthProvider {
        self.provider
    }

    /// The URL to open in the user's browser.
    pub fn authorize_url(&self) -> Url {
        self.authorize_url.clone()
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// The `state` this login will accept, for a caller that wants to show it
    /// or log the attempt. It is a public value in the authorize URL.
    pub fn state(&self) -> &str {
        &self.state
    }

    /// The PKCE verifier, to hand back to [`OauthClient::exchange`].
    pub fn verifier(&self) -> &Verifier {
        &self.verifier
    }

    /// Serve the loopback callback until the code arrives or `timeout`
    /// passes.
    ///
    /// The port is bound here, so start awaiting this *before* opening the
    /// browser — `tokio::join!` of this and the browser launch is the shape
    /// the caller wants.
    ///
    /// Borrows rather than consumes, so a front end can keep the login and
    /// race this against [`Self::code_from_redirect`]: a user whose browser
    /// cannot reach the loopback port pastes the redirect URL while this is
    /// still waiting.
    pub async fn wait_for_code(&self, timeout: Duration) -> Result<AuthCode, OauthError> {
        let listener = callback::bind(self.provider.callback_port)?;
        callback::wait_for_code(listener, self.provider.callback_path, &self.state, timeout).await
    }

    /// The paste-the-URL fallback for a browser that cannot reach this
    /// machine. Accepts the full redirect URL, Anthropic's `code#state`, or a
    /// bare code.
    pub fn code_from_redirect(&self, redirect: &str) -> Result<AuthCode, OauthError> {
        let redirect = redirect.trim();
        match Url::parse(redirect) {
            Ok(url) if url.query().is_some() => callback::validate_redirect(&url, &self.state),
            _ => callback::validate_pasted(redirect, &self.state),
        }
    }
}

/// Milliseconds since the Unix epoch. A clock before 1970, or after year
/// 292 million, is not a case worth an error type.
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn params(url: &Url) -> HashMap<String, String> {
        url.query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect()
    }

    #[test]
    fn anthropic_authorize_url_matches_the_shipped_flow() {
        #[allow(clippy::expect_used)]
        let login = OauthFlow::start(&ANTHROPIC_OAUTH).expect("flow starts");
        let url = login.authorize_url();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("claude.ai"));
        assert_eq!(url.path(), "/oauth/authorize");

        let params = params(&url);
        assert_eq!(
            params.get("response_type").map(String::as_str),
            Some("code")
        );
        assert_eq!(
            params.get("client_id").map(String::as_str),
            Some("9d1c250a-e61b-44d9-88ed-5944d1962f5e")
        );
        assert_eq!(
            params.get("redirect_uri").map(String::as_str),
            Some("http://localhost:54545/callback")
        );
        assert_eq!(
            params.get("scope").map(String::as_str),
            Some(
                "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload"
            )
        );
        assert_eq!(
            params.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(params.get("code").map(String::as_str), Some("true"));
        assert_eq!(
            params.get("code_challenge").map(String::as_str),
            Some(login.verifier().challenge().as_str())
        );
        assert_eq!(params.get("state").map(String::as_str), Some(login.state()));
        // Scopes must be space separated, not the `+`-as-plus of a form.
        assert!(
            url.as_str()
                .contains("scope=org%3Acreate_api_key+user%3Aprofile"),
            "unexpected scope encoding in {url}"
        );
    }

    #[test]
    fn openai_authorize_url_matches_the_shipped_flow() {
        #[allow(clippy::expect_used)]
        let login = OauthFlow::start(&OPENAI_CODEX).expect("flow starts");
        let url = login.authorize_url();
        assert_eq!(url.host_str(), Some("auth.openai.com"));
        assert_eq!(url.path(), "/oauth/authorize");

        let params = params(&url);
        assert_eq!(
            params.get("client_id").map(String::as_str),
            Some("app_EMoamEEZ73f0CkXaXp7hrann")
        );
        assert_eq!(
            params.get("redirect_uri").map(String::as_str),
            Some("http://localhost:1455/auth/callback")
        );
        assert_eq!(
            params.get("scope").map(String::as_str),
            Some("openid profile email offline_access api.connectors.read api.connectors.invoke")
        );
        assert_eq!(
            params.get("id_token_add_organizations").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            params.get("codex_cli_simplified_flow").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            params.get("originator").map(String::as_str),
            Some(ORIGINATOR)
        );
        assert_eq!(
            params.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert!(params.contains_key("code_challenge"));
    }

    #[test]
    fn two_logins_never_share_a_challenge_or_a_state() {
        #[allow(clippy::expect_used)]
        let first = OauthFlow::start(&ANTHROPIC_OAUTH).expect("flow starts");
        #[allow(clippy::expect_used)]
        let second = OauthFlow::start(&ANTHROPIC_OAUTH).expect("flow starts");
        assert_ne!(first.state(), second.state());
        assert_ne!(
            params(&first.authorize_url()).get("code_challenge"),
            params(&second.authorize_url()).get("code_challenge")
        );
    }

    #[test]
    fn a_redirect_from_another_login_is_refused() {
        #[allow(clippy::expect_used)]
        let login = OauthFlow::start(&OPENAI_CODEX).expect("flow starts");
        let error = login.code_from_redirect(
            "http://localhost:1455/auth/callback?code=ac_1&state=someone-elses-state",
        );
        assert!(matches!(error, Err(OauthError::StateMismatch)));
    }

    #[test]
    fn a_denied_redirect_reports_the_vendor_code() {
        #[allow(clippy::expect_used)]
        let login = OauthFlow::start(&OPENAI_CODEX).expect("flow starts");
        let error =
            login.code_from_redirect("http://localhost:1455/auth/callback?error=access_denied");
        match error {
            Err(OauthError::Denied { code }) => assert_eq!(code, "access_denied"),
            other => panic!("expected a denial, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_loopback_listener_hands_back_the_code_the_browser_delivered() {
        // Port 0: the real listener, without fighting a vendor's fixed port
        // or another test for it.
        #[allow(clippy::expect_used)]
        let listener = callback::bind(0).expect("binds a loopback port");
        let port = callback::bound_port(&listener);
        let serving = tokio::spawn(async move {
            callback::wait_for_code(listener, "/callback", "st_1", Duration::from_secs(10)).await
        });

        #[allow(clippy::expect_used)]
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("client builds");
        // A browser asks for the icon too; the listener must not treat that
        // as the callback.
        let _ = client
            .get(format!("http://127.0.0.1:{port}/favicon.ico"))
            .send()
            .await;
        #[allow(clippy::expect_used)]
        let response = client
            .get(format!(
                "http://127.0.0.1:{port}/callback?code=ac_loopback&state=st_1"
            ))
            .send()
            .await
            .expect("callback answered");
        assert!(response.status().is_success());
        #[allow(clippy::expect_used)]
        let page = response.text().await.expect("page body");
        assert!(page.contains("close this tab"));

        #[allow(clippy::expect_used)]
        let code = serving.await.expect("listener task").expect("code arrives");
        assert_eq!(format!("{code:?}"), "AuthCode(••••)");
        assert!(
            code_is(&code, "ac_loopback"),
            "the listener delivered the wrong code"
        );
    }

    #[tokio::test]
    async fn the_loopback_listener_refuses_a_foreign_state() {
        #[allow(clippy::expect_used)]
        let listener = callback::bind(0).expect("binds a loopback port");
        let port = callback::bound_port(&listener);
        let serving = tokio::spawn(async move {
            callback::wait_for_code(listener, "/callback", "st_1", Duration::from_secs(10)).await
        });

        let response = reqwest::get(format!(
            "http://127.0.0.1:{port}/callback?code=ac_1&state=st_evil"
        ))
        .await;
        #[allow(clippy::expect_used)]
        let response = response.expect("callback answered");
        assert_eq!(response.status().as_u16(), 400);

        #[allow(clippy::expect_used)]
        let outcome = serving.await.expect("listener task");
        assert!(matches!(outcome, Err(OauthError::StateMismatch)));
    }

    #[tokio::test]
    async fn the_loopback_listener_gives_up_on_time() {
        #[allow(clippy::expect_used)]
        let listener = callback::bind(0).expect("binds a loopback port");
        let outcome =
            callback::wait_for_code(listener, "/callback", "st_1", Duration::from_millis(50)).await;
        assert!(matches!(outcome, Err(OauthError::Timeout { .. })));
    }

    fn code_is(code: &AuthCode, expected: &str) -> bool {
        format!("{code:?}").contains("••••") && code_value(code) == expected
    }

    fn code_value(code: &AuthCode) -> String {
        // The accessor is crate-internal; a test in the crate may read it.
        code.code().to_owned()
    }

    /// One real sign-in, end to end: print the authorize URL, serve the
    /// loopback callback, and exchange the code at the vendor's own token
    /// endpoint. Nothing is stored and no token is printed.
    async fn live_login(provider: &'static OauthProvider) {
        #[allow(clippy::expect_used)]
        let login = OauthFlow::start(provider).expect("flow starts");
        eprintln!("open this in your browser:\n{}", login.authorize_url());
        // `wait_for_code` consumes the login, so the verifier is taken first
        // — the same two lines a real caller writes.
        let verifier = login.verifier().clone();
        #[allow(clippy::expect_used)]
        let client = OauthClient::hosted(provider).expect("client builds");
        #[allow(clippy::expect_used)]
        let code = login
            .wait_for_code(Duration::from_secs(300))
            .await
            .expect("the browser delivered a code");
        #[allow(clippy::expect_used)]
        let credential = client
            .exchange(&code, &verifier)
            .await
            .expect("the vendor exchanged the code");
        assert!(!credential.access_token.is_empty());
        assert!(!credential.refresh_token.is_empty());
        assert!(credential.expires_at_ms > now_ms());
        eprintln!(
            "signed in: account={:?} email={:?} plan={:?}",
            credential.account_id, credential.email, credential.plan
        );
    }

    /// Requires a Claude Pro/Max plan and a browser on this machine.
    ///
    /// `cargo test -p neo-agent --lib oauth::tests::live_anthropic_login -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "opens a browser and needs a real Claude Pro/Max subscription"]
    async fn live_anthropic_login() {
        live_login(&ANTHROPIC_OAUTH).await;
    }

    /// Requires a ChatGPT Plus/Pro plan and a browser on this machine.
    ///
    /// `cargo test -p neo-agent --lib oauth::tests::live_openai_login -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "opens a browser and needs a real ChatGPT Plus/Pro subscription"]
    async fn live_openai_login() {
        live_login(&OPENAI_CODEX).await;
    }
}
