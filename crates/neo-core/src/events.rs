use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use crate::{
    AddressingMode, AskId, ConfirmId, ConversationId, DisplayId, KeyState, MediaJobId, Message,
    MessageId, ProviderAccount, RunId, Settings, Task, TaskId, TaskStatus, TimestampMs, Usage,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListenState {
    Listening,
    Hearing,
    Transcribing,
    Speaking,
    Muted,
    Paused,
    MicLost,
    NoPermission,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageUpdate {
    pub id: MessageId,
    pub text: Option<String>,
    pub task_id: Option<TaskId>,
    pub intake: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TraceItem {
    pub kind: String,
    pub body: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfirmView {
    pub id: ConfirmId,
    pub task_id: TaskId,
    pub cause: String,
    pub action_sentence: String,
    pub context: Option<String>,
    pub estimated_cost: Option<Usage>,
    pub can_remember: bool,
    pub expires_at: TimestampMs,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AskView {
    pub id: AskId,
    pub task_id: TaskId,
    pub question: String,
    pub options: Vec<String>,
    pub voice_window_ends: Option<TimestampMs>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BrowserPresence {
    NotRunning,
    Idle,
    Holding {
        tab_title: String,
        origin: String,
        favicon: Option<String>,
    },
    NeedsSignIn {
        origin: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Assist,
    Design,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaJobView {
    pub id: MediaJobId,
    pub state: String,
    pub progress: Option<f32>,
    pub takes: Vec<String>,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Healthy,
    Degraded,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthView {
    pub openai: Health,
    pub jev: Health,
    pub mic: Health,
    pub ax: Health,
    pub chrome: Health,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpendView {
    pub usd: f64,
    pub exact: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionKind {
    Microphone,
    Accessibility,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionStatus {
    Granted,
    Denied,
    NotDetermined,
    Stale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionVia {
    Card,
    Thread,
    Pill,
    Voice,
    Timeout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateOutcome {
    Confirmed,
    Denied,
    TimedOut,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelRegistryView {
    pub refreshed_at: Option<TimestampMs>,
    pub models: Vec<crate::ModelInfo>,
}

/// Which of the four things a step did, as a value a front end can switch on
/// rather than a sentence it has to parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Browse,
    App,
    Answer,
    Ask,
}

/// What a step is doing, in the fields a card renders and an eval asserts
/// against.
///
/// Structured rather than pre-rendered, because a consumer that only has the
/// line cannot tell a URL from a goal — and an eval that checks "it browsed
/// the right page" has to. [`fmt::Display`] gives the one-line form for the
/// consumers that only want that.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionSummary {
    pub kind: ActionKind,
    /// The URL or the application named; `None` for `answer` and `ask`.
    pub target: Option<String>,
    /// The one-sentence outcome asked of the surface; `None` for `answer` and
    /// `ask`.
    pub goal: Option<String>,
    /// The answer, or the question; `None` for `browse` and `app`.
    pub text: Option<String>,
}

impl fmt::Display for ActionSummary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            ActionKind::Browse | ActionKind::App => {
                let verb = if self.kind == ActionKind::Browse {
                    "browse"
                } else {
                    "app"
                };
                let target = self.target.as_deref().unwrap_or("?");
                let goal = self.goal.as_deref().unwrap_or("?");
                write!(formatter, "{verb} {target} — {goal}")
            }
            ActionKind::Answer => formatter.write_str("answer"),
            ActionKind::Ask => formatter.write_str("ask the user"),
        }
    }
}

/// What a whole turn cost, summed over the model round trips it made.
///
/// The vendors' own usage objects are kept verbatim in the `turns` table;
/// this is the normalised subset a cost view can add up. There is no price:
/// plan-backed work is [`crate::Usd::Unpriced`] (05 §7), and inventing a
/// dollar figure for it would be a number nobody can reconcile with a bill.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Prompt tokens the vendor served from its own cache. Counted apart
    /// because they are billed apart.
    pub cached_input_tokens: u64,
    /// How many model round trips this covers, so an average is possible.
    pub requests: u32,
}

impl TurnUsage {
    /// Read one vendor usage object.
    ///
    /// Both spellings are accepted — Anthropic's `input_tokens`/
    /// `output_tokens` and OpenAI's `prompt_tokens`/`completion_tokens` —
    /// because a turn may be served by either runtime and a cost view that
    /// understood only one would silently read zero for the other. `None`
    /// when the vendor reported nothing recognisable, which is different from
    /// reporting zero.
    #[must_use]
    pub fn from_vendor(usage: &Value) -> Option<Self> {
        let count = |names: &[&str]| -> Option<u64> {
            names
                .iter()
                .find_map(|name| usage.get(*name).and_then(Value::as_u64))
        };
        let input = count(&["input_tokens", "prompt_tokens"]);
        let output = count(&["output_tokens", "completion_tokens"]);
        let cached = count(&["cache_read_input_tokens", "cached_tokens"]);
        if input.is_none() && output.is_none() && cached.is_none() {
            return None;
        }
        Some(Self {
            input_tokens: input.unwrap_or(0),
            output_tokens: output.unwrap_or(0),
            cached_input_tokens: cached.unwrap_or(0),
            requests: 1,
        })
    }

    /// Add another round trip's usage in place. Saturating: a count that
    /// overflowed a `u64` is a number no report could use anyway.
    pub fn add(&mut self, other: Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cached_input_tokens = self
            .cached_input_tokens
            .saturating_add(other.cached_input_tokens);
        self.requests = self.requests.saturating_add(other.requests);
    }

    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }
}

/// Which surface a navigator step drove.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NavSurface {
    Browser,
    App,
}

/// One navigator decision, with the timings and the safety verdicts that
/// explain it.
///
/// The typed text is counted, never carried: a field being typed into may hold
/// a password or a one-time code, and an event stream that kept the characters
/// would be a credential store nobody asked for. The length is enough to see
/// that something was typed and how much.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NavDecision {
    pub surface: NavSurface,
    pub operation: String,
    pub label: Option<String>,
    pub operation_confidence: f32,
    pub target_confidence: Option<f32>,
    pub candidates: usize,
    /// The surface changed under the decision, so it was not executed as
    /// observed.
    pub stale: bool,
    pub typed_chars: Option<usize>,
    pub observe_ms: u64,
    pub jev_ms: u64,
    pub text_ms: u64,
    pub act_ms: u64,
    pub elapsed_ms: u64,
    /// Each safety head and the probability it returned.
    pub safety: Vec<(String, f32)>,
}

impl fmt::Display for NavDecision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let safety: Vec<String> = self
            .safety
            .iter()
            .map(|(head, probability)| format!("{head}={probability:.2}"))
            .collect();
        let label: String = self
            .label
            .clone()
            .unwrap_or_default()
            .chars()
            .take(44)
            .collect();
        let target = self
            .target_confidence
            .map_or_else(|| "-".to_owned(), |confidence| format!("{confidence:.2}"));
        let typed = self
            .typed_chars
            .map_or_else(String::new, |chars| format!("  typed {chars} chars"));
        write!(
            formatter,
            "{:>6} ms  {:<11} {label:<44} p={:.2} tgt={target}  obs {:>3} · jev {:>4} · text {:>4} · act {:>3} ms  [{} cands]{typed}{}  {}",
            self.elapsed_ms,
            self.operation,
            self.operation_confidence,
            self.observe_ms,
            self.jev_ms,
            self.text_ms,
            self.act_ms,
            self.candidates,
            if self.stale { "  STALE" } else { "" },
            safety.join(" "),
        )
    }
}

/// What a [`AppEvent::NavStep`] line is about.
///
/// A run reports more than decisions — it says what it launched, how it ended
/// and what it read — and a front end that wants to show only the decisions
/// should not have to match on prose to find them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NavStepKind {
    /// The surface was opened or brought forward.
    Launch,
    /// One observe → decide → act cycle. Boxed: it is far larger than the
    /// other variants, and an event this common should not pay for that.
    Decision(Box<NavDecision>),
    /// How the run ended.
    Outcome,
    /// What the navigator made of the whole run.
    Summary,
}

/// Where one eval case got to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EvalCaseState {
    Started,
    Passed { runs: u32 },
    Failed { runs: u32, detail: String },
    Skipped { reason: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppEvent {
    ListenState {
        state: ListenState,
        device: Option<String>,
        addressing: AddressingMode,
    },
    Message {
        message: Message,
    },
    MessageUpdated {
        update: MessageUpdate,
    },
    ConversationReset {
        conversation_id: ConversationId,
    },
    /// One agent turn began. `run` is the id every later event of this turn
    /// carries, so a front end that started listening mid-turn can still
    /// group what it sees — and a second observer needs no handshake to
    /// follow along.
    TurnStarted {
        run: RunId,
        conversation: ConversationId,
    },
    /// The model chose an action. `step` counts from zero within the run, and
    /// this fires *before* the action runs: it is what makes a front end show
    /// work in flight rather than a spinner.
    TurnStep {
        run: RunId,
        step: u32,
        thought: String,
        action: ActionSummary,
    },
    /// What the action produced, in the words fed back to the model. A failed
    /// action reports its failure here rather than ending the turn: the model
    /// gets to try something else.
    TurnStepDone {
        run: RunId,
        step: u32,
        observation: String,
        duration_ms: u64,
    },
    /// A line from inside a step. Separate from [`AppEvent::NavStep`] because
    /// a note has no structure to offer: it is prose the step produced.
    TurnNote {
        run: RunId,
        step: u32,
        line: String,
    },
    /// A slice of assistant text as the model produces it. `seq` orders
    /// slices within a run; a front end appends, it never reorders.
    ///
    /// Separate from [`AppEvent::TurnFinished`] because the two answer
    /// different questions: a delta is "here is more of the answer", the
    /// finish is "that was all of it". A front end that only had the finish
    /// showed a spinner for the whole turn and then a wall of text.
    TurnDelta {
        run: RunId,
        seq: u32,
        text: String,
    },
    /// A message the user sent into a turn that was already running.
    ///
    /// Published when the running turn *accepted* the message, so a front end
    /// can show it landing in that turn rather than guessing whether it was
    /// heard. A message the run could not take is a new turn instead, and
    /// arrives as an ordinary [`AppEvent::Message`].
    TurnSteered {
        run: RunId,
        text: String,
    },
    /// What the turn has cost so far, published after each model round trip.
    ///
    /// The running total *including the round trip just completed*, never an
    /// increment: a status line renders the latest event and needs no
    /// arithmetic and no memory of the ones it missed — summing these events
    /// would multiply the bill. [`AppEvent::TurnFinished::usage`] remains the
    /// authoritative final total, so a front end that ignores this variant is
    /// exactly as correct as before; it just shows nothing until the turn
    /// ends.
    TurnCost {
        run: RunId,
        usage: TurnUsage,
    },
    /// The turn ended with something to show the user. `exhausted` says the
    /// step budget ran out rather than the model answering, which is a
    /// different thing to render.
    TurnFinished {
        run: RunId,
        text: String,
        steps: u32,
        exhausted: bool,
        usage: Option<TurnUsage>,
    },
    /// The turn ended in a failure — including a cancelled one. `error` is a
    /// sentence built from our own error types, which cannot format a
    /// `neo_keys::Secret`: no credential can reach it, and nothing here is
    /// echoed from a request.
    ///
    /// `code` is the same failure classified for a machine — `agent_cancelled`,
    /// `agent_graph` and the rest of `neo-agent`'s `error_code`. It exists
    /// because the webview had to tell a stopped turn from a broken one and
    /// the only thing crossing the bridge was the sentence, so it ran
    /// `/cancel/i` over English prose: a reworded message, or a vendor error
    /// containing the word, flipped the card.
    TurnFailed {
        run: RunId,
        error: String,
        code: String,
    },
    /// One navigator step, from an agent turn or a hand-driven `neo nav`.
    ///
    /// `line` is the rendered step exactly as a terminal shows it, so a log
    /// pane needs no formatter; `kind` carries the same thing structured, so a
    /// desktop pane can lay it out in columns. Both, because a front end with
    /// only the string cannot sort by latency, and one with only the struct
    /// has to reimplement the rendering.
    NavStep {
        run: RunId,
        step: u32,
        line: String,
        kind: NavStepKind,
    },
    TaskUpserted {
        task: Task,
    },
    TaskRemoved {
        id: TaskId,
    },
    QueueState {
        paused: bool,
        reasons: Vec<String>,
        idle_wait_ms: Option<u32>,
    },
    Trace {
        task_id: TaskId,
        seq: u32,
        item: TraceItem,
    },
    ConfirmRequest {
        confirm: ConfirmView,
    },
    ConfirmResolved {
        confirm_id: ConfirmId,
        outcome: GateOutcome,
        via: ResolutionVia,
    },
    AskRequest {
        ask: AskView,
    },
    AskResolved {
        ask_id: AskId,
        answer: String,
        via: ResolutionVia,
    },
    Ring {
        display: DisplayId,
        rect: Option<Rect>,
    },
    BrowserPresence {
        presence: BrowserPresence,
    },
    ModeRequest {
        mode: Mode,
        reason: String,
    },
    EnablementOffer {
        pack_id: String,
        task_id: Option<TaskId>,
    },
    MediaJob {
        job: MediaJobView,
    },
    PackChanged {
        pack_id: String,
        enabled: bool,
    },
    Health {
        health: HealthView,
    },
    ProviderAccount {
        account: ProviderAccount,
    },
    Spend {
        today: SpendView,
        task: Option<SpendView>,
    },
    Latency {
        step_ms_p50: u32,
        jev_ms_p50: u32,
    },
    SettingsChanged {
        settings: Box<Settings>,
    },
    ModelsChanged {
        models: ModelRegistryView,
    },
    KeyStatus {
        account: String,
        status: KeyState,
    },
    PermissionChanged {
        kind: PermissionKind,
        status: PermissionStatus,
    },
    SoulChanged {
        saved_at: TimestampMs,
        applies_from_next_task: bool,
    },
    UpdateAvailable {
        version: String,
        ready: bool,
    },
    Notice {
        level: NoticeLevel,
        code: String,
        text: String,
    },
    TaskEnded {
        id: TaskId,
        status: TaskStatus,
    },
    /// One eval case moving through the suite. Suite-level rather than
    /// turn-level: a case may run several turns, and a front end showing
    /// progress wants the case count, not the turns.
    EvalCase {
        run: RunId,
        index: u32,
        total: u32,
        case: String,
        state: EvalCaseState,
    },
}

/// One published event, in order.
///
/// `seq` is monotonic per process and gapless: a subscriber that receives
/// `n + 2` after `n` knows an event was dropped and must re-bootstrap rather
/// than render a thread it cannot trust (14 §4). It is carried here rather
/// than inferred from `broadcast::error::RecvError::Lagged`, because lagging
/// is only one of the ways a front end misses an event — a webview that
/// reconnects misses them without ever having had a receiver.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub seq: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub event: AppEvent,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_wire_format_is_tagged_snake_case() {
        let event = Envelope {
            at: OffsetDateTime::UNIX_EPOCH,
            seq: 7,
            event: AppEvent::QueueState {
                paused: true,
                reasons: vec!["daily_cap".into()],
                idle_wait_ms: None,
            },
        };
        let value = serde_json::to_value(event).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(value["seq"], 7);
        assert_eq!(value["event"]["type"], "queue_state");
        assert_eq!(value["event"]["reasons"][0], "daily_cap");
    }

    /// A cost view must read the same numbers whichever runtime served the
    /// turn; a parser that knew only one vendor's spelling would report zero
    /// for the other and nobody would notice until the bill arrived.
    #[test]
    fn usage_is_read_from_either_vendors_spelling_and_sums() {
        let anthropic = serde_json::json!({
            "input_tokens": 1_200,
            "output_tokens": 300,
            "cache_read_input_tokens": 900,
            "cache_creation_input_tokens": 40
        });
        let openai = serde_json::json!({
            "prompt_tokens": 800,
            "completion_tokens": 100,
            "total_tokens": 900
        });

        let mut total =
            TurnUsage::from_vendor(&anthropic).unwrap_or_else(|| panic!("anthropic usage"));
        assert_eq!(total.input_tokens, 1_200);
        assert_eq!(total.cached_input_tokens, 900);

        total.add(TurnUsage::from_vendor(&openai).unwrap_or_else(|| panic!("openai usage")));
        assert_eq!(total.input_tokens, 2_000);
        assert_eq!(total.output_tokens, 400);
        assert_eq!(total.total_tokens(), 2_400);
        assert_eq!(total.requests, 2);

        // A vendor that reported nothing is different from one that reported
        // zero: the caller still counts the round trip, but it must not
        // invent token counts.
        assert_eq!(TurnUsage::from_vendor(&Value::Null), None);
        assert_eq!(TurnUsage::from_vendor(&serde_json::json!({})), None);
    }

    /// The one-line form is what a log pane shows; it has to name the surface
    /// and the goal, because "browse" alone tells a reader nothing.
    #[test]
    fn an_action_summary_renders_its_target_and_goal() {
        let browse = ActionSummary {
            kind: ActionKind::Browse,
            target: Some("https://example.com/pricing".to_owned()),
            goal: Some("read the per-seat price".to_owned()),
            text: None,
        };
        assert_eq!(
            browse.to_string(),
            "browse https://example.com/pricing — read the per-seat price"
        );

        let ask = ActionSummary {
            kind: ActionKind::Ask,
            target: None,
            goal: None,
            text: Some("which account?".to_owned()),
        };
        assert_eq!(ask.to_string(), "ask the user");
    }
}
