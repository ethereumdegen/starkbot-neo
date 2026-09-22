//! The pure reducer (14 §4): `(State, AppEvent) -> State` and
//! `(State, Action) -> (State, Option<Command>)`.
//!
//! Nothing in this module touches the terminal, the clock, the store or the
//! Keychain. It is the unit-test surface for everything the TUI decides.

use std::collections::VecDeque;

use neo_agent::agent::STOPPED;
use neo_agent::ax::AxRequest;
use neo_agent::doctor::{DoctorReport, Health};
use neo_agent::projects::ProjectDocuments;
use neo_agent::runtime::{Bootstrap, StoreInfo};
use neo_core::{
    AppEvent, AskId, AskView, ConfirmId, ConfirmView, ConversationId, GateOutcome,
    InferenceConnection, KeyState, KeyStatus, ListenState, MessageId, MessageKind, ModelRef,
    PROVIDER_ANTHROPIC, PROVIDER_CHATGPT_CODEX, PROVIDER_CLAUDE_SUBSCRIPTION, PROVIDER_OPENAI,
    Project, ProviderAccount, ProviderAccountStatus, ProviderId, ReasoningEffort, ResolutionVia,
    RunId, Settings, TaskId, TimestampMs, TurnUsage, Usd,
};
use neo_eval::Selection;
use serde_json::{Value, json};

use crate::keys::Action;
use crate::runs::{RUNS_CAP, Run, RunKind, RunState, TraceKind, eval_line, nav_trace};
// The renderer's report of the frame it just drew. The only thing this
// module takes from `ui`, and it is data, not rendering.
use crate::ui::Painted;

/// Shown on a first run without replacing the conversation.
const SETUP_HINT: &str = "setup needed — /login opens connections and keys";

/// How many `AppEvent` summaries the Activity ring keeps.
pub const ACTIVITY_CAP: usize = 200;

/// The Keychain account that backs speech (K6: speech is OpenAI-API-key only).
/// Account names are the core's, taken from the `key_status` list the core
/// reports; `neo-tui` never links `neo-keys` (14 §1).
const SPEECH_ACCOUNT: &str = PROVIDER_OPENAI;
/// Jev's key: required on every path, so setup is not finished without it.
const TYPESAFE_ACCOUNT: &str = "typesafe";

/// What the composer says when it is ready to send. The newline keys are the
/// ones the keymap actually binds: `Shift-Enter` is not reported by most
/// terminals, so advertising it was a lie a user could only discover by
/// losing a draft.
const COMPOSER_HINT: &str = "Enter sends · Alt-Enter or Ctrl-J newline · Esc leaves insert";
/// What it says while a turn is running. The composer is never locked — a
/// second message steers the turn that is already going rather than being
/// refused, which is the whole point of the mailbox. `Esc twice` because
/// the first one leaves insert: the help overlay spells that out.
const COMPOSER_STEERS: &str = "Enter steers the running turn · Esc twice stops it";
/// What it says while no runtime can answer.
const COMPOSER_BLOCKED: &str = "no inference connection — type /login to connect";

/// What the status line says when a key would have resolved a card that is
/// not armed yet. Not silence: a keystroke that did nothing and said nothing
/// reads as a broken binding, and the user presses it again harder.
const CARD_UNREAD: &str = "give the card a moment — y and n go live once it has been on screen";

/// Full-screen surfaces reached from chat slash commands.
///
/// Conversation is the home surface. The others replace it until Esc or q
/// returns to chat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    Conversation,
    Runs,
    Mind,
    Projects,
}

impl Pane {
    pub const fn index(self) -> usize {
        match self {
            Self::Conversation => 0,
            Self::Runs => 1,
            Self::Mind => 2,
            Self::Projects => 3,
        }
    }
}

/// Input mode (14 §3). `Card` is what a raised confirm or ask puts the front
/// end into: the card owns the keyboard while it is up, and the mode word on
/// the status line is how a user reads that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    Card,
    Command,
}

impl Mode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::Insert => "INSERT",
            Self::Card => "CARD",
            Self::Command => "COMMAND",
        }
    }
}

/// What occupies the body of the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Panes,
    Settings,
}

/// The Settings sections. One per [`Settings`] section plus the two the core
/// reports rather than stores — Connections and Doctor — so every field the
/// core will accept a patch for has a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Connections,
    Models,
    Identity,
    Listen,
    Voice,
    Intake,
    Safety,
    Caps,
    Queue,
    Browser,
    Hotkeys,
    General,
    Privacy,
    Doctor,
}

impl Section {
    pub const ALL: [Self; 14] = [
        Self::Connections,
        Self::Models,
        Self::Identity,
        Self::Listen,
        Self::Voice,
        Self::Intake,
        Self::Safety,
        Self::Caps,
        Self::Queue,
        Self::Browser,
        Self::Hotkeys,
        Self::General,
        Self::Privacy,
        Self::Doctor,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            Self::Connections => "Connections & keys",
            Self::Models => "Models",
            Self::Identity => "Identity",
            Self::Listen => "Listening",
            Self::Voice => "Voice",
            Self::Intake => "Intake",
            Self::Safety => "Safety",
            Self::Caps => "Caps",
            Self::Queue => "Queue",
            Self::Browser => "Browser",
            Self::Hotkeys => "Hotkeys",
            Self::General => "General",
            Self::Privacy => "Privacy",
            Self::Doctor => "Doctor",
        }
    }

    /// The `patch_settings` section this maps onto, for the sections that are
    /// stored. Connections and Doctor are reported, not stored.
    pub const fn key(self) -> Option<&'static str> {
        match self {
            Self::Connections | Self::Doctor => None,
            Self::Models => Some("models"),
            Self::Identity => Some("identity"),
            Self::Listen => Some("listen"),
            Self::Voice => Some("voice"),
            Self::Intake => Some("intake"),
            Self::Safety => Some("safety"),
            Self::Caps => Some("caps"),
            Self::Queue => Some("queue"),
            Self::Browser => Some("browser"),
            Self::Hotkeys => Some("hotkeys"),
            Self::General => Some("general"),
            Self::Privacy => Some("privacy"),
        }
    }

    /// The word `/settings <name>` jumps by.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Connections => "connections",
            Self::Models => "models",
            Self::Identity => "identity",
            Self::Listen => "listen",
            Self::Voice => "voice",
            Self::Intake => "intake",
            Self::Safety => "safety",
            Self::Caps => "caps",
            Self::Queue => "queue",
            Self::Browser => "browser",
            Self::Hotkeys => "hotkeys",
            Self::General => "general",
            Self::Privacy => "privacy",
            Self::Doctor => "doctor",
        }
    }
}

/// What kind of value a settings field holds, with its current value.
///
/// One enum rather than a widget per section: every stored setting is a bool,
/// a number, a string, a closed set of words or a list of strings, and a row
/// that knows which of those it is can be edited without a bespoke screen.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldKind {
    Bool(bool),
    Text(String),
    /// A string field the core accepts `null` for; an empty edit clears it.
    OptionalText(Option<String>),
    Int(i64),
    Float(f64),
    /// A closed set, cycled by `Enter`. The strings are the wire values the
    /// core's `serde(rename_all = "snake_case")` expects.
    Choice {
        options: &'static [&'static str],
        current: usize,
    },
    /// Comma-separated strings.
    List(Vec<String>),
}

impl FieldKind {
    /// What the row shows.
    fn display(&self) -> String {
        match self {
            Self::Bool(value) => bool_label(*value).to_owned(),
            Self::Text(value) => value.clone(),
            Self::OptionalText(value) => value.clone().unwrap_or_else(|| "system default".into()),
            Self::Int(value) => value.to_string(),
            Self::Float(value) => format!("{value:.2}"),
            Self::Choice { options, current } => {
                (*options.get(*current).unwrap_or(&"?")).to_owned()
            }
            Self::List(values) => {
                if values.is_empty() {
                    "none".to_owned()
                } else {
                    values.join(", ")
                }
            }
        }
    }

    /// What a prompt pre-fills with, for the kinds that are typed.
    fn editable(&self) -> Option<String> {
        match self {
            Self::Bool(_) | Self::Choice { .. } => None,
            Self::Text(value) => Some(value.clone()),
            Self::OptionalText(value) => Some(value.clone().unwrap_or_default()),
            Self::Int(value) => Some(value.to_string()),
            Self::Float(value) => Some(short_float(*value)),
            Self::List(values) => Some(values.join(", ")),
        }
    }

    /// The JSON one edited string becomes, or the reason it cannot.
    fn parse(&self, raw: &str) -> Result<Value, &'static str> {
        let trimmed = raw.trim();
        match self {
            Self::Bool(_) | Self::Choice { .. } => Err("this row toggles with Enter"),
            Self::Text(_) => {
                if trimmed.is_empty() {
                    Err("this field cannot be empty")
                } else {
                    Ok(json!(trimmed))
                }
            }
            Self::OptionalText(_) => Ok(if trimmed.is_empty() {
                Value::Null
            } else {
                json!(trimmed)
            }),
            Self::Int(_) => trimmed
                .parse::<i64>()
                .map(|value| json!(value))
                .map_err(|_| "a whole number, e.g. 30"),
            Self::Float(_) => trimmed
                .parse::<f64>()
                .map(|value| json!(value))
                .map_err(|_| "a number, e.g. 0.40"),
            Self::List(_) => Ok(json!(
                trimmed
                    .split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
            )),
        }
    }

    /// The next value of a row `Enter` flips or cycles.
    fn cycled(&self) -> Option<Value> {
        match self {
            Self::Bool(value) => Some(json!(!value)),
            Self::Choice { options, current } => {
                let next = (current + 1) % options.len().max(1);
                options.get(next).map(|value| json!(value))
            }
            _ => None,
        }
    }
}

/// What pressing `Enter` (and `s` / `x` on key rows) does to a Settings row.
#[derive(Clone, Debug, PartialEq)]
pub enum RowAction {
    /// A heading or a read-only fact.
    Inert,
    /// One of the four K6 inference paths. `Enter` selects it by writing the
    /// provider into `models.inference`, which is exactly what
    /// `InferenceConnection::detect` reads. `account` is the Keychain account
    /// backing the path, if it is a key path: `s` sets it, `x` removes it.
    Path {
        provider: &'static str,
        account: Option<String>,
        subscription: bool,
    },
    /// A Keychain-backed account that is not an inference path (TypeSafe/Jev).
    Key(String),
    CycleEffort(ReasoningEffort),
    EditModel(&'static str),
    /// Any other stored setting. `pointer` is dotted within the section, so
    /// `safety` + `confirm_at.outward` patches `{"confirm_at":{"outward":…}}`
    /// — a merge patch that leaves every sibling field alone.
    Field {
        section: &'static str,
        pointer: &'static str,
        kind: FieldKind,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub section: Section,
    pub heading: bool,
    pub label: String,
    pub value: String,
    pub action: RowAction,
}

impl Row {
    fn heading(section: Section) -> Self {
        Self {
            section,
            heading: true,
            label: section.title().to_owned(),
            value: String::new(),
            action: RowAction::Inert,
        }
    }

    fn fact(section: Section, label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            section,
            heading: false,
            label: label.into(),
            value: value.into(),
            action: RowAction::Inert,
        }
    }

    /// One editable settings field.
    fn field(
        section: Section,
        pointer: &'static str,
        label: impl Into<String>,
        kind: FieldKind,
    ) -> Self {
        let Some(key) = section.key() else {
            return Self::fact(section, label, kind.display());
        };
        Self {
            section,
            heading: false,
            label: label.into(),
            value: kind.display(),
            action: RowAction::Field {
                section: key,
                pointer,
                kind,
            },
        }
    }

    fn with(mut self, action: RowAction) -> Self {
        self.action = action;
        self
    }
}

/// A subscription login in progress (K7): what to show while the user is in
/// their browser.
///
/// Every field here is a public value — the authorize URL, the redirect URI,
/// the provider. The authorization code and the tokens never reach the front
/// end at all: the core exchanges them and hands back a redacted
/// [`ProviderAccount`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Login {
    pub provider: &'static str,
    /// The plan the user recognises, not the provider id.
    pub title: &'static str,
    pub url: String,
    pub redirect_uri: String,
    pub phase: LoginPhase,
    /// What the user has typed into the paste fallback. A redirect URL is not
    /// a secret, so unlike [`Prompt`] this is shown.
    pub paste: String,
    /// Seconds left before the core stops waiting, recomputed by the loop.
    pub remaining: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginPhase {
    /// The browser is open and the loopback listener is waiting.
    Waiting,
    /// The user chose to paste the redirect URL instead.
    Pasting,
    /// A code arrived and is being exchanged.
    Exchanging,
    Done,
    Failed(String),
}

impl Login {
    pub const fn settled(&self) -> bool {
        matches!(self.phase, LoginPhase::Done | LoginPhase::Failed(_))
    }
}

/// A one-line overlay that collects text. The key variant is masked and its
/// buffer is the only secret-shaped string the TUI ever holds; it is dropped the
/// moment the prompt closes and it is never rendered (14 §5).
#[derive(Clone, Debug, PartialEq)]
pub struct Prompt {
    pub kind: PromptKind,
    pub label: String,
    pub hint: &'static str,
    pub masked: bool,
    buffer: String,
}

impl Prompt {
    pub fn len(&self) -> usize {
        self.buffer.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// The characters a renderer may draw. A masked prompt yields nothing: the
    /// UI draws one bullet per `len()` instead.
    pub fn visible(&self) -> &str {
        if self.masked { "" } else { &self.buffer }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PromptKind {
    SetKey {
        account: String,
    },
    ModelRef {
        field: &'static str,
    },
    /// A typed settings field: the parsed value is merged into `section`.
    Field {
        section: &'static str,
        pointer: &'static str,
        kind: FieldKind,
    },
    /// Change a project's heartbeat interval without leaving the TUI.
    ProjectInterval {
        slug: String,
        enabled: bool,
        on_gate: neo_core::HeartbeatGate,
    },
    /// Rename the open conversation.
    Rename,
    /// The typed answer to a free-text question (16 §5.5). The same one-line
    /// overlay a model id uses, unmasked: a question with no options has to
    /// be answered in words, and a card cannot collect them — while a card is
    /// up it owns the keyboard, so the prompt is what lets the user type at
    /// all.
    Ask {
        ask: AskId,
    },
}

/// One row of the session picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRow {
    pub id: ConversationId,
    pub title: String,
    /// When it was last touched, already rendered: the reducer owns no clock.
    pub when: String,
    pub active: bool,
}

/// The conversation switcher overlay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sessions {
    pub rows: Vec<SessionRow>,
    pub row: usize,
}

/// One line of the conversation as the TUI holds it.
///
/// Stored-message shaped rather than model-shaped: the id keys the row across
/// a reload, `kind` tells a tool result from an answer, and `at` groups by
/// time. The model-facing history is derived from this by [`State::history`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadRow {
    pub id: MessageId,
    pub role: Role,
    pub kind: MessageKind,
    pub at: TimestampMs,
    pub text: String,
    /// This message was typed into a turn that was already running, so it
    /// reached the model mid-flight rather than starting a turn of its own.
    /// Rendered differently because "I said that while it was working" is
    /// the only way to read a transcript where the answer above the message
    /// already accounts for it.
    pub steered: bool,
}

impl ThreadRow {
    /// A stored message as a thread row, or `None` for the roles a front end
    /// does not render — a stored `system` message is configuration.
    #[must_use]
    pub fn of(message: &neo_core::Message) -> Option<Self> {
        let role = match message.role {
            neo_core::MessageRole::User => Role::User,
            neo_core::MessageRole::Assistant => Role::Assistant,
            neo_core::MessageRole::Tool => Role::Tool,
            neo_core::MessageRole::System => return None,
        };
        Some(Self {
            id: message.id,
            role,
            kind: message.kind,
            at: message.at,
            text: message.text.clone(),
            steered: false,
        })
    }
}

/// Work only the core can do. `run.rs` executes these against `Runtime`.
#[derive(Clone, PartialEq)]
pub enum Command {
    /// Run one agent turn for this message (P2′): the model chooses actions,
    /// the core drives the browser or an application, the thread records what
    /// happened.
    Send {
        text: String,
    },
    /// Hand a message to a turn that is already running (`Runtime::steer`).
    ///
    /// Separate from [`Command::Send`] because the two cannot be decided
    /// here: only the core knows whether that run is still reading its
    /// mailbox. A refused steer becomes a [`Command::Send`] in the loop, so
    /// what the user typed is never dropped on the floor.
    Steer {
        run: RunId,
        text: String,
    },
    /// Drive a web page to a goal (`neo nav`).
    Nav {
        options: NavSpec,
    },
    /// Drive a native application to a goal (`neo nav app`).
    AppGoal {
        app: String,
        goal: String,
    },
    /// One direct accessibility call (`neo ax …`).
    Ax {
        request: AxRequest,
    },
    /// Run the evaluation suite (`neo eval`). One at a time, always: the cases
    /// share the keyboard and the frontmost application.
    Eval {
        selection: Selection,
    },
    /// What `neo eval --list` prints, without spending a token.
    EvalList,
    /// Re-run the readiness checks rather than re-rendering the cached ones.
    Doctor,
    /// Cancel one run's token.
    StopRun {
        run: RunId,
    },
    /// The kill switch: cancel every run this front end has live.
    StopAll,
    /// Begin recording an utterance.
    StartDictation,
    /// Stop recording and transcribe it into the composer.
    StopDictation,
    NewConversation {
        title: Option<String>,
    },
    /// Fill the session picker.
    ListConversations,
    SwitchConversation {
        conversation: ConversationId,
    },
    RenameConversation {
        title: String,
    },
    OpenProject {
        slug: String,
    },
    RunProjectHeartbeat {
        slug: String,
    },
    ToggleProjectHeartbeat {
        slug: String,
        enabled: bool,
        every_seconds: u64,
        on_gate: neo_core::HeartbeatGate,
    },
    EditProjectDocument {
        slug: String,
        document: &'static str,
    },
    PatchSettings {
        section: &'static str,
        patch: Value,
    },
    SetKey {
        account: String,
        raw: String,
    },
    RemoveKey {
        account: String,
    },
    /// Hand the terminal to a vendor's own login flow, then come back.
    ///
    /// The front end runs no arbitrary process: this names one of the two
    /// subscription paths and the core launches that vendor's pinned helper
    /// (K6, A22). P3 still holds — there is no shell here.
    ConnectSubscription {
        provider: &'static str,
    },
    DisconnectSubscription {
        provider: &'static str,
    },
    /// Start a subscription OAuth login (K7): the core mints PKCE material,
    /// opens the vendor page and serves the loopback callback. The front end
    /// only ever sees the authorize URL and, at the end, a redacted account.
    BeginLogin {
        provider: &'static str,
    },
    /// Open the vendor page again, for a user whose browser did not come up.
    OpenLoginPage,
    /// Hand the core what the user pasted from their browser's address bar.
    FinishLoginPasted {
        pasted: String,
    },
    CancelLogin,
    /// Re-read the model catalogue for one provider from the vendor.
    RefreshModels {
        account: String,
    },
    CheckKey {
        account: String,
    },
    /// Answer a confirm gate (`Runtime::resolve_confirm`). `via` is decided
    /// here rather than in the loop: the front end knows which of its own
    /// surfaces the answer came from, and the decision trail records it.
    ResolveConfirm {
        confirm: ConfirmId,
        outcome: GateOutcome,
        via: ResolutionVia,
    },
    /// Answer a question (`Runtime::answer_ask`).
    AnswerAsk {
        ask: AskId,
        answer: String,
        via: ResolutionVia,
    },
    /// Repaint the whole screen (`Ctrl-L`).
    ///
    /// Marking the state dirty would only re-diff against a back buffer
    /// that already agrees with what ratatui believes is on the display —
    /// which repairs nothing, and the case this key exists for is exactly
    /// the one where they disagree, because something else wrote over the
    /// terminal. Only the loop holds the terminal, so it is a command.
    Redraw,
    ReBootstrap,
}

/// Everything `/nav` can be asked for, in the front end's own words.
///
/// Not [`neo_agent::agent::BrowserOptions`]: that carries a confirm threshold
/// the reducer has no business inventing. The loop resolves it from settings,
/// exactly as `neo nav` does.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct NavSpec {
    pub url: String,
    pub goal: String,
    /// Run without a visible window. Headed is the default (Q1.1): a browser
    /// the user cannot see doing their banking is not the product.
    pub headless: bool,
    pub profile: Option<String>,
    /// Ask the Jev safety heads. Off means *no* head is asked, so nothing can
    /// trip the confirm gate: fixtures only.
    pub safety_heads: bool,
}

impl std::fmt::Debug for Command {
    /// Hand-written so a `{:?}` of a command can never print key material.
    #[allow(clippy::too_many_lines)]
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Send { text } => formatter
                .debug_struct("Send")
                .field("chars", &text.chars().count())
                .finish(),
            Self::Steer { run, text } => formatter
                .debug_struct("Steer")
                .field("run", run)
                .field("chars", &text.chars().count())
                .finish(),
            Self::Nav { options } => formatter
                .debug_struct("Nav")
                .field("options", options)
                .finish(),
            Self::AppGoal { app, goal } => formatter
                .debug_struct("AppGoal")
                .field("app", app)
                .field("goal", goal)
                .finish(),
            // An `ax set`/`ax type` request carries whatever the user is
            // putting into a field, which may be a one-time code. The shape
            // is printed; the text is counted.
            Self::Ax { request } => formatter
                .debug_struct("Ax")
                .field("request", &ax_debug(request))
                .finish(),
            Self::Eval { selection } => formatter
                .debug_struct("Eval")
                .field("selection", selection)
                .finish(),
            Self::EvalList => formatter.write_str("EvalList"),
            Self::Doctor => formatter.write_str("Doctor"),
            Self::StopRun { run } => formatter.debug_struct("StopRun").field("run", run).finish(),
            Self::StopAll => formatter.write_str("StopAll"),
            Self::StartDictation => formatter.write_str("StartDictation"),
            Self::StopDictation => formatter.write_str("StopDictation"),
            Self::NewConversation { title } => formatter
                .debug_struct("NewConversation")
                .field("title", title)
                .finish(),
            Self::ListConversations => formatter.write_str("ListConversations"),
            Self::SwitchConversation { conversation } => formatter
                .debug_struct("SwitchConversation")
                .field("conversation", conversation)
                .finish(),
            Self::RenameConversation { title } => formatter
                .debug_struct("RenameConversation")
                .field("title", title)
                .finish(),
            Self::OpenProject { slug } => formatter
                .debug_struct("OpenProject")
                .field("slug", slug)
                .finish(),
            Self::RunProjectHeartbeat { slug } => formatter
                .debug_struct("RunProjectHeartbeat")
                .field("slug", slug)
                .finish(),
            Self::ToggleProjectHeartbeat { slug, enabled, .. } => formatter
                .debug_struct("ToggleProjectHeartbeat")
                .field("slug", slug)
                .field("enabled", enabled)
                .finish(),
            Self::EditProjectDocument { slug, document } => formatter
                .debug_struct("EditProjectDocument")
                .field("slug", slug)
                .field("document", document)
                .finish(),
            Self::PatchSettings { section, patch } => formatter
                .debug_struct("PatchSettings")
                .field("section", section)
                .field("patch", patch)
                .finish(),
            Self::SetKey { account, .. } => formatter
                .debug_struct("SetKey")
                .field("account", account)
                .field("raw", &"••••")
                .finish(),
            Self::RemoveKey { account } => formatter
                .debug_struct("RemoveKey")
                .field("account", account)
                .finish(),
            Self::ConnectSubscription { provider } => formatter
                .debug_struct("ConnectSubscription")
                .field("provider", provider)
                .finish(),
            Self::DisconnectSubscription { provider } => formatter
                .debug_struct("DisconnectSubscription")
                .field("provider", provider)
                .finish(),
            Self::BeginLogin { provider } => formatter
                .debug_struct("BeginLogin")
                .field("provider", provider)
                .finish(),
            Self::OpenLoginPage => formatter.write_str("OpenLoginPage"),
            // The pasted value is a redirect URL carrying a single-use
            // authorization code. It is not a stored credential, but it is not
            // ours to print either.
            Self::FinishLoginPasted { .. } => formatter
                .debug_struct("FinishLoginPasted")
                .field("pasted", &"••••")
                .finish(),
            Self::CancelLogin => formatter.write_str("CancelLogin"),
            Self::RefreshModels { account } => formatter
                .debug_struct("RefreshModels")
                .field("account", account)
                .finish(),
            Self::CheckKey { account } => formatter
                .debug_struct("CheckKey")
                .field("account", account)
                .finish(),
            Self::ResolveConfirm {
                confirm,
                outcome,
                via,
            } => formatter
                .debug_struct("ResolveConfirm")
                .field("confirm", confirm)
                .field("outcome", outcome)
                .field("via", via)
                .finish(),
            // A typed answer is whatever the page asked for, which may be a
            // one-time code or an invoice number. The shape is printed; the
            // text is counted, exactly as `Ax` does.
            Self::AnswerAsk { ask, answer, via } => formatter
                .debug_struct("AnswerAsk")
                .field("ask", ask)
                .field("chars", &answer.chars().count())
                .field("via", via)
                .finish(),
            Self::ReBootstrap => formatter.write_str("ReBootstrap"),
            Self::Redraw => formatter.write_str("Redraw"),
        }
    }
}

/// An accessibility request with the typed text counted rather than copied.
fn ax_debug(request: &AxRequest) -> String {
    match request {
        AxRequest::Trusted => "trusted".to_owned(),
        AxRequest::Apps => "apps".to_owned(),
        AxRequest::Table { app } => format!("table {app}"),
        AxRequest::Press { app, index } => format!("press {app} [{index}]"),
        AxRequest::Set { app, index, text } => {
            format!("set {app} [{index}] {} chars", text.chars().count())
        }
        AxRequest::Menu { app, path } => format!("menu {app} {path}"),
        AxRequest::Type { app, text } => format!("type {app} {} chars", text.chars().count()),
        AxRequest::Key { app, key } => format!("key {app} {key}"),
    }
}

/// One line of the Activity ring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Activity {
    pub kind: &'static str,
    pub detail: String,
}

/// How long a card's sentence has to have been on screen before `y`/`n` are
/// live (04 §13, 14 §3). A gate the user has not had time to read is not a
/// gate; the debounce is what makes the keystroke a decision.
pub const CARD_ARM_MS: u64 = 600;

/// A pending confirm or ask: the one thing on screen a paused run is waiting
/// for (04 §13, 14 §3, 16 §5.5).
///
/// Raised by [`AppEvent::ConfirmRequest`]/[`AppEvent::AskRequest`] and cleared
/// by the matching resolution — including one this front end did not make,
/// because a timeout or another surface's answer settles the same gate.
/// `armed` and `rendered` are the two halves of the safety rule: nothing
/// resolves a card whose sentence has not reached a frame and stayed there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    /// Which gate this is, with the id its resolution needs.
    pub kind: CardKind,
    /// What is about to happen, as one complete sentence in the product's
    /// voice. Never abbreviated on screen: half a sentence about money is
    /// worse than no sentence.
    pub sentence: String,
    /// Where it is about to happen — "<page title> — <url>".
    pub context: Option<String>,
    /// Why the gate tripped, as the machine tagged it: `safety:spends`,
    /// `label:pay`, `upload:forms.test`.
    pub cause: Option<String>,
    /// What the action is expected to cost, already in words: the reducer
    /// owns no pricing table, exactly as it owns no clock.
    pub cost: Option<String>,
    /// Whether an approval could be remembered. False for every card in Q2 —
    /// approvals are single-shot — so the affordance is not offered.
    pub can_remember: bool,
    /// Set once the debounce has elapsed with the sentence on screen.
    pub armed: bool,
    /// Set by the renderer after the card's sentence actually reached the
    /// frame, and cleared when it stops reaching it.
    pub rendered: bool,
    /// When the sentence first reached a frame, on the reducer's clock.
    shown_ms: Option<u64>,
}

/// The two gates, each carrying what its answer has to be addressed to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardKind {
    /// Yes or no to one action.
    Confirm { id: ConfirmId, task: TaskId },
    /// One question. `options` empty means free text, which is typed into the
    /// prompt overlay rather than chosen.
    Ask {
        id: AskId,
        task: TaskId,
        options: Vec<String>,
        /// Which option is highlighted, for the numbered list.
        selected: usize,
    },
}

impl Card {
    /// A confirm card from the core's view of the gate.
    #[must_use]
    pub fn confirm(view: &ConfirmView) -> Self {
        Self {
            kind: CardKind::Confirm {
                id: view.id,
                task: view.task_id,
            },
            sentence: view.action_sentence.clone(),
            context: view.context.clone(),
            cause: Some(view.cause.clone()),
            cost: view
                .estimated_cost
                .as_ref()
                .and_then(|usage| cost_words(usage.usd)),
            can_remember: view.can_remember,
            armed: false,
            rendered: false,
            shown_ms: None,
        }
    }

    /// An ask card. The question is the sentence: there is nothing else to
    /// say about a question, and the options carry the rest.
    #[must_use]
    pub fn ask(view: &AskView) -> Self {
        Self {
            kind: CardKind::Ask {
                id: view.id,
                task: view.task_id,
                options: view.options.clone(),
                selected: 0,
            },
            sentence: view.question.clone(),
            context: None,
            cause: None,
            cost: None,
            can_remember: false,
            armed: false,
            rendered: false,
            shown_ms: None,
        }
    }

    /// The answer a keystroke would give, which is what `y` takes: the
    /// highlighted option, or nothing for a free-text question.
    #[must_use]
    pub fn highlighted(&self) -> Option<&str> {
        match &self.kind {
            CardKind::Confirm { .. } => None,
            CardKind::Ask {
                options, selected, ..
            } => options.get(*selected).map(String::as_str),
        }
    }

    /// A question with no options, which has to be typed rather than chosen.
    #[must_use]
    pub fn free_text(&self) -> bool {
        matches!(&self.kind, CardKind::Ask { options, .. } if options.is_empty())
    }

    /// Whether a keystroke may resolve this card: its sentence is on the
    /// frame and has been there long enough to have been read.
    #[must_use]
    pub const fn live(&self) -> bool {
        self.armed && self.rendered
    }

    fn is_confirm(&self, id: ConfirmId) -> bool {
        matches!(self.kind, CardKind::Confirm { id: mine, .. } if mine == id)
    }

    fn is_ask(&self, id: AskId) -> bool {
        matches!(self.kind, CardKind::Ask { id: mine, .. } if mine == id)
    }

    /// The numbered answers. Empty for a confirm and for a free-text
    /// question, which is exactly what "there is nothing to pick" means.
    #[must_use]
    pub fn options(&self) -> &[String] {
        match &self.kind {
            CardKind::Confirm { .. } => &[],
            CardKind::Ask { options, .. } => options,
        }
    }

    /// Which numbered answer is highlighted.
    #[must_use]
    pub const fn selected(&self) -> usize {
        match &self.kind {
            CardKind::Confirm { .. } => 0,
            CardKind::Ask { selected, .. } => *selected,
        }
    }

    /// Highlight one answer by index, and say whether there was one there.
    fn select(&mut self, index: usize) -> bool {
        let CardKind::Ask {
            options, selected, ..
        } = &mut self.kind
        else {
            return false;
        };
        if index >= options.len() {
            return false;
        }
        *selected = index;
        true
    }

    /// Move the highlight, clamped: a list of two answers does not wrap
    /// round to the dangerous one because a key was held down.
    fn move_selection(&mut self, delta: i32) {
        let CardKind::Ask {
            options, selected, ..
        } = &mut self.kind
        else {
            return;
        };
        if options.is_empty() {
            return;
        }
        let last = i64::try_from(options.len() - 1).unwrap_or(0);
        let next = (i64::try_from(*selected).unwrap_or(0) + i64::from(delta)).clamp(0, last);
        *selected = usize::try_from(next).unwrap_or(0);
    }

    /// Two cards for the same gate. The id, not the sentence: the core may
    /// republish a pending gate with reworded context after a re-bootstrap.
    fn same_gate(&self, other: &Self) -> bool {
        match other.kind {
            CardKind::Confirm { id, .. } => self.is_confirm(id),
            CardKind::Ask { id, .. } => self.is_ask(id),
        }
    }

    /// The renderer's report, folded in: the sentence reached a frame, or it
    /// stopped reaching one. Says whether anything changed, so a frame is
    /// only owed when it did.
    fn painted(&mut self, on_screen: bool, now_ms: u64) -> bool {
        if self.rendered == on_screen {
            return false;
        }
        self.rendered = on_screen;
        if on_screen {
            self.shown_ms = Some(now_ms);
        } else {
            // A card a resize pushed off the frame is unarmed again: the
            // debounce is about *this* sighting of the sentence.
            self.shown_ms = None;
            self.armed = false;
        }
        true
    }

    /// Arm the card once the debounce has elapsed on screen.
    fn arm(&mut self, now_ms: u64) -> bool {
        if self.armed || !self.rendered {
            return false;
        }
        let Some(shown) = self.shown_ms else {
            return false;
        };
        if now_ms.saturating_sub(shown) < CARD_ARM_MS {
            return false;
        }
        self.armed = true;
        true
    }
}

/// What a confirm says the action will cost, or nothing when the core could
/// not price it. Plan-backed work is [`Usd::Unpriced`] (05 §7), and inventing
/// a figure for it would be a number nobody can reconcile with a bill.
fn cost_words(usd: Usd) -> Option<String> {
    match usd {
        Usd::Exact(amount) => Some(format!("${amount:.2}")),
        Usd::Estimated(amount) => Some(format!("about ${amount:.2}")),
        Usd::Unpriced => None,
    }
}

pub use neo_agent::agent::{ChatMessage, Role};

/// One tool call as the conversation shows it.
///
/// Opened by [`AppEvent::TurnStep`] the moment the model chooses an action,
/// kept current by the lines that step produces, closed by
/// [`AppEvent::TurnStepDone`]. The card keeps its place in the thread while
/// its state moves underneath, which is the difference between watching an
/// agent work and watching a spinner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepCard {
    /// The step index this card is for, counted from zero within the run.
    pub step: u32,
    /// The action in one line, in the model's own terms.
    pub intent: String,
    /// The reason the model gave for choosing it, when it gave one.
    pub thought: Option<String>,
    /// The newest line from inside the step — a navigator decision, a note.
    pub detail: Option<String>,
    /// Running while the action is in flight, then how it ended. This is
    /// [`RunState`] rather than a second enum so a card borrows the runs
    /// pane's glyph, word and colour instead of inventing a vocabulary for
    /// the same five states.
    pub state: RunState,
    /// How long the action took, once it is done.
    pub duration_ms: Option<u64>,
}

impl StepCard {
    /// Close the card on the observation the step fed back to the model.
    pub fn close(&mut self, observation: &str, duration_ms: u64) {
        self.detail = Some(observation.to_owned());
        self.duration_ms = Some(duration_ms);
        self.state = if failed_observation(observation) {
            RunState::Failed(observation.to_owned())
        } else {
            RunState::Done
        };
    }
}

/// Whether the sentence an action fed back to the model is a failure.
///
/// A failed action does not end the turn — the agent reports it as an
/// observation and lets the model try something else (`agent/mod.rs`, which
/// writes `that failed: {error}`). The card has to read the outcome out of
/// that sentence, because there is no other signal: rendering every
/// observation as a success paints half of them the wrong colour.
fn failed_observation(observation: &str) -> bool {
    observation.trim_start().starts_with("that failed")
}

/// A turn in flight: the cards for the actions it has taken and the answer
/// as the model writes it.
///
/// Reduced entirely from this run's events, so a turn started by this front
/// end and a turn started by anything else in the process would render the
/// same way — the only reason the TUI shows one and not the other is that it
/// only registers what it started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnProgress {
    /// Which run's events fill this.
    pub run: RunId,
    /// One card per action, oldest first; the last one is the live one.
    pub cards: Vec<StepCard>,
    /// The answer so far, assembled from [`AppEvent::TurnDelta`].
    pub answer: String,
    /// The next delta sequence number expected. Slices are appended in
    /// `seq` order and never reordered on screen.
    pub next_seq: u32,
    /// Slices that arrived early, smallest `seq` first. The broadcast
    /// preserves order today; holding the out-of-order ones costs nothing
    /// and means a reordering transport cannot scramble a sentence.
    pub pending: Vec<(u32, String)>,
    /// What the vendor charged, once it has said. No event carries usage
    /// mid-turn, so this is `None` for most of a turn's life.
    pub usage: Option<TurnUsage>,
    /// Actions completed so far this turn.
    pub done: usize,
}

impl TurnProgress {
    fn new(run: RunId) -> Self {
        Self {
            run,
            cards: Vec::new(),
            answer: String::new(),
            next_seq: 0,
            pending: Vec::new(),
            usage: None,
            done: 0,
        }
    }

    /// Append one slice of the answer, in `seq` order.
    ///
    /// A slice from the future is held rather than appended: writing it
    /// straight out would interleave two halves of a word, and a front end
    /// that reorders a sentence after the user has read it is worse than one
    /// that waits a frame.
    fn delta(&mut self, seq: u32, text: &str) {
        if seq < self.next_seq {
            return;
        }
        if seq > self.next_seq {
            if let Err(at) = self.pending.binary_search_by_key(&seq, |(seq, _)| *seq) {
                self.pending.insert(at, (seq, text.to_owned()));
            }
            return;
        }
        self.answer.push_str(text);
        self.next_seq = seq.saturating_add(1);
        while let Some((seq, _)) = self.pending.first()
            && *seq == self.next_seq
        {
            let (seq, text) = self.pending.remove(0);
            self.answer.push_str(&text);
            self.next_seq = seq.saturating_add(1);
        }
    }

    /// The card the current step's lines belong to: the newest open one.
    fn live_card(&mut self) -> Option<&mut StepCard> {
        self.cards
            .iter_mut()
            .rev()
            .find(|card| card.state == RunState::Running)
    }
}

/// How many cards the Conversation pane keeps for one turn. A steered turn
/// can run for as long as the user keeps talking to it; the pane shows the
/// tail, and the whole trace is in the Mind pane regardless.
const TURN_CARD_CAP: usize = 40;

pub struct State {
    pub bridge_version: u32,
    pub settings: Settings,
    pub keys: Vec<KeyStatus>,
    pub inference: InferenceConnection,
    pub account: Option<ProviderAccount>,
    /// Every subscription row the core reports, so all of them render.
    pub accounts: Vec<ProviderAccount>,
    pub store: StoreInfo,
    /// The readiness checks the core computed for this frame (05 §10).
    pub doctor: DoctorReport,
    pub projects: Vec<Project>,
    pub project_row: usize,
    pub project_detail: Option<(ProjectDocuments, Vec<neo_core::HeartbeatTick>)>,
    /// The selected control on an open project page.
    pub project_detail_row: usize,

    pub listen: Option<ListenState>,
    pub mic_device: Option<String>,

    pub view: View,
    /// Restricts Settings to the page opened by `/model` or `/login`.
    pub settings_section: Option<Section>,
    pub mode: Mode,
    pub focus: Pane,
    pub scroll: [u16; 4],
    pub follow: [bool; 4],

    /// The conversation, oldest first.
    pub thread: Vec<ThreadRow>,
    /// The stored conversation the thread belongs to, so it survives a
    /// restart. `None` only if the store could not open one.
    pub conversation: Option<ConversationId>,
    /// Its title, for the header and the picker.
    pub conversation_title: Option<String>,
    /// Set while a chat turn is running.
    pub turn: Option<TurnProgress>,
    /// Rows this front end put in the thread the moment the user steered a
    /// running turn, still waiting for the core's own record of them.
    ///
    /// A steering message is shown before it is stored — the point is that
    /// it never looks dropped — and the core persists it and publishes
    /// [`AppEvent::Message`] a moment later. Without this list the thread
    /// would then hold the same sentence twice; with it, the stored row is
    /// adopted into the one already on screen.
    pub pending_steers: Vec<MessageId>,
    /// Everything this front end has started, oldest first.
    pub runs: Vec<Run>,
    /// Which run the Mind pane is tracing.
    pub selected_run: Option<RunId>,
    /// The conversation switcher, while it is up.
    pub sessions: Option<Sessions>,
    /// Monotonic milliseconds since the loop started. The reducer's only
    /// notion of time, handed in rather than read, so elapsed is testable.
    pub now_ms: u64,
    /// True while the microphone is recording an utterance.
    pub dictating: bool,
    /// Peak input level 0.0..=1.0 while dictating, for the level meter.
    pub level: f32,

    pub composer: String,
    pub line: String,
    pub row: usize,
    pub prompt: Option<Prompt>,
    /// The login overlay, when a subscription sign-in is in flight.
    pub login: Option<Login>,
    /// The card on screen: the gate a paused run is waiting on.
    pub card: Option<Card>,
    /// Gates that tripped while another card was already up, oldest first.
    ///
    /// One slot on screen, because two sentences competing for one keystroke
    /// is how the wrong thing gets approved. A queue rather than a drop,
    /// because a confirm nobody is ever shown is the dead end this phase
    /// exists to remove.
    pub queued_cards: VecDeque<Card>,
    pub help: bool,
    pub quit_prompt: bool,
    pub quit: bool,

    pub activity: VecDeque<Activity>,
    pub status: Option<String>,
    pub pending_g: bool,
    pub dirty: bool,
}

impl State {
    /// Build the first frame's state. Chat is always the entry surface;
    /// incomplete setup is explained in the composer instead of replacing it.
    pub fn new(bootstrap: Bootstrap) -> Self {
        let mut state = Self::of(bootstrap);
        if state.setup_needed() {
            state.status = Some(SETUP_HINT.into());
        }
        state
    }

    fn of(bootstrap: Bootstrap) -> Self {
        let Bootstrap {
            bridge_version,
            settings,
            keys,
            inference,
            account,
            accounts,
            store,
            doctor,
            projects,
        } = bootstrap;
        Self {
            bridge_version,
            settings,
            keys,
            inference,
            account,
            accounts,
            store,
            doctor,
            projects,
            project_row: 0,
            project_detail: None,
            project_detail_row: 0,
            listen: None,
            mic_device: None,
            view: View::Panes,
            settings_section: None,
            mode: Mode::Insert,
            focus: Pane::Conversation,
            scroll: [0; 4],
            follow: [true; 4],
            thread: Vec::new(),
            conversation: None,
            conversation_title: None,
            turn: None,
            pending_steers: Vec::new(),
            runs: Vec::new(),
            selected_run: None,
            sessions: None,
            now_ms: 0,
            dictating: false,
            level: 0.0,
            composer: String::new(),
            line: String::new(),
            row: 0,
            prompt: None,
            login: None,
            card: None,
            queued_cards: VecDeque::new(),
            help: false,
            quit_prompt: false,
            quit: false,
            activity: VecDeque::new(),
            status: None,
            pending_g: false,
            dirty: true,
        }
    }

    /// Replace everything the core owns, keeping the throwaway view state
    /// (14 §4). Used on start and after a dropped-event gap.
    pub fn rebootstrap(&mut self, bootstrap: Bootstrap) {
        let Bootstrap {
            bridge_version,
            settings,
            keys,
            inference,
            account,
            accounts,
            store,
            doctor,
            projects,
        } = bootstrap;
        self.bridge_version = bridge_version;
        self.settings = settings;
        self.keys = keys;
        self.inference = inference;
        self.account = account;
        self.accounts = accounts;
        self.store = store;
        self.projects = projects;
        self.project_row = self.project_row.min(self.projects.len().saturating_sub(1));
        self.project_detail = None;
        self.doctor = doctor;
        self.row = self.row.min(self.rows().len().saturating_sub(1));
        self.dirty = true;
    }

    /// Advance the reducer's clock. Only redraws when a live run's rendered
    /// elapsed would actually change, or when the card's debounce just
    /// elapsed, so an idle front end still draws zero frames (14 §4).
    pub fn tick(&mut self, now_ms: u64) {
        let before = self
            .runs
            .iter()
            .any(Run::is_live)
            .then(|| self.elapsed_second());
        self.now_ms = now_ms;
        if let Some(before) = before
            && before != self.elapsed_second()
        {
            self.dirty = true;
        }
        // Arming is a clock fact, and this is where the clock arrives. The
        // frame it owes is the one that stops saying "reading…" and starts
        // offering the keys.
        if self.card.as_mut().is_some_and(|card| card.arm(now_ms)) {
            self.dirty = true;
        }
    }

    /// What the renderer painted on the frame it just drew.
    ///
    /// The only thing the reducer cannot work out for itself: whether the
    /// card's sentence actually reached the terminal. A card the frame was
    /// too small for, or one another overlay covered, is not on screen, and
    /// nothing a keystroke does may pretend otherwise (04 §13).
    pub fn painted(&mut self, painted: Painted) {
        let now = self.now_ms;
        if self
            .card
            .as_mut()
            .is_some_and(|card| card.painted(painted.card, now))
        {
            self.dirty = true;
        }
        // The renderer measured what the reducer could only estimate. Taking
        // its number keeps a scroll that ran past the top of the thread from
        // becoming a column of presses on the way back down: the pane stopped
        // moving long before the count did.
        //
        // No repaint is owed — the frame this came from is already showing
        // exactly this position.
        if let Some(back) = painted.conversation_back {
            let index = Pane::Conversation.index();
            self.scroll[index] = back;
            self.follow[index] = back == 0;
        }
    }

    /// The coarsest thing the runs pane renders from the clock: whole seconds
    /// of the oldest live run. If that has not moved, no frame is owed.
    fn elapsed_second(&self) -> u64 {
        self.runs
            .iter()
            .filter(|run| run.is_live())
            .map(|run| run.elapsed_ms(self.now_ms) / 1000)
            .max()
            .unwrap_or(0)
    }

    /// Is anything Starkbot cannot run without still missing? The TypeSafe key
    /// (Jev, always required) or an inference connection.
    pub fn setup_needed(&self) -> bool {
        self.key_state(TYPESAFE_ACCOUNT) != KeyState::Present
            || self.inference == InferenceConnection::None
    }

    /// The first row a user can actually do something with, so a fresh install
    /// starts with the cursor on a credential and not on a heading.
    fn first_actionable_row(&self) -> usize {
        self.rows()
            .iter()
            .position(|row| !matches!(row.action, RowAction::Inert))
            .unwrap_or(0)
    }

    /// One line for the machine-local roster: what this process is doing, in
    /// words another Starkbot's user would understand. Never a prompt and
    /// never a credential — the thread is the user's, and it does not belong
    /// in a roster another session reads.
    pub fn activity_line(&self) -> &'static str {
        if self.dictating {
            "dictating"
        } else if self.turn.is_some() {
            "running a turn"
        } else if self.login.is_some() {
            "signing in to a subscription"
        } else if self.view == View::Settings {
            "in settings"
        } else {
            "idle"
        }
    }

    pub fn composer_hint(&self) -> &'static str {
        if self.inference == InferenceConnection::None {
            COMPOSER_BLOCKED
        } else if self.turn.is_some() {
            COMPOSER_STEERS
        } else {
            COMPOSER_HINT
        }
    }

    /// Whether anything this front end started is still going, which is what
    /// makes `Esc` a stop rather than a quit prompt.
    #[must_use]
    pub fn has_live_run(&self) -> bool {
        self.runs.iter().any(Run::is_live)
    }

    /// The thread as the model should see it.
    ///
    /// Derived rather than kept alongside: two lists of the same conversation
    /// is exactly how a front end starts disagreeing with the store.
    #[must_use]
    pub fn history(&self) -> Vec<ChatMessage> {
        self.thread
            .iter()
            .map(|row| ChatMessage {
                role: row.role,
                text: row.text.clone(),
            })
            .collect()
    }

    /// Send what is in the composer.
    ///
    /// A turn already running is not a refusal: the message is steered into
    /// that turn instead. The composer stays editable the whole time, which
    /// is the difference between talking to an agent and filing a ticket
    /// with one. Only a missing runtime can refuse, and it names the fix.
    fn send_message(&mut self) -> Option<Command> {
        let text = self.composer.trim().to_owned();
        if text.is_empty() {
            return None;
        }
        if self.inference == InferenceConnection::None {
            self.status = Some(COMPOSER_BLOCKED.into());
            return None;
        }
        self.composer.clear();
        self.follow[Pane::Conversation.index()] = true;
        self.scroll[Pane::Conversation.index()] = 0;
        match self.turn.as_ref().map(|turn| turn.run) {
            Some(run) => {
                self.push_steered(&text);
                self.status = Some("steering the running turn".into());
                Some(Command::Steer { run, text })
            }
            None => Some(Command::Send { text }),
        }
    }

    /// Put a steering message in the thread before anything has stored it.
    ///
    /// The id is this front end's own until the core's record arrives and is
    /// adopted onto the row (see `pending_steers`): a user who typed into a
    /// running turn has to see the sentence land in the transcript on the
    /// keystroke, not one round trip later.
    fn push_steered(&mut self, text: &str) {
        let id = MessageId::new();
        self.thread.push(ThreadRow {
            id,
            role: Role::User,
            kind: MessageKind::Text,
            // The reducer's clock is a monotonic count, not a wall time; the
            // stored row carries the real stamp and is adopted onto this one.
            at: TimestampMs::try_from(self.now_ms).unwrap_or(TimestampMs::MAX),
            text: text.to_owned(),
            steered: true,
        });
        self.pending_steers.push(id);
        if self.follow[Pane::Conversation.index()] {
            self.scroll[Pane::Conversation.index()] = 0;
        }
        self.dirty = true;
    }

    /// The run had already finished, so the steer becomes a new turn.
    ///
    /// The optimistic row goes away rather than being left marked "steered"
    /// at a turn that never read it; the ordinary send path records it and
    /// the stored row takes its place.
    pub fn steer_missed(&mut self, text: &str) {
        if let Some(index) = self.thread.iter().rposition(|row| {
            row.steered && row.text == text && self.pending_steers.contains(&row.id)
        }) {
            let row = self.thread.remove(index);
            self.pending_steers.retain(|id| *id != row.id);
        }
        self.status = Some("that turn had already finished — sending it as a new turn".into());
        self.dirty = true;
    }

    // ------------------------------------------------------------------ runs

    /// Register something this front end just started.
    ///
    /// The loop mints the [`RunId`] before it spawns the work, so this is the
    /// only place a run enters the pane: an event carrying an id nobody
    /// registered belongs to another observer's run and stays in the ring.
    pub fn start_run(&mut self, id: RunId, kind: RunKind, title: impl Into<String>) {
        let run = Run::new(id, kind, title.into(), self.now_ms);
        if kind == RunKind::Chat {
            self.turn = Some(TurnProgress::new(id));
        }
        self.runs.push(run);
        // Follow-live on the Runs pane means "show me what I just started";
        // with nothing selected yet, the first run to arrive is the selection
        // either way.
        if self.follow[Pane::Runs.index()] || self.selected_run.is_none() {
            self.selected_run = Some(id);
        }
        self.prune_runs();
        self.dirty = true;
    }

    /// Drop the oldest settled runs once the list is over its cap. A live run
    /// is never dropped: it is the one thing `x` has to be able to reach.
    fn prune_runs(&mut self) {
        while self.runs.len() > RUNS_CAP {
            let Some(index) = self.runs.iter().position(|run| !run.is_live()) else {
                return;
            };
            let dropped = self.runs.remove(index);
            if self.selected_run == Some(dropped.id) {
                self.selected_run = self.runs.last().map(|run| run.id);
            }
        }
    }

    /// How a run ended, as the loop saw the call return.
    ///
    /// Needed beside the event stream because only a chat turn publishes a
    /// terminal event: a `/nav`, an `/ax` or a suite reports its outcome as a
    /// return value, and a run that failed before it published anything would
    /// otherwise sit at `running` forever.
    pub fn finish_run(&mut self, id: RunId, detail: Vec<String>, outcome: Result<String, String>) {
        let now = self.now_ms;
        let Some(run) = self.runs.iter_mut().find(|run| run.id == id) else {
            return;
        };
        // A chat turn already settled from `TurnFinished`/`TurnFailed`; the
        // job's return value arrives afterwards and must not overwrite it
        // with a second, blander sentence.
        if !run.is_live() {
            return;
        }
        let stopping = run.state == RunState::Stopping;
        for line in detail {
            run.push(TraceKind::Observation, line);
        }
        match outcome {
            Ok(summary) => {
                run.push(TraceKind::Result, summary.clone());
                run.last = summary;
                run.settle(RunState::Done, now);
            }
            Err(error) => {
                run.push(TraceKind::Result, error.clone());
                run.last = error.clone();
                run.settle(
                    if stopping {
                        RunState::Cancelled
                    } else {
                        RunState::Failed(error)
                    },
                    now,
                );
            }
        }
        if self.turn.as_ref().is_some_and(|turn| turn.run == id) {
            self.turn = None;
        }
        self.dirty = true;
    }

    #[must_use]
    pub fn selected_run(&self) -> Option<&Run> {
        let id = self.selected_run?;
        self.runs.iter().find(|run| run.id == id)
    }

    /// The index of the selected run in the pane's list.
    #[must_use]
    pub fn selected_run_index(&self) -> Option<usize> {
        let id = self.selected_run?;
        self.runs.iter().position(|run| run.id == id)
    }

    fn move_run_selection(&mut self, delta: i32) {
        if self.runs.is_empty() {
            return;
        }
        let last = self.runs.len() - 1;
        let current = self.selected_run_index().unwrap_or(last);
        let next = (i64::from(i32::try_from(current).unwrap_or(0) + delta))
            .clamp(0, i64::try_from(last).unwrap_or(0));
        let next = usize::try_from(next).unwrap_or(0);
        self.selected_run = self.runs.get(next).map(|run| run.id);
        // Moving by hand means the pane stops jumping to whatever starts next.
        self.follow[Pane::Runs.index()] = next == last;
    }

    /// The run `x` and `/stop` act on: the one being traced, or the chat turn
    /// when nothing is selected.
    #[must_use]
    pub fn stoppable_run(&self) -> Option<RunId> {
        if let Some(run) = self.selected_run()
            && run.state.live()
        {
            return Some(run.id);
        }
        self.runs
            .iter()
            .rev()
            .find(|run| run.state.live())
            .map(|run| run.id)
    }

    /// The microphone is now recording, or has stopped.
    pub fn set_dictating(&mut self, dictating: bool) {
        self.dictating = dictating;
        if !dictating {
            self.level = 0.0;
        }
        self.dirty = true;
    }

    /// Input level while recording; only redraws when the meter would change.
    pub fn set_level(&mut self, level: f32) {
        // One bar is 1/20th of the meter, so smaller moves are invisible.
        if (level - self.level).abs() >= 0.05 {
            self.level = level;
            self.dirty = true;
        }
    }

    /// Put a transcript into the composer, where the user can edit it before
    /// sending. Dictation never sends by itself: a misheard sentence must not
    /// become an action.
    pub fn dictated(&mut self, text: &str) {
        if !self.composer.is_empty() && !self.composer.ends_with(' ') {
            self.composer.push(' ');
        }
        self.composer.push_str(text.trim());
        self.mode = Mode::Insert;
        self.dirty = true;
    }

    /// Replace the thread, after a switch or a reload.
    pub fn load_thread(
        &mut self,
        conversation: ConversationId,
        title: Option<String>,
        messages: &[neo_core::Message],
    ) {
        self.conversation = Some(conversation);
        self.conversation_title = title;
        self.thread = messages.iter().filter_map(ThreadRow::of).collect();
        self.follow[Pane::Conversation.index()] = true;
        self.scroll[Pane::Conversation.index()] = 0;
        self.dirty = true;
    }

    /// Fill the session picker and put it up.
    pub fn show_sessions(&mut self, rows: Vec<SessionRow>) {
        let row = rows.iter().position(|row| row.active).unwrap_or(0);
        self.sessions = Some(Sessions { rows, row });
        self.dirty = true;
    }

    /// One subscription row's label, from the accounts the core reported.
    fn subscription_label(&self, provider: &str) -> String {
        let row = self
            .accounts
            .iter()
            .find(|account| account.provider.as_str() == provider);
        account_label(row, provider)
    }

    pub fn key_state(&self, account: &str) -> KeyState {
        self.keys
            .iter()
            .find(|status| status.account == account)
            .map_or(KeyState::Missing, |status| status.state)
    }

    /// Whether Starkbot can speak its replies. Dictation is a separate
    /// question: on-device transcription needs no key (K6 as amended), so a
    /// user without an OpenAI key can still talk *to* it.
    pub fn typed_only(&self) -> bool {
        self.key_state(SPEECH_ACCOUNT) != KeyState::Present
    }

    pub fn scroll_of(&self, pane: Pane) -> u16 {
        self.scroll[pane.index()]
    }

    pub fn follows(&self, pane: Pane) -> bool {
        self.follow[pane.index()]
    }

    // ---------------------------------------------------------------- events

    pub fn apply(&mut self, event: AppEvent) {
        let activity = summarize(&event);
        self.reduce_run(&event);
        match event {
            AppEvent::SettingsChanged { settings } => {
                self.settings = *settings;
                self.refresh_inference();
            }
            AppEvent::KeyStatus { account, status } => {
                match self
                    .keys
                    .iter_mut()
                    .find(|existing| existing.account == account)
                {
                    // `AppEvent::KeyStatus` carries only the state (04 §14), so
                    // the source a previous read reported is no longer known.
                    Some(existing) => {
                        existing.state = status;
                        existing.source = None;
                    }
                    None => self.keys.push(KeyStatus::new(account, status)),
                }
                self.refresh_inference();
            }
            AppEvent::ProviderAccount { account } => {
                self.account = Some(account);
                self.refresh_inference();
            }
            AppEvent::ListenState { state, device, .. } => {
                self.listen = Some(state);
                self.mic_device = device;
            }
            // The thread is appended from the store's own record, not from
            // what the composer held: one list, one source, no optimistic row
            // to reconcile when the write is rejected (14 §4). The one
            // exception is a steering message, which is on screen before it
            // is stored — that row is adopted rather than doubled.
            AppEvent::Message { message } => {
                if Some(message.conversation_id) == self.conversation
                    && let Some(row) = ThreadRow::of(&message)
                    && !self.adopt_steered(&row)
                {
                    self.thread.push(row);
                    if self.follow[Pane::Conversation.index()] {
                        self.scroll[Pane::Conversation.index()] = 0;
                    }
                }
            }
            // The core grows one row as an answer is written; a front end
            // that ignored this would keep showing the first slice of a
            // message the store has since finished.
            AppEvent::MessageUpdated { ref update } => {
                if let Some(text) = update.text.as_ref()
                    && let Some(row) = self.thread.iter_mut().find(|row| row.id == update.id)
                {
                    row.text = text.clone();
                }
            }
            AppEvent::ConversationReset { conversation_id } => {
                if Some(conversation_id) == self.conversation {
                    self.thread.clear();
                }
            }
            // A tripped gate is the one thing on screen the run cannot get
            // past without the user, so it goes up as a card (16 §5.5). It
            // arrives unarmed and unrendered: the keys only come alive once
            // the renderer says the sentence reached the frame and the
            // debounce has elapsed on it.
            AppEvent::ConfirmRequest { ref confirm } => self.raise_card(Card::confirm(confirm)),
            AppEvent::AskRequest { ref ask } => self.raise_card(Card::ask(ask)),
            // Resolved is resolved, whoever resolved it: this front end, the
            // webview, a voice answer, or the broker's own timeout. The card
            // simply goes away — a front end that waited for its own
            // keystroke would leave a dead card over a run that has moved on.
            AppEvent::ConfirmResolved { confirm_id, .. } => {
                self.clear_card(|card| card.is_confirm(confirm_id));
            }
            AppEvent::AskResolved { ask_id, .. } => {
                self.clear_card(|card| card.is_ask(ask_id));
            }
            AppEvent::Notice { ref text, .. } => self.status = Some(text.clone()),
            _ => {}
        }
        self.push_activity(activity);
        self.row = self.row.min(self.rows().len().saturating_sub(1));
        self.dirty = true;
    }

    /// Put a gate on screen, or behind the one already there.
    ///
    /// The same gate republished — a re-bootstrap, a reconnected webview —
    /// must not double: it is matched by id and the card on screen keeps its
    /// arming, because the sentence never left the frame.
    fn raise_card(&mut self, card: Card) {
        if self.card.as_ref().is_some_and(|live| live.same_gate(&card))
            || self
                .queued_cards
                .iter()
                .any(|queued| queued.same_gate(&card))
        {
            return;
        }
        if self.card.is_some() {
            self.queued_cards.push_back(card);
            return;
        }
        self.card = Some(card);
        self.mode = Mode::Card;
    }

    /// Take down the card the resolution settled, and raise the next one.
    ///
    /// A queued gate can settle before it is ever shown — the broker times it
    /// out, or another surface answers it — so the queue is swept too.
    fn clear_card(&mut self, settled: impl Fn(&Card) -> bool) {
        self.queued_cards.retain(|card| !settled(card));
        if !self.card.as_ref().is_some_and(&settled) {
            return;
        }
        self.card = self.queued_cards.pop_front();
        if self.card.is_none() && self.mode == Mode::Card {
            self.mode = Mode::Normal;
        }
    }

    /// Take the store's record of a steering message onto the row already on
    /// screen, and say whether it did.
    ///
    /// Matched on the text rather than the id, because the id is the one
    /// thing the two records cannot share: the front end minted one to have
    /// something to key the row by, and the store minted the real one. The
    /// row keeps its place and its marker and gains the stored id, so a
    /// later [`AppEvent::MessageUpdated`] can still find it.
    fn adopt_steered(&mut self, stored: &ThreadRow) -> bool {
        if self.pending_steers.is_empty() || stored.role != Role::User {
            return false;
        }
        let Some(row) = self.thread.iter_mut().find(|row| {
            row.steered && row.text == stored.text && self.pending_steers.contains(&row.id)
        }) else {
            return false;
        };
        let minted = row.id;
        row.id = stored.id;
        row.at = stored.at;
        row.kind = stored.kind;
        self.pending_steers.retain(|id| *id != minted);
        true
    }

    /// Fold one event into the run it belongs to, if this front end started
    /// that run.
    #[allow(clippy::too_many_lines)]
    fn reduce_run(&mut self, event: &AppEvent) {
        let Some(id) = run_of(event) else { return };
        let now = self.now_ms;
        let chat = self.turn.as_ref().is_some_and(|turn| turn.run == id);
        let Some(run) = self.runs.iter_mut().find(|run| run.id == id) else {
            return;
        };
        match event {
            AppEvent::TurnStarted { conversation, .. } => {
                run.push(TraceKind::Note, format!("turn started in {conversation}"));
            }
            AppEvent::TurnStep {
                step,
                thought,
                action,
                ..
            } => {
                run.steps = step.saturating_add(1);
                let line = action.to_string();
                run.last = line.clone();
                run.push(TraceKind::Step, format!("#{step} {line}"));
                if !thought.is_empty() {
                    run.push(TraceKind::Thought, thought.clone());
                }
                if chat && let Some(turn) = self.turn.as_mut() {
                    if turn.cards.len() == TURN_CARD_CAP {
                        turn.cards.remove(0);
                    }
                    turn.cards.push(StepCard {
                        step: *step,
                        intent: line,
                        thought: (!thought.is_empty()).then(|| thought.clone()),
                        detail: None,
                        state: RunState::Running,
                        duration_ms: None,
                    });
                }
            }
            AppEvent::TurnStepDone {
                step,
                observation,
                duration_ms,
                ..
            } => {
                run.push(
                    TraceKind::Observation,
                    format!("{observation} · {duration_ms} ms"),
                );
                if chat && let Some(turn) = self.turn.as_mut() {
                    turn.done += 1;
                    // By step, not by "the newest open card": a step that
                    // finished out of order would otherwise close the wrong
                    // card and leave its own running forever.
                    if let Some(card) = turn.cards.iter_mut().find(|card| card.step == *step) {
                        card.close(observation, *duration_ms);
                    }
                }
            }
            AppEvent::TurnDelta { seq, text, .. } => {
                if chat && let Some(turn) = self.turn.as_mut() {
                    turn.delta(*seq, text);
                }
            }
            // The running total, republished after every model round trip.
            // Kept rather than traced: a cost that scrolled past in the Mind
            // pane is not what a user checking "what is this costing me"
            // needs — the status line is.
            AppEvent::TurnCost { usage, .. } => {
                if chat && let Some(turn) = self.turn.as_mut() {
                    turn.usage = Some(*usage);
                }
            }
            // A steer is recorded in the trace even when this front end is
            // the one that sent it: the Mind pane is the record of what
            // reached the turn, and a message the model read mid-flight is
            // exactly the sort of thing a later "why did it do that?" needs.
            AppEvent::TurnSteered { text, .. } => {
                run.push(TraceKind::Note, format!("steered: {text}"));
                run.last = format!("steered: {text}");
                if !self.thread.iter().any(|row| row.text == *text) {
                    self.push_steered(text);
                }
            }
            AppEvent::TurnNote { line, .. } => {
                run.push(TraceKind::Note, line.clone());
                run.last = line.clone();
                if chat && let Some(card) = self.turn.as_mut().and_then(TurnProgress::live_card) {
                    // [`STOPPED`] is the agent's own marker for a step that
                    // was cut short. The card it belongs to never finished,
                    // and leaving it spinning after the turn is over claims
                    // work that did not happen.
                    if line == STOPPED {
                        card.state = RunState::Cancelled;
                    } else {
                        card.detail = Some(line.clone());
                    }
                }
            }
            // Every turn ends here now, including a cancelled one: the agent
            // returns the partial answer rather than an error. `Stopping` is
            // what tells the two apart — the run only reaches it because a
            // stop was asked for.
            AppEvent::TurnFinished {
                text,
                steps,
                exhausted,
                usage,
                ..
            } => {
                run.steps = *steps;
                let stopping = run.state == RunState::Stopping;
                let cost = usage.map_or_else(String::new, |usage| {
                    format!(
                        " · {} in / {} out over {} request(s)",
                        usage.input_tokens, usage.output_tokens, usage.requests
                    )
                });
                let outcome = if stopping {
                    "stopped"
                } else if *exhausted {
                    "step budget spent"
                } else {
                    "answered"
                };
                let summary = format!("{outcome}{cost}");
                run.push(TraceKind::Result, format!("{summary} — {text}"));
                run.last = summary;
                run.settle(
                    if stopping {
                        RunState::Cancelled
                    } else {
                        RunState::Done
                    },
                    now,
                );
                if chat {
                    self.turn = None;
                }
            }
            // The in-flight block goes away, but the work does not: the core
            // persists the answer it had written and every observation it
            // already took, and those rows land in the thread a moment
            // later. Interrupting must leave the partial answer and the
            // finished cards readable, or `Esc` reads as "undo".
            AppEvent::TurnFailed { error, .. } => {
                let stopping = run.state == RunState::Stopping;
                run.push(TraceKind::Result, error.clone());
                run.last = error.clone();
                run.settle(
                    if stopping {
                        RunState::Cancelled
                    } else {
                        RunState::Failed(error.clone())
                    },
                    now,
                );
                if chat {
                    self.turn = None;
                }
            }
            AppEvent::NavStep {
                step, line, kind, ..
            } => {
                run.steps = run.steps.max(*step);
                let trace = nav_trace(line, kind);
                run.last = trace.text.clone();
                run.push(trace.kind, trace.text.clone());
                if chat && let Some(card) = self.turn.as_mut().and_then(TurnProgress::live_card) {
                    card.detail = Some(trace.text);
                }
            }
            AppEvent::EvalCase {
                index,
                total,
                case,
                state,
                ..
            } => {
                run.steps = index.saturating_add(1);
                let line = eval_line(*index, *total, case, state);
                run.last = line.clone();
                run.push(TraceKind::Case, line);
            }
            _ => {}
        }
        self.dirty = true;
    }

    pub fn note(&mut self, detail: impl Into<String>) {
        let detail = detail.into();
        self.status = Some(detail.clone());
        self.push_activity(Activity {
            kind: "tui",
            detail,
        });
        self.dirty = true;
    }

    fn push_activity(&mut self, activity: Activity) {
        if self.activity.len() == ACTIVITY_CAP {
            self.activity.pop_front();
        }
        self.activity.push_back(activity);
    }

    fn refresh_inference(&mut self) {
        self.inference =
            InferenceConnection::detect(&self.settings, &self.keys, self.account.as_ref());
    }

    // --------------------------------------------------------------- actions

    /// Apply one action. Returns the core work it implies, if any.
    pub fn apply_action(&mut self, action: Action) -> Option<Command> {
        let clears_g = !matches!(action, Action::PendingG);
        // Marked before the dispatch, not after it: every arm that produces a
        // `Command` returns early, so a flag set at the bottom of `dispatch`
        // is the one thing those arms never reach. `/settings` changed the
        // view and the loop never redrew — a terminal that has stopped
        // painting is indistinguishable from one that has hung. Marking it
        // here also covers the status line the loop clears on every key.
        //
        // An action that changed nothing costs nothing: the renderer diffs
        // against the back buffer, so an identical frame writes no bytes.
        self.dirty = true;
        let result = self.dispatch(action);
        if clears_g {
            self.pending_g = false;
        }
        result
    }

    #[allow(clippy::too_many_lines)]
    fn dispatch(&mut self, action: Action) -> Option<Command> {
        match action {
            Action::None => return None,
            Action::Quit => self.quit = true,
            Action::AskQuit => self.quit_prompt = true,
            Action::ConfirmQuit => self.quit = true,
            Action::CancelQuit => self.quit_prompt = false,
            Action::Redraw => return Some(Command::Redraw),
            Action::ToggleHelp => self.help = !self.help,
            Action::CloseOverlay => {
                if self.help {
                    self.help = false;
                } else if self.sessions.is_some() {
                    self.sessions = None;
                } else if self.view == View::Settings || self.focus != Pane::Conversation {
                    self.view = View::Panes;
                    self.focus = Pane::Conversation;
                    self.project_detail = None;
                    self.settings_section = None;
                    self.mode = Mode::Insert;
                } else {
                    self.quit_prompt = true;
                }
            }
            Action::KillSwitch => {
                // Reachable in every mode (14 §3). Dropping the prompt here also
                // drops any half-typed secret.
                self.prompt = None;
                if self.runs.iter().any(Run::is_live) {
                    for run in &mut self.runs {
                        if run.state == RunState::Running {
                            run.state = RunState::Stopping;
                        }
                    }
                    self.status = Some(stop_notice(&self.runs));
                    return Some(Command::StopAll);
                }
                self.status = Some("kill switch: nothing is running".into());
            }
            Action::StopRun => return self.stop_selected(),
            Action::OpenProject => return self.open_project(),
            Action::ProjectBack => {
                self.project_detail = None;
                self.project_detail_row = 0;
            }
            Action::ToggleFollow => {
                let index = self.focus.index();
                self.follow[index] = !self.follow[index];
                if self.follow[index] {
                    self.scroll[index] = 0;
                    if self.focus == Pane::Runs {
                        self.selected_run = self.runs.last().map(|run| run.id);
                    }
                }
            }
            // With a card up, `i` is "answer the question", not "compose":
            // the card owns the keyboard, so insert mode would have nothing
            // to type into.
            Action::EnterInsert => {
                if self.card.is_some() {
                    self.open_ask_prompt();
                } else {
                    self.mode = Mode::Insert;
                }
            }
            Action::LeaveInsert => self.mode = Mode::Normal,
            Action::EnterCommand => {
                self.mode = Mode::Command;
                self.line = String::new();
            }
            Action::ComposerChar(character) => self.composer.push(character),
            Action::ComposerNewline => self.composer.push('\n'),
            Action::ComposerBackspace => {
                self.composer.pop();
            }
            Action::ComposerDeleteWord => delete_word(&mut self.composer),
            Action::ComposerDeleteLine => self.composer.clear(),
            Action::ComposerSubmit => return self.send_message(),
            Action::LineChar(character) => self.line.push(character),
            Action::LineBackspace => {
                self.line.pop();
            }
            Action::LineCancel => {
                self.mode = Mode::Insert;
                self.line = String::new();
            }
            Action::LineComplete => {
                if let Some(completion) = complete_command(&self.line) {
                    self.line = completion;
                }
            }
            Action::LineSubmit => return self.run_line(),
            // A card owns the keyboard while it is up, so motion moves its
            // options rather than a settings row nobody can see behind it.
            Action::SelectNext => {
                if self.card.is_some() {
                    self.move_option(1);
                } else {
                    self.move_row(1);
                }
            }
            Action::SelectPrevious => {
                if self.card.is_some() {
                    self.move_option(-1);
                } else {
                    self.move_row(-1);
                }
            }
            Action::SelectFirst => self.select_first(),
            Action::SelectLast => self.select_last(),
            Action::SelectPageDown => self.move_row(10),
            Action::SelectPageUp => self.move_row(-10),
            Action::SelectHalfDown => self.move_row(5),
            Action::SelectHalfUp => self.move_row(-5),
            Action::Activate => return self.activate_row(),
            Action::BeginSetKey => return self.begin_set_key(),
            Action::RemoveKey => return self.remove_key(),
            Action::ConnectSubscription => return self.subscription_command(true),
            Action::DisconnectSubscription => return self.subscription_command(false),
            Action::LoginOpenPage => {
                if self.login.as_ref().is_some_and(Login::settled) {
                    self.login = None;
                } else if self.login.is_some() {
                    return Some(Command::OpenLoginPage);
                }
            }
            Action::LoginBeginPaste => {
                if let Some(login) = self.login.as_mut()
                    && !login.settled()
                {
                    login.phase = LoginPhase::Pasting;
                }
            }
            Action::LoginPasteChar(character) => {
                if let Some(login) = self.login.as_mut() {
                    login.paste.push(character);
                }
            }
            Action::LoginPasteBackspace => {
                if let Some(login) = self.login.as_mut() {
                    login.paste.pop();
                }
            }
            Action::LoginSubmitPaste => return self.submit_paste(),
            Action::LoginClose => {
                let settled = self.login.as_ref().is_some_and(Login::settled);
                self.login = None;
                // Cancelling mid-flight has to reach the core: the loopback
                // listener is holding a port.
                if !settled {
                    return Some(Command::CancelLogin);
                }
            }
            Action::ToggleDictation => {
                return Some(if self.dictating {
                    Command::StopDictation
                } else {
                    Command::StartDictation
                });
            }
            Action::ToggleListen => {
                let enabled = self.settings.listen.enabled;
                self.status = Some(format!("listen.enabled → {}", bool_label(!enabled)));
                return Some(Command::PatchSettings {
                    section: "listen",
                    patch: json!({ "enabled": !enabled }),
                });
            }
            Action::NewConversation => return Some(Command::NewConversation { title: None }),
            Action::OpenSessions => return Some(Command::ListConversations),
            Action::SessionNext => self.move_session(1),
            Action::SessionPrevious => self.move_session(-1),
            Action::SessionOpen => return self.open_session(),
            Action::SessionClose => self.sessions = None,
            Action::RefreshModels => return self.refresh_models(),
            Action::CheckKey => return self.check_key(),
            Action::PromptChar(character) => {
                if let Some(prompt) = self.prompt.as_mut() {
                    prompt.buffer.push(character);
                }
            }
            Action::PromptBackspace => {
                if let Some(prompt) = self.prompt.as_mut() {
                    prompt.buffer.pop();
                }
            }
            Action::PromptDeleteLine => {
                if let Some(prompt) = self.prompt.as_mut() {
                    prompt.buffer.clear();
                }
            }
            Action::PromptCancel => self.prompt = None,
            Action::PromptSubmit => return self.submit_prompt(),
            Action::ResolveConfirm { approve } => return self.resolve_card(approve),
            Action::ToggleRemember => {
                // Q2 approvals are single-shot: the broker has nowhere to
                // keep a remembered allow yet, and a card that offered to
                // remember one would be promising something nothing honours.
                self.status = Some(
                    "remembering an allow is not built yet — this answer covers this one action"
                        .into(),
                );
            }
            Action::ShowMe => {
                self.status = Some(
                    "showing the page needs the long-lived managed Chrome, which is not built yet"
                        .into(),
                );
            }
            Action::AnswerAsk(option) => return self.answer_option(option),
            // The card stays: the run is waiting on it, and `Esc` is not an
            // answer. It leaves CARD mode so the mode word stops claiming a
            // focus the keyboard no longer has.
            Action::UnfocusCard => {
                self.mode = Mode::Normal;
                if self.card.is_some() {
                    self.status =
                        Some("the run is waiting on this — answer it or stop the run".into());
                }
            }
            Action::PendingG => self.pending_g = !self.pending_g,
            Action::Unavailable(reason) => self.status = Some(reason.into()),
        }
        None
    }

    // ----------------------------------------------------------- the card

    /// `y`/`n` on the card that is on screen.
    ///
    /// The keymap already refuses these unless the card is armed and
    /// rendered. This is the same rule again, on purpose: the keymap is only
    /// one way an [`Action`] reaches the reducer, and "nothing resolves a
    /// card the user has not seen" has to hold for all of them (04 §13).
    fn resolve_card(&mut self, approve: bool) -> Option<Command> {
        let card = self.card.as_ref()?;
        if !card.live() {
            self.status = Some(CARD_UNREAD.into());
            return None;
        }
        match card.kind {
            CardKind::Confirm { id, .. } => {
                let (outcome, note) = if approve {
                    (GateOutcome::Confirmed, "approved — the run carries on")
                } else {
                    // Denial is not a failure: the broker turns it into a
                    // steer, so the turn tries something else (16 §5.3).
                    (
                        GateOutcome::Denied,
                        "denied — the run is told, and tries another way",
                    )
                };
                self.status = Some(note.into());
                Some(Command::ResolveConfirm {
                    confirm: id,
                    outcome,
                    via: ResolutionVia::Card,
                })
            }
            // `y` takes the highlighted answer. There is no `n` to a
            // question: refusing to answer is leaving it alone.
            CardKind::Ask { .. } => {
                if !approve {
                    self.status =
                        Some("a question has no \"no\" — answer it, or stop the run with x".into());
                    return None;
                }
                self.answer_highlighted()
            }
        }
    }

    /// Answer an ask with the option under the highlight.
    fn answer_highlighted(&mut self) -> Option<Command> {
        let card = self.card.as_ref()?;
        let CardKind::Ask { id, .. } = card.kind else {
            return None;
        };
        let Some(answer) = card.highlighted().map(str::to_owned) else {
            // Nothing to take: the question wants words, so collect them.
            self.open_ask_prompt();
            return None;
        };
        self.status = Some(format!("answered “{answer}”"));
        Some(Command::AnswerAsk {
            ask: id,
            answer,
            via: ResolutionVia::Card,
        })
    }

    /// A digit highlights one answer and sends it in the same keystroke.
    ///
    /// Behind the arming rule like `y` is: a numbered list is easier to hit
    /// by accident than a single letter, not harder.
    fn answer_option(&mut self, option: u8) -> Option<Command> {
        let card = self.card.as_mut()?;
        if !card.live() {
            self.status = Some(CARD_UNREAD.into());
            return None;
        }
        if !card.select(usize::from(option).saturating_sub(1)) {
            self.status = Some(format!("there is no answer {option} on this card"));
            return None;
        }
        self.answer_highlighted()
    }

    /// `j`/`k` over the numbered answers.
    fn move_option(&mut self, delta: i32) {
        if let Some(card) = self.card.as_mut() {
            card.move_selection(delta);
        }
    }

    /// Collect the typed answer to a free-text question.
    ///
    /// The prompt overlay rather than the composer: while a card is up it
    /// owns the keyboard, and the prompt is the one overlay that takes it
    /// back (`keys.rs` checks it first). Unmasked — an invoice number is not
    /// a secret, and a user retyping one they cannot see gets it wrong.
    fn open_ask_prompt(&mut self) {
        let Some(card) = self.card.as_ref() else {
            return;
        };
        let CardKind::Ask { id, .. } = card.kind else {
            self.status = Some("this one is y or n".into());
            return;
        };
        if !card.free_text() {
            self.status = Some("pick an answer by its number".into());
            return;
        }
        let label = card.sentence.clone();
        self.prompt = Some(Prompt {
            kind: PromptKind::Ask { ask: id },
            label,
            hint: "Enter answers · Ctrl-U clears · Esc cancels",
            masked: false,
            buffer: String::new(),
        });
    }

    fn selected_project(&self) -> Option<&Project> {
        self.projects.get(self.project_row)
    }

    fn open_project(&mut self) -> Option<Command> {
        if self.project_detail.is_some() {
            return self.activate_project_control();
        }
        self.selected_project().map(|project| Command::OpenProject {
            slug: project.slug.clone(),
        })
    }

    fn activate_project_control(&mut self) -> Option<Command> {
        let project = self.selected_project()?.clone();
        match self.project_detail_row {
            0 => Some(Command::ToggleProjectHeartbeat {
                slug: project.slug,
                enabled: !project.heartbeat_enabled,
                every_seconds: project.heartbeat_every_seconds,
                on_gate: project.on_gate,
            }),
            1 => {
                self.prompt = Some(Prompt {
                    kind: PromptKind::ProjectInterval {
                        slug: project.slug,
                        enabled: project.heartbeat_enabled,
                        on_gate: project.on_gate,
                    },
                    label: "heartbeat interval in seconds".to_owned(),
                    hint: "Enter saves · Ctrl-U clears · Esc cancels",
                    masked: false,
                    buffer: project.heartbeat_every_seconds.to_string(),
                });
                None
            }
            2 => Some(Command::ToggleProjectHeartbeat {
                slug: project.slug,
                enabled: project.heartbeat_enabled,
                every_seconds: project.heartbeat_every_seconds,
                on_gate: match project.on_gate {
                    neo_core::HeartbeatGate::Hold => neo_core::HeartbeatGate::Skip,
                    neo_core::HeartbeatGate::Skip => neo_core::HeartbeatGate::Hold,
                },
            }),
            3 => self.edit_project_document("soul.md"),
            4 => self.edit_project_document("heartbeat.md"),
            _ => self.run_project_heartbeat(),
        }
    }

    fn run_project_heartbeat(&self) -> Option<Command> {
        self.selected_project()
            .map(|project| Command::RunProjectHeartbeat {
                slug: project.slug.clone(),
            })
    }

    fn edit_project_document(&self, document: &'static str) -> Option<Command> {
        self.selected_project()
            .map(|project| Command::EditProjectDocument {
                slug: project.slug.clone(),
                document,
            })
    }

    pub fn show_project(
        &mut self,
        projects: Vec<Project>,
        documents: ProjectDocuments,
        ticks: Vec<neo_core::HeartbeatTick>,
    ) {
        self.projects = projects;
        self.project_row = self.project_row.min(self.projects.len().saturating_sub(1));
        self.project_detail = Some((documents, ticks));
        self.project_detail_row = self.project_detail_row.min(5);
        self.dirty = true;
    }

    /// `x` and `/stop`: cancel the run being traced.
    ///
    /// The row goes to `stopping`, not `cancelled`: the token still has to
    /// reach the navigator, and the navigator still has to close the Chrome
    /// it launched. Claiming a clean stop before that has happened is the
    /// one thing a stop button must not do.
    fn stop_selected(&mut self) -> Option<Command> {
        let Some(id) = self.stoppable_run() else {
            self.status = Some("nothing is running".into());
            return None;
        };
        if let Some(run) = self.runs.iter_mut().find(|run| run.id == id) {
            run.state = RunState::Stopping;
        }
        self.status = Some(stop_notice(&self.runs));
        Some(Command::StopRun { run: id })
    }

    fn move_session(&mut self, delta: i32) {
        let Some(sessions) = self.sessions.as_mut() else {
            return;
        };
        if sessions.rows.is_empty() {
            return;
        }
        let last = i64::try_from(sessions.rows.len() - 1).unwrap_or(0);
        let next = i64::from(i32::try_from(sessions.row).unwrap_or(0) + delta).clamp(0, last);
        sessions.row = usize::try_from(next).unwrap_or(0);
    }

    fn open_session(&mut self) -> Option<Command> {
        let sessions = self.sessions.as_ref()?;
        let row = sessions.rows.get(sessions.row)?;
        let conversation = row.id;
        self.sessions = None;
        Some(Command::SwitchConversation { conversation })
    }

    /// `gg`: the top of whatever the focused pane is showing.
    ///
    /// "Top" is not "oldest": the Runs and Mind panes render newest-first,
    /// so their top is the newest row, while the Conversation reads
    /// oldest-first like a transcript. `gg` means the same thing on screen
    /// in all three, which is the only definition a user can act on.
    fn select_first(&mut self) {
        match (self.view, self.focus) {
            (View::Settings, _) => self.row = 0,
            (_, Pane::Runs) => {
                self.selected_run = self.runs.last().map(|run| run.id);
                self.follow[Pane::Runs.index()] = true;
            }
            (_, Pane::Projects) => {
                if self.project_detail.is_some() {
                    self.project_detail_row = 0;
                } else {
                    self.project_row = 0;
                }
            }
            (_, Pane::Conversation) => self.scroll_to(Pane::Conversation, u16::MAX),
            (_, Pane::Mind) => self.scroll_to(Pane::Mind, 0),
        }
    }

    /// `G`: the bottom of whatever the focused pane is showing.
    fn select_last(&mut self) {
        match (self.view, self.focus) {
            (View::Settings, _) => self.row = self.rows().len().saturating_sub(1),
            (_, Pane::Runs) => {
                self.selected_run = self.runs.first().map(|run| run.id);
                self.follow[Pane::Runs.index()] = self.runs.len() <= 1;
            }
            (_, Pane::Projects) => {
                if self.project_detail.is_some() {
                    self.project_detail_row = 5;
                } else {
                    self.project_row = self.projects.len().saturating_sub(1);
                }
            }
            (_, Pane::Conversation) => self.scroll_to(Pane::Conversation, 0),
            (_, Pane::Mind) => self.scroll_to(Pane::Mind, u16::MAX),
        }
    }

    /// How many lines the pane holds, so a scroll cannot run off the end
    /// into blank space — which is what `G` used to do.
    ///
    /// An estimate, not a measurement: the renderer wraps, so the row count
    /// depends on a width this module does not know. Erring high leaves a
    /// little slack at the end of a long thread; erring low would hide the
    /// last line, which is worse.
    fn pane_lines(&self, pane: Pane) -> usize {
        match pane {
            // A long message occupies several rows once it is wrapped, and
            // the reducer does not know the pane's width. Erring high is
            // safe here and erring low is not: the renderer clamps a scroll
            // that overshoots, but nothing can recover a scroll that cannot
            // reach the top of a long thread. 24 columns is narrower than
            // any pane the layout produces.
            Pane::Conversation => {
                self.thread
                    .iter()
                    .map(|row| row.text.chars().count().div_ceil(24).max(1))
                    .sum::<usize>()
                    + self.turn.as_ref().map_or(0, |turn| {
                        // Up to four rows a card — intent, reason, detail,
                        // outcome — plus the blank line above the block and
                        // the answer as it grows.
                        turn.cards.len() * 4 + turn.answer.chars().count().div_ceil(24) + 1
                    })
            }
            // Three lines per run: the state row, the title, the last line.
            Pane::Runs => self.runs.len() * 3,
            Pane::Mind => self
                .selected_run()
                .map_or(self.activity.len(), |run| run.trace.len()),
            Pane::Projects => self.projects.len().saturating_mul(3),
        }
    }

    /// Set a pane's scroll, clamped to its content, and keep `follow` in
    /// step: a pane is live exactly when it is showing its newest end.
    fn scroll_to(&mut self, pane: Pane, offset: u16) {
        let last = u16::try_from(self.pane_lines(pane).saturating_sub(1)).unwrap_or(u16::MAX);
        let index = pane.index();
        self.scroll[index] = offset.min(last);
        self.follow[index] = self.scroll[index] == 0;
    }

    fn move_row(&mut self, delta: i32) {
        if self.view == View::Settings {
            let last = self.rows().len().saturating_sub(1);
            let next = i64::from(i32::try_from(self.row).unwrap_or(0) + delta)
                .clamp(0, i64::try_from(last).unwrap_or(0));
            self.row = usize::try_from(next).unwrap_or(0);
            return;
        }
        // In the Runs pane the same motion moves the selection, because the
        // list is the pane: scrolling a three-line list is not a thing anyone
        // wants and selecting a run is.
        if self.focus == Pane::Runs {
            self.move_run_selection(delta.signum());
            return;
        }
        if self.focus == Pane::Projects {
            if self.project_detail.is_some() {
                let next =
                    i64::from(i32::try_from(self.project_detail_row).unwrap_or(0) + delta.signum())
                        .clamp(0, 5);
                self.project_detail_row = usize::try_from(next).unwrap_or(0);
            } else {
                let last = self.projects.len().saturating_sub(1);
                let next = i64::from(i32::try_from(self.project_row).unwrap_or(0) + delta.signum())
                    .clamp(0, i64::try_from(last).unwrap_or(0));
                self.project_row = usize::try_from(next).unwrap_or(0);
            }
            return;
        }
        // The conversation reads oldest-first and is anchored to its bottom,
        // so its `scroll` counts rows *back* from the newest: up must add and
        // down must subtract. Sharing the sign with the top-anchored panes
        // inverted both keys — `k` at the bottom of a thread did nothing at
        // all, which is what "I cannot scroll up" looks like.
        let delta = if self.focus == Pane::Conversation {
            -delta
        } else {
            delta
        };
        let scroll = i32::from(self.scroll[self.focus.index()]) + delta;
        self.scroll_to(self.focus, u16::try_from(scroll.max(0)).unwrap_or(u16::MAX));
    }

    // ------------------------------------------------------------- slash line

    fn run_line(&mut self) -> Option<Command> {
        let line = std::mem::take(&mut self.line);
        self.mode = Mode::Insert;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }
        let (name, rest) = trimmed
            .split_once(char::is_whitespace)
            .map_or((trimmed, ""), |(name, rest)| (name, rest.trim()));
        match name {
            "chat" => {
                self.view = View::Panes;
                self.focus = Pane::Conversation;
            }
            "project" => {
                self.view = View::Panes;
                self.focus = Pane::Projects;
                self.project_detail = None;
                self.project_detail_row = 0;
                self.mode = Mode::Normal;
            }
            "runs" => {
                self.view = View::Panes;
                self.focus = Pane::Runs;
                self.mode = Mode::Normal;
            }
            "mind" => {
                self.view = View::Panes;
                self.focus = Pane::Mind;
                self.mode = Mode::Normal;
            }
            "activity" => {
                self.view = View::Panes;
                self.focus = Pane::Mind;
                self.selected_run = None;
                self.mode = Mode::Normal;
            }
            "settings" => {
                self.view = View::Settings;
                self.settings_section = Section::ALL
                    .iter()
                    .find(|section| section.name() == rest)
                    .copied();
                self.row = 0;
                self.mode = Mode::Normal;
            }
            "login" => {
                self.view = View::Settings;
                self.settings_section = Some(Section::Connections);
                self.row = self.first_actionable_row();
                self.mode = Mode::Normal;
            }
            "model" => {
                if rest == "refresh" {
                    let account = self.settings.models.inference.provider.as_str().to_owned();
                    self.status = Some(format!("refreshing the {account} catalogue"));
                    return Some(Command::RefreshModels { account });
                }
                self.view = View::Settings;
                self.settings_section = Some(Section::Models);
                self.row = self.first_actionable_row();
                self.mode = Mode::Normal;
            }
            "doctor" => {
                self.view = View::Settings;
                self.settings_section = Some(Section::Doctor);
                self.row = 0;
                self.mode = Mode::Normal;
                return Some(Command::Doctor);
            }
            "search" => return self.search_thread(rest),
            "nav" => return self.parse_nav(rest),
            "app" => return self.parse_app(rest),
            "ax" => return self.parse_ax(rest),
            "eval" => return self.parse_eval(rest),
            "new" => {
                let title = (!rest.is_empty()).then(|| rest.to_owned());
                return Some(Command::NewConversation { title });
            }
            "sessions" => return Some(Command::ListConversations),
            "rename" => {
                if rest.is_empty() {
                    self.prompt = Some(Prompt {
                        kind: PromptKind::Rename,
                        label: "rename this conversation".to_owned(),
                        hint: "Enter saves · Ctrl-U clears · Esc cancels",
                        masked: false,
                        buffer: self.conversation_title.clone().unwrap_or_default(),
                    });
                    return None;
                }
                return Some(Command::RenameConversation {
                    title: rest.to_owned(),
                });
            }
            "stop" => return self.stop_selected(),
            "kill" => {
                if self.runs.iter().any(Run::is_live) {
                    for run in &mut self.runs {
                        if run.state == RunState::Running {
                            run.state = RunState::Stopping;
                        }
                    }
                    self.status = Some(stop_notice(&self.runs));
                    return Some(Command::StopAll);
                }
                self.status = Some("nothing is running".into());
            }
            "help" => self.help = true,
            "quit" => self.quit = true,
            other if lookup(other).is_some() => {
                self.status = lookup(other)
                    .and_then(|spec| spec.unavailable)
                    .map(ToOwned::to_owned);
            }
            other => self.status = Some(format!("unknown command: /{other}")),
        }
        None
    }

    /// `/` searches the thread. It selects rather than filters: the point is
    /// to land on the message, with the surrounding conversation intact.
    fn search_thread(&mut self, needle: &str) -> Option<Command> {
        if needle.is_empty() {
            return None;
        }
        let lowered = needle.to_lowercase();
        let hits = self
            .thread
            .iter()
            .filter(|row| row.text.to_lowercase().contains(&lowered))
            .count();
        match self
            .thread
            .iter()
            .rposition(|row| row.text.to_lowercase().contains(&lowered))
        {
            Some(index) => {
                // Scroll is counted from the bottom, so the offset is how many
                // rows sit below the match.
                let below = self.thread.len().saturating_sub(index + 1);
                self.scroll[Pane::Conversation.index()] = u16::try_from(below).unwrap_or(u16::MAX);
                self.follow[Pane::Conversation.index()] = below == 0;
                self.focus = Pane::Conversation;
                self.status = Some(format!("{hits} match(es) for “{needle}”"));
            }
            None => self.status = Some(format!("no message contains “{needle}”")),
        }
        None
    }

    fn parse_nav(&mut self, rest: &str) -> Option<Command> {
        let mut words = rest.split_whitespace().peekable();
        let mut spec = NavSpec {
            safety_heads: true,
            ..NavSpec::default()
        };
        let mut goal: Vec<&str> = Vec::new();
        while let Some(word) = words.next() {
            match word {
                "--headless" => spec.headless = true,
                "--no-safety" => spec.safety_heads = false,
                "--profile" => match words.next() {
                    Some(path) => spec.profile = Some(path.to_owned()),
                    None => {
                        self.status = Some("--profile needs a directory".into());
                        return None;
                    }
                },
                other if spec.url.is_empty() => spec.url = other.to_owned(),
                other => goal.push(other),
            }
        }
        spec.goal = goal.join(" ");
        if spec.url.is_empty() || spec.goal.is_empty() {
            self.status = Some(usage("nav"));
            return None;
        }
        Some(Command::Nav { options: spec })
    }

    fn parse_app(&mut self, rest: &str) -> Option<Command> {
        let Some((app, goal)) = rest.split_once(char::is_whitespace) else {
            self.status = Some(usage("app"));
            return None;
        };
        let goal = goal.trim();
        if app.is_empty() || goal.is_empty() {
            self.status = Some(usage("app"));
            return None;
        }
        Some(Command::AppGoal {
            app: app.to_owned(),
            goal: goal.to_owned(),
        })
    }

    #[allow(clippy::too_many_lines)]
    fn parse_ax(&mut self, rest: &str) -> Option<Command> {
        let mut words = rest.splitn(2, char::is_whitespace);
        let sub = words.next().unwrap_or_default();
        let tail = words.next().unwrap_or_default().trim();
        let request = match sub {
            "trusted" | "" => AxRequest::Trusted,
            "apps" => AxRequest::Apps,
            "table" => {
                if tail.is_empty() {
                    self.status = Some("ax table <app>".into());
                    return None;
                }
                AxRequest::Table {
                    app: tail.to_owned(),
                }
            }
            "press" => {
                let (app, index) = self.two_words(tail, "ax press <app> <index>")?;
                let Ok(index) = index.parse::<u16>() else {
                    self.status = Some("the row index is a whole number".into());
                    return None;
                };
                AxRequest::Press { app, index }
            }
            "set" => {
                let mut parts = tail.splitn(3, char::is_whitespace);
                let (Some(app), Some(index), Some(text)) =
                    (parts.next(), parts.next(), parts.next())
                else {
                    self.status = Some("ax set <app> <index> <text>".into());
                    return None;
                };
                let Ok(index) = index.parse::<u16>() else {
                    self.status = Some("the row index is a whole number".into());
                    return None;
                };
                AxRequest::Set {
                    app: app.to_owned(),
                    index,
                    text: text.to_owned(),
                }
            }
            "menu" => {
                let (app, path) = self.rest_after(tail, "ax menu <app> <path with › or >>")?;
                AxRequest::Menu { app, path }
            }
            "type" => {
                let (app, text) = self.rest_after(tail, "ax type <app> <text>")?;
                AxRequest::Type { app, text }
            }
            "key" => {
                let (app, key) = self.two_words(tail, "ax key <app> <key>")?;
                AxRequest::Key { app, key }
            }
            other => {
                self.status = Some(format!("unknown ax request: {other}"));
                return None;
            }
        };
        Some(Command::Ax { request })
    }

    fn two_words(&mut self, tail: &str, usage: &str) -> Option<(String, String)> {
        let mut parts = tail.split_whitespace();
        match (parts.next(), parts.next()) {
            (Some(first), Some(second)) => Some((first.to_owned(), second.to_owned())),
            _ => {
                self.status = Some(usage.to_owned());
                None
            }
        }
    }

    fn rest_after(&mut self, tail: &str, usage: &str) -> Option<(String, String)> {
        match tail.split_once(char::is_whitespace) {
            Some((first, rest)) if !rest.trim().is_empty() => {
                Some((first.to_owned(), rest.trim().to_owned()))
            }
            _ => {
                self.status = Some(usage.to_owned());
                None
            }
        }
    }

    fn parse_eval(&mut self, rest: &str) -> Option<Command> {
        let mut words = rest.split_whitespace();
        let mut selection = Selection::default();
        let mut list = false;
        while let Some(word) = words.next() {
            match word {
                "--list" => list = true,
                "--once" => selection.once = true,
                "--filter" => match words.next() {
                    Some(filter) => selection.filter = Some(filter.to_owned()),
                    None => {
                        self.status = Some("--filter needs a substring".into());
                        return None;
                    }
                },
                "--tag" => match words.next() {
                    Some(tag) => selection.tags.push(tag.to_owned()),
                    None => {
                        self.status = Some("--tag needs a tag".into());
                        return None;
                    }
                },
                other => {
                    self.status = Some(format!("unknown eval flag: {other}"));
                    return None;
                }
            }
        }
        if list {
            return Some(Command::EvalList);
        }
        // One suite at a time, always: the cases share the keyboard and the
        // frontmost application, so a second run would fight the first.
        if self
            .runs
            .iter()
            .any(|run| run.kind == RunKind::Eval && run.state.live())
        {
            self.status =
                Some("an eval suite is already running — the cases share the keyboard".into());
            return None;
        }
        Some(Command::Eval { selection })
    }

    fn selected(&self) -> Option<Row> {
        self.rows().get(self.row).cloned()
    }

    /// The Keychain account a row can set or remove, if any.
    fn row_account(row: &Row) -> Option<String> {
        match &row.action {
            RowAction::Key(account) => Some(account.clone()),
            RowAction::Path { account, .. } => account.clone(),
            _ => None,
        }
    }

    /// `c` / `d` on one of the two subscription rows. Any other row says so
    /// rather than doing something surprising.
    fn subscription_command(&mut self, connect: bool) -> Option<Command> {
        let row = self.selected()?;
        match row.action {
            RowAction::Path {
                provider,
                subscription: true,
                ..
            } => Some(match (connect, oauth_path(provider)) {
                // The OAuth paths are Starkbot's own login (K7): an overlay,
                // no terminal handover, and the front end stays interactive
                // while the browser is open.
                (true, true) => Command::BeginLogin { provider },
                (true, false) => Command::ConnectSubscription { provider },
                (false, _) => Command::DisconnectSubscription { provider },
            }),
            _ => {
                self.status = Some("not a subscription row".into());
                None
            }
        }
    }

    /// Hand the core the pasted redirect URL. An empty buffer is a no-op
    /// rather than a request the core has to reject.
    fn submit_paste(&mut self) -> Option<Command> {
        let login = self.login.as_mut()?;
        let pasted = login.paste.trim().to_owned();
        if pasted.is_empty() {
            self.status = Some("paste the URL your browser was redirected to".into());
            return None;
        }
        login.phase = LoginPhase::Exchanging;
        login.paste.clear();
        Some(Command::FinishLoginPasted { pasted })
    }

    /// `r` on a row: re-read that provider's catalogue from the vendor.
    fn refresh_models(&mut self) -> Option<Command> {
        let row = self.selected()?;
        let account = match &row.action {
            RowAction::Path { provider, .. } => (*provider).to_owned(),
            RowAction::Key(account) => account.clone(),
            _ => {
                self.status = Some("no provider on this row".into());
                return None;
            }
        };
        Some(Command::RefreshModels { account })
    }

    /// `K` on a row: ask the vendor what the stored key is worth.
    ///
    /// Shifted because lowercase `k` is list motion in every other list, and
    /// a check-key that fired on `k` meant the cursor could not be moved up
    /// with the keyboard everyone's fingers already know.
    fn check_key(&mut self) -> Option<Command> {
        let row = self.selected()?;
        let Some(account) = Self::row_account(&row) else {
            self.status = Some("no key on this row".into());
            return None;
        };
        Some(Command::CheckKey { account })
    }

    fn begin_set_key(&mut self) -> Option<Command> {
        let row = self.selected()?;
        let Some(account) = Self::row_account(&row) else {
            self.status = Some("no key on this row".into());
            return None;
        };
        self.prompt = Some(Prompt {
            label: format!("Paste the {account} key"),
            kind: PromptKind::SetKey { account },
            hint: "paste only · echoes ••• · Enter saves · Esc cancels",
            masked: true,
            buffer: String::new(),
        });
        None
    }

    fn remove_key(&mut self) -> Option<Command> {
        let row = self.selected()?;
        let Some(account) = Self::row_account(&row) else {
            self.status = Some("no key on this row".into());
            return None;
        };
        Some(Command::RemoveKey { account })
    }

    fn submit_prompt(&mut self) -> Option<Command> {
        let prompt = self.prompt.take()?;
        match prompt.kind {
            PromptKind::SetKey { account } => {
                if prompt.buffer.is_empty() {
                    self.status = Some("nothing entered".into());
                    return None;
                }
                Some(Command::SetKey {
                    account,
                    raw: prompt.buffer,
                })
            }
            PromptKind::ModelRef { field } => match parse_model_ref(&prompt.buffer) {
                Some(model) => Some(Command::PatchSettings {
                    section: "models",
                    patch: json!({ field: model }),
                }),
                None => {
                    self.status =
                        Some("a model reads as provider/id, e.g. openai/sol-latest".into());
                    None
                }
            },
            PromptKind::Field {
                section,
                pointer,
                kind,
            } => match kind.parse(&prompt.buffer) {
                Ok(value) => Some(Command::PatchSettings {
                    section,
                    patch: patch_for(pointer, value),
                }),
                Err(reason) => {
                    self.status = Some(format!("{section}.{pointer}: {reason}"));
                    None
                }
            },
            PromptKind::ProjectInterval {
                slug,
                enabled,
                on_gate,
            } => match prompt.buffer.trim().parse::<u64>() {
                Ok(every_seconds) if every_seconds > 0 => Some(Command::ToggleProjectHeartbeat {
                    slug,
                    enabled,
                    every_seconds,
                    on_gate,
                }),
                _ => {
                    self.status =
                        Some("the heartbeat interval must be a positive whole number".into());
                    None
                }
            },
            PromptKind::Rename => {
                let title = prompt.buffer.trim().to_owned();
                if title.is_empty() {
                    self.status = Some("a conversation title cannot be empty".into());
                    return None;
                }
                Some(Command::RenameConversation { title })
            }
            // The card stays up until the core says the question is settled:
            // the answer has to reach the run before the question can leave
            // the screen, and only [`AppEvent::AskResolved`] knows that.
            PromptKind::Ask { ask } => {
                let answer = prompt.buffer.trim().to_owned();
                if answer.is_empty() {
                    self.status = Some("nothing typed — the question is still waiting".into());
                    return None;
                }
                self.status = Some("answer sent".into());
                Some(Command::AnswerAsk {
                    ask,
                    answer,
                    via: ResolutionVia::Card,
                })
            }
        }
    }

    fn activate_row(&mut self) -> Option<Command> {
        let row = self.selected()?;
        match row.action {
            RowAction::Inert => {
                if !row.heading {
                    self.status = Some("this row is reported by the core, not edited here".into());
                }
                None
            }
            RowAction::Key(_) => self.begin_set_key(),
            RowAction::Path {
                provider,
                subscription,
                ..
            } => {
                if subscription {
                    self.status =
                        Some(format!("{provider} selected — `c` signs in, `d` signs out"));
                }
                Some(Command::PatchSettings {
                    section: "models",
                    patch: json!({ "inference": { "provider": provider } }),
                })
            }
            RowAction::CycleEffort(effort) => Some(Command::PatchSettings {
                section: "models",
                patch: json!({ "sol_effort": next_effort(effort) }),
            }),
            RowAction::EditModel(field) => {
                let current = model_field(&self.settings, field);
                self.prompt = Some(Prompt {
                    kind: PromptKind::ModelRef { field },
                    label: format!("models.{field} — provider/id"),
                    hint: "Enter saves · Ctrl-U clears · Esc cancels",
                    masked: false,
                    buffer: current,
                });
                None
            }
            RowAction::Field {
                section,
                pointer,
                kind,
            } => {
                // A bool or a closed set flips in place; anything typed opens
                // the same one-line prompt a model id uses.
                if let Some(value) = kind.cycled() {
                    return Some(Command::PatchSettings {
                        section,
                        patch: patch_for(pointer, value),
                    });
                }
                let buffer = kind.editable().unwrap_or_default();
                self.prompt = Some(Prompt {
                    kind: PromptKind::Field {
                        section,
                        pointer,
                        kind,
                    },
                    label: format!("{section}.{pointer}"),
                    hint: "Enter saves · Ctrl-U clears · Esc cancels",
                    masked: false,
                    buffer,
                });
                None
            }
        }
    }

    // ---------------------------------------------------------------- rows

    /// The Settings view's rows, rebuilt from core state on every call so the
    /// list can never drift from what the core last reported.
    #[allow(clippy::too_many_lines)]
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = self.connection_rows();
        rows.extend(self.model_rows());

        let settings = &self.settings;

        rows.push(Row::heading(Section::Identity));
        rows.push(Row::field(
            Section::Identity,
            "name",
            "name",
            FieldKind::Text(settings.identity.name.clone()),
        ));

        rows.push(Row::heading(Section::Listen));
        rows.push(Row::field(
            Section::Listen,
            "enabled",
            "enabled",
            FieldKind::Bool(settings.listen.enabled),
        ));
        rows.push(Row::field(
            Section::Listen,
            "addressing",
            "addressing",
            choice(
                &["open", "name_required", "push_to_talk"],
                match settings.listen.addressing {
                    neo_core::AddressingMode::Open => 0,
                    neo_core::AddressingMode::NameRequired => 1,
                    neo_core::AddressingMode::PushToTalk => 2,
                },
            ),
        ));
        rows.push(Row::field(
            Section::Listen,
            "push_to_talk",
            "push to talk",
            FieldKind::Bool(settings.listen.push_to_talk),
        ));
        rows.push(Row::field(
            Section::Listen,
            "mic_device",
            "mic device",
            FieldKind::OptionalText(settings.listen.mic_device.clone()),
        ));

        rows.push(Row::heading(Section::Voice));
        rows.push(Row::field(
            Section::Voice,
            "tts_enabled",
            "speak replies",
            FieldKind::Bool(settings.voice.tts_enabled),
        ));
        rows.push(Row::field(
            Section::Voice,
            "tts_voice",
            "voice",
            FieldKind::Text(settings.voice.tts_voice.clone()),
        ));
        rows.push(Row::field(
            Section::Voice,
            "speak",
            "speak",
            choice(
                &["questions_only", "questions_and_results", "everything"],
                match settings.voice.speak {
                    neo_core::SpeakMode::QuestionsOnly => 0,
                    neo_core::SpeakMode::QuestionsAndResults => 1,
                    neo_core::SpeakMode::Everything => 2,
                },
            ),
        ));
        rows.push(Row::field(
            Section::Voice,
            "duplex",
            "duplex",
            choice(
                &["auto", "half", "full"],
                match settings.voice.duplex {
                    neo_core::DuplexMode::Auto => 0,
                    neo_core::DuplexMode::Half => 1,
                    neo_core::DuplexMode::Full => 2,
                },
            ),
        ));
        rows.push(Row::field(
            Section::Voice,
            "keep_recordings",
            "keep recordings",
            FieldKind::Bool(settings.voice.keep_recordings),
        ));

        rows.push(Row::heading(Section::Intake));
        rows.push(Row::field(
            Section::Intake,
            "enqueue_at",
            "enqueue at",
            FieldKind::Float(f64::from(settings.intake.enqueue_at)),
        ));
        rows.push(Row::field(
            Section::Intake,
            "offer_at",
            "offer at",
            FieldKind::Float(f64::from(settings.intake.offer_at)),
        ));

        rows.push(Row::heading(Section::Safety));
        rows.push(Row::field(
            Section::Safety,
            "confirm_at.outward",
            "confirm · outward",
            FieldKind::Float(f64::from(settings.safety.confirm_at.outward)),
        ));
        rows.push(Row::field(
            Section::Safety,
            "confirm_at.destructive",
            "confirm · destructive",
            FieldKind::Float(f64::from(settings.safety.confirm_at.destructive)),
        ));
        rows.push(Row::field(
            Section::Safety,
            "confirm_at.spends",
            "confirm · spends",
            FieldKind::Float(f64::from(settings.safety.confirm_at.spends)),
        ));
        rows.push(Row::field(
            Section::Safety,
            "on_task_floor",
            "on-task floor",
            FieldKind::Float(f64::from(settings.safety.on_task_floor)),
        ));
        rows.push(Row::field(
            Section::Safety,
            "confirm_timeout_s",
            "confirm timeout (s)",
            FieldKind::Int(i64::from(settings.safety.confirm_timeout_s)),
        ));
        rows.push(Row::field(
            Section::Safety,
            "confirm_labels",
            "always-confirm labels",
            FieldKind::List(settings.safety.confirm_labels.clone()),
        ));
        rows.push(Row::fact(
            Section::Safety,
            "",
            "thresholds are capped at 0.60 by the core — a looser value is rejected",
        ));

        rows.push(Row::heading(Section::Caps));
        rows.push(Row::field(
            Section::Caps,
            "usd_per_task",
            "USD per task",
            FieldKind::Float(settings.caps.usd_per_task),
        ));
        rows.push(Row::field(
            Section::Caps,
            "usd_media_call_confirm",
            "USD · media call confirm",
            FieldKind::Float(settings.caps.usd_media_call_confirm),
        ));
        rows.push(Row::field(
            Section::Caps,
            "usd_per_day",
            "USD per day",
            FieldKind::Float(settings.caps.usd_per_day),
        ));
        rows.push(Row::field(
            Section::Caps,
            "sol_steps",
            "Sol steps",
            FieldKind::Int(i64::from(settings.caps.sol_steps)),
        ));
        rows.push(Row::field(
            Section::Caps,
            "nav_actions",
            "navigator actions",
            FieldKind::Int(i64::from(settings.caps.nav_actions)),
        ));
        rows.push(Row::field(
            Section::Caps,
            "nav_decisions",
            "navigator decisions",
            FieldKind::Int(i64::from(settings.caps.nav_decisions)),
        ));
        rows.push(Row::field(
            Section::Caps,
            "wall_minutes",
            "wall minutes",
            FieldKind::Int(i64::from(settings.caps.wall_minutes)),
        ));

        rows.push(Row::heading(Section::Queue));
        rows.push(Row::field(
            Section::Queue,
            "idle_wait_s",
            "idle wait (s)",
            FieldKind::Float(f64::from(settings.queue.idle_wait_s)),
        ));
        rows.push(Row::field(
            Section::Queue,
            "paused",
            "paused",
            FieldKind::Bool(settings.queue.paused),
        ));
        rows.push(Row::fact(
            Section::Queue,
            "",
            "the queue worker itself is not built yet — this is the setting it will read",
        ));

        rows.push(Row::heading(Section::Browser));
        rows.push(Row::field(
            Section::Browser,
            "mode",
            "mode",
            choice(
                &["managed", "attach"],
                match settings.browser.mode {
                    neo_core::BrowserMode::Managed => 0,
                    neo_core::BrowserMode::Attach => 1,
                },
            ),
        ));
        rows.push(Row::field(
            Section::Browser,
            "keep_running",
            "keep Chrome running",
            FieldKind::Bool(settings.browser.keep_running),
        ));
        rows.push(Row::field(
            Section::Browser,
            "close_task_tabs",
            "close task tabs",
            FieldKind::Bool(settings.browser.close_task_tabs),
        ));

        rows.push(Row::heading(Section::Hotkeys));
        rows.push(Row::field(
            Section::Hotkeys,
            "toggle_listen",
            "toggle listen",
            FieldKind::Text(settings.hotkeys.toggle_listen.clone()),
        ));
        rows.push(Row::field(
            Section::Hotkeys,
            "quick_entry",
            "quick entry",
            FieldKind::Text(settings.hotkeys.quick_entry.clone()),
        ));
        rows.push(Row::field(
            Section::Hotkeys,
            "kill",
            "kill switch",
            FieldKind::Text(settings.hotkeys.kill.clone()),
        ));
        rows.push(Row::fact(
            Section::Hotkeys,
            "",
            "global hotkeys are the desktop app's; the terminal keymap is fixed",
        ));

        rows.push(Row::heading(Section::General));
        rows.push(Row::field(
            Section::General,
            "autostart",
            "autostart",
            FieldKind::Bool(settings.general.autostart),
        ));
        rows.push(Row::field(
            Section::General,
            "update_check",
            "check for updates",
            FieldKind::Bool(settings.general.update_check),
        ));
        rows.push(Row::field(
            Section::General,
            "notifications",
            "notifications",
            FieldKind::Bool(settings.general.notifications),
        ));

        rows.push(Row::heading(Section::Privacy));
        rows.push(Row::field(
            Section::Privacy,
            "trace_days",
            "keep traces (days)",
            FieldKind::Int(i64::from(settings.privacy.trace_days)),
        ));
        rows.push(Row::field(
            Section::Privacy,
            "verdict_days",
            "keep verdicts (days)",
            FieldKind::Int(i64::from(settings.privacy.verdict_days)),
        ));
        rows.push(Row::field(
            Section::Privacy,
            "ignored_text_days",
            "keep ignored text (days)",
            FieldKind::Int(i64::from(settings.privacy.ignored_text_days)),
        ));

        rows.extend(self.doctor_rows());
        if let Some(section) = self.settings_section {
            rows.retain(|row| row.section == section);
        }
        rows
    }

    #[allow(clippy::too_many_lines)]
    fn connection_rows(&self) -> Vec<Row> {
        let selected = self.settings.models.inference.provider.as_str();
        let mark = |provider: &str, value: String| {
            if provider == selected {
                format!("{value} · selected")
            } else {
                value
            }
        };
        let key_account = |account: &str| {
            self.keys
                .iter()
                .find(|status| status.account == account)
                .map(|status| status.account.clone())
        };

        let mut rows = vec![Row::heading(Section::Connections)];
        rows.push(Row::fact(
            Section::Connections,
            "active inference",
            connection_label(self.inference),
        ));
        rows.push(Row::fact(
            Section::Connections,
            "",
            "Enter selects · s sets a key · x removes · K checks · c signs in",
        ));
        rows.push(
            Row::fact(
                Section::Connections,
                "(a) OpenAI API key",
                mark(
                    PROVIDER_OPENAI,
                    key_label(self.key_state(PROVIDER_OPENAI)).to_owned(),
                ),
            )
            .with(RowAction::Path {
                provider: PROVIDER_OPENAI,
                account: key_account(PROVIDER_OPENAI),
                subscription: false,
            }),
        );
        rows.push(
            Row::fact(
                Section::Connections,
                "(b) Claude Pro/Max",
                mark(
                    neo_core::PROVIDER_ANTHROPIC_OAUTH,
                    self.subscription_label(neo_core::PROVIDER_ANTHROPIC_OAUTH),
                ),
            )
            .with(RowAction::Path {
                provider: neo_core::PROVIDER_ANTHROPIC_OAUTH,
                account: None,
                subscription: true,
            }),
        );
        rows.push(
            Row::fact(
                Section::Connections,
                "(c) Anthropic API key",
                mark(
                    PROVIDER_ANTHROPIC,
                    key_label(self.key_state(PROVIDER_ANTHROPIC)).to_owned(),
                ),
            )
            .with(RowAction::Path {
                provider: PROVIDER_ANTHROPIC,
                account: key_account(PROVIDER_ANTHROPIC),
                subscription: false,
            }),
        );
        rows.push(
            Row::fact(
                Section::Connections,
                "(d) ChatGPT Plus/Pro",
                mark(
                    neo_core::PROVIDER_OPENAI_CODEX,
                    self.subscription_label(neo_core::PROVIDER_OPENAI_CODEX),
                ),
            )
            .with(RowAction::Path {
                provider: neo_core::PROVIDER_OPENAI_CODEX,
                account: None,
                subscription: true,
            }),
        );
        rows.push(Row::fact(
            Section::Connections,
            "",
            "c signs in · d signs out · fallbacks below need the vendor's own CLI",
        ));
        // A25 demoted these two to fallbacks: they need the vendor's CLI or
        // app-server installed, and they hand it the terminal.
        rows.push(
            Row::fact(
                Section::Connections,
                "(e) Claude via CLI",
                mark(
                    PROVIDER_CLAUDE_SUBSCRIPTION,
                    self.subscription_label(PROVIDER_CLAUDE_SUBSCRIPTION),
                ),
            )
            .with(RowAction::Path {
                provider: PROVIDER_CLAUDE_SUBSCRIPTION,
                account: None,
                subscription: true,
            }),
        );
        rows.push(
            Row::fact(
                Section::Connections,
                "(f) ChatGPT via Codex",
                mark(
                    PROVIDER_CHATGPT_CODEX,
                    self.subscription_label(PROVIDER_CHATGPT_CODEX),
                ),
            )
            .with(RowAction::Path {
                provider: PROVIDER_CHATGPT_CODEX,
                account: None,
                subscription: true,
            }),
        );
        // Every other account the core reports — TypeSafe today — as a key row.
        for status in &self.keys {
            if status.account == PROVIDER_OPENAI || status.account == PROVIDER_ANTHROPIC {
                continue;
            }
            rows.push(
                Row::fact(
                    Section::Connections,
                    format!("{} key", status.account),
                    key_label(status.state),
                )
                .with(RowAction::Key(status.account.clone())),
            );
        }
        if self.typed_only() {
            rows.push(Row::fact(
                Section::Connections,
                "",
                "typed-only: speech needs an OpenAI API key (K6)",
            ));
        }
        rows
    }

    fn model_rows(&self) -> Vec<Row> {
        let settings = &self.settings;
        let mut rows = vec![Row::heading(Section::Models)];
        for field in ["inference", "text_helper", "stt", "tts"] {
            rows.push(
                Row::fact(Section::Models, field, model_field(settings, field))
                    .with(RowAction::EditModel(field)),
            );
        }
        rows.push(
            Row::fact(
                Section::Models,
                "sol_effort",
                effort_label(settings.models.sol_effort),
            )
            .with(RowAction::CycleEffort(settings.models.sol_effort)),
        );
        rows.push(Row::field(
            Section::Models,
            "stt_live.enabled",
            "live transcription",
            FieldKind::Bool(settings.models.stt_live.enabled),
        ));
        rows.push(Row::fact(
            Section::Models,
            "stt_live.model",
            format!(
                "{}/{}",
                settings.models.stt_live.model.provider.as_str(),
                settings.models.stt_live.model.id
            ),
        ));
        rows.push(Row::fact(
            Section::Models,
            "",
            "ids are typed, not picked — `/model refresh` re-reads the catalogue",
        ));
        rows
    }

    fn doctor_rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::heading(Section::Doctor)];
        rows.push(Row::fact(
            Section::Doctor,
            "store",
            self.store.path.display().to_string(),
        ));
        rows.push(Row::fact(
            Section::Doctor,
            "schema",
            format!(
                "v{} · application_id {}",
                self.store.schema_version, self.store.application_id
            ),
        ));
        rows.push(Row::fact(
            Section::Doctor,
            "bridge",
            format!("v{}", self.bridge_version),
        ));
        rows.push(Row::fact(
            Section::Doctor,
            "events seen",
            self.activity.len().to_string(),
        ));
        // The core's own readiness checks (05 §10), each with the word and the
        // glyph so state is never colour-only (14 §2).
        for check in &self.doctor.checks {
            if check.name == "store" {
                continue;
            }
            let fix = check
                .fix
                .as_deref()
                .map(|fix| format!(" — fix: {fix}"))
                .unwrap_or_default();
            rows.push(Row::fact(
                Section::Doctor,
                &check.name,
                format!(
                    "{} {} · {}{fix}",
                    health_glyph(check.health),
                    health_word(check.health),
                    check.detail
                ),
            ));
        }
        rows
    }
}

/// Which run an event belongs to, for the events that name one.
const fn run_of(event: &AppEvent) -> Option<RunId> {
    match event {
        AppEvent::TurnStarted { run, .. }
        | AppEvent::TurnStep { run, .. }
        | AppEvent::TurnStepDone { run, .. }
        | AppEvent::TurnNote { run, .. }
        | AppEvent::TurnDelta { run, .. }
        | AppEvent::TurnCost { run, .. }
        | AppEvent::TurnSteered { run, .. }
        | AppEvent::TurnFinished { run, .. }
        | AppEvent::TurnFailed { run, .. }
        | AppEvent::NavStep { run, .. }
        | AppEvent::EvalCase { run, .. } => Some(*run),
        _ => None,
    }
}

/// What a stop can and cannot reclaim, said plainly.
///
/// Cancelling reaches the navigator, which closes the Chrome it launched on
/// its way out — but a page it already navigated stays navigated, and a
/// keystroke already delivered to an application cannot be un-typed. A front
/// end that renders "stopped" the instant `x` is pressed is lying about all
/// three.
fn stop_notice(runs: &[Run]) -> String {
    let browser = runs.iter().any(|run| {
        run.state == RunState::Stopping && matches!(run.kind, RunKind::Nav | RunKind::Chat)
    });
    if browser {
        "stopping — the run closes its Chrome as it unwinds; pages it already changed stay changed"
            .to_owned()
    } else {
        "stopping — keystrokes already delivered cannot be un-typed".to_owned()
    }
}

/// A float in the digits it was written in, not the ones binary widening
/// gives it back.
///
/// Most of these fields are stored as `f32`; `f64::from(0.4_f32)` is
/// `0.4000000059604645`, and pre-filling an edit box with that means a user
/// who only wanted to change a threshold saves seventeen digits of noise.
/// Six decimals is finer than any field here and rounds the artefact away.
fn short_float(value: f64) -> String {
    let rendered = format!("{value:.6}");
    let trimmed = rendered.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() {
        "0".to_owned()
    } else {
        trimmed.to_owned()
    }
}

const fn choice(options: &'static [&'static str], current: usize) -> FieldKind {
    FieldKind::Choice { options, current }
}

/// Turn a dotted pointer and a value into the merge patch the core wants.
///
/// RFC 7386: only the named leaf is replaced, so patching
/// `safety.confirm_at.outward` leaves `destructive` and `spends` alone.
fn patch_for(pointer: &str, value: Value) -> Value {
    let mut patch = value;
    for part in pointer.rsplit('.') {
        patch = json!({ part: patch });
    }
    patch
}

/// One entry of the slash-command line.
///
/// Still a closed list (14 §5): nothing here runs a program, opens a path for
/// reading or forwards unknown input anywhere. The arguments it does take are
/// the same ones `neo nav`, `neo app`, `neo ax` and `neo eval` take, because
/// a front end that cannot reach a capability the CLI has is not parity — it
/// is a second, smaller product.
pub struct CommandSpec {
    pub name: &'static str,
    /// Rendered after the name in the help overlay.
    pub args: &'static str,
    pub help: &'static str,
    /// Set when the capability genuinely does not exist yet. These are the
    /// only honest "not yet" strings left.
    pub unavailable: Option<&'static str>,
}

pub const COMMAND_LINE: [CommandSpec; 26] = [
    CommandSpec {
        name: "project",
        args: "",
        help: "open the project index",
        unavailable: None,
    },
    CommandSpec {
        name: "model",
        args: "[refresh]",
        help: "configure models or refresh the active catalogue",
        unavailable: None,
    },
    CommandSpec {
        name: "login",
        args: "",
        help: "configure connections, keys, and subscriptions",
        unavailable: None,
    },
    CommandSpec {
        name: "settings",
        args: "[section]",
        help: "open all settings or one named section",
        unavailable: None,
    },
    CommandSpec {
        name: "runs",
        args: "",
        help: "show work started by this TUI",
        unavailable: None,
    },
    CommandSpec {
        name: "mind",
        args: "",
        help: "show the selected run trace",
        unavailable: None,
    },
    CommandSpec {
        name: "activity",
        args: "",
        help: "show recent core activity",
        unavailable: None,
    },
    CommandSpec {
        name: "chat",
        args: "",
        help: "return to the conversation",
        unavailable: None,
    },
    CommandSpec {
        name: "search",
        args: "<text>",
        help: "find text in the conversation",
        unavailable: None,
    },
    CommandSpec {
        name: "nav",
        args: "<url> <goal> [--headless] [--profile P] [--no-safety]",
        help: "drive a web page to a goal",
        unavailable: None,
    },
    CommandSpec {
        name: "app",
        args: "<app> <goal>",
        help: "drive a native application to a goal",
        unavailable: None,
    },
    CommandSpec {
        name: "ax",
        args: "trusted|apps|table|press|set|menu|type|key",
        help: "one direct accessibility call",
        unavailable: None,
    },
    CommandSpec {
        name: "eval",
        args: "[--filter F] [--tag T] [--once] [--list]",
        help: "run the app-control suite",
        unavailable: None,
    },
    CommandSpec {
        name: "stop",
        args: "",
        help: "cancel the selected run",
        unavailable: None,
    },
    CommandSpec {
        name: "kill",
        args: "",
        help: "cancel every live run",
        unavailable: None,
    },
    CommandSpec {
        name: "new",
        args: "[title]",
        help: "start a conversation",
        unavailable: None,
    },
    CommandSpec {
        name: "sessions",
        args: "",
        help: "switch conversation",
        unavailable: None,
    },
    CommandSpec {
        name: "rename",
        args: "<title>",
        help: "retitle this conversation",
        unavailable: None,
    },
    CommandSpec {
        name: "doctor",
        args: "",
        help: "re-run readiness checks",
        unavailable: None,
    },
    CommandSpec {
        name: "help",
        args: "",
        help: "show commands and keys",
        unavailable: None,
    },
    CommandSpec {
        name: "quit",
        args: "",
        help: "leave Starkbot Neo",
        unavailable: None,
    },
    CommandSpec {
        name: "packs",
        args: "",
        help: "capability packs",
        unavailable: Some("/packs needs the pack registry, which is not built yet"),
    },
    CommandSpec {
        name: "soul",
        args: "",
        help: "edit soul.md",
        unavailable: Some("/soul needs the global soul buffer, which is not built yet"),
    },
    CommandSpec {
        name: "pause",
        args: "",
        help: "pause the queue",
        unavailable: Some("/pause needs the queue worker, which is not built yet"),
    },
    CommandSpec {
        name: "resume",
        args: "",
        help: "resume the queue",
        unavailable: Some("/resume needs the queue worker, which is not built yet"),
    },
    CommandSpec {
        name: "listen",
        args: "on|off",
        help: "microphone",
        unavailable: Some(
            "/listen needs the always-on listener; `m` toggles the setting and `v` dictates",
        ),
    },
];

/// The subcommands `/ax` accepts, for completion and for the help overlay.
pub const AX_REQUESTS: [&str; 8] = [
    "trusted", "apps", "table", "press", "set", "menu", "type", "key",
];

#[must_use]
pub fn lookup(name: &str) -> Option<&'static CommandSpec> {
    COMMAND_LINE.iter().find(|spec| spec.name == name)
}

fn usage(name: &str) -> String {
    lookup(name).map_or_else(
        || format!("unknown command: /{name}"),
        |spec| format!("{}: /{} {}", spec.help, spec.name, spec.args),
    )
}

/// Tab completion: the command name while one is being typed, then the `/ax`
/// subcommand, which is the only argument drawn from a closed set.
fn complete_command(prefix: &str) -> Option<String> {
    match prefix.split_once(char::is_whitespace) {
        None => unique(COMMAND_LINE.iter().map(|spec| spec.name), prefix).map(|name| {
            let spec = lookup(name);
            if spec.is_some_and(|spec| !spec.args.is_empty()) {
                format!("{name} ")
            } else {
                name.to_owned()
            }
        }),
        Some(("ax", rest)) if !rest.contains(char::is_whitespace) => {
            unique(AX_REQUESTS.into_iter(), rest.trim_start()).map(|sub| format!("ax {sub}"))
        }
        Some(_) => None,
    }
}

/// The one candidate with this prefix, or nothing when it is ambiguous.
fn unique<'a>(candidates: impl Iterator<Item = &'a str>, prefix: &str) -> Option<&'a str> {
    let mut found = None;
    for candidate in candidates {
        if candidate.starts_with(prefix) {
            if found.is_some() {
                return None;
            }
            found = Some(candidate);
        }
    }
    found
}

fn delete_word(buffer: &mut String) {
    while buffer.ends_with(' ') {
        buffer.pop();
    }
    while !buffer.is_empty() && !buffer.ends_with(' ') {
        buffer.pop();
    }
}

fn parse_model_ref(raw: &str) -> Option<ModelRef> {
    let (provider, id) = raw.trim().split_once('/')?;
    if provider.is_empty() || id.is_empty() {
        return None;
    }
    Some(ModelRef::new(ProviderId::new(provider), id))
}

fn model_field(settings: &Settings, field: &str) -> String {
    let model = match field {
        "text_helper" => &settings.models.text_helper,
        "stt" => &settings.models.stt,
        "tts" => &settings.models.tts,
        _ => &settings.models.inference,
    };
    format!("{}/{}", model.provider.as_str(), model.id)
}

const fn next_effort(effort: ReasoningEffort) -> ReasoningEffort {
    match effort {
        ReasoningEffort::Minimal => ReasoningEffort::Low,
        ReasoningEffort::Low => ReasoningEffort::Medium,
        ReasoningEffort::Medium => ReasoningEffort::High,
        ReasoningEffort::High => ReasoningEffort::Minimal,
    }
}

pub const fn effort_label(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Minimal => "minimal",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
    }
}

pub const fn bool_label(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

/// Never a value, only a state (14 §5).
pub const fn key_label(state: KeyState) -> &'static str {
    match state {
        KeyState::Missing => "missing",
        KeyState::Present => "present",
        KeyState::Invalid => "invalid",
        KeyState::Unchecked => "unchecked",
        KeyState::Limited => "limited",
    }
}

/// Whether this provider signs in through Starkbot's own OAuth flow (K7) as
/// opposed to a vendor CLI that wants the terminal (A22).
pub const fn oauth_path(provider: &str) -> bool {
    matches!(provider.as_bytes(), b"anthropic-oauth" | b"openai-codex")
}

/// The plan name a user recognises, for the login overlay's title.
pub const fn plan_title(provider: &str) -> &'static str {
    match provider.as_bytes() {
        b"anthropic-oauth" => "Claude Pro/Max",
        b"openai-codex" => "ChatGPT Plus/Pro",
        b"claude-subscription" => "Claude subscription (CLI)",
        b"chatgpt-codex" => "ChatGPT plan (Codex app-server)",
        _ => "subscription",
    }
}

pub const fn connection_label(connection: InferenceConnection) -> &'static str {
    match connection {
        InferenceConnection::None => "none — add a key or sign in",
        InferenceConnection::OpenAiKey => "OpenAI API key",
        InferenceConnection::ChatGptCodex => "ChatGPT plan (Codex)",
        InferenceConnection::AnthropicKey => "Anthropic API key",
        InferenceConnection::ClaudeSubscription => "Claude subscription (CLI)",
        InferenceConnection::AnthropicOauth => "Claude Pro/Max",
        InferenceConnection::OpenAiCodexOauth => "ChatGPT Plus/Pro",
    }
}

pub const fn account_status_label(status: ProviderAccountStatus) -> &'static str {
    match status {
        ProviderAccountStatus::SignedOut => "signed out",
        ProviderAccountStatus::Connected => "connected",
        ProviderAccountStatus::RateLimited => "rate limited",
        // Not "unavailable": the check could not be made, which is not a
        // claim about the account. See `neo_core::ProviderAccountStatus`.
        ProviderAccountStatus::Unavailable => "could not check",
    }
}

fn account_label(account: Option<&ProviderAccount>, provider: &str) -> String {
    match account {
        Some(account) if account.provider.as_str() == provider => {
            let status = account_status_label(account.status);
            match (&account.email, &account.plan_type) {
                (Some(email), Some(plan)) => format!("{status} · {email} · {plan}"),
                (Some(email), None) => format!("{status} · {email}"),
                (None, Some(plan)) => format!("{status} · {plan}"),
                (None, None) => status.to_owned(),
            }
        }
        _ => "signed out".to_owned(),
    }
}

pub const fn listen_label(state: Option<ListenState>) -> (&'static str, &'static str) {
    match state {
        None => ("○", "NO MIC YET"),
        Some(ListenState::Listening) => ("●", "LISTENING"),
        Some(ListenState::Hearing) => ("◉", "HEARING"),
        Some(ListenState::Transcribing) => ("◍", "TRANSCRIBING"),
        Some(ListenState::Speaking) => ("◎", "SPEAKING"),
        Some(ListenState::Muted) => ("⊘", "MUTED"),
        Some(ListenState::Paused) => ("‖", "PAUSED"),
        Some(ListenState::MicLost) => ("✕", "MIC LOST"),
        Some(ListenState::NoPermission) => ("✕", "NO PERMISSION"),
    }
}

/// One activity line per event. The match is exhaustive on purpose: a new
/// `AppEvent` variant must not compile until the TUI can name it (14 §1).
#[allow(clippy::too_many_lines)]
fn summarize(event: &AppEvent) -> Activity {
    let (kind, detail) = match event {
        AppEvent::ListenState { state, device, .. } => (
            "listen",
            format!(
                "{} · {}",
                listen_label(Some(*state)).1,
                device.as_deref().unwrap_or("no device")
            ),
        ),
        AppEvent::Message { message } => ("message", format!("id {}", message.id)),
        AppEvent::MessageUpdated { update } => ("message", format!("updated {}", update.id)),
        AppEvent::ConversationReset { conversation_id } => {
            ("conversation", format!("reset {conversation_id}"))
        }
        AppEvent::TurnStarted { conversation, .. } => {
            ("turn", format!("started in {conversation}"))
        }
        AppEvent::TurnStep { step, action, .. } => ("turn", format!("step {step} · {action}")),
        AppEvent::TurnStepDone {
            step, duration_ms, ..
        } => ("turn", format!("step {step} done · {duration_ms} ms")),
        AppEvent::TurnNote { step, line, .. } => ("turn", format!("step {step} · {line}")),
        // The ring counts the slice rather than repeating the answer: the
        // text is already on screen in the conversation, and a ring full of
        // half-words is unreadable.
        AppEvent::TurnDelta { seq, text, .. } => (
            "turn",
            format!("delta #{seq} · {} char(s)", text.chars().count()),
        ),
        AppEvent::TurnSteered { text, .. } => ("turn", format!("steered · {text}")),
        AppEvent::TurnCost { usage, .. } => (
            "turn",
            format!(
                "{} in / {} out over {} request(s)",
                usage.input_tokens, usage.output_tokens, usage.requests
            ),
        ),
        AppEvent::TurnFinished {
            steps, exhausted, ..
        } => (
            "turn",
            format!(
                "{steps} step(s) · {}",
                if *exhausted {
                    "budget spent"
                } else {
                    "answered"
                }
            ),
        ),
        // `error` is built from our own error types, which cannot format a
        // secret, so it is safe to show verbatim (see the variant's doc).
        AppEvent::TurnFailed { error, .. } => ("turn", error.clone()),
        AppEvent::NavStep { step, line, .. } => ("nav", format!("#{step} {line}")),
        AppEvent::EvalCase {
            index, total, case, ..
        } => ("eval", format!("{}/{total} {case}", index + 1)),
        AppEvent::TaskUpserted { task } => ("task", format!("{} {:?}", task.id, task.status)),
        AppEvent::TaskRemoved { id } => ("task", format!("removed {id}")),
        AppEvent::QueueState {
            paused, reasons, ..
        } => (
            "queue",
            format!(
                "{} · {}",
                if *paused { "paused" } else { "running" },
                reasons.join(", ")
            ),
        ),
        AppEvent::Trace { task_id, seq, .. } => ("trace", format!("{task_id} #{seq}")),
        AppEvent::ConfirmRequest { confirm } => ("confirm", confirm.id.to_string()),
        AppEvent::ConfirmResolved {
            confirm_id,
            outcome,
            ..
        } => ("confirm", format!("{confirm_id} {outcome:?}")),
        AppEvent::AskRequest { ask } => ("ask", ask.id.to_string()),
        AppEvent::AskResolved { ask_id, .. } => ("ask", format!("{ask_id} answered")),
        AppEvent::Ring { display, .. } => ("ring", display.to_string()),
        AppEvent::BrowserPresence { .. } => ("browser", "presence changed".into()),
        AppEvent::ModeRequest { mode, reason } => ("mode", format!("{mode:?} · {reason}")),
        AppEvent::EnablementOffer { pack_id, .. } => ("pack", format!("offer {pack_id}")),
        AppEvent::MediaJob { job } => ("media", job.id.to_string()),
        AppEvent::PackChanged { pack_id, enabled } => {
            ("pack", format!("{pack_id} {}", bool_label(*enabled)))
        }
        AppEvent::Health { .. } => ("health", "updated".into()),
        AppEvent::ProviderAccount { account } => (
            "account",
            format!(
                "{} · {}",
                account.provider.as_str(),
                account_status_label(account.status)
            ),
        ),
        AppEvent::Spend { today, .. } => ("spend", format!("${:.2} today", today.usd)),
        AppEvent::Latency {
            step_ms_p50,
            jev_ms_p50,
        } => (
            "latency",
            format!("step {step_ms_p50}ms · jev {jev_ms_p50}ms"),
        ),
        AppEvent::SettingsChanged { .. } => ("settings", "changed".into()),
        AppEvent::ModelsChanged { models } => ("models", format!("{} models", models.models.len())),
        AppEvent::KeyStatus { account, status } => {
            ("key", format!("{account} · {}", key_label(*status)))
        }
        AppEvent::PermissionChanged { kind, status } => {
            ("permission", format!("{kind:?} {status:?}"))
        }
        AppEvent::SoulChanged { .. } => ("soul", "saved".into()),
        AppEvent::UpdateAvailable { version, ready } => (
            "update",
            format!("{version} · {}", if *ready { "ready" } else { "pending" }),
        ),
        AppEvent::Notice { level, code, text } => ("notice", format!("{level:?} {code} · {text}")),
        AppEvent::TaskEnded { id, status } => ("task", format!("{id} ended {status:?}")),
    };
    Activity { kind, detail }
}

/// Plain words for the readiness states, so a screen reader and a monochrome
/// terminal say the same thing the colour would (14 §2).
pub fn health_word(health: Health) -> &'static str {
    match health {
        Health::Ok => "ok",
        Health::Warn => "warn",
        Health::Fail => "FAIL",
        Health::Unknown => "unknown",
    }
}

pub fn health_glyph(health: Health) -> char {
    match health {
        Health::Ok => '•',
        Health::Warn => '!',
        Health::Fail => '×',
        Health::Unknown => '?',
    }
}
