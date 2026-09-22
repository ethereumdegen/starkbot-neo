//! The bridge: thin wrappers over `neo_agent::Runtime`.
//!
//! Every command is a translation and nothing else — no vendor knowledge, no
//! HTTP, no Keychain, no decision. Secrets travel one way only: a key value
//! arrives from the masked field, goes straight into `Runtime::set_key`, and
//! is never read back, logged or echoed.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use neo_agent::agent::{
    AppOptions, BrowserOptions, ChatMessage, ChatRequest, run_app, run_browser,
};
use neo_agent::oauth::OauthProvider;
use neo_agent::{Runtime, RuntimeError};
use neo_core::{
    AppEvent, AskId, ConfirmId, ConversationId, GateOutcome, HeartbeatGate, PROVIDER_ANTHROPIC,
    PROVIDER_ANTHROPIC_OAUTH, PROVIDER_OPENAI, PROVIDER_OPENAI_CODEX, Project, ProviderAccount,
    ResolutionVia, RunId,
};
use neo_eval::Selection;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::error::UiError;
use crate::state::{Desktop, Runs, provider_by_id};
use crate::view::{
    AxRequestView, AxResponseView, BootstrapView, CaseListingView, CheckView, ConnectionRow,
    ConversationView, Fix, InferenceView, KeyRow, LoginFailed, LoginStart, MessageView, ModelRow,
    ProjectDetailView, RunKind, SUBSCRIPTIONS, Session, SettingsView, selectable_models,
};

/// How much of a thread the window paints, and how many threads the switcher
/// lists. Both bounded because a first frame that reads an unbounded history
/// is a first frame that takes a visible pause.
const THREAD_LIMIT: u32 = 200;
const CONVERSATION_LIMIT: u32 = 50;

/// How long the loopback listener waits for the vendor to redirect: a browser
/// round-trip, a password manager and a second factor. Matches the façade's
/// own all-in-one login.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

/// The inference runtimes this screen can select (K6).
const SELECTABLE_RUNTIMES: [&str; 4] = [
    PROVIDER_ANTHROPIC_OAUTH,
    PROVIDER_OPENAI_CODEX,
    PROVIDER_OPENAI,
    PROVIDER_ANTHROPIC,
];

/// Run one of the façade's blocking calls off the UI's async threads: the
/// store is an actor and answers on a channel.
pub(crate) async fn blocking<T, F>(task: F) -> Result<T, UiError>
where
    F: FnOnce() -> Result<T, RuntimeError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(task)
        .await
        .map_err(UiError::from)?
        .map_err(UiError::from)
}

async fn project_blocking<T, F>(task: F) -> Result<T, UiError>
where
    F: FnOnce() -> Result<T, neo_agent::ProjectError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(task)
        .await
        .map_err(UiError::from)?
        .map_err(UiError::from)
}

fn project_detail(
    runtime: &Runtime,
    slug: &str,
) -> Result<ProjectDetailView, neo_agent::ProjectError> {
    let project = runtime.project(slug)?;
    let documents = runtime.project_documents(slug)?;
    let ticks = runtime.project_ticks(slug, 20)?;
    Ok(ProjectDetailView {
        project,
        soul: documents.soul,
        heartbeat: documents.heartbeat,
        ticks,
    })
}

/// Both subscription rows as the *store* last recorded them: no Keychain
/// read, no network, no prompt. This is what the first frame paints from, so
/// the window is never blocked behind macOS asking whether this build may
/// read a credential.
async fn stored_rows(runtime: Arc<Runtime>) -> Result<Vec<ProviderAccount>, UiError> {
    let stored = blocking(move || runtime.subscription_accounts()).await?;
    Ok(SUBSCRIPTIONS
        .into_iter()
        .map(|provider| {
            stored
                .iter()
                .find(|account| account.provider.as_str() == provider.id)
                .cloned()
                .unwrap_or_else(|| ProviderAccount {
                    provider: neo_core::ProviderId::new(provider.id),
                    status: neo_core::ProviderAccountStatus::SignedOut,
                    email: None,
                    plan_type: None,
                    workspace: None,
                    allowance: None,
                    updated_at: 0,
                })
        })
        .collect())
}

/// Both subscription rows, verified against the Keychain — and against the
/// vendor, when a stored token is close to expiry.
async fn verified_rows(runtime: &Runtime) -> Result<Vec<ProviderAccount>, UiError> {
    let mut rows = Vec::with_capacity(SUBSCRIPTIONS.len());
    for provider in SUBSCRIPTIONS {
        rows.push(runtime.oauth_account(provider).await?);
    }
    Ok(rows)
}

async fn selected_provider(runtime: Arc<Runtime>) -> Result<String, UiError> {
    let settings = blocking(move || runtime.settings()).await?;
    Ok(settings.models.inference.provider.as_str().to_owned())
}

/// The first call the webview makes: do the two halves of this app speak the
/// same protocol?
///
/// The TUI cannot skew — it is compiled against the same core — but the
/// webview can: `ui/dist` is a build artefact that outlives the binary it was
/// built for, and a stale bundle would otherwise discover the mismatch by
/// reading a field that is no longer there, several screens in. Rejecting the
/// handshake instead turns that into one sentence with the command that fixes
/// it.
#[tauri::command]
pub fn handshake(ui_version: u32) -> Result<u32, UiError> {
    if ui_version == neo_agent::BRIDGE_VERSION {
        return Ok(neo_agent::BRIDGE_VERSION);
    }
    Err(UiError::new(
        "bridge_version",
        format!(
            "this window was built for bridge v{ui_version}, but the app speaks v{}.",
            neo_agent::BRIDGE_VERSION
        ),
    )
    .with_fix(Fix::Manual {
        detail: "rebuild the UI: `npm run build` in ui/".to_owned(),
    }))
}

/// Everything the window paints its first frame from, in one call (04 §14).
///
/// One blocking hop for the lot: the store is an actor, and six round trips
/// to it would be six queue waits before anything appears. Nothing here
/// reads the Keychain or the network, so the frame is never held behind
/// macOS asking whether this build may read a credential — the screen
/// follows this with `connections`.
#[tauri::command]
pub async fn get_bootstrap(state: State<'_, Desktop>) -> Result<BootstrapView, UiError> {
    let runtime = state.runtime();
    let accounts = stored_rows(Arc::clone(&runtime)).await?;
    let runs = state.runs().snapshot();
    let store = Arc::clone(&runtime);
    let (boot, conversation, messages, conversations, models) = blocking(move || {
        let boot = store.bootstrap()?;
        // Opening rather than listing: a window with no thread to type into
        // is not a usable screen, and the store's newest thread is the one
        // the last session was in.
        let conversation = store.open_conversation()?;
        let messages = store.thread(conversation.id, THREAD_LIMIT)?;
        let conversations = store.conversations(CONVERSATION_LIMIT)?;
        let models = store.models(boot.settings.models.inference.provider.as_str())?;
        Ok((boot, conversation, messages, conversations, models))
    })
    .await?;
    // Walks candidate bundle paths on disk, so it is blocking too.
    let eval_cases = tokio::task::spawn_blocking(neo_eval::list_cases).await?;

    let session = Session {
        conversation,
        messages,
        conversations,
        models: models
            .iter()
            .map(|model| ModelRow::new(&model.info, model.hidden))
            .collect(),
        eval_cases,
        runs,
    };
    Ok(BootstrapView::new(&runtime, &boot, &accounts, session))
}

#[tauri::command]
pub async fn list_projects(state: State<'_, Desktop>) -> Result<Vec<Project>, UiError> {
    let runtime = state.runtime();
    project_blocking(move || runtime.projects()).await
}
#[tauri::command]
pub async fn create_project(
    state: State<'_, Desktop>,
    name: String,
    root: Option<String>,
) -> Result<ProjectDetailView, UiError> {
    let runtime = state.runtime();
    project_blocking(move || {
        let root = root
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from);
        let project = runtime.create_project(&name, root.as_deref())?;
        project_detail(&runtime, &project.slug)
    })
    .await
}

#[tauri::command]
pub async fn show_project(
    state: State<'_, Desktop>,
    slug: String,
) -> Result<ProjectDetailView, UiError> {
    let runtime = state.runtime();
    project_blocking(move || project_detail(&runtime, &slug)).await
}

#[tauri::command]
pub async fn save_project_document(
    state: State<'_, Desktop>,
    slug: String,
    document: String,
    content: String,
) -> Result<ProjectDetailView, UiError> {
    let runtime = state.runtime();
    project_blocking(move || {
        runtime.write_project_document(&slug, &document, &content)?;
        project_detail(&runtime, &slug)
    })
    .await
}

#[tauri::command]
pub async fn configure_project_heartbeat(
    state: State<'_, Desktop>,
    slug: String,
    enabled: bool,
    every_seconds: u64,
    on_gate: HeartbeatGate,
) -> Result<ProjectDetailView, UiError> {
    let runtime = state.runtime();
    project_blocking(move || {
        runtime.configure_project_heartbeat(&slug, enabled, every_seconds, on_gate)?;
        project_detail(&runtime, &slug)
    })
    .await
}

#[tauri::command]
pub async fn run_project_heartbeat(
    state: State<'_, Desktop>,
    slug: String,
) -> Result<ProjectDetailView, UiError> {
    let runtime = state.runtime();
    runtime.run_project_heartbeat(&slug).await?;
    project_detail(&runtime, &slug).map_err(UiError::from)
}

/// The two subscription rows, verified: this is the call that reads the
/// Keychain, so the screen makes it after its first paint.
#[tauri::command]
pub async fn connections(state: State<'_, Desktop>) -> Result<Vec<ConnectionRow>, UiError> {
    let runtime = state.runtime();
    let accounts = verified_rows(&runtime).await?;
    let selected = selected_provider(runtime).await?;
    Ok(accounts
        .iter()
        .map(|account| ConnectionRow::new(account, &selected))
        .collect())
}

/// Start a login and immediately begin serving its loopback callback.
///
/// The listener is bound by the wait, not by `begin_oauth`, so the wait is
/// spawned here — before the screen can show the URL, and long before the
/// user can press *Open in browser*. The handle stays in managed state so the
/// paste fallback can borrow the same PKCE material.
#[tauri::command]
pub async fn begin_login<R: tauri::Runtime>(
    app: AppHandle<R>,
    state: State<'_, Desktop>,
    provider: String,
) -> Result<LoginStart, UiError> {
    let provider = provider_by_id(&provider)?;
    let runtime = state.runtime();
    let handle = Arc::new(runtime.begin_oauth(provider)?);
    let start = LoginStart {
        provider: provider.id.to_owned(),
        authorize_url: handle.authorize_url().to_string(),
        redirect_uri: handle.redirect_uri().to_owned(),
        timeout_secs: LOGIN_TIMEOUT.as_secs(),
    };

    let wait = tokio::spawn({
        let handle = Arc::clone(&handle);
        let runtime = Arc::clone(&runtime);
        async move {
            let settled = runtime.finish_oauth(&handle, LOGIN_TIMEOUT).await;
            let selected = selected_provider(runtime)
                .await
                .unwrap_or_else(|_| String::new());
            let _ = match settled {
                Ok(account) => {
                    // Signed in: the handle has done its work.
                    app.state::<Desktop>().finish_login(provider);
                    app.emit("login:done", ConnectionRow::new(&account, &selected))
                }
                // The browser never came back, or the redirect was refused.
                // The handle stays in state on purpose: the same PKCE
                // material is what the paste fallback needs.
                Err(error) => app.emit(
                    "login:failed",
                    LoginFailed {
                        provider: provider.id.to_owned(),
                        error: UiError::from(error),
                    },
                ),
            };
        }
    });
    state.start_login(provider, handle, wait);
    Ok(start)
}

/// Hand the authorize URL this app minted to the user's browser. The URL is
/// taken from the stored handle, never from the webview.
#[tauri::command]
pub async fn open_login_page(state: State<'_, Desktop>, provider: String) -> Result<(), UiError> {
    let provider = provider_by_id(&provider)?;
    let handle = state
        .login_handle(provider)
        .ok_or_else(|| no_login(provider))?;
    let url = handle.authorize_url().to_string();
    let runtime = state.runtime();
    blocking(move || runtime.open_in_browser(&url)).await
}

/// The fallback for when the loopback never arrives: the user pastes the
/// redirect URL they were left on.
#[tauri::command]
pub async fn finish_login_pasted<R: tauri::Runtime>(
    app: AppHandle<R>,
    state: State<'_, Desktop>,
    provider: String,
    pasted: String,
) -> Result<ConnectionRow, UiError> {
    let provider = provider_by_id(&provider)?;
    let handle = state
        .login_handle(provider)
        .ok_or_else(|| no_login(provider))?;
    let runtime = state.runtime();
    let account = runtime.finish_oauth_pasted(&handle, &pasted).await?;
    // The code is spent; the loopback wait has nothing left to serve.
    state.cancel_login(provider);
    let selected = selected_provider(runtime).await?;
    let row = ConnectionRow::new(&account, &selected);
    let _ = app.emit("login:done", row.clone());
    Ok(row)
}

/// Drop the handle and the loopback wait. Nothing was stored, so there is
/// nothing to undo.
#[tauri::command]
pub async fn cancel_login(state: State<'_, Desktop>, provider: String) -> Result<bool, UiError> {
    let provider = provider_by_id(&provider)?;
    Ok(state.cancel_login(provider))
}

/// Forget one subscription credential.
#[tauri::command]
pub async fn disconnect(
    state: State<'_, Desktop>,
    provider: String,
) -> Result<ConnectionRow, UiError> {
    let provider = provider_by_id(&provider)?;
    state.cancel_login(provider);
    let runtime = state.runtime();
    let account = {
        let runtime = Arc::clone(&runtime);
        blocking(move || runtime.disconnect_oauth(provider)).await?
    };
    let selected = selected_provider(runtime).await?;
    Ok(ConnectionRow::new(&account, &selected))
}

/// The local readiness report, with a control attached to every row this
/// window can fix.
#[tauri::command]
pub async fn run_doctor(state: State<'_, Desktop>) -> Result<Vec<CheckView>, UiError> {
    let runtime = state.runtime();
    let report = blocking(move || runtime.doctor()).await?;
    Ok(crate::view::checks(&report))
}

#[tauri::command]
pub async fn key_status(state: State<'_, Desktop>) -> Result<Vec<KeyRow>, UiError> {
    let runtime = state.runtime();
    let statuses = blocking(move || runtime.key_status()).await?;
    Ok(statuses.iter().map(KeyRow::new).collect())
}

/// Store a pasted key and immediately ask the vendor what it is worth.
///
/// `value` is moved into the Keychain and dropped. It is never returned, and
/// the only thing that comes back is a `KeyState`.
#[tauri::command]
pub async fn set_key(
    state: State<'_, Desktop>,
    account: String,
    value: String,
) -> Result<KeyRow, UiError> {
    let account = known_account(&account)?;
    let runtime = state.runtime();
    {
        let runtime = Arc::clone(&runtime);
        blocking(move || runtime.set_key(account, &value)).await?;
    }
    let checked = runtime.check_key(account).await?;
    Ok(KeyRow::new(&checked))
}

#[tauri::command]
pub async fn check_key(state: State<'_, Desktop>, account: String) -> Result<KeyRow, UiError> {
    let account = known_account(&account)?;
    let runtime = state.runtime();
    let status = runtime.check_key(account).await?;
    Ok(KeyRow::new(&status))
}

#[tauri::command]
pub async fn remove_key(state: State<'_, Desktop>, account: String) -> Result<KeyRow, UiError> {
    let account = known_account(&account)?;
    let runtime = state.runtime();
    let status = blocking(move || runtime.remove_key(account)).await?;
    Ok(KeyRow::new(&status))
}

/// Point inference at one of the K6 runtimes, keeping the symbolic model id
/// unless the caller names another (K3).
#[tauri::command]
pub async fn set_inference_runtime(
    state: State<'_, Desktop>,
    provider: String,
    model: Option<String>,
) -> Result<InferenceView, UiError> {
    if !SELECTABLE_RUNTIMES.contains(&provider.as_str()) {
        return Err(UiError::new(
            "unknown_runtime",
            format!("`{provider}` is not an inference runtime"),
        )
        .with_fix(Fix::ChooseRuntime));
    }
    let runtime = state.runtime();
    let accounts = stored_rows(Arc::clone(&runtime)).await?;
    let boot = {
        let runtime = Arc::clone(&runtime);
        blocking(move || {
            let settings = runtime.settings()?;
            let id = model.unwrap_or_else(|| settings.models.inference.id.clone());
            runtime.patch_settings(
                "models",
                json!({ "inference": { "provider": provider, "id": id } }),
            )?;
            runtime.bootstrap()
        })
        .await?
    };
    Ok(InferenceView::new(
        &boot.settings,
        boot.inference,
        &boot.keys,
        &accounts,
    ))
}

/// One runtime's catalogue as the store last cached it. Omitting `provider`
/// asks about the runtime inference is currently pointed at, which is what a
/// model picker wants and saves it a settings round trip.
#[tauri::command]
pub async fn list_models(
    state: State<'_, Desktop>,
    provider: Option<String>,
) -> Result<Vec<ModelRow>, UiError> {
    let runtime = state.runtime();
    let (provider, models) = blocking(move || {
        let provider = match provider {
            Some(provider) => provider,
            None => runtime
                .settings()?
                .models
                .inference
                .provider
                .as_str()
                .to_owned(),
        };
        let models = runtime.models(&provider)?;
        Ok((provider, models))
    })
    .await?;
    let rows = models
        .iter()
        .map(|model| ModelRow::new(&model.info, model.hidden))
        .collect();
    Ok(selectable_models(&provider, rows))
}

/// Re-read one API-key runtime's catalogue from the vendor.
#[tauri::command]
pub async fn refresh_models(
    state: State<'_, Desktop>,
    account: String,
) -> Result<Vec<ModelRow>, UiError> {
    let account = known_account(&account)?;
    let runtime = state.runtime();
    let models = runtime.refresh_models(account).await?;
    Ok(models
        .iter()
        .map(|model| ModelRow::new(&model.info, model.hidden))
        .collect())
}

/// Every setting, as the store holds them.
#[tauri::command]
pub async fn get_settings(state: State<'_, Desktop>) -> Result<SettingsView, UiError> {
    let runtime = state.runtime();
    let settings = blocking(move || runtime.settings()).await?;
    Ok(SettingsView::new(settings))
}

/// Merge `value` into one settings section (RFC 7386).
///
/// Answers with the whole of settings rather than nothing, because the store
/// validates the merged result and may normalise it: a pane that kept its
/// own copy of what it sent would show a value the store rejected.
#[tauri::command]
pub async fn patch_settings(
    state: State<'_, Desktop>,
    section: String,
    value: Value,
) -> Result<SettingsView, UiError> {
    let runtime = state.runtime();
    let settings = blocking(move || runtime.patch_settings(&section, value)).await?;
    Ok(SettingsView::new(settings))
}

#[tauri::command]
pub async fn list_conversations(
    state: State<'_, Desktop>,
    limit: Option<u32>,
) -> Result<Vec<ConversationView>, UiError> {
    let runtime = state.runtime();
    let limit = limit.unwrap_or(CONVERSATION_LIMIT);
    let conversations = blocking(move || runtime.conversations(limit)).await?;
    Ok(conversations.iter().map(ConversationView::new).collect())
}

#[tauri::command]
pub async fn new_conversation(
    state: State<'_, Desktop>,
    title: Option<String>,
) -> Result<ConversationView, UiError> {
    let runtime = state.runtime();
    let conversation = blocking(move || runtime.new_conversation(title)).await?;
    Ok(ConversationView::new(&conversation))
}

#[tauri::command]
pub async fn rename_conversation(
    state: State<'_, Desktop>,
    id: ConversationId,
    title: String,
) -> Result<(), UiError> {
    let runtime = state.runtime();
    blocking(move || runtime.rename_conversation(id, &title)).await
}

#[tauri::command]
pub async fn delete_conversation(
    state: State<'_, Desktop>,
    id: ConversationId,
) -> Result<(), UiError> {
    let runtime = state.runtime();
    blocking(move || runtime.delete_conversation(id)).await
}

/// One thread, oldest first, as stored — including the `result` rows an
/// action produced, so a reopened window shows the steps a turn took and not
/// just its answer.
#[tauri::command]
pub async fn load_thread(
    state: State<'_, Desktop>,
    id: ConversationId,
    limit: Option<u32>,
) -> Result<Vec<MessageView>, UiError> {
    let runtime = state.runtime();
    let limit = limit.unwrap_or(THREAD_LIMIT);
    let messages = blocking(move || runtime.thread(id, limit)).await?;
    Ok(messages.iter().map(MessageView::new).collect())
}

/// Start an agent turn and answer with its run id immediately.
///
/// The turn drives real applications and can take minutes; awaiting it here
/// would hold the IPC call open for all of it, and the window would have
/// nothing to show and nothing to press. Progress is `TurnStarted`,
/// `TurnStep`, `NavStep`, `TurnStepDone` and finally `TurnFinished` or
/// `TurnFailed`, every one carrying the id this returns.
#[tauri::command]
pub async fn send_message(
    state: State<'_, Desktop>,
    conversation: ConversationId,
    text: String,
) -> Result<RunId, UiError> {
    start_turn(state.runtime(), state.runs(), conversation, text).await
}

/// The body of [`send_message`], reachable without a `State` borrow.
///
/// Split out for the control socket (`crate::control`), which starts a turn
/// on behalf of a shell command rather than a click. There is deliberately
/// only one of these: a second way to start a turn would be a second place
/// to forget the registry entry the Stop button reads, or to record the
/// user's message twice.
pub(crate) async fn start_turn(
    runtime: Arc<Runtime>,
    runs: Arc<Runs>,
    conversation: ConversationId,
    text: String,
) -> Result<RunId, UiError> {
    // Recorded before the turn starts, so the thread the model is shown
    // contains what the user just said — and so a second window sees the
    // message the moment it is sent rather than when the turn ends.
    let history = {
        let runtime = Arc::clone(&runtime);
        blocking(move || {
            runtime.record_message(conversation, &ChatMessage::user(text), false)?;
            runtime.history(conversation, THREAD_LIMIT)
        })
        .await?
    };

    let request = ChatRequest::new(conversation, history);
    let run = request.run;
    let cancel = runs
        .start(run, RunKind::Chat)
        .ok_or_else(|| busy(RunKind::Chat))?;
    let request = request.with_cancel(cancel);

    tokio::spawn(async move {
        // Nothing is recorded or published here. `chat` writes the turn to
        // the thread as it produces it — the user's steer, the assistant text
        // as it grows, the observation each step left — and publishes its own
        // `TurnFinished`/`TurnFailed`. A second writer here would put every
        // row in the thread twice, and a second terminal event would have a
        // front end render the turn's end twice.
        let _ = runtime.chat(request).await;
        runs.finish(run);
    });
    Ok(run)
}

/// Deliver a message into a turn that is already running.
///
/// The composer does not go quiet while the agent works: a user who sees the
/// wrong tool open wants to say so *now*, and a window that refused would
/// make them wait out a turn they already know is wrong. The message reaches
/// the running turn at its next step boundary.
///
/// `false` means that run had already finished between the keystroke and the
/// command — not a failure, and the caller's repair is to send it as a new
/// turn instead.
#[tauri::command]
pub async fn steer_run(
    state: State<'_, Desktop>,
    run: RunId,
    text: String,
) -> Result<bool, UiError> {
    // No `blocking` hop: the mailbox send is a channel push, not a store
    // round trip, and the point of steering is that it lands immediately.
    Ok(state.runtime().steer(run, &text)?)
}

/// Ask a run to stop. `true` when there was one to ask.
///
/// Not an undo, and the window should not pretend otherwise: the model round
/// trip in flight is dropped but already paid for, keystrokes already
/// delivered to an application cannot be un-typed, and a Chrome the
/// navigator launched is closed on the way out rather than the instant this
/// returns.
#[tauri::command]
pub async fn stop_run(state: State<'_, Desktop>, run: RunId) -> Result<bool, UiError> {
    Ok(state.runs().stop(run))
}

/// The window's answer to a confirm card.
///
/// Nothing is published here: the run that raised the card publishes
/// `ConfirmResolved` with the outcome it actually used, and a command that
/// announced its own would have the thread showing an approval the run never
/// acted on.
#[tauri::command]
pub async fn resolve_confirm(
    state: State<'_, Desktop>,
    id: ConfirmId,
    outcome: GateOutcome,
    via: ResolutionVia,
) -> Result<(), UiError> {
    // No `blocking` hop: answering a card is a oneshot send to the waiting
    // run, not a store round trip, and a card is what a person is sitting
    // in front of waiting on.
    state
        .runtime()
        .resolve_confirm(id, outcome, via)
        .map_err(card_error)
}

/// The window's answer to a question.
#[tauri::command]
pub async fn answer_ask(
    state: State<'_, Desktop>,
    id: AskId,
    answer: String,
    via: ResolutionVia,
) -> Result<(), UiError> {
    state
        .runtime()
        .answer_ask(id, answer, via)
        .map_err(card_error)
}

/// A card nobody is waiting on any more gets its own code.
///
/// Both front ends can be showing the same card and a voice answer can beat
/// them both, so losing the race is the ordinary case, not a fault: the
/// window says something else answered first rather than painting a failure
/// over an action that did happen.
fn card_error(error: RuntimeError) -> UiError {
    if matches!(error, RuntimeError::NoSuchCard(_)) {
        return UiError::new("no_such_card", error.to_string());
    }
    UiError::from(error)
}

/// Drive a web page to a goal, as `neo nav` does. Returns the run id at once;
/// progress is `NavStep`, and the end is `TurnFinished`/`TurnFailed`.
///
/// `attach` absent and `attach` empty mean the same thing: a JavaScript
/// object drops an `undefined` field on the way out, and a run refused
/// because a list of no files was missing would be a bad joke.
#[tauri::command]
pub async fn run_nav(
    state: State<'_, Desktop>,
    url: String,
    goal: String,
    headless: bool,
    profile: Option<PathBuf>,
    attach: Option<Vec<PathBuf>>,
    safety: bool,
) -> Result<RunId, UiError> {
    let runtime = state.runtime();
    let runs = state.runs();
    let options = {
        let runtime = Arc::clone(&runtime);
        blocking(move || {
            let settings = runtime.settings()?;
            let mut options = BrowserOptions::unattended(&settings, url, goal);
            options.headless = headless;
            options.safety_heads = safety;
            options.profile = profile;
            options.attach = attach.unwrap_or_default();
            Ok(options)
        })
        .await?
    };

    let run = RunId::new();
    let cancel = runs
        .start(run, RunKind::Nav)
        .ok_or_else(|| busy(RunKind::Nav))?;
    tokio::spawn(async move {
        let outcome = run_browser(&runtime, &options, run, &cancel)
            .await
            .map(|finished| (finished.observation, finished.steps))
            .map_err(UiError::from);
        ended(&runtime, &runs, run, outcome);
    });
    Ok(run)
}

/// Drive a native macOS application to a goal, as `neo nav app` does.
#[tauri::command]
pub async fn run_app_goal(
    state: State<'_, Desktop>,
    app: String,
    goal: String,
) -> Result<RunId, UiError> {
    let runtime = state.runtime();
    let runs = state.runs();
    let options = {
        let runtime = Arc::clone(&runtime);
        blocking(move || {
            let settings = runtime.settings()?;
            Ok(AppOptions::unattended(&settings, app, goal))
        })
        .await?
    };

    let run = RunId::new();
    let cancel = runs
        .start(run, RunKind::App)
        .ok_or_else(|| busy(RunKind::App))?;
    tokio::spawn(async move {
        let outcome = run_app(&runtime, &options, run, &cancel)
            .await
            .map(|finished| (finished.observation, finished.steps))
            .map_err(UiError::from);
        ended(&runtime, &runs, run, outcome);
    });
    Ok(run)
}

/// One accessibility request, answered in place.
///
/// Not a run: observing a table or pressing one row is a round trip, not
/// minutes of work, and there is nothing a Stop button would usefully
/// interrupt. It is *named* like one, though — `ax()` publishes its activate
/// line as an [`AppEvent::NavStep`] carrying this id — so it owes the stream
/// a terminal event all the same. Without one every Inspect press left a
/// permanently-running run in every front end watching: a climbing badge, a
/// timer kept alive for the life of the window, and a run list that only
/// grew.
#[tauri::command]
pub async fn run_ax(
    state: State<'_, Desktop>,
    request: AxRequestView,
) -> Result<AxResponseView, UiError> {
    let runtime = state.runtime();
    let run = RunId::new();
    let label = ax_label(&request);
    let answered = neo_agent::ax::ax(
        &runtime,
        request.into(),
        run,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .map_err(UiError::from);
    let outcome = match &answered {
        Ok(_) => Ok((label, 0)),
        Err(error) => Err(error.clone()),
    };
    settle(&runtime, run, outcome);
    Ok(answered?.into())
}

/// Every eval case and whether this machine can run it. Costs nothing: no
/// model, no application launched.
#[tauri::command]
pub async fn list_eval_cases() -> Result<Vec<CaseListingView>, UiError> {
    let cases = tokio::task::spawn_blocking(neo_eval::list_cases).await?;
    Ok(cases.iter().map(CaseListingView::new).collect())
}

/// Run the evaluation suite.
///
/// Refuses a second concurrent suite: the cases drive real applications
/// through the keyboard and the frontmost window, so two suites would type
/// into each other and both reports would be fiction.
#[tauri::command]
pub async fn run_eval(
    state: State<'_, Desktop>,
    filter: Option<String>,
    tags: Option<Vec<String>>,
    once: bool,
) -> Result<RunId, UiError> {
    let runtime = state.runtime();
    let runs = state.runs();
    let selection = Selection {
        filter,
        tags: tags.unwrap_or_default(),
        once,
    };

    let run = RunId::new();
    let cancel = runs
        .start(run, RunKind::Eval)
        .ok_or_else(|| busy(RunKind::Eval))?;
    // Tauri's runtime is multi-threaded, which `run_suite` needs: it reaches
    // the synchronous store through `block_in_place`.
    tokio::spawn(async move {
        let outcome = neo_eval::run_suite(&runtime, selection, run, &cancel)
            .await
            .map(|report| {
                let skipped = report.total.saturating_sub(report.passed + report.failed);
                (
                    format!(
                        "{} passed, {} failed, {skipped} skipped of {}",
                        report.passed, report.failed, report.total
                    ),
                    report.total,
                )
            })
            .map_err(UiError::from);
        ended(&runtime, &runs, run, outcome);
    });
    Ok(run)
}

/// The one terminal event a hand-driven run owes its watchers, and the
/// registry entry dropped in the same breath.
///
/// `Runtime::chat` publishes these itself; the navigator, the application
/// path and the eval suite do not, and without them a front end would have
/// to guess that a run ended from the absence of further steps — a nav run
/// that failed before its first step would spin forever.
fn ended(runtime: &Runtime, runs: &Runs, run: RunId, outcome: Result<(String, usize), UiError>) {
    settle(runtime, run, outcome);
    runs.finish(run);
}

/// The terminal event on its own, for the one caller that has no registry
/// entry to drop: `run_ax` is a round trip rather than a run, but `ax()`
/// publishes under its id, so the stream still has to be told it ended.
fn settle(runtime: &Runtime, run: RunId, outcome: Result<(String, usize), UiError>) {
    match outcome {
        Ok((text, steps)) => runtime.publish(AppEvent::TurnFinished {
            run,
            text,
            steps: u32::try_from(steps).unwrap_or(u32::MAX),
            exhausted: false,
            usage: None,
        }),
        // The code travels beside the sentence because the sentence is
        // written for a person: a front end styling a stopped run as
        // stopped rather than broken used to have to run a regex over the
        // prose, and `UiError::CANCELLED` exists precisely so it does not.
        Err(error) => runtime.publish(AppEvent::TurnFailed {
            run,
            error: error.message,
            code: error.code,
        }),
    }
}

/// What an accessibility request was, in one line, for the event that ends
/// it.
///
/// The text a `set` or a `type` was carrying is deliberately not in it: this
/// goes onto a broadcast every front end and the telemetry exporter sees,
/// and the thing a user types into an application is as often a password as
/// it is a search term.
fn ax_label(request: &AxRequestView) -> String {
    match request {
        AxRequestView::Trusted => "ax trusted".to_owned(),
        AxRequestView::Apps => "ax apps".to_owned(),
        AxRequestView::Table { app } => format!("ax table {app}"),
        AxRequestView::Press { app, index } => format!("ax press {app} #{index}"),
        AxRequestView::Set { app, index, .. } => format!("ax set {app} #{index}"),
        AxRequestView::Menu { app, path } => format!("ax menu {app} {path}"),
        AxRequestView::Type { app, .. } => format!("ax type {app}"),
        AxRequestView::Key { app, key } => format!("ax key {app} {key}"),
    }
}

/// The refusal an exclusive run kind gives when one is already going.
fn busy(kind: RunKind) -> UiError {
    match kind {
        RunKind::Eval => UiError::new(
            "eval_busy",
            "an evaluation is already running; the suite drives the keyboard and the frontmost window, so only one can run at a time",
        ),
        _ => UiError::new("busy", "that run could not be started"),
    }
}

/// One of the three Starkbot-owned Keychain accounts, as a `'static` name the
/// façade takes.
fn known_account(account: &str) -> Result<&'static str, UiError> {
    Runtime::accounts()
        .iter()
        .copied()
        .find(|known| *known == account)
        .ok_or_else(|| {
            UiError::new(
                "unknown_account",
                format!("`{account}` is not a Starkbot key"),
            )
        })
}

fn no_login(provider: &'static OauthProvider) -> UiError {
    UiError::new(
        "no_login",
        format!("no sign-in is in progress for {}", provider.id),
    )
    .with_fix(Fix::SignIn {
        provider: provider.id.to_owned(),
    })
}
