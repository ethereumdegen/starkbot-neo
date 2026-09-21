//! Managed state: the one `Runtime`, the logins that are in flight, and the
//! runs that can still be stopped.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use neo_agent::oauth::OauthProvider;
use neo_agent::runtime::LoginHandle;
use neo_agent::{Runtime, RuntimeError};
use neo_core::{RunId, TimestampMs};
use time::OffsetDateTime;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::error::UiError;
use crate::view::{RunKind, RunView, SUBSCRIPTIONS};

/// A login that has been started: the handle both the loopback wait and the
/// paste fallback borrow, and the task serving the loopback.
struct Login {
    handle: Arc<LoginHandle>,
    wait: JoinHandle<()>,
}

pub struct Desktop {
    runtime: Arc<Runtime>,
    logins: Mutex<HashMap<&'static str, Login>>,
    runs: Arc<Runs>,
}

impl Desktop {
    pub fn open(data_dir: &Path) -> Result<Self, RuntimeError> {
        Ok(Self {
            runtime: Arc::new(Runtime::open(data_dir)?),
            logins: Mutex::new(HashMap::new()),
            runs: Arc::new(Runs::default()),
        })
    }

    /// The run registry, as a handle that outlives the command that took it.
    #[must_use]
    pub fn runs(&self) -> Arc<Runs> {
        Arc::clone(&self.runs)
    }

    #[must_use]
    pub fn runtime(&self) -> Arc<Runtime> {
        Arc::clone(&self.runtime)
    }

    /// A poisoned map only ever held login handles, so recovering it is
    /// correct: nothing in it can be half-written.
    fn logins(&self) -> MutexGuard<'_, HashMap<&'static str, Login>> {
        self.logins
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Remember a started login, replacing (and cancelling) any earlier one
    /// for the same provider.
    pub fn start_login(
        &self,
        provider: &'static OauthProvider,
        handle: Arc<LoginHandle>,
        wait: JoinHandle<()>,
    ) {
        if let Some(previous) = self.logins().insert(provider.id, Login { handle, wait }) {
            previous.wait.abort();
        }
    }

    /// The handle the paste fallback borrows while the loopback wait borrows
    /// the same one — first code to arrive wins.
    #[must_use]
    pub fn login_handle(&self, provider: &'static OauthProvider) -> Option<Arc<LoginHandle>> {
        self.logins()
            .get(provider.id)
            .map(|login| Arc::clone(&login.handle))
    }

    /// Drop the handle and stop the loopback wait. `true` if there was one.
    pub fn cancel_login(&self, provider: &'static OauthProvider) -> bool {
        match self.logins().remove(provider.id) {
            Some(login) => {
                login.wait.abort();
                true
            }
            None => false,
        }
    }

    /// Forget a login whose own wait task has just settled. Never aborts:
    /// this is called from inside that task.
    pub fn finish_login(&self, provider: &'static OauthProvider) {
        self.logins().remove(provider.id);
    }
}

/// One run in flight: what it is, when it started, and the token that stops
/// it.
struct Run {
    kind: RunKind,
    started_at: TimestampMs,
    cancel: CancellationToken,
}

/// Every run this process started and has not yet seen end.
///
/// Behind its own `Arc` rather than living inline in [`Desktop`] because the
/// task that clears an entry outlives the command that made it: a
/// `State<'_, Desktop>` borrow cannot be moved into `tokio::spawn`, and a
/// registry a spawned turn cannot reach would leave the Stop button pointing
/// at nothing.
#[derive(Default)]
pub struct Runs {
    live: Mutex<HashMap<RunId, Run>>,
}

impl Runs {
    /// A poisoned map only ever held tokens and timestamps, so recovering it
    /// is correct — and refusing to would wedge every later run.
    fn live(&self) -> MutexGuard<'_, HashMap<RunId, Run>> {
        self.live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Register a run and hand back the token the work must be given.
    ///
    /// `None` means an exclusive kind is already running. The check and the
    /// insert happen under one lock on purpose: two `run_eval` commands
    /// arriving together would otherwise both see an empty registry, and the
    /// second suite would type into the first one's frontmost window.
    pub fn start(&self, run: RunId, kind: RunKind) -> Option<CancellationToken> {
        let mut live = self.live();
        if kind.exclusive() && live.values().any(|other| other.kind == kind) {
            return None;
        }
        let cancel = CancellationToken::new();
        live.insert(run, Run {
            kind,
            started_at: now_ms(),
            cancel: cancel.clone(),
        });
        Some(cancel)
    }

    /// Ask a run to stop. `true` when there was one to ask.
    ///
    /// The entry stays until the work itself ends: cancellation is a request,
    /// not an undo, and a run that is still closing its browser is still a
    /// run. [`Runs::finish`] is what removes it.
    pub fn stop(&self, run: RunId) -> bool {
        match self.live().get(&run) {
            Some(entry) => {
                entry.cancel.cancel();
                true
            }
            None => false,
        }
    }

    /// Forget a run whose task has just published its terminal event.
    pub fn finish(&self, run: RunId) {
        self.live().remove(&run);
    }

    /// What is still running, newest last. A reloaded webview paints its Stop
    /// buttons from this: the run it was watching did not stop just because
    /// the window did.
    #[must_use]
    pub fn snapshot(&self) -> Vec<RunView> {
        let mut runs: Vec<RunView> = self
            .live()
            .iter()
            .map(|(run, entry)| RunView {
                run: *run,
                kind: entry.kind,
                started_at: entry.started_at,
            })
            .collect();
        runs.sort_by_key(|run| run.started_at);
        runs
    }
}

/// Now, in the store's own unit. Saturates rather than panicking: a clock a
/// hundred million years out is not worth a crash in a view model.
fn now_ms() -> TimestampMs {
    let nanos = OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000;
    TimestampMs::try_from(nanos).unwrap_or(TimestampMs::MAX)
}

/// The two subscription providers, by the id the front end sends.
pub fn provider_by_id(id: &str) -> Result<&'static OauthProvider, UiError> {
    SUBSCRIPTIONS
        .into_iter()
        .find(|provider| provider.id == id)
        .ok_or_else(|| UiError::new("unknown_provider", format!("`{id}` is not a subscription provider")))
}

/// Where `neo` keeps its data. `neo-cli`'s `default_data_dir` is private, so
/// these two lines are duplicated on purpose — both front ends must open the
/// same store, so keep them in step.
#[must_use]
pub fn default_data_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("com.starkbot.neo"),
    )
}
