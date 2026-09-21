//! The one façade both front ends drive.
//!
//! [`Runtime`] owns the store and is the only place in the process that talks
//! to `neo-keys`. It is synchronous, because the store actors are: a caller
//! that needs it off the main thread wraps a call in `spawn_blocking`. Every
//! mutation publishes its [`AppEvent`] only after the write has landed, so a
//! subscriber never sees state the store does not hold.

use std::path::{Path, PathBuf};
use std::time::Instant;

use neo_core::{
    AppEvent, CoreError, Envelope, InferenceConnection, KeyStatus, NoticeLevel, ProviderAccount,
    ProviderId, RunId, Settings,
};
use neo_keys::{
    ACCOUNT_ANTHROPIC, ACCOUNT_OPENAI, ACCOUNT_TYPESAFE, KeySource, KeyState, Keychain,
    KeychainError, Secret, SecretError, ValidationError,
};
use neo_store::{APPLICATION_ID, CachedModel, SCHEMA_VERSION, Store, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::claude::{self, ClaudeCode, ClaudeCodeConfig, ClaudeError};
use std::sync::Arc;

use url::Url;

use crate::doctor::DoctorReport;
use crate::oauth::{
    ANTHROPIC_OAUTH, OPENAI_CODEX, OauthClient, OauthCredential, OauthError, OauthFlow,
    OauthProvider, OauthStore,
};
use crate::providers::{self, KeyBases};
use crate::providers::{AnthropicOauthInference, CodexOauthInference, Turn};

/// Version of the event/command surface both front ends compile against.
///
/// 2: a turn streams. The bridge gained `steer_run`, and the event stream
/// gained `turn_delta`, `turn_steered` and `turn_cost` — a front end built
/// against version 1 would call a command this binary did not have.
pub const BRIDGE_VERSION: u32 = 2;

/// How long a lease survives without a heartbeat. Long enough to cover a slow
/// navigator step, short enough that a crashed process does not block the
/// machine for more than a few seconds.
const LEASE_TTL_MS: i64 = 30_000;

/// A session that has not been seen for this long is gone. Three missed
/// heartbeats.
const SESSION_STALE_MS: i64 = 90_000;

/// This machine's name, for a roster that will one day span more than one.
fn hostname() -> String {
    std::env::var("HOST")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "localhost".to_owned())
}

/// How long a subscription login may take: a browser round-trip, possibly a
/// password manager and a second factor.
const OAUTH_LOGIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Events buffered per subscriber before a slow front end starts lagging.
const EVENT_CAPACITY: usize = 256;

/// Keychain accounts Starkbot itself owns (K6). Subscription paths are absent
/// on purpose: their credentials live in the vendor helpers' own homes.
const ACCOUNTS: [&str; 3] = [ACCOUNT_OPENAI, ACCOUNT_ANTHROPIC, ACCOUNT_TYPESAFE];

/// Subscription runtimes whose account row, when present, describes a
/// connection the user made.
const SUBSCRIPTION_PROVIDERS: [&str; 4] = [
    neo_core::PROVIDER_ANTHROPIC_OAUTH,
    neo_core::PROVIDER_OPENAI_CODEX,
    neo_core::PROVIDER_CHATGPT_CODEX,
    neo_core::PROVIDER_CLAUDE_SUBSCRIPTION,
];

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("store error: {0}")]
    Store(#[from] StoreError),
    #[error("keychain error: {0}")]
    Keychain(#[from] KeychainError),
    #[error(transparent)]
    Core(#[from] CoreError),
    /// The value offered for a key is not usable as a secret. Never carries
    /// the value itself.
    #[error("invalid key value: {0}")]
    Secret(#[from] SecretError),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("could not prepare the data directory: {0}")]
    Io(#[from] std::io::Error),
    #[error("key validation failed: {0}")]
    Validation(#[from] ValidationError),
    #[error("system clock error: {0}")]
    Clock(#[from] std::time::SystemTimeError),
    #[error("system clock cannot fit in a SQLite integer")]
    ClockOverflow,
    #[error(transparent)]
    Claude(#[from] ClaudeError),
    #[error(transparent)]
    Oauth(#[from] crate::oauth::OauthError),
    #[error(transparent)]
    Provider(#[from] neo_core::ProviderError),
    #[error(transparent)]
    Voice(#[from] neo_voice::VoiceError),
    /// Another Starkbot on this machine is using something there is only one
    /// of. Not a failure of the request: a fact about the laptop.
    #[error("{resource} is in use by the {who}, which is {doing}")]
    Busy {
        resource: String,
        who: String,
        doing: String,
    },
    #[error("no `{0}` key is stored; run `neo keys set {0}`")]
    MissingKey(String),
    /// The user selected an inference runtime this build cannot drive yet.
    #[error("the `{0}` inference runtime is not wired up yet; the Claude subscription path is")]
    RuntimeUnavailable(String),
}

/// Where the store lives and which schema it is on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoreInfo {
    pub path: PathBuf,
    pub schema_version: u32,
    pub application_id: i32,
}

/// Everything a front end needs before it renders its first frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bootstrap {
    pub bridge_version: u32,
    pub settings: Settings,
    pub keys: Vec<KeyStatus>,
    pub inference: InferenceConnection,
    pub account: Option<ProviderAccount>,
    /// Every subscription row the store holds, so a front end can render all
    /// of them rather than only the selected one (K7, A25).
    pub accounts: Vec<ProviderAccount>,
    pub store: StoreInfo,
    /// The local readiness checks (05 §10), so a front end's first frame can
    /// already say what is missing.
    pub doctor: DoctorReport,
}

pub struct Runtime {
    data_dir: PathBuf,
    store: Store,
    keychain: Keychain,
    /// Secrets already read from the Keychain this process.
    ///
    /// **This cache exists to stop macOS asking the user.** Every Keychain
    /// read from an unsigned or ad-hoc-signed binary raises an authorization
    /// prompt, and the binary's identity changes on every rebuild, so the
    /// user's "Always Allow" does not carry over. An agent turn reads a
    /// credential once per step; without this, a single turn produced a
    /// dialog per step — and because the dialog is modal, the turn appeared
    /// to hang.
    ///
    /// The values are `Secret`s, which do not implement `Debug`/`Display` and
    /// zero themselves on drop, and the cache is invalidated by `set_key`,
    /// `remove_key` and the OAuth store's writes, so a rotated credential is
    /// never served stale.
    secrets: std::sync::Mutex<std::collections::HashMap<String, CachedSecret>>,
    /// This process's row on the machine-local roster, announced on first use.
    session: std::sync::OnceLock<String>,
    /// Subscription credentials already read this process, for the same
    /// reason as `secrets`: an agent turn asks for a token on every step.
    /// Dropped on login, logout and refresh.
    credentials: std::sync::Mutex<std::collections::HashMap<String, OauthCredential>>,
    /// One refresh at a time, per provider.
    ///
    /// [`Runtime::oauth_token`] used to read the cache, drop the lock and
    /// await the refresh with nothing between two callers. Two overlapping
    /// turns then POSTed `grant_type=refresh_token` with the same token —
    /// and OpenAI rotates refresh tokens, so the second POST came back
    /// `invalid_grant`, which `OauthStore::access_token_at` reads as a dead
    /// grant and answers by **deleting the stored credential**. The loser of
    /// the race signed the user out mid-turn, on a credential that had just
    /// been refreshed successfully.
    ///
    /// A `tokio` mutex, not a `std` one: the guard is held across the
    /// refresh await, which the workspace's `await_holding_lock` denies for
    /// the blocking kind. The map itself is guarded by a `std` mutex that is
    /// never held across an await — it only hands out an `Arc`.
    refreshes:
        std::sync::Mutex<std::collections::HashMap<&'static str, Arc<tokio::sync::Mutex<()>>>>,
    /// Where the subscription token endpoint is, when it is not the vendor's.
    ///
    /// `None` in every shipped build: the provider constant is used. A test
    /// points it at `wiremock`, because no test may reach a vendor (08 rule
    /// 1) and the token endpoint is a provider constant rather than one of
    /// the injected [`KeyBases`].
    oauth_token_url: Option<Url>,
    key_bases: KeyBases,
    events: broadcast::Sender<Envelope>,
    /// The sequence number the next published event gets.
    ///
    /// Process-monotonic and gapless, which is what lets a front end detect
    /// that it missed an event: a receiver that was too slow, or one that
    /// attached after a reload, sees a jump and re-bootstraps. Without it the
    /// only signal is `broadcast`'s own `Lagged`, which a webview that
    /// reconnects never receives at all.
    seq: std::sync::atomic::AtomicU64,
    /// The machine's keyboard-and-frontmost-window lease.
    ///
    /// Held here because every capability that drives an application goes
    /// through this runtime, and the lease has to be the same one for all of
    /// them. It is keyed on the data directory, so two processes sharing a
    /// data dir contend — which is the case that matters: the TUI and the
    /// desktop app are separate processes. See [`crate::screen`].
    screen: crate::screen::ScreenLease,
    /// The turns running right now, by run id.
    ///
    /// **This is what makes a running turn reachable.** A graph is otherwise
    /// closed — only its own nodes change its state — so a person who types
    /// while the agent works could only be heard after it finished. A turn
    /// registers its inbox here for as long as it lasts, and
    /// [`Runtime::steer`] posts into it; a run that has ended is simply
    /// absent, which is the difference between "queued" and "send it as a new
    /// turn".
    runs: std::sync::Mutex<std::collections::HashMap<RunId, Arc<crate::agent::metal::Steering>>>,
}

/// Where one runtime keeps its secrets.
///
/// The file backend is what the dev loop and every test use, and it is
/// anchored to the runtime's own `data_dir` so two data directories never
/// share one `keys.json`. See `neo_keys::KEYCHAIN_BACKEND_ENV` for why a
/// debug build does not use the login Keychain at all.
fn keychain_for(data_dir: &Path) -> Keychain {
    match neo_keys::wanted_file_backend() {
        Some(Some(path)) => Keychain::file(neo_keys::DEFAULT_SERVICE, path),
        Some(None) => Keychain::file(neo_keys::DEFAULT_SERVICE, data_dir.join("keys.json")),
        None => Keychain::default(),
    }
}

impl Runtime {
    /// Open (creating if needed) the store under `data_dir` and start the
    /// event channel. Blocking: run it on a blocking thread from async code.
    pub fn open(data_dir: &Path) -> Result<Self, RuntimeError> {
        Self::with_key_bases(data_dir, KeyBases::hosted())
    }

    /// `open`, with somewhere other than the hosted vendors to validate keys
    /// against. Tests point every base at one `wiremock` server so a check
    /// never leaves the machine.
    pub fn with_key_bases(data_dir: &Path, key_bases: KeyBases) -> Result<Self, RuntimeError> {
        std::fs::create_dir_all(data_dir)?;
        let store = Store::open(data_dir.join("neo.db"), data_dir.join("backups"))?;
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        Ok(Self {
            data_dir: data_dir.to_owned(),
            store,
            // Keys live beside the store they belong to when the file backend
            // is selected (the dev loop, and every test), so two data dirs
            // never share one file. See `neo_keys::KEYCHAIN_BACKEND_ENV`.
            keychain: keychain_for(data_dir),
            secrets: std::sync::Mutex::new(std::collections::HashMap::new()),
            session: std::sync::OnceLock::new(),
            credentials: std::sync::Mutex::new(std::collections::HashMap::new()),
            refreshes: std::sync::Mutex::new(std::collections::HashMap::new()),
            oauth_token_url: None,
            key_bases,
            events,
            seq: std::sync::atomic::AtomicU64::new(1),
            screen: crate::screen::ScreenLease::new(data_dir, neo_otel::surface()),
            runs: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// Post subscription token refreshes at `token_url` instead of the
    /// vendor. The seam `KeyBases` is for the API-key paths (08 rule 1).
    #[cfg(test)]
    #[must_use]
    fn with_token_endpoint(mut self, token_url: Url) -> Self {
        self.oauth_token_url = Some(token_url);
        self
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Take the screen for `run`, or find out who has it.
    ///
    /// Everything that drives an application — an app turn, a headed browser
    /// run, an accessibility action, an eval suite — goes through here, and
    /// the guard must be held for as long as the work runs. Dropping it
    /// releases the screen.
    ///
    /// `what` is shown to whoever gets refused, so write it as the refusal
    /// will read: `"drive TextEdit"`, `"eval suite"`.
    pub fn acquire_screen(
        &self,
        run: neo_core::RunId,
        what: impl Into<String>,
    ) -> Result<crate::screen::ScreenGuard, crate::screen::ScreenBusy> {
        self.screen.acquire(run, what)
    }

    /// [`Runtime::acquire_screen`] for work running inside a hold somebody
    /// else already took.
    ///
    /// `scope` comes from that hold's [`crate::screen::ScreenGuard::scope`]
    /// and is how the eval suite's per-case turns are let in: each case has a
    /// run id of its own, so nothing about the id says the work belongs to
    /// the suite. `None` is an ordinary acquisition, refused while any other
    /// run holds the screen.
    pub fn acquire_screen_within(
        &self,
        scope: Option<crate::screen::ScreenScope>,
        run: neo_core::RunId,
        what: impl Into<String>,
    ) -> Result<crate::screen::ScreenGuard, crate::screen::ScreenBusy> {
        self.screen.acquire_within(scope, run, what)
    }

    /// Who is driving the screen, if anyone — for a front end that wants to
    /// disable a button rather than let the user press it and be refused.
    #[must_use]
    pub fn screen_holder(&self) -> Option<crate::screen::Holder> {
        self.screen.holder()
    }

    pub fn store_info(&self) -> StoreInfo {
        StoreInfo {
            path: self.store.path().to_owned(),
            schema_version: SCHEMA_VERSION as u32,
            application_id: APPLICATION_ID as i32,
        }
    }

    /// The Claude subscription helper, confined and pointed at its own config
    /// home — or at the home `NEO_CLAUDE_CONFIG_DIR` names, which is how a
    /// developer reuses an existing Claude Code login (K6, 05 §6).
    pub fn claude(&self) -> ClaudeCode {
        let home = claude::default_claude_home(&self.data_dir);
        let mut config = ClaudeCodeConfig::new(claude::configured_executable(), &home);
        if let Some(shared) = std::env::var_os("NEO_CLAUDE_CONFIG_DIR") {
            config = config.with_config_dir(PathBuf::from(shared));
        }
        ClaudeCode::new(config)
    }

    /// Re-read the Claude subscription's state, persist the redacted row and
    /// announce it.
    pub async fn refresh_claude_account(&self) -> Result<ProviderAccount, RuntimeError> {
        let account = self.claude().account().await?;
        self.put_account(account)
    }

    /// Hand the terminal to `claude auth login`; the CLI owns the credential.
    pub async fn connect_claude(&self) -> Result<ProviderAccount, RuntimeError> {
        let account = self.claude().login().await?;
        self.put_account(account)
    }

    /// Sign this app's dedicated Claude session out.
    pub async fn disconnect_claude(&self) -> Result<ProviderAccount, RuntimeError> {
        let account = self.claude().logout().await?;
        self.put_account(account)
    }

    /// The OAuth credential store, one Keychain item per subscription path
    /// (K7).
    ///
    /// Built from *this* runtime's data directory rather than the process
    /// default: the two resolve to the same file for the real app, but a test
    /// runtime on a temporary directory used to read the developer's own
    /// `keys.json` and find a live subscription there.
    pub fn oauth(&self) -> OauthStore {
        OauthStore::new(keychain_for(&self.data_dir))
    }

    /// Begin a subscription login and hand the caller the URL to open (K7).
    ///
    /// Pure: no network, no Keychain, no browser. A front end shows
    /// [`LoginHandle::authorize_url`] immediately, then awaits
    /// [`Runtime::finish_oauth`] (loopback) or calls
    /// [`Runtime::finish_oauth_pasted`] with what the user pasted — which is
    /// what lets the TUI stay interactive, and lets the desktop app render a
    /// real screen, while the vendor page is open.
    pub fn begin_oauth(
        &self,
        provider: &'static OauthProvider,
    ) -> Result<LoginHandle, RuntimeError> {
        let login = OauthFlow::start(provider)?;
        Ok(LoginHandle {
            authorize_url: login.authorize_url(),
            login,
        })
    }

    /// Open a URL in the user's browser. Exposed because a front end that
    /// shows the URL should also be able to open it.
    pub fn open_in_browser(&self, url: &str) -> Result<(), RuntimeError> {
        open_url(url)
    }

    /// Serve the loopback callback, exchange the code, store the credential.
    ///
    /// `timeout` bounds only the wait for the browser; a cancelled login is
    /// [`OauthError::Timeout`], and nothing is stored.
    pub async fn finish_oauth(
        &self,
        handle: &LoginHandle,
        timeout: std::time::Duration,
    ) -> Result<ProviderAccount, RuntimeError> {
        let code = handle.login.wait_for_code(timeout).await?;
        self.store_exchange(handle.login.provider(), &code, handle.login.verifier())
            .await
    }

    /// The paste-the-URL fallback: same exchange, no listener.
    pub async fn finish_oauth_pasted(
        &self,
        handle: &LoginHandle,
        pasted: &str,
    ) -> Result<ProviderAccount, RuntimeError> {
        let provider = handle.login.provider();
        let code = handle.login.code_from_redirect(pasted)?;
        self.store_exchange(provider, &code, handle.login.verifier())
            .await
    }

    async fn store_exchange(
        &self,
        provider: &'static OauthProvider,
        code: &crate::oauth::AuthCode,
        verifier: &crate::oauth::Verifier,
    ) -> Result<ProviderAccount, RuntimeError> {
        let client = self.oauth_client(provider)?;
        let credential = client.exchange(code, verifier).await?;
        let account = account_of(provider, &credential, now_ms()?);
        self.oauth().save(provider, &credential)?;
        self.forget_credential(provider);
        self.put_account(account)
    }

    /// The whole login in one call, for a caller with nothing to draw:
    /// begin, open the browser, wait, exchange, store.
    pub async fn connect_oauth(
        &self,
        provider: &'static OauthProvider,
    ) -> Result<ProviderAccount, RuntimeError> {
        let handle = self.begin_oauth(provider)?;
        let url = handle.authorize_url.to_string();
        // `wait_for_code` binds the port, so the browser is opened from inside
        // the same future — after a short grace in which the listener starts
        // accepting, or a fast vendor redirect would find nothing there.
        let open = async {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            open_url(&url)
        };
        let (account, opened) = tokio::join!(self.finish_oauth(&handle, OAUTH_LOGIN_TIMEOUT), open);
        opened?;
        account
    }

    /// Forget one subscription OAuth credential.
    pub fn disconnect_oauth(
        &self,
        provider: &'static OauthProvider,
    ) -> Result<ProviderAccount, RuntimeError> {
        self.oauth().clear(provider)?;
        self.forget_credential(provider);
        let account = ProviderAccount {
            provider: ProviderId::new(provider.id),
            status: neo_core::ProviderAccountStatus::SignedOut,
            email: None,
            plan_type: None,
            workspace: None,
            allowance: None,
            updated_at: now_ms()?,
        };
        self.put_account(account)
    }

    /// What the stored credential says about one subscription path, refreshed
    /// if it is about to expire.
    ///
    /// Only the vendor saying the grant is dead reports `SignedOut`. Every
    /// other failure — an unreachable network, a 5xx, a Keychain that would
    /// not open — reports `Unavailable`, which says "could not check" rather
    /// than "your subscription is gone". This is the rule `key_check::check`
    /// already applies to API keys, where every reachability failure folds
    /// into `KeyState::Unchecked`; mapping `Err(_)` to `SignedOut` told an
    /// offline user nothing about their key and that their plan had lapsed.
    pub async fn oauth_account(
        &self,
        provider: &'static OauthProvider,
    ) -> Result<ProviderAccount, RuntimeError> {
        let store = self.oauth();
        let account = match store.load(provider)? {
            Some(credential) => {
                // Refreshing here is what makes `status` honest: a credential
                // the vendor has revoked is reported signed out.
                let client = self.oauth_client(provider)?;
                let status = match store.access_token(&client, provider).await {
                    Ok(_) => None,
                    Err(OauthError::SignedOut { .. }) => {
                        Some(neo_core::ProviderAccountStatus::SignedOut)
                    }
                    Err(_) => Some(neo_core::ProviderAccountStatus::Unavailable),
                };
                match status {
                    None => account_of(provider, &credential, now_ms()?),
                    Some(status) => ProviderAccount {
                        provider: ProviderId::new(provider.id),
                        status,
                        email: credential.email.clone(),
                        plan_type: credential.plan.clone(),
                        workspace: None,
                        allowance: None,
                        updated_at: now_ms()?,
                    },
                }
            }
            // Nothing is stored, so there is nothing to check and nothing to
            // be unsure about.
            None => ProviderAccount {
                provider: ProviderId::new(provider.id),
                status: neo_core::ProviderAccountStatus::SignedOut,
                email: None,
                plan_type: None,
                workspace: None,
                allowance: None,
                updated_at: now_ms()?,
            },
        };
        self.put_account(account)
    }

    /// The concrete model id to send for one provider.
    ///
    /// Settings hold a *symbolic* id — `sol-latest` by default (05 §7) — and a
    /// vendor API rejects that string. It resolves against the cached
    /// catalogue when one has been fetched; otherwise the provider's own
    /// documented default is used, because refusing to answer until the user
    /// runs a catalogue refresh would make a fresh install unusable.
    pub(crate) fn resolved_model(
        &self,
        provider: &'static OauthProvider,
        requested: Option<&str>,
        saved: &str,
    ) -> Result<String, RuntimeError> {
        let wanted = requested.unwrap_or(saved);
        // An id the caller spelled out in full is used as given: the model
        // registry is a convenience, not a gate.
        if wanted != neo_core::SOL_LATEST {
            return Ok(wanted.to_owned());
        }
        let ids: Vec<String> = self
            .models(provider.id)?
            .into_iter()
            .map(|model| model.info.reference.id)
            .collect();
        if let Some(resolved) = neo_core::registry::resolve(provider.id, wanted, &ids) {
            return Ok(resolved);
        }
        Ok(default_model(provider).to_owned())
    }

    /// The access token for one subscription path, read once per process and
    /// refreshed in place.
    ///
    /// Every Keychain read prompts the user on an unsigned build, and an agent
    /// turn needs a token on every step, so the credential is cached here and
    /// only re-read when it is written (login, logout) or has expired.
    ///
    /// The refresh itself is single-flighted per provider — see
    /// [`Runtime::refreshes`] for the credential this used to delete.
    pub(crate) async fn oauth_token(
        &self,
        provider: &'static OauthProvider,
    ) -> Result<Secret, RuntimeError> {
        // A cached credential that is still inside its window needs no
        // Keychain access at all.
        if let Some(token) = self.cached_token(provider)? {
            return Ok(token);
        }
        let gate = self.refresh_gate(provider);
        let _refreshing = gate.lock().await;
        // Checked again behind the gate: the winner of the race has already
        // saved and cached a fresh credential, and refreshing a second time
        // would POST the refresh token the winner just rotated away — which
        // the vendor answers `invalid_grant`, and which
        // `OauthStore::access_token_at` acts on by clearing the credential.
        if let Some(token) = self.cached_token(provider)? {
            return Ok(token);
        }
        let store = self.oauth();
        let client = self.oauth_client(provider)?;
        // `access_token_at` refreshes and re-saves when needed; the reload
        // below then caches whatever is now stored.
        let token = store.access_token_at(&client, provider, now_ms()?).await?;
        if let Some(credential) = store.load(provider)?
            && let Ok(mut cache) = self.credentials.lock()
        {
            cache.insert(provider.id.to_owned(), credential);
        }
        Ok(token)
    }

    /// The cached access token while it is still inside its refresh window.
    ///
    /// Its own function so the `std` guard cannot be alive across the refresh
    /// await below, which `await_holding_lock` denies and which would hold
    /// every other caller out of the cache for the length of a round trip.
    fn cached_token(
        &self,
        provider: &'static OauthProvider,
    ) -> Result<Option<Secret>, RuntimeError> {
        let now = now_ms()?;
        if let Ok(cache) = self.credentials.lock()
            && let Some(credential) = cache.get(provider.id)
            && !credential.needs_refresh(now)
        {
            return Ok(Some(Secret::new(&credential.access_token)?));
        }
        Ok(None)
    }

    /// This provider's refresh gate, created on first use.
    fn refresh_gate(&self, provider: &'static OauthProvider) -> Arc<tokio::sync::Mutex<()>> {
        let Ok(mut gates) = self.refreshes.lock() else {
            // The map holds nothing but `Arc`s, so nothing in here can panic
            // and poison it. If it somehow is, a gate of its own is better
            // than refusing the turn: the worst case is the unserialised
            // refresh this exists to prevent, which is where the code was
            // before.
            return Arc::new(tokio::sync::Mutex::new(()));
        };
        Arc::clone(gates.entry(provider.id).or_default())
    }

    /// The token-endpoint client for one provider: the vendor's, or whatever
    /// [`Runtime::oauth_token_url`] points at.
    fn oauth_client(&self, provider: &'static OauthProvider) -> Result<OauthClient, RuntimeError> {
        match &self.oauth_token_url {
            Some(url) => Ok(OauthClient::new(provider, url.clone())?),
            None => Ok(OauthClient::hosted(provider)?),
        }
    }

    /// A source a long-running caller asks for `provider`'s token on every
    /// request it makes.
    ///
    /// An agent turn is many requests over several minutes, and a plan access
    /// token expires on its own schedule — so a caller handed one token at
    /// construction sends a stale one for the rest of the turn and fails
    /// halfway through with a `401` that reads like a revoked subscription.
    /// Each call lands on [`Runtime::oauth_token`], which serves the cached
    /// credential until it is close to expiry and refreshes it in place, so
    /// this costs nothing per request in the common case.
    pub(crate) fn token_source(
        self: &Arc<Self>,
        provider: &'static OauthProvider,
    ) -> crate::providers::anthropic_oauth_model::TokenSource {
        let runtime = Arc::clone(self);
        Arc::new(move || {
            let runtime = Arc::clone(&runtime);
            Box::pin(async move {
                runtime
                    .oauth_token(provider)
                    .await
                    .map(Arc::new)
                    // The transport can only carry a string. This one names
                    // the provider and what went wrong — never the
                    // credential, which no `RuntimeError` can hold.
                    .map_err(|error| {
                        neo_core::ProviderError::Transport(format!(
                            "no usable `{}` token: {error}",
                            provider.id
                        ))
                    })
            })
        })
    }

    /// The account id the ChatGPT backend wants, from the cached credential.
    pub(crate) fn oauth_account_id(
        &self,
        provider: &'static OauthProvider,
    ) -> Result<Option<String>, RuntimeError> {
        if let Ok(cache) = self.credentials.lock()
            && let Some(credential) = cache.get(provider.id)
        {
            return Ok(credential.account_id.clone());
        }
        Ok(self
            .oauth()
            .load(provider)?
            .and_then(|credential| credential.account_id.clone()))
    }

    /// Forget a cached subscription credential, so the next call re-reads it.
    fn forget_credential(&self, provider: &'static OauthProvider) {
        if let Ok(mut cache) = self.credentials.lock() {
            cache.remove(provider.id);
        }
    }

    /// One turn on a subscription-OAuth runtime (K7, A25).
    async fn oauth_turn(
        &self,
        provider: &'static OauthProvider,
        model: &str,
        prompt: &str,
        schema: Option<&Value>,
    ) -> Result<(Option<Value>, Turn), RuntimeError> {
        let token = self.oauth_token(provider).await?;
        if provider.id == ANTHROPIC_OAUTH.id {
            let inference = AnthropicOauthInference::hosted()?;
            return match schema {
                Some(schema) => {
                    let (value, turn) = inference
                        .complete_json(&token, model, prompt, schema)
                        .await?;
                    Ok((Some(value), turn))
                }
                None => Ok((None, inference.complete_text(&token, model, prompt).await?)),
            };
        }
        let mut inference = CodexOauthInference::hosted()?;
        if let Some(account_id) = self.oauth_account_id(provider)? {
            inference = inference.with_account(account_id);
        }
        match schema {
            Some(schema) => {
                let (value, turn) = inference
                    .complete_json(&token, model, prompt, schema)
                    .await?;
                Ok((Some(value), turn))
            }
            None => Ok((None, inference.complete_text(&token, model, prompt).await?)),
        }
    }

    /// One turn on the runtime the user selected for inference.
    ///
    /// The Claude subscription path is live; the API-key paths wait for
    /// metalcraft's runtime (M5), and saying so is better than pretending.
    pub async fn ask(&self, prompt: &str, model: Option<&str>) -> Result<Turn, RuntimeError> {
        let settings = self.settings()?;
        let selected = settings.models.inference.provider.as_str();
        let saved = settings.models.inference.id.as_str();
        let started = Instant::now();
        let result = self.ask_selected(selected, prompt, model, saved).await;
        record_inference(selected, saved, false, prompt, started, result.as_ref());
        result
    }

    /// The provider half of [`Runtime::ask`], split out so the public call can
    /// time one attempt and record it once for every path — including the
    /// refusals, which are the interesting ones in a trace.
    async fn ask_selected(
        &self,
        selected: &str,
        prompt: &str,
        model: Option<&str>,
        saved: &str,
    ) -> Result<Turn, RuntimeError> {
        match selected {
            id_ if id_ == ANTHROPIC_OAUTH.id => {
                let id = self.resolved_model(&ANTHROPIC_OAUTH, model, saved)?;
                Ok(self
                    .oauth_turn(&ANTHROPIC_OAUTH, &id, prompt, None)
                    .await?
                    .1)
            }
            id_ if id_ == OPENAI_CODEX.id => {
                let id = self.resolved_model(&OPENAI_CODEX, model, saved)?;
                Ok(self.oauth_turn(&OPENAI_CODEX, &id, prompt, None).await?.1)
            }
            neo_core::PROVIDER_CLAUDE_SUBSCRIPTION => {
                let turn = self.selected_claude(model)?.complete_text(prompt).await?;
                Ok(Turn {
                    text: turn.text,
                    model: turn.model,
                    usage: turn.usage,
                    duration_ms: turn.duration_ms,
                })
            }
            other => Err(RuntimeError::RuntimeUnavailable(other.to_owned())),
        }
    }

    /// One strict-JSON turn on the selected runtime: the text helper's and the
    /// extractor's shape.
    pub async fn ask_json(
        &self,
        prompt: &str,
        schema: &Value,
        model: Option<&str>,
    ) -> Result<(Value, Turn), RuntimeError> {
        let settings = self.settings()?;
        let selected = settings.models.inference.provider.as_str();
        let saved = settings.models.inference.id.as_str();
        let started = Instant::now();
        let result = self
            .ask_json_selected(selected, prompt, schema, model, saved)
            .await;
        record_inference(
            selected,
            saved,
            true,
            prompt,
            started,
            result.as_ref().map(|(_, turn)| turn),
        );
        result
    }

    /// The provider half of [`Runtime::ask_json`], for the same reason
    /// [`Runtime::ask_selected`] exists.
    async fn ask_json_selected(
        &self,
        selected: &str,
        prompt: &str,
        schema: &Value,
        model: Option<&str>,
        saved: &str,
    ) -> Result<(Value, Turn), RuntimeError> {
        let provider = match selected {
            id_ if id_ == ANTHROPIC_OAUTH.id => Some(&ANTHROPIC_OAUTH),
            id_ if id_ == OPENAI_CODEX.id => Some(&OPENAI_CODEX),
            _ => None,
        };
        if let Some(provider) = provider {
            let id = self.resolved_model(provider, model, saved)?;
            let (value, turn) = self.oauth_turn(provider, &id, prompt, Some(schema)).await?;
            let value = value.ok_or_else(|| {
                RuntimeError::RuntimeUnavailable(format!("{selected} answered without JSON"))
            })?;
            return Ok((value, turn));
        }
        if selected == neo_core::PROVIDER_CLAUDE_SUBSCRIPTION {
            let (value, turn) = self
                .selected_claude(model)?
                .complete_json(prompt, schema)
                .await?;
            return Ok((
                value,
                Turn {
                    text: turn.text,
                    model: turn.model,
                    usage: turn.usage,
                    duration_ms: turn.duration_ms,
                },
            ));
        }
        Err(RuntimeError::RuntimeUnavailable(selected.to_owned()))
    }

    /// The helper for the selected runtime, or a plain refusal naming the
    /// runtime that is not wired up yet.
    ///
    /// `model` overrides the saved id for this one turn; the saved symbolic id
    /// (`sol-latest`) is left to the CLI's own default when nothing is given,
    /// because only the vendor knows which concrete model a plan may use.
    fn selected_claude(&self, model: Option<&str>) -> Result<ClaudeCode, RuntimeError> {
        let settings = self.settings()?;
        let provider = settings.models.inference.provider.as_str();
        if provider != neo_core::PROVIDER_CLAUDE_SUBSCRIPTION {
            return Err(RuntimeError::RuntimeUnavailable(provider.to_owned()));
        }
        let saved = settings.models.inference.id.as_str();
        let requested = model.or((saved != neo_core::SOL_LATEST).then_some(saved));
        let claude = self.claude();
        Ok(match requested {
            Some(model) => ClaudeCode::new(claude.config().clone().with_model(model)),
            None => claude,
        })
    }

    fn put_account(&self, account: ProviderAccount) -> Result<ProviderAccount, RuntimeError> {
        self.store.provider_accounts().put(account.clone())?;
        self.publish(AppEvent::ProviderAccount {
            account: account.clone(),
        });
        Ok(account)
    }

    pub fn bootstrap(&self) -> Result<Bootstrap, RuntimeError> {
        let settings = self.settings()?;
        let keys = self.key_status()?;
        let account = self.account(&settings)?;
        let accounts = self.subscription_accounts()?;
        let inference = InferenceConnection::detect(&settings, &keys, account.as_ref());
        Ok(Bootstrap {
            bridge_version: BRIDGE_VERSION,
            settings,
            keys,
            inference,
            account,
            accounts,
            store: self.store_info(),
            doctor: self.doctor()?,
        })
    }

    /// Watch everything the core does, from now on.
    ///
    /// Any number of receivers, and none of them privileged: a TUI, a desktop
    /// window and a detail pane all see the same stream. A receiver that
    /// falls behind loses the oldest events — [`Envelope::seq`] is how it
    /// finds out, and [`Runtime::bootstrap`] is how it recovers.
    pub fn subscribe(&self) -> broadcast::Receiver<Envelope> {
        self.events.subscribe()
    }

    pub fn settings(&self) -> Result<Settings, RuntimeError> {
        Ok(self.store.settings().load()?)
    }

    /// Merge `patch` into one settings section. The store validates the whole
    /// of `Settings` before committing, so an invalid patch changes nothing.
    pub fn patch_settings(&self, section: &str, patch: Value) -> Result<Settings, RuntimeError> {
        let settings = self.store.settings().patch(section, patch)?;
        self.publish(AppEvent::SettingsChanged {
            settings: Box::new(settings.clone()),
        });
        Ok(settings)
    }

    /// The Keychain accounts Starkbot itself owns (K6), in the order the UI
    /// reports them. A front end asks the façade rather than linking
    /// `neo-keys` itself.
    #[must_use]
    pub fn accounts() -> &'static [&'static str] {
        &ACCOUNTS
    }

    pub fn key_status(&self) -> Result<Vec<KeyStatus>, RuntimeError> {
        ACCOUNTS
            .iter()
            .map(|account| self.read_key(account))
            .collect()
    }

    pub fn set_key(&self, account: &str, raw: &str) -> Result<KeyStatus, RuntimeError> {
        let secret = Secret::new(raw)?;
        self.keychain.set(account, &secret)?;
        self.forget_secret(account);
        self.report_key(account)
    }

    pub fn remove_key(&self, account: &str) -> Result<KeyStatus, RuntimeError> {
        self.keychain.delete(account)?;
        self.forget_secret(account);
        self.report_key(account)
    }

    /// The speech-to-text backend for this machine (K6 as amended).
    ///
    /// An OpenAI key selects `gpt-transcribe`; without one, macOS on-device
    /// dictation is used, which needs no key, costs nothing and never sends
    /// audio anywhere. A front end asks for this once and keeps it: building
    /// it touches the Keychain.
    pub fn transcriber(&self) -> Result<Box<dyn neo_voice::Transcriber>, RuntimeError> {
        let key = self.secret(neo_keys::ACCOUNT_OPENAI)?;
        let base = std::env::var("OPENAI_BASE_URL")
            .ok()
            .and_then(|value| Url::parse(&value).ok());
        Ok(neo_voice::transcriber(key.as_deref(), base.as_ref())?)
    }

    /// Announce this process on the machine-local roster (cross-process
    /// coordination).
    ///
    /// Several Starkbot processes share one data directory. A process that
    /// has announced itself can be seen by the others and can say what it is
    /// doing, and it is the holder a [`neo_store::Resource::Chrome`] lease is
    /// recorded against. It is *not* what excludes two agents from the
    /// keyboard — that is the `flock` in [`crate::screen`], which the kernel
    /// releases when a run dies and which needs no renewal.
    pub fn announce(&self, kind: neo_store::SessionKind) -> Result<String, RuntimeError> {
        let id = self
            .session
            .get_or_init(|| uuid::Uuid::new_v4().to_string())
            .clone();
        let pid = i32::try_from(std::process::id()).unwrap_or(0);
        let host = hostname();
        self.store
            .presence()
            .announce(&id, kind, pid, &host, now_ms()?)?;
        Ok(id)
    }

    /// This process's session id if it has one, without announcing.
    #[must_use]
    pub fn announced_session(&self) -> Option<String> {
        self.session.get().cloned()
    }

    /// This process's session id, announcing it as a plain command if no front
    /// end has claimed a kind yet.
    ///
    /// Auto-announcing matters: a one-off `neo nav --profile …` must appear
    /// on the roster before it claims the shared Chrome profile, or a TUI
    /// that is mid-run cannot be told who took it.
    pub fn session_id(&self) -> String {
        if let Some(id) = self.session.get() {
            return id.clone();
        }
        match self.announce(neo_store::SessionKind::Cli) {
            Ok(id) => id,
            // A roster that cannot be written must not stop the work; the
            // lease below then simply succeeds, which is the pre-existing
            // single-process behaviour.
            Err(error) => {
                tracing::warn!(%error, "could not announce this session");
                self.session
                    .get_or_init(|| uuid::Uuid::new_v4().to_string())
                    .clone()
            }
        }
    }

    /// Hold a resource for as long as the returned guard lives.
    pub fn hold(
        self: &Arc<Self>,
        resource: neo_store::Resource,
        reason: &str,
    ) -> Result<LeaseGuard, RuntimeError> {
        let holder = self.session_id();
        self.lease(&resource, &holder, Some(reason))?;
        Ok(LeaseGuard {
            runtime: Arc::clone(self),
            resource,
            holder,
        })
    }

    /// Say what this process is doing, and renew its Chrome lease.
    ///
    /// # Errors
    ///
    /// Fails with `StoreError::SessionEvicted` when the roster no longer has
    /// a row for `id` — `sessions()` prunes a stale heartbeat, and the
    /// `leases.holder` cascade takes the lease with it. The caller learns
    /// rather than heartbeating into a row that is not there: re-announcing
    /// is the repair, and it needs the [`neo_store::SessionKind`] only the
    /// surface knows.
    pub fn heartbeat(&self, id: &str, activity: Option<&str>) -> Result<(), RuntimeError> {
        Ok(self
            .store
            .presence()
            .heartbeat(id, activity, now_ms()?, LEASE_TTL_MS)?)
    }

    /// Every live Starkbot process, this one included.
    pub fn sessions(&self) -> Result<Vec<neo_store::Session>, RuntimeError> {
        Ok(self
            .store
            .presence()
            .sessions(now_ms()?, SESSION_STALE_MS)?)
    }

    /// Leave the roster and drop every lease.
    pub fn depart(&self, id: &str) -> Result<(), RuntimeError> {
        Ok(self.store.presence().depart(id)?)
    }

    /// Take an exclusive claim on something there is only one of.
    ///
    /// Answers `Err(RuntimeError::Busy)` naming the holder rather than
    /// proceeding: two Chromes on one profile directory either refuse to
    /// start or steal each other's session. The keyboard is not leased here
    /// — see [`crate::screen`] for why one mechanism guards it, not two.
    pub fn lease(
        &self,
        resource: &neo_store::Resource,
        holder: &str,
        reason: Option<&str>,
    ) -> Result<(), RuntimeError> {
        let presence = self.store.presence();
        let now = now_ms()?;
        if presence
            .acquire(resource, holder, reason, now, LEASE_TTL_MS)?
            .is_some()
        {
            return Ok(());
        }
        // Who has it, so the message can name them.
        let held_by = presence.holder(resource, now)?;
        let (who, doing) = match held_by {
            Some(lease) => {
                let session = self
                    .sessions()?
                    .into_iter()
                    .find(|session| session.id == lease.holder);
                let who = session
                    .as_ref()
                    .map(|session| format!("{} (pid {})", session.kind.label(), session.pid))
                    .unwrap_or_else(|| "another Starkbot".to_owned());
                let doing = session
                    .and_then(|session| session.activity)
                    .or(lease.reason)
                    .unwrap_or_else(|| "something else".to_owned());
                (who, doing)
            }
            None => ("another Starkbot".to_owned(), "something else".to_owned()),
        };
        Err(RuntimeError::Busy {
            resource: resource.label(),
            who,
            doing,
        })
    }

    /// Give a claim back.
    pub fn unlease(
        &self,
        resource: &neo_store::Resource,
        holder: &str,
    ) -> Result<(), RuntimeError> {
        Ok(self.store.presence().release(resource, holder)?)
    }

    /// The conversation a front end should show: the most recent one, or a new
    /// one if there is none. The thread survives a restart because of this.
    pub fn open_conversation(&self) -> Result<neo_core::Conversation, RuntimeError> {
        let conversations = self.store.conversations();
        match conversations.latest()? {
            Some(conversation) => Ok(conversation),
            None => Ok(conversations.create(None, now_ms()?)?),
        }
    }

    /// The `limit` most recently active threads, newest first. What a
    /// conversation switcher lists.
    pub fn conversations(&self, limit: u32) -> Result<Vec<neo_core::Conversation>, RuntimeError> {
        Ok(self.store.conversations().list(limit)?)
    }

    /// Start a thread and announce that the front end is now in it.
    ///
    /// The event is `ConversationReset` rather than a new variant: to every
    /// observer, "a new conversation" and "this one was cleared" are the same
    /// instruction — drop what you were rendering and read the thread again.
    pub fn new_conversation(
        &self,
        title: Option<String>,
    ) -> Result<neo_core::Conversation, RuntimeError> {
        let conversation = self.store.conversations().create(title, now_ms()?)?;
        self.publish(AppEvent::ConversationReset {
            conversation_id: conversation.id,
        });
        Ok(conversation)
    }

    /// Give a thread a title. Renaming one that is not there is an error, not
    /// a silent no-op, so a stale switcher row is reported rather than
    /// swallowed.
    pub fn rename_conversation(
        &self,
        conversation: neo_core::ConversationId,
        title: &str,
    ) -> Result<(), RuntimeError> {
        self.store
            .conversations()
            .rename(conversation, title, now_ms()?)?;
        Ok(())
    }

    /// The `limit` most recent model round trips of one thread, oldest first.
    /// One row per call, which is what a cost view sums — a turn that
    /// appended three messages is still one call.
    pub fn turns(
        &self,
        conversation: neo_core::ConversationId,
        limit: u32,
    ) -> Result<Vec<neo_core::Turn>, RuntimeError> {
        Ok(self.store.conversations().turns(conversation, limit)?)
    }

    /// The thread, oldest first, exactly as stored.
    ///
    /// Stored [`neo_core::Message`]s rather than the model-facing
    /// [`crate::agent::ChatMessage`], because a front end needs the id to key
    /// a row, the timestamp to group by day, and `kind`/`meta` to render a
    /// tool-call card instead of a wall of text. The agent's own shape is one
    /// [`crate::agent::ChatMessage::from_stored`] away.
    pub fn thread(
        &self,
        conversation: neo_core::ConversationId,
        limit: u32,
    ) -> Result<Vec<neo_core::Message>, RuntimeError> {
        Ok(self.store.conversations().messages(conversation, limit)?)
    }

    /// The thread as the agent loop wants it.
    ///
    /// Only the roles a model should see: a stored `system` message is
    /// configuration, not conversation.
    pub fn history(
        &self,
        conversation: neo_core::ConversationId,
        limit: u32,
    ) -> Result<Vec<crate::agent::ChatMessage>, RuntimeError> {
        Ok(self
            .thread(conversation, limit)?
            .iter()
            .filter_map(crate::agent::ChatMessage::from_stored)
            .collect())
    }

    /// Record one line of the conversation and announce it.
    pub fn record_message(
        &self,
        conversation: neo_core::ConversationId,
        message: &crate::agent::ChatMessage,
        spoken: bool,
    ) -> Result<neo_core::Message, RuntimeError> {
        let (role, kind) = match message.role {
            crate::agent::Role::User => (neo_core::MessageRole::User, neo_core::MessageKind::Text),
            crate::agent::Role::Assistant => (
                neo_core::MessageRole::Assistant,
                neo_core::MessageKind::Answer,
            ),
            // What an action produced, fed back to the model next step.
            crate::agent::Role::Tool => {
                (neo_core::MessageRole::Tool, neo_core::MessageKind::Result)
            }
        };
        let source = if spoken {
            neo_core::MessageSource::Voice
        } else {
            neo_core::MessageSource::Typed
        };
        let stored = self.store.conversations().append_message(
            neo_store::NewMessage::new(conversation, role, source, message.text.clone(), now_ms()?)
                .with_kind(kind),
        )?;
        self.publish(AppEvent::Message {
            message: stored.clone(),
        });
        Ok(stored)
    }

    /// Append one row a turn produced, already shaped by the agent, and
    /// announce it.
    ///
    /// Separate from [`Runtime::record_message`] because a turn's rows carry
    /// `meta` — the run they belong to, and the step inside it — which is how
    /// a front end correlates a stored row with the turn it watched. Doing
    /// that through the [`crate::agent::ChatMessage`] shape would mean
    /// widening it with fields only the store cares about.
    pub(crate) fn record_agent_message(
        &self,
        message: neo_store::NewMessage,
    ) -> Result<neo_core::Message, RuntimeError> {
        let stored = self.store.conversations().append_message(message)?;
        self.publish(AppEvent::Message {
            message: stored.clone(),
        });
        Ok(stored)
    }

    /// Grow the assistant row of a run with the next slice of its answer.
    ///
    /// Announces nothing: while a turn runs, the only assistant-text events
    /// are [`AppEvent::TurnDelta`], and a `Message` per slice would have a
    /// front end paint the answer twice.
    pub(crate) fn grow_agent_answer(
        &self,
        message: &neo_store::NewMessage,
        slice: &str,
    ) -> Result<neo_core::MessageId, RuntimeError> {
        Ok(self
            .store
            .conversations()
            .upsert_streaming_message(message, slice)?)
    }

    /// Make a turn reachable while it runs. See [`Runtime::steer`].
    pub(crate) fn register_run(&self, run: RunId, steering: Arc<crate::agent::metal::Steering>) {
        if let Ok(mut runs) = self.runs.lock() {
            runs.insert(run, steering);
        }
    }

    /// Forget a turn that has ended. A run left here would accept steering
    /// nothing would ever read.
    pub(crate) fn forget_run(&self, run: RunId) {
        if let Ok(mut runs) = self.runs.lock() {
            runs.remove(&run);
        }
    }

    /// The inbox of a turn that is still running, if it is.
    pub(crate) fn live_run(&self, run: RunId) -> Option<Arc<crate::agent::metal::Steering>> {
        self.runs
            .lock()
            .ok()
            .and_then(|runs| runs.get(&run).map(Arc::clone))
    }

    /// Neo's own OpenAI key, or `None` when the user has not set one.
    pub(crate) fn openai_secret(&self) -> Result<Option<Arc<Secret>>, RuntimeError> {
        self.secret(neo_keys::ACCOUNT_OPENAI)
    }

    /// Where OpenAI requests go. Injected, so a test points inference at a
    /// local server and nothing ever leaves the machine (08 rule 1).
    pub(crate) fn openai_base(&self) -> &Url {
        &self.key_bases.openai
    }

    /// Where Anthropic requests go. Injected for the same reason
    /// [`Runtime::openai_base`] is: a test points the Claude subscription's
    /// chat path at a local server and nothing leaves the machine.
    pub(crate) fn anthropic_base(&self) -> &Url {
        &self.key_bases.anthropic
    }

    /// Copy every stored credential out of the login Keychain into whatever
    /// backend this process is configured to use.
    ///
    /// For the dev loop: `NEO_KEYCHAIN_FILE` moves storage to a file so that
    /// `cargo test` and `cargo run` never raise an authorization prompt, but
    /// the credentials the real app stored are still in the login Keychain.
    /// This reads each one once — one prompt per item, once — and writes it
    /// where this process will look from now on. A no-op when this process is
    /// already using the login Keychain.
    pub fn import_login_keychain(&self) -> Result<Vec<String>, RuntimeError> {
        if self.keychain.is_login_keychain() {
            return Ok(Vec::new());
        }
        let login = Keychain::login(self.keychain.service());
        let mut imported = Vec::new();
        for account in ACCOUNTS {
            if let Some(secret) = login.get(account)? {
                self.keychain.set(account, &secret)?;
                self.forget_secret(account);
                imported.push((*account).to_owned());
            }
        }
        for provider in [&ANTHROPIC_OAUTH, &OPENAI_CODEX] {
            if let Some(secret) = login.get(provider.id)? {
                self.keychain.set(provider.id, &secret)?;
                self.forget_credential(provider);
                imported.push(provider.id.to_owned());
            }
        }
        Ok(imported)
    }

    /// Re-write every stored credential, so each item's ACL is created by the
    /// binary running now.
    ///
    /// **Why this is needed.** A Keychain item's ACL is bound to the
    /// code-signing identity of whatever created it (05 §Keychain). An item
    /// written by an unsigned — or ad-hoc-signed — build is sealed to an
    /// identity that changes on the next `cargo build`, so macOS asks the user
    /// to authorise every single read, for ever. Reading each value once and
    /// writing it back re-seals it to the current identity: with a signed
    /// binary, that is one prompt per item and then silence. Measured on this
    /// machine: 4.7 s and a dialog per read before, 0.04 s and no dialog
    /// after.
    ///
    /// Returns the accounts that were re-sealed. An account with nothing
    /// stored is skipped rather than created empty.
    pub fn reseal_credentials(&self) -> Result<Vec<String>, RuntimeError> {
        let mut resealed = Vec::new();
        for account in ACCOUNTS {
            // Only Keychain-backed items have an ACL; an environment fallback
            // has nothing to re-seal.
            let Some(secret) = self.keychain.get(account)? else {
                continue;
            };
            self.keychain.delete(account)?;
            self.keychain.set(account, &secret)?;
            self.forget_secret(account);
            resealed.push((*account).to_owned());
        }
        let store = self.oauth();
        for provider in [&ANTHROPIC_OAUTH, &OPENAI_CODEX] {
            let Some(credential) = store.load(provider)? else {
                continue;
            };
            store.clear(provider)?;
            store.save(provider, &credential)?;
            self.forget_credential(provider);
            resealed.push(provider.id.to_owned());
        }
        Ok(resealed)
    }

    /// Ask the vendor what a stored key is worth and announce the answer.
    ///
    /// `set_key` only reports what the Keychain now holds; this is the
    /// authenticated half of 04 §14's `set_key` contract, so a caller that
    /// stores a key follows it with a check. A missing secret makes no request.
    /// A vendor that cannot be reached leaves the key `Unchecked`, never
    /// `Invalid` — an offline user is not told their key is bad (05 §6).
    pub async fn check_key(&self, account: &str) -> Result<KeyStatus, RuntimeError> {
        let status = self.read_key(account)?;
        let Some(secret) = self.secret(account)? else {
            self.publish_key(&status);
            return Ok(status);
        };
        let required = providers::required_model(&self.settings()?, account);
        let state = match self.key_bases.validator(account, required)? {
            Some(validator) => validator.validate(&secret).await?,
            // No validator in this crate can judge this account: `typesafe`
            // belongs to `neo-judge` (05 §1), pack keys to `neo-packs`.
            None => KeyState::Unchecked,
        };
        let checked = KeyStatus::with_source(account, state, status.source);
        self.publish_key(&checked);
        Ok(checked)
    }

    /// The cached catalogue for one runtime (05 §7): what the `models` table
    /// holds, which is what the UI serves until a refresh lands.
    pub fn models(&self, provider: &str) -> Result<Vec<CachedModel>, RuntimeError> {
        Ok(self
            .store
            .models()
            .list(&ProviderId::new(provider), neo_store::GLOBAL_SCOPE)?)
    }

    /// Re-read one API-key runtime's catalogue and replace its cache.
    ///
    /// A runtime whose key is missing is left alone: there is nothing to ask
    /// with, and a stale catalogue beats an empty one. Answers the models now
    /// cached and publishes `ModelsChanged` when the catalogue was replaced.
    pub async fn refresh_models(&self, account: &str) -> Result<Vec<CachedModel>, RuntimeError> {
        let provider = match account {
            ACCOUNT_OPENAI => neo_core::PROVIDER_OPENAI,
            ACCOUNT_ANTHROPIC => neo_core::PROVIDER_ANTHROPIC,
            other => return Err(ValidationError::Unsupported(other.to_owned()).into()),
        };
        let Some(secret) = self.secret(account)? else {
            return self.models(provider);
        };
        let catalog = self.key_bases.catalog(account, &secret).await?;
        let now = now_ms()?;
        self.store.models().replace(
            &ProviderId::new(provider),
            neo_store::GLOBAL_SCOPE,
            catalog.models,
            now,
        )?;
        let cached = self.models(provider)?;
        self.publish(AppEvent::ModelsChanged {
            models: neo_core::ModelRegistryView {
                refreshed_at: Some(now),
                models: cached.iter().map(|model| model.info.clone()).collect(),
            },
        });
        Ok(cached)
    }

    /// Every subscription account row the store holds (K6 paths b and d).
    pub fn subscription_accounts(&self) -> Result<Vec<ProviderAccount>, RuntimeError> {
        let accounts = self.store.provider_accounts();
        let mut rows = Vec::new();
        for provider in SUBSCRIPTION_PROVIDERS {
            if let Some(account) = accounts.get(&ProviderId::new(provider))? {
                rows.push(account);
            }
        }
        Ok(rows)
    }

    /// The subscription account behind the selected runtime, or the one
    /// subscription the user has connected. Only rows the store holds.
    fn account(&self, settings: &Settings) -> Result<Option<ProviderAccount>, RuntimeError> {
        let accounts = self.store.provider_accounts();
        let selected = settings.models.inference.provider.as_str();
        let order = SUBSCRIPTION_PROVIDERS
            .into_iter()
            .filter(|provider| *provider == selected)
            .chain(
                SUBSCRIPTION_PROVIDERS
                    .into_iter()
                    .filter(|provider| *provider != selected),
            );
        for provider in order {
            if let Some(account) = accounts.get(&ProviderId::new(provider))? {
                return Ok(Some(account));
            }
        }
        Ok(None)
    }

    /// What the Keychain — or, failing that, the development environment
    /// fallback — holds for `account`, with the source it came from.
    /// What the Keychain — or, failing that, the development environment
    /// fallback — holds for `account`, with the source it came from.
    ///
    /// This goes through the same cache as a value read: asking the Keychain
    /// whether an item *exists* raises the same authorization prompt as asking
    /// for its contents, and both front ends call `key_status` on every
    /// bootstrap.
    fn read_key(&self, account: &str) -> Result<KeyStatus, RuntimeError> {
        let cached = self.cached_key(account)?;
        let state = match cached.secret {
            Some(_) => KeyState::Present,
            None => KeyState::Missing,
        };
        Ok(KeyStatus::with_source(account, state, cached.source))
    }

    /// The TypeSafe credential, for the one client that may hold it (A6).
    pub(crate) fn typesafe_secret(&self) -> Result<Option<Arc<Secret>>, RuntimeError> {
        self.secret(ACCOUNT_TYPESAFE)
    }

    /// The secret behind `account`: the Keychain first, then the development
    /// environment fallback (05 §6).
    fn secret(&self, account: &str) -> Result<Option<Arc<Secret>>, RuntimeError> {
        Ok(self.cached_key(account)?.secret)
    }

    /// The value and the source behind `account`, read once per process.
    ///
    /// Served from the cache when it has been read already: see the `secrets`
    /// field's doc for why that matters on macOS.
    fn cached_key(&self, account: &str) -> Result<CachedSecret, RuntimeError> {
        if let Ok(cache) = self.secrets.lock()
            && let Some(cached) = cache.get(account)
        {
            return Ok(cached.clone());
        }
        let cached = match self.keychain.get(account)? {
            Some(secret) => CachedSecret {
                secret: Some(Arc::new(secret)),
                source: Some(KeySource::Keychain),
            },
            // The development fallback (05 §6): `TYPESAFE_API_KEY` and friends
            // in the environment, which is how a fresh checkout works before
            // anything is stored.
            None => match neo_keys::from_env(account) {
                Some(secret) => CachedSecret {
                    secret: Some(Arc::new(secret)),
                    source: Some(KeySource::Environment),
                },
                None => CachedSecret {
                    secret: None,
                    source: None,
                },
            },
        };
        if let Ok(mut cache) = self.secrets.lock() {
            cache.insert(account.to_owned(), cached.clone());
        }
        Ok(cached)
    }

    /// Forget what was cached for `account`, so the next read goes to the
    /// Keychain. Called by every write path.
    pub(crate) fn forget_secret(&self, account: &str) {
        if let Ok(mut cache) = self.secrets.lock() {
            cache.remove(account);
        }
    }

    /// Read back what the Keychain now holds and announce it.
    fn report_key(&self, account: &str) -> Result<KeyStatus, RuntimeError> {
        let status = self.read_key(account)?;
        self.publish_key(&status);
        Ok(status)
    }

    /// `AppEvent::KeyStatus` carries the account and the state only — never
    /// the value, and never the source, which the front ends read from
    /// `key_status`/`bootstrap` (04 §14).
    fn publish_key(&self, status: &KeyStatus) {
        self.publish(AppEvent::KeyStatus {
            account: status.account.clone(),
            status: status.state,
        });
    }

    /// Fan an event out, stamped and numbered. No subscribers is normal, not
    /// a failure.
    ///
    /// Every event the core produces passes through here, which is why the
    /// sequence number and the span event are assigned here rather than at
    /// each call site: a new event kind cannot be forgotten this way. An
    /// event that will not serialize is still published — only its span
    /// event is dropped, because a trace must never be the reason a front
    /// end misses an update.
    ///
    /// The event lands on whatever span is open — a turn, a step, a
    /// navigator run — which is what makes a waterfall readable: the notice
    /// about a missing key sits inside the step that needed it.
    ///
    /// One event does not go to the trace. [`AppEvent::TurnDelta`] is
    /// published once per streamed slice, so a 2,000-token answer appended
    /// 2,000 span events to one span, each carrying the whole serialized
    /// event — the answer over again, a few words at a time. And the
    /// serialization itself is behind [`neo_otel::enabled`]: it allocated a
    /// `Value`, a name and a `Vec` per event before anything checked whether
    /// a trace existed, against a doc comment claiming an unobserved Neo
    /// pays one atomic load.
    ///
    /// Public because progress belongs to whoever is doing the work: the
    /// agent loop, the navigator and the eval runner all publish here rather
    /// than each inventing a callback for one observer.
    pub fn publish(&self, event: AppEvent) {
        if neo_otel::enabled()
            && !matches!(event, AppEvent::TurnDelta { .. })
            && let Ok(value) = serde_json::to_value(&event)
        {
            let kind = value
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            neo_otel::event(
                &format!("app_event.{kind}"),
                vec![("starkbot.event", value)],
            );
        }
        let envelope = Envelope {
            seq: self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            at: time::OffsetDateTime::now_utc(),
            event,
        };
        let _ = self.events.send(envelope);
    }

    /// Record one finished agent run as a row in `turns`, the table a cost
    /// view reads.
    ///
    /// A store failure here loses the accounting, never the answer: the run
    /// has already produced what the user asked for, so the failure is
    /// reported as a notice and the turn returns. Callers get `()` because
    /// there is nothing useful to do with the row.
    pub(crate) fn record_turn(&self, turn: neo_store::NewTurn) {
        let Err(error) = self.store.conversations().record_turn(turn) else {
            return;
        };
        self.publish(AppEvent::Notice {
            level: NoticeLevel::Warning,
            code: "turn_not_recorded".to_owned(),
            text: format!("this turn's cost could not be stored: {error}"),
        });
    }
}

/// A login in progress: the URL to show, and the PKCE material the exchange
/// needs. Held by a front end between "show the page" and "the user came
/// back".
pub struct LoginHandle {
    authorize_url: Url,
    login: crate::oauth::PendingLogin,
}

impl LoginHandle {
    /// The vendor page to open. Public values only — no secret is in it.
    #[must_use]
    pub fn authorize_url(&self) -> &Url {
        &self.authorize_url
    }

    #[must_use]
    pub fn provider(&self) -> &'static OauthProvider {
        self.login.provider()
    }

    /// Where the vendor will redirect, which is what a user pastes from.
    #[must_use]
    pub fn redirect_uri(&self) -> &str {
        self.login.redirect_uri()
    }
}

/// The model a subscription path uses before any catalogue has been fetched.
///
/// Both are the current general-purpose model of each vendor's plan, which is
/// what a user on that plan expects a turn to cost against their quota.
const fn default_model(provider: &OauthProvider) -> &'static str {
    if matches!(provider.id.as_bytes(), b"openai-codex") {
        "gpt-5.1-codex"
    } else {
        "claude-sonnet-4-5-20250929"
    }
}

impl Drop for Runtime {
    /// Leave the roster on the way out.
    ///
    /// Without this a one-shot command — `neo app …`, `neo eval` — announced
    /// itself, exited, and left a dead row behind until its heartbeat went
    /// stale; a handful of those in a row makes `neo sessions` useless. A
    /// process that is *killed* still relies on the staleness window, which is
    /// what that window is for.
    fn drop(&mut self) {
        if let Some(id) = self.session.get()
            && let Err(error) = self.store.presence().depart(id)
        {
            tracing::warn!(%error, "could not leave the roster");
        }
    }
}

/// Holds a machine-local resource until it is dropped.
///
/// Release has to happen on every exit path — a navigator error, a cancel, a
/// panic — or one crashed run would keep the keyboard until the lease expired.
/// `Drop` covers all three; the expiry is the backstop for a killed process.
pub struct LeaseGuard {
    runtime: Arc<Runtime>,
    resource: neo_store::Resource,
    holder: String,
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        if let Err(error) = self.runtime.unlease(&self.resource, &self.holder) {
            tracing::warn!(%error, "could not release a lease");
        }
    }
}

/// A Keychain read that has already happened, kept so it happens once.
/// A Keychain read that has already happened, kept so it happens once.
///
/// The value is behind an `Arc` because [`Secret`] is deliberately not
/// `Clone` — it zeroes itself on drop, and a type that copies freely defeats
/// that. Sharing one is fine: it is still dropped, and still zeroed, when the
/// last handle goes.
#[derive(Clone)]
struct CachedSecret {
    secret: Option<Arc<Secret>>,
    source: Option<KeySource>,
}

/// Which inference runtimes `ask`/`ask_json` can actually serve today (A25).
///
/// One list, so the text helper never offers itself for a runtime that would
/// then refuse the request.
#[must_use]
pub fn routes_inference(provider: &str) -> bool {
    matches!(
        provider,
        id if id == ANTHROPIC_OAUTH.id
            || id == OPENAI_CODEX.id
            || id == neo_core::PROVIDER_CLAUDE_SUBSCRIPTION
    )
}

/// The redacted row a stored OAuth credential becomes. Never the token: only
/// what the Connections screen shows (K7).
fn account_of(
    provider: &OauthProvider,
    credential: &OauthCredential,
    updated_at: i64,
) -> ProviderAccount {
    ProviderAccount {
        provider: ProviderId::new(provider.id),
        status: neo_core::ProviderAccountStatus::Connected,
        email: credential.email.clone(),
        plan_type: credential.plan.clone(),
        workspace: None,
        // Plan rate windows arrive with the usage endpoints; an unknown
        // allowance is `None`, never an invented number.
        allowance: None,
        updated_at,
    }
}

/// Hand a URL to the user's browser. One fixed system binary, one argument —
/// not a shell, and not a command a model chose (P3).
fn open_url(url: &str) -> Result<(), RuntimeError> {
    let status = std::process::Command::new("/usr/bin/open")
        .arg(url)
        .status()
        .map_err(RuntimeError::Io)?;
    // `open` spawns fine and then exits non-zero when nothing handles the
    // scheme. Checking only the spawn meant a sign-in whose browser never
    // appeared sat on the callback listener for the full login timeout and
    // then reported a timeout, with nothing saying the page was never shown.
    // The URL is not quoted: it carries this login's `state` and PKCE
    // challenge, and no `RuntimeError` may hold login material.
    if !status.success() {
        return Err(RuntimeError::Io(std::io::Error::other(format!(
            "/usr/bin/open could not open the sign-in page ({status})"
        ))));
    }
    Ok(())
}

/// Record one inference attempt as a `chat {model}` span, following the
/// OpenTelemetry GenAI semantic conventions so a generic trace tool can
/// price and count Neo's model calls without knowing what Neo is.
///
/// The prompt is counted, never copied: a prompt carries whatever the user
/// and the page put into it, and a character count answers the only question
/// a latency report asks of it. The model is the one the vendor says answered
/// — the Claude subscription path resolves an alias on its side, so that is
/// the only place the concrete id appears — and a failed attempt has nothing
/// but the saved id to report.
fn record_inference(
    provider: &str,
    saved: &str,
    json: bool,
    prompt: &str,
    started: Instant,
    result: Result<&Turn, &RuntimeError>,
) {
    let (model, usage) = match result {
        Ok(turn) => (turn.model.as_str(), Some(&turn.usage)),
        Err(_) => (saved, None),
    };
    let span = neo_otel::SpanBuilder::client(format!("chat {model}"))
        .elapsed(started.elapsed())
        .text("gen_ai.operation.name", "chat")
        .text("gen_ai.system", gen_ai_system(provider))
        .text("gen_ai.request.model", model)
        .maybe_int(
            "gen_ai.usage.input_tokens",
            usage.and_then(|usage| tokens(usage, "input_tokens", "prompt_tokens")),
        )
        .maybe_int(
            "gen_ai.usage.output_tokens",
            usage.and_then(|usage| tokens(usage, "output_tokens", "completion_tokens")),
        )
        .flag("starkbot.json_mode", json)
        .int("starkbot.prompt_chars", prompt.chars().count());
    neo_otel::record(span.outcome(result));
}

/// The vendor behind a provider id, in the vocabulary the GenAI conventions
/// use. Anything unrecognised goes through verbatim rather than becoming
/// `other`: a provider this does not know about is still worth grouping by.
fn gen_ai_system(provider: &str) -> &str {
    if provider.contains("anthropic") || provider.contains("claude") {
        return "anthropic";
    }
    if provider.contains("openai") || provider.contains("codex") {
        return "openai";
    }
    provider
}

/// A token count out of a vendor's usage object.
///
/// Two vendors, two spellings, and a count that is missing is missing: a
/// turn recorded as zero input tokens would quietly understate a bill,
/// which is worse than a gap a reader can see.
fn tokens(usage: &Value, primary: &str, fallback: &str) -> Option<u64> {
    usage
        .get(primary)
        .or_else(|| usage.get(fallback))
        .and_then(Value::as_u64)
}

/// Wall-clock Unix milliseconds, the way every stored timestamp is recorded.
pub(crate) fn now_ms() -> Result<i64, RuntimeError> {
    let milliseconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis();
    i64::try_from(milliseconds).map_err(|_| RuntimeError::ClockOverflow)
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]

    use super::*;

    use tempfile::TempDir;
    use tokio::sync::broadcast::error::TryRecvError;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

    fn runtime() -> (TempDir, Runtime) {
        let directory = match TempDir::new() {
            Ok(directory) => directory,
            Err(error) => panic!("{error}"),
        };
        let runtime = match Runtime::open(directory.path()) {
            Ok(runtime) => runtime,
            Err(error) => panic!("{error}"),
        };
        (directory, runtime)
    }

    /// A runtime whose token endpoint is `server`, so no test reaches a
    /// vendor (08 rule 1).
    fn runtime_against(server: &MockServer) -> (TempDir, Arc<Runtime>) {
        let directory = TempDir::new().expect("a temporary data directory");
        let endpoint = Url::parse(&server.uri()).expect("wiremock hands out a valid URL");
        let runtime = Runtime::open(directory.path())
            .expect("the store opens")
            .with_token_endpoint(endpoint);
        (directory, Arc::new(runtime))
    }

    /// A stored Claude credential that is inside its refresh window, so the
    /// next caller has to refresh it.
    fn store_expiring_credential(runtime: &Runtime) {
        runtime
            .oauth()
            .save(
                &ANTHROPIC_OAUTH,
                &OauthCredential {
                    access_token: "at_old".to_owned(),
                    refresh_token: "rt_old".to_owned(),
                    expires_at_ms: now_ms().expect("a clock"),
                    account_id: Some("acct_1".to_owned()),
                    email: Some("someone@example.com".to_owned()),
                    plan: Some("max".to_owned()),
                },
            )
            .expect("the credential stores");
    }

    /// The one place a test may read a token back out.
    fn exposed(secret: &Secret) -> String {
        #[allow(clippy::disallowed_methods)]
        secret.expose().to_owned()
    }

    /// A token endpoint that rotates the refresh token on its first call and
    /// refuses every call after it — which is what a vendor does once the
    /// token it was sent has been replaced.
    #[derive(Default)]
    struct RotateOnce(std::sync::Mutex<usize>);

    impl Respond for RotateOnce {
        fn respond(&self, _request: &wiremock::Request) -> ResponseTemplate {
            let mut calls = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *calls += 1;
            if *calls == 1 {
                return ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "at_new",
                    "refresh_token": "rt_new",
                    "expires_in": 3600,
                }));
            }
            ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_grant",
            }))
        }
    }

    /// Two overlapping turns both wanted a token, and both refreshed.
    ///
    /// Against a vendor that rotates refresh tokens — OpenAI does — the
    /// loser's POST carried a token the winner had already replaced, the
    /// vendor answered `invalid_grant`, `is_dead_grant` matched, and
    /// `OauthStore::access_token_at` deleted the credential. A refresh that
    /// had just succeeded signed the user out mid-turn.
    #[tokio::test]
    async fn two_overlapping_turns_refresh_once_and_keep_the_credential() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(RotateOnce::default())
            .mount(&server)
            .await;
        let (_directory, runtime) = runtime_against(&server);
        store_expiring_credential(&runtime);

        let (first, second) = tokio::join!(
            runtime.oauth_token(&ANTHROPIC_OAUTH),
            runtime.oauth_token(&ANTHROPIC_OAUTH),
        );

        assert_eq!(exposed(&first.expect("the winner refreshes")), "at_new");
        assert_eq!(
            exposed(&second.expect("the loser is served the winner's token")),
            "at_new"
        );
        assert_eq!(
            server
                .received_requests()
                .await
                .expect("recording is on")
                .len(),
            1,
            "one refresh for the provider, not one per caller"
        );
        assert!(
            runtime
                .oauth()
                .load(&ANTHROPIC_OAUTH)
                .expect("the store reads")
                .is_some(),
            "the credential must survive the race that used to delete it"
        );
    }

    /// An offline user is correctly told nothing about their API key
    /// (`key_check::check` folds every reachability failure into
    /// `Unchecked`) and used to be told their subscription was gone, because
    /// `oauth_account` mapped every `Err(_)` to `SignedOut`.
    #[tokio::test]
    async fn a_transport_failure_leaves_the_subscription_unchecked() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503).set_body_string("upstream down"))
            .mount(&server)
            .await;
        let (_directory, runtime) = runtime_against(&server);
        store_expiring_credential(&runtime);

        let account = runtime
            .oauth_account(&ANTHROPIC_OAUTH)
            .await
            .expect("a vendor that cannot be reached is not an error");

        assert_eq!(
            account.status,
            neo_core::ProviderAccountStatus::Unavailable,
            "a 503 says nothing about whether the plan is still there"
        );
        assert_eq!(account.email.as_deref(), Some("someone@example.com"));
        assert!(
            runtime
                .oauth()
                .load(&ANTHROPIC_OAUTH)
                .expect("the store reads")
                .is_some(),
            "and it must not touch the credential"
        );
    }

    /// The one failure that really is a sign out still reads as one.
    #[tokio::test]
    async fn a_dead_grant_is_reported_as_signed_out() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({"error": "invalid_grant"})),
            )
            .mount(&server)
            .await;
        let (_directory, runtime) = runtime_against(&server);
        store_expiring_credential(&runtime);

        let account = runtime
            .oauth_account(&ANTHROPIC_OAUTH)
            .await
            .expect("a refused grant is an account state, not an error");

        assert_eq!(account.status, neo_core::ProviderAccountStatus::SignedOut);
    }

    #[test]
    fn bootstrap_describes_a_fresh_store() {
        let (directory, runtime) = runtime();
        let bootstrap = match runtime.bootstrap() {
            Ok(bootstrap) => bootstrap,
            Err(error) => panic!("{error}"),
        };

        assert_eq!(bootstrap.bridge_version, BRIDGE_VERSION);
        assert_eq!(bootstrap.settings, Settings::default());
        assert_eq!(bootstrap.store.path, directory.path().join("neo.db"));
        assert_eq!(bootstrap.store.schema_version, SCHEMA_VERSION as u32);
        assert_eq!(bootstrap.store.application_id, APPLICATION_ID as i32);
        assert!(directory.path().join("backups").is_dir());
        assert_eq!(bootstrap.account, None);
    }

    #[test]
    fn patching_settings_persists_and_announces_once() {
        let (_directory, runtime) = runtime();
        let mut events = runtime.subscribe();

        let settings = match runtime.patch_settings("identity", serde_json::json!({"name": "Nova"}))
        {
            Ok(settings) => settings,
            Err(error) => panic!("{error}"),
        };
        assert_eq!(settings.identity.name, "Nova");

        let reloaded = match runtime.settings() {
            Ok(settings) => settings,
            Err(error) => panic!("{error}"),
        };
        assert_eq!(reloaded.identity.name, "Nova");

        match events.try_recv() {
            Ok(Envelope {
                event: AppEvent::SettingsChanged { settings },
                ..
            }) => {
                assert_eq!(settings.identity.name, "Nova");
            }
            other => panic!("expected one settings event, got {other:?}"),
        }
        assert!(matches!(events.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn an_invalid_patch_changes_nothing_and_announces_nothing() {
        let (_directory, runtime) = runtime();
        let mut events = runtime.subscribe();

        let rejected = runtime.patch_settings(
            "safety",
            serde_json::json!({"confirm_at": {"destructive": 0.95}}),
        );
        assert!(rejected.is_err());

        let long_name = "n".repeat(25);
        let rejected_name =
            runtime.patch_settings("identity", serde_json::json!({"name": long_name}));
        assert!(rejected_name.is_err());

        let settings = match runtime.settings() {
            Ok(settings) => settings,
            Err(error) => panic!("{error}"),
        };
        assert_eq!(settings, Settings::default());
        assert!(matches!(events.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn an_unknown_section_is_rejected() {
        let (_directory, runtime) = runtime();

        assert!(matches!(
            runtime.patch_settings("wardrobe", serde_json::json!({})),
            Err(RuntimeError::Store(StoreError::UnknownSettingsSection(_)))
        ));
    }

    /// `seq` is the only thing a front end can use to know it has the whole
    /// story, so it must count by exactly one per event, for every observer.
    #[test]
    fn every_event_is_numbered_once_for_every_subscriber() {
        let (_directory, runtime) = runtime();
        let mut first = runtime.subscribe();
        let mut second = runtime.subscribe();

        for index in 0..5_u32 {
            runtime.publish(AppEvent::Latency {
                step_ms_p50: index,
                jev_ms_p50: 0,
            });
        }

        let mut previous = None;
        for _ in 0..5 {
            let envelope = match first.try_recv() {
                Ok(envelope) => envelope,
                Err(error) => panic!("{error}"),
            };
            if let Some(previous) = previous {
                assert_eq!(envelope.seq, previous + 1, "seq must not skip or repeat");
            }
            previous = Some(envelope.seq);
        }
        assert!(matches!(first.try_recv(), Err(TryRecvError::Empty)));

        // The second subscriber sees the same numbers: `seq` describes the
        // process, not one receiver's view of it.
        let same = match second.try_recv() {
            Ok(envelope) => envelope,
            Err(error) => panic!("{error}"),
        };
        assert_eq!(Some(same.seq + 4), previous);
    }

    /// The re-bootstrap trigger of 14 §4: a subscriber that fell behind sees
    /// the numbers jump, which is how it learns to throw its view away rather
    /// than render a thread with holes in it.
    #[test]
    fn a_subscriber_that_falls_behind_sees_a_gap_in_seq() {
        let (_directory, runtime) = runtime();
        let mut events = runtime.subscribe();

        let published = u32::try_from(EVENT_CAPACITY).unwrap_or(u32::MAX) + 10;
        for index in 0..published {
            runtime.publish(AppEvent::Latency {
                step_ms_p50: index,
                jev_ms_p50: 0,
            });
        }

        // The channel reports the overrun once; what matters is that the
        // first envelope after it is not the first event published.
        assert!(matches!(events.try_recv(), Err(TryRecvError::Lagged(_))));
        let resumed = match events.try_recv() {
            Ok(envelope) => envelope,
            Err(error) => panic!("{error}"),
        };
        assert!(
            resumed.seq > 1,
            "a lagged subscriber must not be handed the first event as if nothing was missed"
        );
        assert_eq!(
            resumed.seq,
            u64::from(published) + 1 - EVENT_CAPACITY as u64,
            "the gap is exactly what the channel dropped"
        );
    }
}
