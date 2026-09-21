//! Where a subscription credential lives: one macOS Keychain item per
//! provider id, holding the credential as a single JSON object.
//!
//! Nothing else in the workspace may persist a token. `neo-core`'s
//! `ProviderAccount` — status, email, plan, allowance — is the redacted
//! shadow that goes into SQLite; the tokens themselves never leave this file
//! except as a `Secret` handed to a provider's request builder.

use std::fmt;
use std::time::Duration;

use neo_keys::{Keychain, Secret};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize as _;

use super::{OauthClient, OauthError, OauthProvider, now_ms};

/// Refresh this far before the vendor's stated expiry, so a long request
/// started at the edge of the window does not die mid-stream.
pub const REFRESH_WINDOW: Duration = Duration::from_secs(5 * 60);

const REFRESH_WINDOW_MS: i64 = 5 * 60 * 1000;

/// What a completed login yields.
///
/// Serialised as one JSON blob into the Keychain account named by
/// [`OauthProvider::id`], and nowhere else. There is no `Debug` derive: the
/// hand-written one prints the metadata and redacts both tokens.
#[derive(Clone, Serialize, Deserialize)]
pub struct OauthCredential {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
}

impl OauthCredential {
    /// Whether this credential needs refreshing at `now_ms` (05: five
    /// minutes of slack).
    pub fn needs_refresh(&self, now_ms: i64) -> bool {
        self.expires_at_ms.saturating_sub(now_ms) < REFRESH_WINDOW_MS
    }
}

impl Drop for OauthCredential {
    fn drop(&mut self) {
        self.access_token.zeroize();
        self.refresh_token.zeroize();
    }
}

impl fmt::Debug for OauthCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OauthCredential")
            .field("access_token", &"••••")
            .field("refresh_token", &"••••")
            .field("expires_at_ms", &self.expires_at_ms)
            .field("account_id", &self.account_id)
            .field("email", &self.email)
            .field("plan", &self.plan)
            .finish()
    }
}

impl fmt::Display for OauthCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, formatter)
    }
}

/// Keychain-backed credential storage, one account per provider id.
pub struct OauthStore {
    keychain: Keychain,
}

impl Default for OauthStore {
    fn default() -> Self {
        Self::new(Keychain::default())
    }
}

impl OauthStore {
    pub fn new(keychain: Keychain) -> Self {
        Self { keychain }
    }

    /// The Keychain service these credentials live under. Tests pass a unique
    /// per-run service so they never touch the developer's real items.
    pub fn service(&self) -> &str {
        self.keychain.service()
    }

    pub fn load(&self, provider: &OauthProvider) -> Result<Option<OauthCredential>, OauthError> {
        let Some(blob) = self.keychain.get(provider.id)? else {
            return Ok(None);
        };
        // The audited credential boundary: the stored JSON leaves `Secret`
        // only to become an `OauthCredential`, which redacts its own
        // formatting. Nothing here logs, and a malformed item is reported by
        // shape, never by content.
        #[allow(clippy::disallowed_methods)]
        let parsed = serde_json::from_str::<OauthCredential>(blob.expose()).ok();
        parsed.map(Some).ok_or(OauthError::StoredCredential {
            provider: provider.id,
        })
    }

    pub fn save(
        &self,
        provider: &OauthProvider,
        credential: &OauthCredential,
    ) -> Result<(), OauthError> {
        let json = serde_json::to_string(credential).map_err(|_| OauthError::StoredCredential {
            provider: provider.id,
        })?;
        // The string is moved into `Secret`, which wipes it on drop.
        let blob = Secret::new(json).map_err(|_| OauthError::StoredCredential {
            provider: provider.id,
        })?;
        self.keychain.set(provider.id, &blob)?;
        Ok(())
    }

    pub fn clear(&self, provider: &OauthProvider) -> Result<(), OauthError> {
        self.keychain.delete(provider.id)?;
        Ok(())
    }

    /// The access token to send on the next request, refreshed first when it
    /// expires within [`REFRESH_WINDOW`].
    ///
    /// A definitive refusal from the vendor clears the stored credential and
    /// answers [`OauthError::SignedOut`], so the caller can say "sign in
    /// again"; a transport failure leaves the credential alone.
    pub async fn access_token(
        &self,
        client: &OauthClient,
        provider: &'static OauthProvider,
    ) -> Result<Secret, OauthError> {
        self.access_token_at(client, provider, now_ms()).await
    }

    /// [`OauthStore::access_token`] against an injected clock.
    pub async fn access_token_at(
        &self,
        client: &OauthClient,
        provider: &'static OauthProvider,
        now_ms: i64,
    ) -> Result<Secret, OauthError> {
        let Some(credential) = self.load(provider)? else {
            return Err(OauthError::SignedOut {
                provider: provider.id,
            });
        };
        if !credential.needs_refresh(now_ms) {
            return secret(provider, &credential.access_token);
        }
        let refresh = secret(provider, &credential.refresh_token)?;
        match client.refresh_at(&refresh, now_ms).await {
            Ok(fresh) => {
                self.save(provider, &fresh)?;
                secret(provider, &fresh.access_token)
            }
            Err(error @ OauthError::SignedOut { .. }) => {
                // The grant is dead; keeping it would only produce the same
                // refusal on every later request.
                self.clear(provider)?;
                Err(error)
            }
            Err(error) => Err(error),
        }
    }
}

fn secret(provider: &OauthProvider, value: &str) -> Result<Secret, OauthError> {
    Secret::new(value).map_err(|_| OauthError::StoredCredential {
        provider: provider.id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn credential(expires_at_ms: i64) -> OauthCredential {
        OauthCredential {
            access_token: "at_live_1".to_owned(),
            refresh_token: "rt_live_1".to_owned(),
            expires_at_ms,
            account_id: Some("acct_1".to_owned()),
            email: Some("someone@example.com".to_owned()),
            plan: Some("pro".to_owned()),
        }
    }

    #[test]
    fn refresh_window_is_five_minutes_wide() {
        let credential = credential(1_000_000_000);
        assert!(!credential.needs_refresh(1_000_000_000 - REFRESH_WINDOW_MS - 1));
        assert!(credential.needs_refresh(1_000_000_000 - REFRESH_WINDOW_MS + 1));
        assert!(credential.needs_refresh(1_000_000_000));
        assert!(credential.needs_refresh(i64::MAX));
    }

    #[test]
    fn formatting_never_shows_a_token() {
        let credential = credential(0);
        let rendered = format!("{credential:?} {credential}");
        assert!(!rendered.contains("at_live_1"));
        assert!(!rendered.contains("rt_live_1"));
        assert!(rendered.contains("someone@example.com"));
    }

    #[test]
    fn json_round_trips_without_line_breaks() {
        #[allow(clippy::expect_used)]
        let json = serde_json::to_string(&credential(42)).expect("serialises");
        assert!(!json.contains('\n'));
        #[allow(clippy::expect_used)]
        let back: OauthCredential = serde_json::from_str(&json).expect("parses");
        assert_eq!(back.access_token, "at_live_1");
        assert_eq!(back.expires_at_ms, 42);
        assert_eq!(back.plan.as_deref(), Some("pro"));
    }
}

/// These touch the developer's real login Keychain. They confine themselves
/// to a unique per-run service name and delete every item they create,
/// however the test ends.
#[cfg(all(test, target_os = "macos"))]
mod keychain_tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;
    use url::Url;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::tests::credential;
    use super::*;
    use crate::oauth::ANTHROPIC_OAUTH;

    /// A credential store for one test, on its own file.
    ///
    /// Never the login Keychain (an unsigned test binary would prompt for
    /// authorization on every read) and never the shared dev key file (these
    /// tests run in parallel, and a shared read-modify-write would race).
    struct TestStore {
        store: OauthStore,
        _directory: tempfile::TempDir,
    }

    impl TestStore {
        fn new(label: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default();
            let service = format!(
                "com.starkbot.neo.test.oauth.{label}.{}.{nanos}",
                std::process::id()
            );
            let directory = match tempfile::tempdir() {
                Ok(directory) => directory,
                Err(error) => panic!("a temporary directory: {error}"),
            };
            Self {
                store: OauthStore::new(Keychain::file(
                    service,
                    directory.path().join("credentials.json"),
                )),
                _directory: directory,
            }
        }
    }

    impl Drop for TestStore {
        fn drop(&mut self) {
            let _ = self.store.clear(&ANTHROPIC_OAUTH);
        }
    }

    fn client(token_url: &str) -> OauthClient {
        #[allow(clippy::expect_used)]
        let url = Url::parse(token_url).expect("token URL");
        #[allow(clippy::expect_used)]
        OauthClient::new(&ANTHROPIC_OAUTH, url).expect("client builds")
    }

    /// The one place a test may read a token back out.
    fn exposed(secret: &Secret) -> String {
        #[allow(clippy::disallowed_methods)]
        secret.expose().to_owned()
    }

    fn stored(fixture: &TestStore) -> Option<OauthCredential> {
        match fixture.store.load(&ANTHROPIC_OAUTH) {
            Ok(credential) => credential,
            Err(error) => panic!("{error}"),
        }
    }

    fn save(fixture: &TestStore, credential: &OauthCredential) {
        if let Err(error) = fixture.store.save(&ANTHROPIC_OAUTH, credential) {
            panic!("{error}");
        }
    }

    #[test]
    fn a_credential_round_trips_through_the_keychain() {
        let fixture = TestStore::new("roundtrip");
        assert!(stored(&fixture).is_none());

        save(&fixture, &credential(1_700_000_000_000));
        #[allow(clippy::expect_used)]
        let read = stored(&fixture).expect("credential is stored");
        assert_eq!(read.access_token, "at_live_1");
        assert_eq!(read.refresh_token, "rt_live_1");
        assert_eq!(read.expires_at_ms, 1_700_000_000_000);
        assert_eq!(read.email.as_deref(), Some("someone@example.com"));

        if let Err(error) = fixture.store.clear(&ANTHROPIC_OAUTH) {
            panic!("{error}");
        }
        assert!(stored(&fixture).is_none());
    }

    #[tokio::test]
    async fn a_token_outside_the_window_is_returned_untouched() {
        let fixture = TestStore::new("fresh");
        save(&fixture, &credential(1_000_000));
        // No mock is mounted: any call to the token endpoint answers 404 and
        // would fail the test.
        let server = MockServer::start().await;
        let client = client(&format!("{}/oauth/token", server.uri()));

        #[allow(clippy::expect_used)]
        let token = fixture
            .store
            .access_token_at(&client, &ANTHROPIC_OAUTH, 1_000_000 - 5 * 60 * 1000 - 1)
            .await
            .expect("stored token is still good");
        assert_eq!(exposed(&token), "at_live_1");
        assert_eq!(
            server.received_requests().await.map(|all| all.len()),
            Some(0)
        );
    }

    /// A real subscription credential is hundreds of bytes — the vendors'
    /// access tokens alone run past 500 — and an implementation that quietly
    /// truncates it stores a credential that authenticates nothing.
    #[test]
    fn a_real_sized_credential_round_trips() {
        let fixture = TestStore::new("longcredential");
        let long = OauthCredential {
            access_token: format!("sk-ant-oat01-{}", "a".repeat(600)),
            refresh_token: format!("sk-ant-ort01-{}", "r".repeat(300)),
            expires_at_ms: 1_700_000_000_000,
            account_id: Some("11111111-2222-3333-4444-555555555555".to_owned()),
            email: Some("someone@example.com".to_owned()),
            plan: Some("max".to_owned()),
        };
        save(&fixture, &long);

        #[allow(clippy::expect_used)]
        let read = stored(&fixture).expect("credential is stored");
        assert_eq!(read.access_token, long.access_token);
        assert_eq!(read.refresh_token, long.refresh_token);
        assert_eq!(read.account_id, long.account_id);
    }

    #[tokio::test]
    async fn a_token_inside_the_window_is_refreshed_and_persisted() {
        let fixture = TestStore::new("refresh");
        save(&fixture, &credential(1_000_000));
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "at_live_2",
                "expires_in": 3600,
            })))
            .mount(&server)
            .await;
        let client = client(&format!("{}/oauth/token", server.uri()));

        let now = 1_000_000 - 5 * 60 * 1000 + 1;
        #[allow(clippy::expect_used)]
        let token = fixture
            .store
            .access_token_at(&client, &ANTHROPIC_OAUTH, now)
            .await
            .expect("refresh succeeds");
        assert_eq!(exposed(&token), "at_live_2");

        #[allow(clippy::expect_used)]
        let read = stored(&fixture).expect("refreshed credential is stored");
        assert_eq!(read.access_token, "at_live_2");
        // The vendor sent no new refresh token, so the old one survives.
        assert_eq!(read.refresh_token, "rt_live_1");
        assert_eq!(read.expires_at_ms, now + 3_600_000);
    }

    #[tokio::test]
    async fn a_dead_grant_clears_the_credential() {
        let fixture = TestStore::new("deadgrant");
        save(&fixture, &credential(0));
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(401).set_body_json(json!({ "error": "invalid_grant" })),
            )
            .mount(&server)
            .await;
        let client = client(&format!("{}/oauth/token", server.uri()));

        let outcome = fixture
            .store
            .access_token_at(&client, &ANTHROPIC_OAUTH, 0)
            .await;
        assert!(matches!(outcome, Err(OauthError::SignedOut { .. })));
        assert!(
            stored(&fixture).is_none(),
            "a dead grant must not stay in the Keychain"
        );
    }

    #[tokio::test]
    async fn an_unreachable_vendor_keeps_the_credential() {
        let fixture = TestStore::new("offline");
        save(&fixture, &credential(0));
        // Port 1 on loopback refuses instantly: a transport failure, not a
        // refusal of the grant.
        let client = client("http://127.0.0.1:1/oauth/token");

        let outcome = fixture
            .store
            .access_token_at(&client, &ANTHROPIC_OAUTH, 0)
            .await;
        assert!(matches!(outcome, Err(OauthError::Transport { .. })));
        assert!(
            stored(&fixture).is_some(),
            "an offline machine must not sign the user out"
        );
    }

    #[tokio::test]
    async fn signing_in_is_required_before_a_token_exists() {
        let fixture = TestStore::new("absent");
        let client = client("http://127.0.0.1:1/oauth/token");
        let outcome = fixture
            .store
            .access_token_at(&client, &ANTHROPIC_OAUTH, 0)
            .await;
        assert!(matches!(outcome, Err(OauthError::SignedOut { .. })));
    }
}
