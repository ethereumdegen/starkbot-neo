//! View models: everything the webview is allowed to see, and nothing else.
//!
//! No token, no refresh token, no authorize code, no key value ever appears in
//! this module. What does: provider ids, the authorize URL (public values
//! only), the redirect URI, the already-redacted `ProviderAccount` fields, and
//! states.

use neo_agent::Runtime;
use neo_agent::ax::{ActReport, AxRequest, AxResponse, TrustReport};
use neo_agent::doctor::{Check, DoctorReport, Health};
use neo_agent::oauth::{ANTHROPIC_OAUTH, OPENAI_CODEX, OauthProvider};
use neo_core::{
    HeartbeatTick, InferenceConnection, KeyState, KeyStatus, PROVIDER_ANTHROPIC,
    PROVIDER_ANTHROPIC_OAUTH, PROVIDER_OPENAI, PROVIDER_OPENAI_CODEX, Project, ProviderAccount,
    ProviderAccountStatus, Settings,
};
use neo_eval::CaseListing;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The two subscription paths this screen signs in to (K7), in display order.
pub const SUBSCRIPTIONS: [&OauthProvider; 2] = [&ANTHROPIC_OAUTH, &OPENAI_CODEX];

/// Keychain accounts the screen can fill in, with the label it shows and
/// whether Starkbot is unusable without it (K6: TypeSafe always is).
const KEY_LABELS: [(&str, &str, bool); 3] = [
    ("typesafe", "TypeSafe (Jev)", true),
    ("openai", "OpenAI API key", false),
    ("anthropic", "Anthropic API key", false),
];

#[must_use]
pub fn display_name(provider: &str) -> &'static str {
    match provider {
        PROVIDER_ANTHROPIC_OAUTH => "Claude Pro/Max",
        PROVIDER_OPENAI_CODEX => "ChatGPT Plus/Pro",
        PROVIDER_OPENAI => "OpenAI API key",
        PROVIDER_ANTHROPIC => "Anthropic API key",
        _ => "Unknown runtime",
    }
}

/// A fix the screen itself can perform. The doctor's own `fix` strings are
/// shell commands for `neo`; the desktop app never shows one — it shows the
/// control that does the job.
#[derive(Clone, Debug, Serialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Fix {
    /// Focus the key field for this Keychain account.
    SetKey { account: String },
    /// Start the OAuth login for this subscription provider.
    SignIn { provider: String },
    /// Pick an inference runtime.
    ChooseRuntime,
    /// Nothing in this window can do it; say what to do in plain words.
    Manual { detail: String },
}

#[derive(Clone, Debug, Serialize, TS)]
pub struct ConnectionRow {
    pub provider: String,
    pub display_name: String,
    pub status: ProviderAccountStatus,
    pub email: Option<String>,
    pub plan: Option<String>,
    pub updated_at: i64,
    /// Is this the runtime settings currently point at?
    pub selected: bool,
}

impl ConnectionRow {
    #[must_use]
    pub fn new(account: &ProviderAccount, selected_provider: &str) -> Self {
        let provider = account.provider.as_str().to_owned();
        Self {
            display_name: display_name(&provider).to_owned(),
            selected: provider == selected_provider,
            status: account.status,
            email: account.email.clone(),
            plan: account.plan_type.clone(),
            updated_at: account.updated_at,
            provider,
        }
    }
}

#[derive(Clone, Debug, Serialize, TS)]
pub struct LoginStart {
    pub provider: String,
    /// Public values only: client id, redirect URI, scopes, state, PKCE
    /// challenge. The verifier stays in Rust.
    pub authorize_url: String,
    pub redirect_uri: String,
    pub timeout_secs: u64,
}

#[derive(Clone, Debug, Serialize, TS)]
pub struct LoginFailed {
    pub provider: String,
    pub error: crate::error::UiError,
}

#[derive(Clone, Debug, Serialize, TS)]
pub struct KeyRow {
    pub account: String,
    pub label: String,
    pub state: KeyState,
    /// `keychain` or `environment` — never the value.
    pub source: Option<neo_core::KeySource>,
    pub required: bool,
    /// Can this account's catalogue be refreshed from the vendor?
    pub refreshable: bool,
}

impl KeyRow {
    #[must_use]
    pub fn new(status: &KeyStatus) -> Self {
        let (label, required) = KEY_LABELS
            .iter()
            .find(|(account, _, _)| *account == status.account)
            .map(|(_, label, required)| (*label, *required))
            .unwrap_or((status.account.as_str(), false));
        Self {
            account: status.account.clone(),
            label: label.to_owned(),
            state: status.state,
            source: status.source,
            required,
            refreshable: matches!(status.account.as_str(), "openai" | "anthropic"),
        }
    }
}

#[derive(Clone, Debug, Serialize, TS)]
pub struct RuntimeOption {
    pub provider: String,
    pub display_name: String,
    /// `subscription` (OAuth login) or `api_key`.
    pub kind: &'static str,
    /// Is the credential this runtime needs actually there?
    pub usable: bool,
    pub selected: bool,
}

#[derive(Clone, Debug, Serialize, TS)]
pub struct InferenceView {
    pub provider: String,
    pub model: String,
    pub connection: InferenceConnection,
    /// `true` once `InferenceConnection::detect` finds a usable credential —
    /// the "inference: ok" the first run is aiming at.
    pub ready: bool,
    pub options: Vec<RuntimeOption>,
}

impl InferenceView {
    #[must_use]
    pub fn new(
        settings: &Settings,
        connection: InferenceConnection,
        keys: &[KeyStatus],
        accounts: &[ProviderAccount],
    ) -> Self {
        let selected = settings.models.inference.provider.as_str();
        let options = vec![
            runtime_option(PROVIDER_ANTHROPIC_OAUTH, "subscription", selected, {
                connected(accounts, PROVIDER_ANTHROPIC_OAUTH)
            }),
            runtime_option(
                PROVIDER_OPENAI_CODEX,
                "subscription",
                selected,
                connected(accounts, PROVIDER_OPENAI_CODEX),
            ),
            runtime_option(
                PROVIDER_OPENAI,
                "api_key",
                selected,
                present(keys, "openai"),
            ),
            runtime_option(
                PROVIDER_ANTHROPIC,
                "api_key",
                selected,
                present(keys, "anthropic"),
            ),
        ];
        Self {
            provider: selected.to_owned(),
            model: settings.models.inference.id.clone(),
            connection,
            ready: connection != InferenceConnection::None,
            options,
        }
    }
}

fn runtime_option(
    provider: &str,
    kind: &'static str,
    selected: &str,
    usable: bool,
) -> RuntimeOption {
    RuntimeOption {
        display_name: display_name(provider).to_owned(),
        kind,
        usable,
        selected: provider == selected,
        provider: provider.to_owned(),
    }
}

fn connected(accounts: &[ProviderAccount], provider: &str) -> bool {
    accounts.iter().any(|account| {
        account.provider.as_str() == provider && account.status == ProviderAccountStatus::Connected
    })
}

fn present(keys: &[KeyStatus], account: &str) -> bool {
    keys.iter().any(|key| {
        key.account == account
            && matches!(
                key.state,
                KeyState::Present | KeyState::Limited | KeyState::Unchecked
            )
    })
}

#[derive(Clone, Debug, Serialize, TS)]
pub struct CheckView {
    pub name: String,
    pub health: Health,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fix: Option<Fix>,
}

/// Turn the doctor report into rows the screen can act on: a failing check
/// carries the control that fixes it, not a command to type somewhere else.
#[must_use]
pub fn checks(report: &DoctorReport) -> Vec<CheckView> {
    report
        .checks
        .iter()
        .map(|check| CheckView {
            name: check.name.clone(),
            health: check.health,
            detail: check.detail.clone(),
            fix: fix_for(check),
        })
        .collect()
}

fn fix_for(check: &Check) -> Option<Fix> {
    if check.health == Health::Ok {
        return None;
    }
    match check.name.as_str() {
        "navigator (jev)" => Some(Fix::SetKey {
            account: "typesafe".to_owned(),
        }),
        "speech" => Some(Fix::SetKey {
            account: "openai".to_owned(),
        }),
        "inference" => Some(Fix::ChooseRuntime),
        // Rows whose fix is a fact about the machine, not a control this
        // window owns: installing a browser, starting a keyring, turning
        // the desktop's accessibility switch on.
        "chrome" | "credential storage" | "a11y bus" | "a11y enabled" | "compositor" => {
            check.fix.clone().map(|detail| Fix::Manual { detail })
        }
        name => subscription_provider(name).map(|provider| {
            if SUBSCRIPTIONS.iter().any(|known| known.id == provider) {
                Fix::SignIn {
                    provider: provider.to_owned(),
                }
            } else {
                Fix::Manual {
                    detail: format!(
                        "{} is signed in with its own vendor CLI.",
                        display_name(provider)
                    ),
                }
            }
        }),
    }
}

/// `subscription (anthropic-oauth)` → `anthropic-oauth`.
fn subscription_provider(name: &str) -> Option<&str> {
    name.strip_prefix("subscription (")?.strip_suffix(')')
}

#[derive(Clone, Debug, Serialize, TS)]
pub struct ModelRow {
    pub id: String,
    pub provider: String,
    pub deprecated: bool,
    pub hidden: bool,
}

impl ModelRow {
    #[must_use]
    pub fn new(info: &neo_core::ModelInfo, hidden: bool) -> Self {
        Self {
            id: info.reference.id.clone(),
            provider: info.reference.provider.as_str().to_owned(),
            deprecated: info.deprecated,
            hidden,
        }
    }
}

/// The parts of the first frame that [`neo_agent::Bootstrap`] does not carry:
/// the thread the window opens in, what else it could open, and the work
/// already in flight.
///
/// A struct rather than eight more parameters, because every one of these is
/// read in the same blocking hop and handing them over one by one made the
/// call unreadable.
pub struct Session {
    pub conversation: neo_core::Conversation,
    pub messages: Vec<neo_core::Message>,
    pub conversations: Vec<neo_core::Conversation>,
    pub models: Vec<ModelRow>,
    pub eval_cases: Vec<CaseListing>,
    pub runs: Vec<RunView>,
}

#[derive(Clone, Debug, Serialize, TS)]
pub struct ProjectDetailView {
    pub project: Project,
    pub soul: String,
    pub heartbeat: String,
    pub ticks: Vec<HeartbeatTick>,
}

#[derive(Clone, Debug, Serialize, TS)]
pub struct BootstrapView {
    pub bridge_version: u32,
    pub data_dir: String,
    pub store_path: String,
    pub connections: Vec<ConnectionRow>,
    pub keys: Vec<KeyRow>,
    pub inference: InferenceView,
    pub doctor: Vec<CheckView>,
    /// Every setting, as `patch_settings` takes them back section by section.
    pub settings: SettingsView,
    /// The thread this window is in, and the thread switcher's rows.
    pub conversation: neo_core::ConversationId,
    pub messages: Vec<MessageView>,
    pub conversations: Vec<ConversationView>,
    /// The catalogue of the runtime inference is pointed at.
    pub models: Vec<ModelRow>,
    pub eval_cases: Vec<CaseListingView>,
    /// Runs still going. A webview that reloaded mid-turn needs these, or it
    /// shows a finished screen over work that is still driving an app.
    pub runs: Vec<RunView>,
    pub projects: Vec<Project>,
}

impl BootstrapView {
    #[must_use]
    pub fn new(
        runtime: &Runtime,
        bootstrap: &neo_agent::Bootstrap,
        accounts: &[ProviderAccount],
        session: Session,
    ) -> Self {
        let selected = bootstrap.settings.models.inference.provider.as_str();
        Self {
            bridge_version: bootstrap.bridge_version,
            data_dir: runtime.data_dir().display().to_string(),
            store_path: bootstrap.store.path.display().to_string(),
            connections: accounts
                .iter()
                .map(|account| ConnectionRow::new(account, selected))
                .collect(),
            projects: bootstrap.projects.clone(),
            keys: bootstrap.keys.iter().map(KeyRow::new).collect(),
            inference: InferenceView::new(
                &bootstrap.settings,
                bootstrap.inference,
                &bootstrap.keys,
                accounts,
            ),
            doctor: checks(&bootstrap.doctor),
            settings: SettingsView(bootstrap.settings.clone()),
            conversation: session.conversation.id,
            messages: session.messages.iter().map(MessageView::new).collect(),
            conversations: session
                .conversations
                .iter()
                .map(ConversationView::new)
                .collect(),
            models: session.models,
            eval_cases: session
                .eval_cases
                .iter()
                .map(CaseListingView::new)
                .collect(),
            runs: session.runs,
        }
    }
}

/// Every setting, verbatim.
///
/// Transparent rather than a field-by-field copy: [`Settings`] is the
/// product's own configuration type, it is validated by the store before it
/// is ever handed out, and it holds no credential — keys live in the
/// Keychain and only a [`KeyState`] crosses this boundary. A hand-written
/// mirror would fall behind the twelve sections the moment one gained a
/// field, and a settings pane that cannot see a section cannot edit it.
///
/// A newtype serialises as its inner value, so no `#[serde(transparent)]` is
/// needed to make `settings` the settings object itself on the wire.
#[derive(Clone, Debug, Serialize, TS)]
pub struct SettingsView(#[ts(type = "SettingsRecord")] Settings);

impl SettingsView {
    #[must_use]
    pub fn new(settings: Settings) -> Self {
        Self(settings)
    }
}

/// What kind of work a run is, so a Stop button can say what it stops and a
/// reloaded window can put a run back on the right screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    /// An agent turn from the composer.
    Chat,
    /// A hand-driven browser run.
    Nav,
    /// A hand-driven native-application run.
    App,
    /// The evaluation suite.
    Eval,
}

impl RunKind {
    /// Whether a second run of this kind may start at the same time.
    ///
    /// Only the eval suite says no: `neo_eval` runs its cases at
    /// `concurrency: 1` because they share the keyboard and the frontmost
    /// application, so two suites would drive each other's windows and both
    /// reports would be fiction.
    #[must_use]
    pub const fn exclusive(self) -> bool {
        matches!(self, Self::Eval)
    }
}

#[derive(Clone, Debug, Serialize, TS)]
pub struct RunView {
    pub run: neo_core::RunId,
    pub kind: RunKind,
    pub started_at: neo_core::TimestampMs,
}

#[derive(Clone, Debug, Serialize, TS)]
pub struct ConversationView {
    pub id: neo_core::ConversationId,
    pub title: Option<String>,
    pub created_at: neo_core::TimestampMs,
    pub updated_at: neo_core::TimestampMs,
}

impl ConversationView {
    #[must_use]
    pub fn new(conversation: &neo_core::Conversation) -> Self {
        Self {
            id: conversation.id,
            title: conversation.title.clone(),
            created_at: conversation.created_at,
            updated_at: conversation.updated_at,
        }
    }
}

/// One stored line of a thread.
///
/// `kind` travels alongside `role` because they answer different questions: a
/// `tool`/`result` row is a step card and an `assistant`/`answer` row is
/// prose, and a front end that only had the role would render an action's
/// observation as something the agent said.
#[derive(Clone, Debug, Serialize, TS)]
pub struct MessageView {
    pub id: neo_core::MessageId,
    pub conversation_id: neo_core::ConversationId,
    pub role: neo_core::MessageRole,
    pub kind: neo_core::MessageKind,
    pub text: String,
    pub at: neo_core::TimestampMs,
    /// Per-message metadata the store kept (model, tool name, token counts).
    /// Never a credential: nothing writes one here, and this is persisted.
    #[ts(type = "Json | null")]
    pub meta: Option<serde_json::Value>,
}

impl MessageView {
    #[must_use]
    pub fn new(message: &neo_core::Message) -> Self {
        Self {
            id: message.id,
            conversation_id: message.conversation_id,
            role: message.role,
            kind: message.kind,
            text: message.text.clone(),
            at: message.at,
            meta: message.meta.clone(),
        }
    }
}

/// One eval case before anything runs.
///
/// `installed` is the whole reason this is a view rather than the library's
/// own listing: a case whose app is missing is skipped with a reason, not run
/// and failed, and a front end that showed it as runnable would be promising
/// a measurement it cannot take.
#[derive(Clone, Debug, Serialize, TS)]
pub struct CaseListingView {
    pub id: String,
    pub name: Option<String>,
    pub tags: Vec<String>,
    /// The application the case drives, by the selector `neo app` takes.
    pub app: String,
    pub installed: bool,
    /// Where the bundle was found, when it was.
    pub path: Option<String>,
}

impl CaseListingView {
    #[must_use]
    pub fn new(case: &CaseListing) -> Self {
        Self {
            id: case.id.clone(),
            name: case.name.clone(),
            tags: case.tags.clone(),
            app: case.app.selector().to_owned(),
            installed: case.runnable(),
            path: case
                .installed
                .as_ref()
                .map(|path| path.display().to_string()),
        }
    }
}

/// One accessibility request, as the webview spells it.
///
/// Tagged where [`neo_agent::ax::AxRequest`] is a plain enum, because the
/// webview has no Rust enum representation to match: `{ kind: "press", app,
/// index }` is what a TypeScript union serialises to.
#[derive(Clone, Debug, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AxRequestView {
    Trusted,
    Apps,
    Table {
        app: String,
    },
    Press {
        app: String,
        index: u16,
    },
    Set {
        app: String,
        index: u16,
        text: String,
    },
    Menu {
        app: String,
        path: String,
    },
    Type {
        app: String,
        text: String,
    },
    Key {
        app: String,
        key: String,
    },
}

impl From<AxRequestView> for AxRequest {
    fn from(request: AxRequestView) -> Self {
        match request {
            AxRequestView::Trusted => Self::Trusted,
            AxRequestView::Apps => Self::Apps,
            AxRequestView::Table { app } => Self::Table { app },
            AxRequestView::Press { app, index } => Self::Press { app, index },
            AxRequestView::Set { app, index, text } => Self::Set { app, index, text },
            AxRequestView::Menu { app, path } => Self::Menu { app, path },
            AxRequestView::Type { app, text } => Self::Type { app, text },
            AxRequestView::Key { app, key } => Self::Key { app, key },
        }
    }
}

/// What an accessibility request answered.
///
/// Tagged where [`AxResponse`] is untagged: the CLI's untagged shape exists so
/// scripts reading `neo ax` see bare values, and telling four object shapes
/// apart by their fields is work a renderer should not be doing. The payloads
/// themselves are `neo_ax`'s own types, passed through — a second copy of the
/// element table would drift from the one Jev is shown.
#[derive(Clone, Debug, Serialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AxResponseView {
    Trust(TrustReport),
    Apps {
        #[ts(type = "AxAppView[]")]
        apps: Vec<neo_ax::AppInfo>,
    },
    Acted(ActReport),
    Table {
        #[ts(type = "AxTableView")]
        table: Box<neo_ax::ElementTable>,
    },
}

impl From<AxResponse> for AxResponseView {
    fn from(response: AxResponse) -> Self {
        match response {
            AxResponse::Trust(report) => Self::Trust(report),
            AxResponse::Apps(apps) => Self::Apps { apps },
            AxResponse::Acted(report) => Self::Acted(report),
            AxResponse::Table(table) => Self::Table { table },
        }
    }
}
