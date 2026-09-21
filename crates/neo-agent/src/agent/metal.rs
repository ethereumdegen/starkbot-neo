//! One agent turn, driven by metalcraft's ReAct graph over a rig model.
//!
//! This replaced a hand-rolled loop that asked the model for one strict-JSON
//! action per step. That loop worked, but it could only ever produce a turn
//! the user watched a spinner through: the model's answer arrived in a single
//! `ask_json` round trip, so there was nothing to show until it was complete,
//! there was no seam to reach a run that was already going, and every vendor
//! tool-calling feature had to be re-expressed as a JSON schema Neo owned.
//!
//! What metalcraft gives us instead is exactly the four things a live chat
//! needs, and all four are why this module exists:
//!
//! - **tokens**, as [`RunEvent::Token`], which become [`AppEvent::TurnDelta`]
//!   so text appears as it is produced;
//! - **a mailbox**, polled at every step boundary, which is how a message
//!   typed *during* a turn reaches that turn ([`Steering`]);
//! - **cancellation** that drops the in-flight future and hands back the
//!   partial state, so a stopped turn keeps what it had already said;
//! - **real tool calls**, so [`run_browser`]/[`run_app`] are described once,
//!   in the shape the vendor already understands.
//!
//! Neo's own vocabulary does not change. Every step is still published as the
//! existing [`AppEvent`] family — `TurnStarted`, `TurnStep`, `NavStep`,
//! `TurnStepDone`, `TurnFinished` — because `neo-eval`, the TUI and the
//! desktop are all built on it, and a second event vocabulary beside the
//! first would be two things to keep in agreement. The navigator is untouched:
//! a tool call lands in `jev-nav` through the same `run_browser`/`run_app`
//! that `neo nav` uses, with the same policy, budgets and safety heads.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt as _;
use metalcraft::rig::client::CompletionClient as _;
use metalcraft::rig::providers::chatgpt;
use metalcraft::rig::providers::openai;
use metalcraft::{
    AgentMessage, AgentOptions, AgentState, AgentUpdate, Executor, GraphError, LlmResponseHook,
    LlmResponseSnapshot, LlmUsage, Mailbox, RunEvent, RunOutcome, Tool, ToolRegistry, UserInput,
    create_react_agent_with_options,
};
use neo_ax::InstalledApp;
use neo_core::{
    ActionKind, ActionSummary, AppEvent, ConversationId, CoreError, Message, MessageId,
    MessageKind, MessageRole, MessageSource, ProviderId, RunId, Settings, TimestampMs, TurnUsage,
};
use serde_json::{Value, json};
use url::Url;

use super::{
    AgentError, AppOptions, BrowserOptions, ChatMessage, ChatRequest, Role, StepRecord, ToolError,
    TurnOutcome, run_app, run_browser,
};
use crate::oauth::{ANTHROPIC_OAUTH, OPENAI_CODEX};
use crate::providers::{ClaudeSubscription, CodexOauthInference};
use crate::runtime::Runtime;

/// What the model is told it is, and what it may do (P2′/P3).
///
/// The surfaces are named, and the absence of a shell is named with them: a
/// model that is not told it has no command execution will try to ask for one,
/// and spend a step finding out.
///
/// Nothing here names a platform any more. The text used to say "this Mac"
/// and list four macOS applications, which is false on half the machines Neo
/// runs on (P16: one product, one policy, Linux and macOS alike) — and a
/// model told it is on a Mac when it is not will reach for a Mac's
/// applications and find none of them. What the machine actually has is not
/// a constant at all, so it is appended at the call site from
/// [`installed_apps_section`] instead of guessed here.
const PREAMBLE: &str = "\
You are Starkbot, a go-to-market marketing and media operator. You get work \
done by operating real applications on this machine: web pages through a \
managed browser, and native applications through the platform's \
accessibility API. You are not a coding assistant and you have no shell, no \
file system and no command execution.

When a task names an application, open that application with the `app` tool \
rather than looking for a web page about it. That is about finding the \
*application* only.

When a task says to match something that already exists — a product, a site, \
a design, another document — find it and read it **first**, before you make \
anything. Its name is the search term; a product's own site usually states \
its colours and marks in text you can read. Every detail the thing you are \
copying already fixes is a detail you do not get to choose, and inventing one \
is the single way a task like this fails while looking finished.

Use a tool when the work is on a screen; answer directly when the \
conversation already contains what is needed — do not open a browser to \
answer a question you know. Write a tool's `goal` the way you would brief a \
person who can see the screen but not your reasoning: one concrete outcome, \
no selectors, no step lists. After each tool call you are told what it \
produced; use that before deciding what to do next. When you have the answer, \
say it in plain prose. If you cannot proceed without the user, say what you \
need and stop.";

/// How many applications the prompt may name (A23).
///
/// A real desktop answers [`neo_ax::inventory`] with far more than a prompt
/// should carry — this machine returns 87, a laptop with a full desktop
/// environment returns roughly 150 — and every settings panel, toolkit demo
/// and D-Bus helper among them is a few hundred tokens per turn to say
/// something that changes only when software is installed.
const APP_LIMIT: usize = 40;

/// The applications this machine can actually open, as a line the model can
/// resolve a name against.
///
/// This is the half of the fix the preamble cannot hold (A23). "Use
/// degen-paint" is only a website to a model that was never told
/// `dev.degenpaint.studio` is installed, and the baseline run spent its whole
/// budget browsing for one. [`neo_ax::inventory`] is the single source of
/// truth for what is here; this function only renders it.
fn installed_apps_section() -> String {
    render_installed_apps(&neo_ax::inventory())
}

/// [`installed_apps_section`] over a list it is handed, so the empty machine —
/// a platform with no accessibility backend — is a case a test can state
/// rather than a case that needs an OS without applications on it.
///
/// An empty inventory renders the empty string, not a heading with nothing
/// under it: a prompt that says "applications on this machine:" and then
/// stops is worse than silence, because the model reads it as "none", and
/// [`neo_ax::lookup`] may still resolve a name the inventory could not list.
///
/// Which `APP_LIMIT` survive the cut is decided here rather than taken from
/// the inventory's own order, for two reasons found by rendering this machine:
///
/// - [`neo_ax::inventory`] sorts by `String` order, which is byte order, so
///   every lowercase-named application — `foot`, `gimp`, `imv`, `chromium`,
///   and `degen-paint Studio` itself — sorts behind every capitalised one.
///   A cut at 40 was therefore not "the first 40 alphabetically" but "the
///   first 40 that happen to start with a capital", and it dropped the one
///   application the failing task named. Ordering is case-insensitive here.
/// - [`InstalledApp`] carries no category, so there is no field that says
///   "this is a work target" — but it carries the path it was found at, and
///   an entry under the user's own home was installed deliberately by this
///   person, while `/usr/share/applications` is whatever the distribution
///   shipped. That is real evidence rather than a hardcoded list of names,
///   which is the macOS list this preamble has just stopped having, so the
///   user's own applications are named first.
///
/// The rest still exists: the closing sentence says the list is partial, and
/// [`neo_ax::lookup`] resolves a name the model passes through anyway.
fn render_installed_apps(apps: &[InstalledApp]) -> String {
    if apps.is_empty() {
        return String::new();
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut ranked: Vec<&InstalledApp> = apps.iter().collect();
    ranked.sort_by_cached_key(|app| {
        let mine = !home.as_ref().is_some_and(|home| app.path.starts_with(home));
        // The id breaks ties, so two applications sharing a display name
        // render in the same order on every turn.
        (mine, app.name.to_lowercase(), app.id.clone())
    });

    let listed = ranked
        .iter()
        .take(APP_LIMIT)
        .map(|app| format!("{} ({})", app.name, app.id))
        .collect::<Vec<_>>()
        .join("; ");
    let mut section = String::from(
        "\n\nApplications installed on this machine, as human name then the \
id to give the `app` tool's target: ",
    );
    section.push_str(&listed);
    section.push('.');
    if ranked.len() > APP_LIMIT {
        section.push_str(
            " This list is partial; an application that is not on it may still \
be installed, and can be named to the `app` tool directly.",
        );
    }
    section
}

/// How long an answer may grow in memory before the thread holds it too.
///
/// Every token could be written through, but that is one transaction per
/// token for a row nobody reads until the turn ends; a quarter of a second
/// bounds what a crash can lose to roughly one sentence.
const FLUSH_EVERY: Duration = Duration::from_millis(250);

/// The note a stopped turn publishes before it finishes.
///
/// [`AppEvent::TurnFinished`] carries no disposition — a turn that answered
/// and a turn that was stopped look the same on it — so this line is how a
/// front end tells them apart. Stable text, matched by both front ends.
pub const STOPPED: &str = "stopped";

/// The note a step that was interrupted mid-flight publishes. Unchanged
/// wording: the TUI renders a card by it.
const STOPPED_STEP: &str = "stopped before this step finished";

/// How a stopped tool call tells this module it was stopped.
///
/// metalcraft has no cancellation variant to return: a failing node's only
/// channel out is a string — `GraphError::Node { message }`, stringified
/// again into `RunOutcome::Failed { error }` — so a cancellation cannot
/// travel up as a type. It travels as this marker instead, written in
/// exactly one place ([`observed`]) and read in exactly one place
/// ([`node_failure`]).
///
/// The marker exists because the classification used to be
/// `error.to_string().contains("cancelled")` against the prose `"cancelled"`:
/// any reword of the tool layer, and any vendor error that happened to
/// mention the word, changed a stop into a failure or a failure into a stop.
/// Nothing a vendor writes contains this.
const CANCELLED_MARKER: &str = "starkbot.cancelled";

/// The node a ReAct graph calls the model in. The mailbox may only deliver
/// here — see [`mailbox`].
const AGENT_NODE: &str = "agent";

// ---------------------------------------------------------------------------
// Steering
// ---------------------------------------------------------------------------

/// A live turn's inbox: what the user said while it was still working.
///
/// A running graph is otherwise closed — only its own nodes change its state —
/// so without this a person who types "no, stop, do the other thing" can only
/// be heard after the turn ends. The queue is drained by [`mailbox`] at the
/// next step boundary, which is the only place the conversation is coherent
/// enough to add to.
pub struct Steering {
    conversation: ConversationId,
    queue: Mutex<Vec<String>>,
}

impl Steering {
    fn new(conversation: ConversationId) -> Self {
        Self {
            conversation,
            queue: Mutex::new(Vec::new()),
        }
    }

    /// The thread this run is answering in, so a steered message is recorded
    /// against the same conversation without the caller having to know it.
    #[must_use]
    pub fn conversation(&self) -> ConversationId {
        self.conversation
    }

    /// Hand the run a message. Queued, not delivered: the run absorbs it at
    /// its next step boundary.
    pub fn post(&self, text: &str) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.push(text.to_owned());
        }
    }

    fn drain(&self) -> Vec<String> {
        self.queue
            .lock()
            .map(|mut queue| std::mem::take(&mut *queue))
            .unwrap_or_default()
    }
}

/// Where the mailbox may deliver, and what it delivers.
///
/// `event.next` is checked rather than trusted: metalcraft polls the mailbox
/// after *every* step, including between a model turn that asked for tool
/// calls and the node that answers them. A user message injected there
/// produces the orphaned-tool-call history the Responses API rejects with a
/// 400, so a message waits until the model itself is about to run.
fn mailbox(steering: Arc<Steering>) -> Mailbox<AgentState> {
    Arc::new(move |_state, event| {
        if event.next != AGENT_NODE {
            return Vec::new();
        }
        steering
            .drain()
            .into_iter()
            .map(|text| AgentUpdate::UserMessage(UserInput::new(text)))
            .collect()
    })
}

// ---------------------------------------------------------------------------
// Reporter
// ---------------------------------------------------------------------------

/// What a turn in flight has produced, and the one place it is announced.
///
/// Progress is published rather than handed to a callback: a callback has
/// exactly one owner, and a turn has as many watchers as the user has windows
/// open. The reporter is shared by the stream loop, the model hook and every
/// tool, so the step numbering, the delta ordering and the running cost all
/// come from one counter each and cannot disagree.
///
/// # Why a tool's card is queued rather than published where it happens
///
/// The graph runs in its own task, and the token events this turn maps
/// arrive through a channel — so a tool that published directly raced ahead
/// of the very text that asked for it: a model that said "let me check the
/// deck" and then called `app` produced the card *before* the sentence,
/// because the sentence was still in the queue. Cards therefore go through
/// [`Cards`], and the stream loop is the single point everything a front end
/// sees leaves through, in one order.
pub(crate) struct Reporter {
    runtime: Arc<Runtime>,
    run: RunId,
    conversation: ConversationId,
    /// The timestamp this turn's rows are written with, so a slice that
    /// lands late never reorders the thread.
    at: TimestampMs,
    /// The next step index. Counted here rather than read off metalcraft's
    /// `StepEvent`, which carries node names and no index.
    steps: AtomicU32,
    /// The next delta sequence number.
    seq: AtomicU32,
    answer: Mutex<Answer>,
    tally: Mutex<Tally>,
    records: Mutex<Vec<StepRecord>>,
    cards: tokio::sync::mpsc::UnboundedSender<Queued>,
    /// The turn's span, captured on the task that opened it.
    ///
    /// The graph runs on a task metalcraft spawns, and a spawned task does
    /// not inherit the task-local `neo_otel` keeps the open span in, so a
    /// tool's own spans would otherwise each start a trace of their own —
    /// leaving `invoke_agent` with none of the work it describes under it.
    /// Every tool re-enters this in [`observed`].
    trace: neo_otel::Attached,
}

/// The queue of card events a turn's tools have produced. Drained by the
/// stream loop; see [`Reporter`].
pub(crate) type Cards = tokio::sync::mpsc::UnboundedReceiver<Queued>;

/// A card waiting to be published, and the tool waiting for it to be.
///
/// **The acknowledgement is what keeps a card both live and in order.** The
/// graph runs in its own task and can outrun this turn's consumer by a whole
/// model call, so a tool that queued and carried on had its card published
/// after the answer that came *later*. Waiting for the loop to apply it means
/// the loop drains everything older first — the tokens of the call that asked
/// for this tool — and then publishes the card immediately, because the graph
/// cannot produce anything newer while the tool is waiting.
pub(crate) struct Queued {
    card: Card,
    applied: tokio::sync::oneshot::Sender<()>,
}

/// One thing a tool has to say about itself, in the order it said it.
enum Card {
    Started {
        step: u32,
        summary: ActionSummary,
    },
    Done {
        step: u32,
        summary: ActionSummary,
        observation: String,
        duration_ms: u64,
    },
    Note(String),
}

/// The assistant message as it is being written.
struct Answer {
    /// Everything published as a delta so far — the turn's answer.
    said: String,
    /// How many bytes of `said` the store already holds.
    flushed: usize,
    /// The row, once the first slice created it.
    id: Option<MessageId>,
    last_flush: Instant,
}

impl Reporter {
    /// Announce a turn and start counting it.
    ///
    /// The announcement happens here, on construction, so a run that is
    /// stopped before it reaches the model has still been seen: a front end
    /// that subscribed before the call needs `TurnStarted` to exist even for a
    /// turn that produces nothing else.
    pub(crate) fn new(
        runtime: Arc<Runtime>,
        request: &ChatRequest,
        settings: &Settings,
    ) -> Result<(Self, Cards), AgentError> {
        let at = crate::runtime::now_ms()?;
        let (cards, queue) = tokio::sync::mpsc::unbounded_channel();
        let reporter = Self {
            runtime,
            run: request.run,
            conversation: request.conversation,
            at,
            steps: AtomicU32::new(0),
            seq: AtomicU32::new(0),
            answer: Mutex::new(Answer {
                said: String::new(),
                flushed: 0,
                id: None,
                last_flush: Instant::now(),
            }),
            tally: Mutex::new(Tally::new(settings, at)),
            records: Mutex::new(Vec::new()),
            cards,
            trace: neo_otel::attached(),
        };
        reporter.runtime.publish(AppEvent::TurnStarted {
            run: reporter.run,
            conversation: reporter.conversation,
        });
        Ok((reporter, queue))
    }

    /// Hand a card to the stream loop and wait for it to go out.
    ///
    /// A queue nobody drains means the loop has gone, which means the turn is
    /// over: there is nothing left to say and nothing to wait for.
    async fn queue(&self, card: Card) {
        let (applied, published) = tokio::sync::oneshot::channel();
        if self.cards.send(Queued { card, applied }).is_err() {
            return;
        }
        let _ = published.await;
    }

    /// Publish one queued card, in the loop's order, and release the tool
    /// that is waiting for it.
    pub(crate) fn apply(&self, queued: Queued) {
        let Queued { card, applied } = queued;
        self.publish_card(card);
        let _ = applied.send(());
    }

    /// Publish one card.
    fn publish_card(&self, card: Card) {
        match card {
            Card::Started { step, summary } => {
                self.runtime.publish(AppEvent::TurnStep {
                    run: self.run,
                    step,
                    // metalcraft has no separate per-call rationale to
                    // carry, and an empty line renders as an empty card: the
                    // intent a card shows is the action itself, rendered
                    // once by `ActionSummary`.
                    thought: summary.to_string(),
                    action: summary,
                });
            }
            Card::Done {
                step,
                summary,
                observation,
                duration_ms,
            } => {
                self.runtime.publish(AppEvent::TurnStepDone {
                    run: self.run,
                    step,
                    observation: observation.clone(),
                    duration_ms,
                });
                self.record(
                    MessageRole::Tool,
                    MessageKind::Result,
                    &observation,
                    json!({"run": self.run.to_string(), "step": step}),
                );
                if let Ok(mut records) = self.records.lock() {
                    records.push(StepRecord {
                        action: summary,
                        observation,
                        duration_ms,
                    });
                }
            }
            Card::Note(line) => {
                self.runtime.publish(AppEvent::TurnNote {
                    run: self.run,
                    step: self.steps.load(Ordering::Relaxed),
                    line,
                });
            }
        }
    }

    /// One slice of the answer, as the model produces it.
    pub(crate) fn delta(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        self.runtime.publish(AppEvent::TurnDelta {
            run: self.run,
            seq,
            text: text.to_owned(),
        });
        let due = match self.answer.lock() {
            Ok(mut answer) => {
                answer.said.push_str(text);
                answer.last_flush.elapsed() >= FLUSH_EVERY
            }
            Err(_) => false,
        };
        if due {
            self.flush();
        }
    }

    /// A whole answer, when it has to be published as one slice.
    ///
    /// Text already published as deltas must not arrive twice, which is what
    /// the suffix check rules out: the answer a run ends with is normally
    /// exactly what the tokens already said, so this is normally a no-op. It
    /// earns its place for a provider that produced no text deltas at all —
    /// then the answer reaches the user here or not at all.
    pub(crate) fn whole_answer(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let streamed = self
            .answer
            .lock()
            .map(|answer| answer.said.ends_with(text))
            .unwrap_or(false);
        if !streamed {
            self.delta(text);
        }
    }

    /// One model round trip, with what it cost.
    ///
    /// The running total is published, not the increment: a status line
    /// renders the latest event, and a front end that missed one is still
    /// right. The same accumulator seals the turn, so the final number
    /// cannot disagree with the last one shown.
    pub(crate) fn round_trip(&self, usage: &LlmUsage) {
        let total = match self.tally.lock() {
            Ok(mut tally) => {
                tally.add(usage);
                tally.usage
            }
            Err(_) => return,
        };
        self.runtime.publish(AppEvent::TurnCost {
            run: self.run,
            usage: total,
        });
    }

    /// A tool call is starting. Returns the step index its completion must
    /// carry, so a card opened here is the card closed later — the index is
    /// assigned where the call happens, not where it is published, so two
    /// tools in one batch cannot swap numbers.
    async fn step_started(&self, summary: &ActionSummary) -> u32 {
        let step = self.steps.fetch_add(1, Ordering::Relaxed);
        self.queue(Card::Started {
            step,
            summary: summary.clone(),
        })
        .await;
        step
    }

    /// A tool call is done, with the sentence the model reads next.
    async fn step_done(
        &self,
        step: u32,
        summary: &ActionSummary,
        observation: &str,
        duration_ms: u64,
    ) {
        self.queue(Card::Done {
            step,
            summary: summary.clone(),
            observation: observation.to_owned(),
            duration_ms,
        })
        .await;
    }

    /// A line from inside a tool that has no structure to offer.
    async fn step_note(&self, line: &str) {
        self.queue(Card::Note(line.to_owned())).await;
    }

    /// A line about the turn itself, published where it is said: the stream
    /// loop is already the caller, so there is no queue to go through.
    fn note(&self, line: &str) {
        self.publish_card(Card::Note(line.to_owned()));
    }

    /// Seal the turn: persist what it said, record what it cost, publish the
    /// ending, and hand back the outcome.
    ///
    /// Every exit goes through here — answered, budget spent, stopped — so the
    /// `turns` row is written whether or not the turn succeeded: the tokens
    /// were spent either way, and a cost view that counted only answers would
    /// understate the bill.
    pub(crate) fn finish(&self, exhausted: bool, cancelled: bool) -> TurnOutcome {
        self.flush();
        let usage = match self.tally.lock() {
            Ok(tally) => tally.record(&self.runtime, self.conversation),
            Err(_) => None,
        };
        let text = self.said();
        let steps = self
            .records
            .lock()
            .map(|records| records.clone())
            .unwrap_or_default();
        neo_otel::annotate(vec![
            ("starkbot.steps", json!(steps.len())),
            ("starkbot.exhausted", json!(exhausted)),
            ("starkbot.cancelled", json!(cancelled)),
            ("starkbot.answer", json!(text)),
        ]);
        self.runtime.publish(AppEvent::TurnFinished {
            run: self.run,
            text: text.clone(),
            steps: u32::try_from(steps.len()).unwrap_or(u32::MAX),
            exhausted,
            usage,
        });
        self.announce_answer(&text);
        TurnOutcome {
            run: self.run,
            text,
            steps,
            exhausted,
            cancelled,
            usage,
        }
    }

    /// Seal a turn that broke. The partial answer is still persisted and
    /// announced: a failure halfway through a sentence should leave the
    /// sentence in the thread, not an empty bubble.
    ///
    /// `code` is [`error_code`], which is why it exists: a front end telling
    /// a stopped turn from a broken one used to match a regex over `error`,
    /// so any reworded message reclassified the card.
    pub(crate) fn fail(&self, error: &AgentError) {
        self.flush();
        if let Ok(tally) = self.tally.lock() {
            tally.record(&self.runtime, self.conversation);
        }
        let code = error_code(error);
        neo_otel::annotate(vec![("starkbot.error_code", json!(code))]);
        neo_otel::fail(&error.to_string());
        self.runtime.publish(AppEvent::TurnFailed {
            run: self.run,
            error: error.to_string(),
            code: code.to_owned(),
        });
        let text = self.said();
        self.announce_answer(&text);
    }

    /// What the turn has said so far.
    fn said(&self) -> String {
        self.answer
            .lock()
            .map(|answer| answer.said.clone())
            .unwrap_or_default()
    }

    /// The runtime a tool runs against. The tools already hold the reporter;
    /// handing them the runtime separately would be the same `Arc` twice.
    fn runtime(&self) -> &Arc<Runtime> {
        &self.runtime
    }

    /// Write the slices the store does not hold yet.
    ///
    /// A store failure is logged, never fatal: a turn that is answering must
    /// not be killed because the thread could not be written, and the next
    /// flush carries the same bytes again.
    fn flush(&self) {
        let Ok(answer) = self.answer.lock() else {
            return;
        };
        let pending = answer.said[answer.flushed..].to_owned();
        let upto = answer.said.len();
        drop(answer);
        if pending.is_empty() {
            return;
        }
        match self.runtime.grow_agent_answer(&self.answer_row(), &pending) {
            Ok(id) => {
                if let Ok(mut answer) = self.answer.lock() {
                    answer.flushed = upto;
                    answer.id = Some(id);
                    answer.last_flush = Instant::now();
                }
            }
            Err(error) => {
                tracing::warn!(%error, "the thread could not be grown with an answer slice");
            }
        }
    }

    /// Announce the assistant row exactly once, at the end.
    ///
    /// While the turn runs, the only assistant-text events are deltas — a
    /// front end appends those into one bubble — so announcing the row per
    /// slice would paint the answer twice. The row is constructed rather than
    /// read back: every field of it was written from here.
    fn announce_answer(&self, text: &str) {
        let Some(id) = self.answer.lock().ok().and_then(|answer| answer.id) else {
            return;
        };
        self.runtime.publish(AppEvent::Message {
            message: Message {
                id,
                conversation_id: self.conversation,
                role: MessageRole::Assistant,
                source: MessageSource::System,
                kind: MessageKind::Answer,
                text: text.to_owned(),
                at: self.at,
                task_id: None,
                spoken: false,
                meta: Some(json!({"run": self.run.to_string()})),
            },
        });
    }

    /// The row one answer grows in. `meta.run` is its identity: one run
    /// writes one assistant row.
    fn answer_row(&self) -> neo_store::NewMessage {
        neo_store::NewMessage::new(
            self.conversation,
            MessageRole::Assistant,
            MessageSource::System,
            "",
            self.at,
        )
        .with_kind(MessageKind::Answer)
        .with_meta(json!({"run": self.run.to_string()}))
    }

    /// Append one row of this turn and announce it. Failures are logged for
    /// the same reason [`Reporter::flush`]'s are.
    fn record(&self, role: MessageRole, kind: MessageKind, text: &str, meta: Value) {
        let source = if role == MessageRole::User {
            MessageSource::Typed
        } else {
            MessageSource::System
        };
        let message = neo_store::NewMessage::new(self.conversation, role, source, text, self.at)
            .with_kind(kind)
            .with_meta(meta);
        if let Err(error) = self.runtime.record_agent_message(message) {
            tracing::warn!(%error, "a turn's message could not be recorded");
        }
    }
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

/// Drive a web page: the `browse` tool.
struct Browse {
    reporter: Arc<Reporter>,
    settings: Settings,
    run: RunId,
    cancel: tokio_util::sync::CancellationToken,
    /// The screen hold this turn runs inside, if any — see
    /// [`ChatRequest::screen`]. A headed browse would otherwise be refused
    /// the screen by the lease its own caller holds.
    screen: Option<crate::screen::ScreenScope>,
}

#[async_trait::async_trait]
impl Tool for Browse {
    fn name(&self) -> &str {
        "browse"
    }

    fn description(&self) -> &str {
        "Open a web page in a managed Chrome and pursue one goal on it. A \
         navigator drives the page and reports back what it did and what the \
         page said. Use this for anything on the web."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "the page to open" },
                "goal": {
                    "type": "string",
                    "description": "the outcome, in one sentence: what should be true when this is done"
                }
            },
            "required": ["url", "goal"],
            "additionalProperties": false
        })
    }

    async fn call(&self, args: Value) -> metalcraft::Result<Value> {
        let url = field(&args, "url");
        let goal = field(&args, "goal");
        let summary = ActionSummary {
            kind: ActionKind::Browse,
            target: url.clone(),
            goal: goal.clone(),
            text: None,
        };
        let reporter = Arc::clone(&self.reporter);
        observed(&reporter, summary, async {
            let (url, goal) = required(url, goal, "browse", "url")?;
            // The unattended options: headless Chrome on a throwaway profile,
            // no attachment to a browser the user is using, and the confirm
            // threshold from settings — an agent turn has no confirm card, so
            // the safety gate must fail closed exactly as `neo nav` does (A9).
            let mut options = BrowserOptions::unattended(&self.settings, &url, &goal);
            options.screen = self.screen;
            let run =
                run_browser(self.reporter.runtime(), &options, self.run, &self.cancel).await?;
            Ok(run.observation)
        })
        .await
    }
}

/// Drive a native macOS application: the `app` tool.
struct App {
    reporter: Arc<Reporter>,
    settings: Settings,
    run: RunId,
    cancel: tokio_util::sync::CancellationToken,
    /// The screen hold this turn runs inside, if any — see
    /// [`ChatRequest::screen`]. An app run always takes the screen, so an
    /// eval case inside its suite's lease depends on this.
    screen: Option<crate::screen::ScreenScope>,
}

#[async_trait::async_trait]
impl Tool for App {
    fn name(&self) -> &str {
        "app"
    }

    fn description(&self) -> &str {
        "Bring a native macOS application to the front and pursue one goal in \
         it through accessibility. Use this for documents, spreadsheets, \
         presentations and media tools."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "app": {
                    "type": "string",
                    "description": "the application: a name, a bundle id, or a pid"
                },
                "goal": {
                    "type": "string",
                    "description": "the outcome, in one sentence: what should be true when this is done"
                }
            },
            "required": ["app", "goal"],
            "additionalProperties": false
        })
    }

    async fn call(&self, args: Value) -> metalcraft::Result<Value> {
        let app = field(&args, "app");
        let goal = field(&args, "goal");
        let summary = ActionSummary {
            kind: ActionKind::App,
            target: app.clone(),
            goal: goal.clone(),
            text: None,
        };
        let reporter = Arc::clone(&self.reporter);
        observed(&reporter, summary, async {
            let (app, goal) = required(app, goal, "app", "app")?;
            let mut options = AppOptions::unattended(&self.settings, &app, &goal);
            options.screen = self.screen;
            let run = run_app(self.reporter.runtime(), &options, self.run, &self.cancel).await?;
            Ok(run.observation)
        })
        .await
    }
}

/// Run one of an installed pack's routines: the `routine` tool (06 §4.2).
///
/// The reason this exists beside `app`: an `app` call is one navigator run
/// against one screen, and a sequence whose state does not outlive a run
/// cannot be spelled as several of them. degen-paint's command palette closes
/// the moment focus leaves it, so *choose an op* and *fill the form it just
/// opened* are not two goals — they are one routine.
struct RunRoutine {
    reporter: Arc<Reporter>,
    run: RunId,
    cancel: tokio_util::sync::CancellationToken,
    screen: Option<crate::screen::ScreenScope>,
}

#[async_trait::async_trait]
impl Tool for RunRoutine {
    fn name(&self) -> &str {
        "routine"
    }

    fn description(&self) -> &str {
        "Run a named recipe an installed application shipped for itself. \
         Prefer this over `app` whenever a routine covers the work: its steps \
         run back to back against one screen, so a dialog or a form one step \
         opens is still open for the next. The routines available on this \
         machine are listed above, each with the parameters it takes; pass \
         every one it names in `params`, for example \
         {\"name\": \"media-apps/dp-run-op\", \"params\": {\"op\": \"vector.object.add-ellipse\", \
         \"values\": \"cx 512, cy 512, rx 360, fill #ffffff\"}}."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "the routine, as `<pack>/<name>` or by its bare name"
                },
                "params": {
                    "type": "object",
                    "description": "the routine's parameters, as its instructions describe them",
                    "additionalProperties": true
                }
            },
            "required": ["name"],
            "additionalProperties": false
        })
    }

    async fn call(&self, args: Value) -> metalcraft::Result<Value> {
        let name = field(&args, "name");
        let params = args
            .get("params")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        // The card says which routine and with what, because a routine call
        // with its parameters left out looks identical to one without them.
        let summary = ActionSummary {
            kind: ActionKind::App,
            target: name.clone(),
            goal: Some(Value::Object(params.clone()).to_string()),
            text: None,
        };
        let reporter = Arc::clone(&self.reporter);
        observed(&reporter, summary, async {
            let name = name.ok_or_else(|| ToolError::App {
                app: "?".to_owned(),
                detail: "`routine` needs the routine's `name`".to_owned(),
            })?;
            let routine = crate::routines::find(&name).ok_or_else(|| ToolError::App {
                app: name.clone(),
                detail: format!(
                    "no routine is named `{name}`; the installed ones are listed \
                         in the application's instructions"
                ),
            })?;
            let outcome = crate::routines::run(
                self.reporter.runtime(),
                &routine,
                &params,
                self.run,
                &self.cancel,
                self.screen,
            )
            .await
            .map_err(|error| ToolError::App {
                app: routine.id(),
                detail: error.to_string(),
            })?;
            Ok(render_routine(&outcome))
        })
        .await
    }
}

/// What the model is told a routine did.
///
/// The steps are named and so is the disposition, because a routine that ran
/// every step and failed its own verify sentence is a different thing from
/// one that fell over in the middle, and the recovery differs: the first
/// needs checking, the second needs redoing.
fn render_routine(run: &crate::routines::RoutineRun) -> String {
    let mut out = format!("routine {} — {:?}\n", run.routine, run.disposition);
    for step in &run.steps {
        out.push_str(&format!("  {step}\n"));
    }
    if let Some(detail) = &run.detail {
        out.push_str(&format!("stopped: {detail}\n"));
    }
    if !run.observation.trim().is_empty() {
        out.push_str("\nWhat the surface says:\n");
        out.push_str(run.observation.trim());
    }
    out
}

/// Look at an application without touching it: the `ax` tool.
///
/// The read-only half of the accessibility surface `neo ax` exposes. It takes
/// no keyboard lease and changes nothing, which is why it is offered to the
/// model at all: "what is on this screen" is otherwise a question that can
/// only be answered by starting a run that acts.
struct Inspect {
    reporter: Arc<Reporter>,
    run: RunId,
    cancel: tokio_util::sync::CancellationToken,
}

#[async_trait::async_trait]
impl Tool for Inspect {
    fn name(&self) -> &str {
        "ax"
    }

    fn description(&self) -> &str {
        "Read what an application is showing right now — its window, its \
         visible text and how many controls it exposes — without clicking or \
         typing anything. Use it to check the state of an app before or after \
         acting on it."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "app": {
                    "type": "string",
                    "description": "the application to look at: a name, a bundle id, or a pid"
                }
            },
            "required": ["app"],
            "additionalProperties": false
        })
    }

    async fn call(&self, args: Value) -> metalcraft::Result<Value> {
        let app = field(&args, "app");
        // Reported as an `app` step: the wire vocabulary has four action
        // kinds and none of them is "inspect", and a read-only look at an
        // application is still work on that application as far as a card is
        // concerned.
        let summary = ActionSummary {
            kind: ActionKind::App,
            target: app.clone(),
            goal: Some("read what it is showing".to_owned()),
            text: None,
        };
        let reporter = Arc::clone(&self.reporter);
        observed(&reporter, summary, async {
            let app = app.ok_or_else(|| ToolError::App {
                app: "?".to_owned(),
                detail: "`ax` needs an `app`".to_owned(),
            })?;
            let request = crate::ax::AxRequest::Table { app: app.clone() };
            let response = crate::ax::ax(self.reporter.runtime(), request, self.run, &self.cancel)
                .await
                .map_err(|error| ToolError::App {
                    app: app.clone(),
                    detail: error.to_string(),
                })?;
            Ok(describe_surface(&app, &response))
        })
        .await
    }
}

/// One tool call as the front end and the model both see it.
///
/// Announce, run, announce what it produced, and hand the model one sentence.
/// A failed call is an observation rather than the end of the turn — the model
/// gets to try something else — and keeps the `that failed:` prefix a front
/// end renders a red card by. A *stopped* call is the end of the turn: feeding
/// "you were stopped" back to the model would have it choose another action,
/// which is the opposite of stopping.
async fn observed(
    reporter: &Arc<Reporter>,
    summary: ActionSummary,
    work: impl Future<Output = Result<String, ToolError>>,
) -> metalcraft::Result<Value> {
    let step = reporter.step_started(&summary).await;
    let started = Instant::now();
    // One span per tool call, re-entering the turn's: the graph runs on a
    // task metalcraft spawns, so without the attach this span — and the
    // `navigate`/`jev` spans the work opens under it — would each root a
    // trace of their own instead of hanging under `invoke_agent`.
    // Attribute names are the GenAI conventions', the same ones the
    // navigator's spans use, so a reader renders this row without knowing
    // anything about Neo.
    let span = neo_otel::SpanBuilder::internal("execute_tool")
        .text("gen_ai.operation.name", "execute_tool")
        .text("gen_ai.tool.name", tool_name(summary.kind))
        .int("gen_ai.tool.call.id", step)
        .maybe_text("starkbot.target", summary.target.clone())
        .maybe_text("starkbot.goal", summary.goal.clone());
    let result = neo_otel::attach(
        reporter.trace.clone(),
        neo_otel::in_span(span, async {
            let result = work.await;
            // Annotated here, while the span is still open: the observation
            // is what this call produced, and it belongs on the call.
            match &result {
                Ok(observation) => {
                    neo_otel::annotate(vec![("starkbot.observation", json!(observation))]);
                }
                Err(ToolError::Cancelled(_)) => {}
                Err(error) => {
                    neo_otel::annotate(vec![(
                        "starkbot.observation",
                        json!(format!("that failed: {error}")),
                    )]);
                    neo_otel::fail(&error.to_string());
                }
            }
            result
        }),
    )
    .await;
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    match result {
        Ok(observation) => {
            reporter
                .step_done(step, &summary, &observation, duration_ms)
                .await;
            Ok(Value::String(observation))
        }
        Err(ToolError::Cancelled(_)) => {
            reporter.step_note(STOPPED_STEP).await;
            // The only place the marker is written. See [`CANCELLED_MARKER`].
            Err(GraphError::Node {
                node: "tools".to_owned(),
                message: CANCELLED_MARKER.to_owned(),
            })
        }
        Err(error) => {
            let observation = format!("that failed: {error}");
            reporter
                .step_done(step, &summary, &observation, duration_ms)
                .await;
            Ok(Value::String(observation))
        }
    }
}

/// The tool a step's action kind came from, for the span that reports it.
///
/// `ax` reports itself as an `app` step — the wire vocabulary has four
/// action kinds and none of them is "inspect" — so the two share a name
/// here as they already do on a card.
const fn tool_name(kind: ActionKind) -> &'static str {
    match kind {
        ActionKind::Browse => "browse",
        ActionKind::App => "app",
        ActionKind::Answer => "answer",
        ActionKind::Ask => "ask",
    }
}

/// One string argument, absent when the model left it out or sent it empty —
/// which are the same thing to a tool that has to open a URL.
fn field(args: &Value, name: &str) -> Option<String> {
    args.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Both arguments of a two-argument surface tool, or the failure that names
/// the missing one. The schema cannot express "and not empty", so this does.
fn required(
    target: Option<String>,
    goal: Option<String>,
    tool: &str,
    target_name: &str,
) -> Result<(String, String), ToolError> {
    match (target, goal) {
        (Some(target), Some(goal)) => Ok((target, goal)),
        (target, _) => Err(ToolError::App {
            app: target.unwrap_or_else(|| "?".to_owned()),
            detail: format!("`{tool}` needs both an `{target_name}` and a `goal`"),
        }),
    }
}

/// What an application is showing, in the words the model reads next.
///
/// The element table is up to 250 rows and 6,000 characters of text, which is
/// a document rather than an observation, so this reports the shape and the
/// readable text and leaves the rows to the navigator that would act on them.
fn describe_surface(app: &str, response: &crate::ax::AxResponse) -> String {
    match response {
        crate::ax::AxResponse::Table(table) => {
            let truncated = if table.truncated { ", truncated" } else { "" };
            format!(
                "{app} shows {} controls and {} characters of text{truncated}. Text: {}",
                table.elements.len(),
                table.text.chars().count(),
                clamp(&table.text)
            )
        }
        // `ax` is asked for a table and answers with one; the other variants
        // exist for the hand-driven CLI surface.
        other => format!(
            "{app}: {}",
            serde_json::to_string(other).unwrap_or_default()
        ),
    }
}

/// Keep an observation bounded without cutting a character in half.
fn clamp(text: &str) -> String {
    const LIMIT: usize = 2_000;
    if text.len() <= LIMIT {
        return text.to_owned();
    }
    let mut end = LIMIT;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

// ---------------------------------------------------------------------------
// The turn
// ---------------------------------------------------------------------------

/// Run one turn on the model the settings select.
///
/// Three inference runtimes reach the graph, and they differ only in how the
/// model is built: `openai` on Neo's own API key, `anthropic-oauth` on the
/// Claude Pro/Max subscription, `openai-codex` on the ChatGPT plan. All three
/// are `rig` completion models, so the graph, the tools, the steering seam
/// and every published event below this function are identical — which is
/// why the subscriptions were worth adding here rather than as a second loop
/// beside the first.
///
/// Each arm calls the same generic [`drive`] with its own model type, so each
/// is monomorphised against its own vendor with no dynamic dispatch on the
/// hot token path.
///
/// # Errors
///
/// Fails when the selected runtime's credential is missing, when the selected
/// runtime is one this path does not serve, when the graph cannot be built,
/// or when a node fails.
pub(crate) async fn turn(
    runtime: &Arc<Runtime>,
    request: &ChatRequest,
    settings: &Settings,
) -> Result<TurnOutcome, AgentError> {
    let (reporter, cards) = Reporter::new(Arc::clone(runtime), request, settings)?;
    let reporter = Arc::new(reporter);
    // A turn that is already stopped never reaches the Keychain or the
    // vendor: it is announced, marked stopped and sealed, so a front end sees
    // the same three events it sees for every other stopped turn.
    if request.cancel.is_cancelled() {
        reporter.note(STOPPED);
        return Ok(reporter.finish(false, true));
    }
    let tools = registry(&reporter, request, settings);
    match settings.models.inference.provider.as_str() {
        neo_core::PROVIDER_OPENAI => match openai_model(runtime, settings) {
            Ok((inference, model)) => {
                drive(&reporter, cards, request, &inference, model, tools).await
            }
            Err(error) => {
                reporter.fail(&error);
                Err(error)
            }
        },
        neo_core::PROVIDER_ANTHROPIC_OAUTH => match claude_model(runtime, settings).await {
            Ok((inference, model)) => {
                drive(&reporter, cards, request, &inference, model, tools).await
            }
            Err(error) => {
                reporter.fail(&error);
                Err(error)
            }
        },
        neo_core::PROVIDER_OPENAI_CODEX => match codex_model(runtime, settings).await {
            Ok((inference, model)) => {
                drive(&reporter, cards, request, &inference, model, tools).await
            }
            Err(error) => {
                reporter.fail(&error);
                Err(error)
            }
        },
        other => {
            let error = AgentError::Request(unsupported(other));
            reporter.fail(&error);
            Err(error)
        }
    }
}

/// Which vendor a turn runs on, and under which concrete model id.
///
/// Both fields used to be read straight off [`Settings`], which worked while
/// there was one path: `gen_ai.system` could be the literal `openai`, and the
/// saved id was already concrete because the OpenAI path never resolves
/// `sol-latest`. Neither holds with a second vendor, and a trace that labels
/// a Claude call `openai` is worse than no trace.
pub(crate) struct Inference {
    /// The OpenTelemetry GenAI `gen_ai.system` value for this vendor.
    system: &'static str,
    /// The model id actually sent, with `sol-latest` already resolved.
    id: String,
}

impl Inference {
    /// A descriptor for a test's own model, named the way a real one is.
    #[cfg(test)]
    fn new(system: &'static str, id: &str) -> Self {
        Self {
            system,
            id: id.to_owned(),
        }
    }
}

/// The turn itself, over a prepared model and a prepared tool registry.
///
/// Split from [`turn`] at exactly those two, because they are the only parts
/// of a turn that need a credential and a real screen: a test drives the real
/// graph, the real steering seam and the whole event mapping against a model
/// and tools it built itself.
pub(crate) async fn drive<M>(
    reporter: &Arc<Reporter>,
    mut cards: Cards,
    request: &ChatRequest,
    inference: &Inference,
    model: M,
    tools: ToolRegistry,
) -> Result<TurnOutcome, AgentError>
where
    M: metalcraft::rig::completion::CompletionModel + Clone + 'static,
{
    let steering = Arc::new(Steering::new(request.conversation));
    let _live = runtime_guard(reporter, request.run, Arc::clone(&steering));

    // Once per turn, not once per step: the graph holds the system prompt for
    // the whole run, and scanning the machine's desktop entries is file system
    // work that would otherwise repeat at every round trip.
    let system = format!(
        "{PREAMBLE}{}{}{}{}",
        installed_apps_section(),
        crate::skills::render(&crate::skills::installed_skills()),
        crate::routines::render(&crate::routines::installed()),
        seat_section(),
    );
    let (llm_call_hook, llm_response_hook) = inference_hooks(reporter, inference);
    let graph = create_react_agent_with_options(
        model,
        tools,
        system,
        AgentOptions {
            llm_call_hook: Some(llm_call_hook),
            llm_response_hook: Some(llm_response_hook),
            model_name: Some(inference.id.clone()),
            // Without this there are no token events at all, and the answer
            // arrives in one piece at the end of the turn.
            stream_tokens: true,
            reasoning_effort: reasoning_effort(&inference.id),
            ..Default::default()
        },
    )
    .map_err(graph_error)?;

    let executor = Arc::new(
        Executor::new(graph)
            .with_cancellation(request.cancel.clone())
            .with_mailbox(mailbox(steering))
            .max_steps(node_budget(request.max_steps)),
    );
    let state = AgentState {
        messages: seed(&request.history),
        pending_tool_calls: Vec::new(),
        is_done: false,
    };
    if state.messages.is_empty() {
        let error = AgentError::Request("a turn needs at least one message".to_owned());
        reporter.fail(&error);
        return Err(error);
    }

    let mut stream = Arc::clone(&executor).stream(state, request.run.to_string());
    loop {
        // `biased` is what makes the ordering a guarantee rather than a
        // scheduling accident: the stream's events are older than anything a
        // tool has queued behind them — a tool only runs after the step that
        // asked for it went out — so the stream is always drained first.
        let event = tokio::select! {
            biased;
            event = stream.next() => event,
            Some(card) = cards.recv() => {
                reporter.apply(card);
                continue;
            }
        };
        let Some(event) = event else {
            break;
        };
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                let error = graph_error(error);
                reporter.fail(&error);
                return Err(error);
            }
        };
        match event {
            // The answer, as the model produces it. Tokens are forwarded
            // before the step that closes the node that produced them, so
            // appending in arrival order is appending in order.
            RunEvent::Token { text } => reporter.delta(&text),
            // Nothing to publish. A tool call announces itself through the
            // card queue — it is the only place that knows what it observed —
            // and a model call's text is settled when the turn is sealed.
            //
            // **Not here** because the hooks run ahead of this loop: the
            // executor task drives the graph while these events queue, so by
            // the time a step arrives the next model call may already have
            // returned. Reconciling a call's text against the tokens seen
            // *at this point in the stream* published the answer twice.
            RunEvent::Step { .. } => {}
            RunEvent::Finished(outcome) => {
                // Cards queued before the ending have to be published before
                // it: a step that closed after the turn finished cannot be
                // rendered, and its observation belongs in the thread ahead
                // of the answer.
                while let Ok(card) = cards.try_recv() {
                    reporter.apply(card);
                }
                return finish(reporter, request, outcome);
            }
        }
    }
    // metalcraft's stream always ends with `Finished`; a stream that merely
    // stopped is a bug upstream, and reporting it as a failure is better than
    // returning a turn nobody produced.
    let error = AgentError::Graph("the run ended without an outcome".to_owned());
    reporter.fail(&error);
    Err(error)
}

/// Neo's real tools, in the shape a vendor understands.
fn registry(reporter: &Arc<Reporter>, request: &ChatRequest, settings: &Settings) -> ToolRegistry {
    ToolRegistry::new()
        .register(Browse {
            reporter: Arc::clone(reporter),
            settings: settings.clone(),
            run: request.run,
            cancel: request.cancel.clone(),
            screen: request.screen,
        })
        .register(App {
            reporter: Arc::clone(reporter),
            settings: settings.clone(),
            run: request.run,
            cancel: request.cancel.clone(),
            screen: request.screen,
        })
        .register(Inspect {
            reporter: Arc::clone(reporter),
            run: request.run,
            cancel: request.cancel.clone(),
        })
        .register(RunRoutine {
            reporter: Arc::clone(reporter),
            run: request.run,
            cancel: request.cancel.clone(),
            screen: request.screen,
        })
}

/// What the model is told about the seat, when the seat is not ours.
///
/// The machine has somebody at it, so a run does not raise windows or type
/// into them (`neo_ax::seat`). That changes which tool does the work, not
/// whether the work is possible — and a model that is not told will spend its
/// budget re-trying an `app` goal that is refused every time, which is
/// exactly what the first run after the policy landed did.
fn seat_section() -> String {
    if neo_ax::may_take_seat() {
        return String::new();
    }
    "\n\nSomebody is using this machine, so you do not take the pointer, the \
     keyboard or the focused window. You can still read any application and \
     press its controls in place. What you cannot do is type into one: a goal \
     that needs text typed into an application will be refused. Do that work \
     through a `routine` instead — an application that ships one can be told \
     to act through its own control channel, which needs none of those things."
        .to_owned()
}

/// What one model round trip cost, to this turn's tally and to its trace.
///
/// The two hooks bracket one call, which is the only way to time it: the
/// response snapshot carries usage and no clock. The call's *text* is not
/// published from here, because a hook fires inside the node while the
/// stream events this turn maps are still queued — see the `Step` arm of
/// [`drive`].
///
/// The span follows the OpenTelemetry GenAI conventions, the same ones
/// [`crate::runtime`] records the subscription path's calls under, so a turn
/// on this path is priced and counted by the same reader. Without it a
/// tool-using turn traced as work with no model calls and no tokens at all.
/// `gen_ai.system` comes from the [`Inference`] the turn was built for, so a
/// Claude call is read as a Claude call: a reader that prices by vendor is
/// the whole reason the attribute exists.
fn inference_hooks(
    reporter: &Arc<Reporter>,
    inference: &Inference,
) -> (metalcraft::LlmCallHook, LlmResponseHook) {
    let started: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let call = {
        let started = Arc::clone(&started);
        Arc::new(move |_snapshot: &metalcraft::LlmCallSnapshot| {
            if let Ok(mut started) = started.lock() {
                *started = Some(Instant::now());
            }
        })
    };
    let reporter = Arc::clone(reporter);
    let system = inference.system;
    let model = inference.id.clone();
    let response = Arc::new(move |snapshot: &LlmResponseSnapshot| {
        reporter.round_trip(&snapshot.usage);
        let elapsed = started
            .lock()
            .ok()
            .and_then(|mut started| started.take())
            .map_or(Duration::ZERO, |started| started.elapsed());
        neo_otel::record_attached(
            &reporter.trace,
            neo_otel::SpanBuilder::client(format!("chat {model}"))
                .elapsed(elapsed)
                .text("gen_ai.operation.name", "chat")
                .text("gen_ai.system", system)
                .text("gen_ai.request.model", &model)
                .int("gen_ai.usage.input_tokens", snapshot.usage.input_tokens)
                .int("gen_ai.usage.output_tokens", snapshot.usage.output_tokens)
                .int("starkbot.tool_calls", snapshot.tool_calls.len()),
        );
    });
    (call, response)
}

/// Map metalcraft's ending onto Neo's.
///
/// `Err` for exactly one of the four endings. A failed run used to be
/// returned as a `TurnOutcome` under a comment claiming "the caller gets the
/// error", which it did not: `neo-tui` rendered a crashed turn as an answer
/// and `neo-eval` scored it `error: None`, so `ExpectNoError` passed on every
/// turn where a node blew up.
fn finish(
    reporter: &Arc<Reporter>,
    request: &ChatRequest,
    outcome: RunOutcome<AgentState>,
) -> Result<TurnOutcome, AgentError> {
    match outcome {
        RunOutcome::Completed(state) => {
            if let Some(answer) = state.final_answer() {
                reporter.whole_answer(answer);
            }
            Ok(reporter.finish(false, false))
        }
        // The step budget, a guard, or a node asking for human input. Neo has
        // no guard and no interrupting node, so this is the budget — and a
        // turn that stopped without answering has to say so, or the user is
        // shown an empty bubble.
        RunOutcome::Interrupted { state, reason, .. } => {
            if let Some(answer) = last_assistant(&state) {
                reporter.whole_answer(answer);
            }
            reporter.delta(&exhausted_sentence(request.max_steps, reason));
            Ok(reporter.finish(true, false))
        }
        // A stopped run keeps what it had already said: metalcraft pushes the
        // partial text into the state it hands back, so the thread and the
        // screen keep the half-finished sentence instead of losing it.
        RunOutcome::Cancelled { state, .. } => Ok(stopped(reporter, &state)),
        RunOutcome::Failed { state, node, error } => {
            // Classified, not assumed. A stop cannot reach this arm on
            // metalcraft 1.2: its `ToolNode` turns a tool's error into a
            // tool *result* and the executor's own token check returns
            // `Cancelled`, so the only way here is a node that returned
            // `Err` — today, a model call that failed. But this arm
            // classified nothing at all, so the first stop that did arrive
            // here would be published as a red error card over a turn the
            // user chose to end, and `graph_error`'s check sat on a path node
            // errors never take. One mapper, both arms.
            let error = node_failure(&node, &error);
            if error.is_cancelled() {
                return Ok(stopped(reporter, &state));
            }
            reporter.fail(&error);
            Err(error)
        }
    }
}

/// Seal a turn the user stopped: whatever it had said, then the marker, then
/// an ordinary finish. Shared by the two endings a stop can arrive on.
fn stopped(reporter: &Arc<Reporter>, state: &AgentState) -> TurnOutcome {
    if let Some(answer) = last_assistant(state) {
        reporter.whole_answer(answer);
    }
    reporter.note(STOPPED);
    reporter.finish(false, true)
}

/// The last thing the model said, whether or not it finished saying it.
fn last_assistant(state: &AgentState) -> Option<&str> {
    state
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            AgentMessage::Assistant(text) => Some(text.as_str()),
            _ => None,
        })
}

/// What a turn that ran out of budget tells the user.
fn exhausted_sentence(max_steps: usize, reason: String) -> String {
    format!(
        "I used all {max_steps} actions for this turn without finishing ({reason}). \
         Tell me how to narrow it down and I will carry on."
    )
}

/// metalcraft's step budget from Neo's action budget.
///
/// One action costs two nodes — the model call that asks for it and the tool
/// node that runs it — plus one more call to say what happened, and one
/// boundary for a steered message to land on. A budget expressed in nodes
/// would be a number no user could predict from the actions they watched.
fn node_budget(max_steps: usize) -> usize {
    max_steps.saturating_mul(2).saturating_add(2)
}

/// The conversation the run starts from.
///
/// A stored tool observation becomes a user message rather than an
/// `AgentMessage::ToolResult`: a tool result with no tool call before it is
/// the orphaned-call history the Responses API rejects, and the observation is
/// still worth showing the model — it is what the last turn learned.
fn seed(history: &[ChatMessage]) -> Vec<AgentMessage> {
    history
        .iter()
        .map(|message| match message.role {
            Role::User => AgentMessage::User(UserInput::new(message.text.clone())),
            Role::Assistant => AgentMessage::Assistant(message.text.clone()),
            Role::Tool => AgentMessage::User(UserInput::new(format!(
                "Result of an earlier action: {}",
                message.text
            ))),
        })
        .collect()
}

/// Reasoning effort for the models that require it.
///
/// OpenAI's reasoning models reject a replayed `function_call` that has no
/// `reasoning` item beside it, and metalcraft only keeps reasoning items when
/// a reasoning parameter was sent. Without this, the *second* step of every
/// tool-using turn on a `gpt-5` model fails with a 400.
fn reasoning_effort(model: &str) -> Option<String> {
    model.starts_with("gpt-5").then(|| "medium".to_owned())
}

/// The OpenAI path: Neo's own API key, the model id from settings, and the
/// base URL the runtime was opened with — which a test points at a local
/// server, so nothing here ever names a vendor host.
///
/// `sol-latest` is not resolved here, and never was: the OpenAI runtime is a
/// key path with no catalogue of its own behind `Runtime::resolved_model`,
/// and a user who selected it typed a concrete id.
fn openai_model(
    runtime: &Runtime,
    settings: &Settings,
) -> Result<(Inference, openai::responses_api::ResponsesCompletionModel), AgentError> {
    let key = runtime
        .openai_secret()?
        .ok_or_else(|| AgentError::NoKey("openai"))?;
    let base = inference_base(runtime.openai_base())?;

    // The audited credential boundary clippy.toml points at: the key becomes
    // the bearer of one rig client aimed at an injected base URL, and is
    // never logged, stored, put in a URL or returned.
    #[allow(clippy::disallowed_methods)]
    let client = openai::Client::builder()
        .api_key(key.expose().to_owned())
        .base_url(base.as_str())
        .build()
        .map_err(|error| AgentError::Graph(error.to_string()))?;
    let id = settings.models.inference.id.clone();
    let model = client.completion_model(id.as_str());
    Ok((
        Inference {
            system: "openai",
            id,
        },
        model,
    ))
}

/// The Claude Pro/Max path: the subscription's own OAuth credential, the model
/// id from settings with `sol-latest` resolved the way every other
/// subscription call resolves it, and the injected Anthropic base URL.
///
/// The token is fetched **twice over**, and both are deliberate. Once here,
/// so a turn on a subscription nobody has connected fails before it announces
/// anything, with the sentence that names the command; and then per request
/// through [`Runtime::token_source`], so a turn that outlives its own access
/// token refreshes instead of 401-ing halfway. The second read costs nothing:
/// `oauth_token` caches, so it is a map lookup until the credential is close
/// to expiry.
async fn claude_model(
    runtime: &Arc<Runtime>,
    settings: &Settings,
) -> Result<(Inference, ClaudeSubscription), AgentError> {
    let id = runtime.resolved_model(&ANTHROPIC_OAUTH, None, &settings.models.inference.id)?;
    // Not `AgentError::NoKey`: that one says `neo keys set`, and no key will
    // ever fix this. A subscription is connected by signing in, and the user
    // who selected this runtime has to be told which command does that.
    runtime
        .oauth_token(&ANTHROPIC_OAUTH)
        .await
        .map_err(|error| {
            AgentError::Request(format!(
                "the Claude subscription is not connected ({error}) — run \
             `neo account --provider anthropic-oauth login` and try again"
            ))
        })?;
    let model = ClaudeSubscription::new(
        runtime.anthropic_base(),
        runtime.token_source(&ANTHROPIC_OAUTH),
        &id,
    )
    .map_err(|error| AgentError::Graph(error.to_string()))?;
    Ok((
        Inference {
            system: "anthropic",
            id,
        },
        model,
    ))
}

/// The ChatGPT Plus/Pro path: the Codex Responses backend on the
/// subscription's own OAuth credential.
///
/// Nearly free, because rig ships the provider: `CodexOauthInference::model`
/// hands back the same `ResponsesCompletionModel` the OpenAI arm drives, with
/// the bearer, the `ChatGPT-Account-Id` and the `originator` the backend
/// wants already on it.
///
/// Unlike the Claude arm the token is bound at construction, because rig's
/// ChatGPT client owns its own authenticator and takes the access token by
/// value. A turn that outlives its access token therefore fails rather than
/// refreshing; the fix is the same shape as `ClaudeSubscription`'s transport
/// and belongs upstream in rig, not in a second copy of the Codex client
/// here.
async fn codex_model(
    runtime: &Arc<Runtime>,
    settings: &Settings,
) -> Result<(Inference, chatgpt::ResponsesCompletionModel), AgentError> {
    let id = runtime.resolved_model(&OPENAI_CODEX, None, &settings.models.inference.id)?;
    let token = runtime.oauth_token(&OPENAI_CODEX).await.map_err(|error| {
        AgentError::Request(format!(
            "the ChatGPT subscription is not connected ({error}) — run \
             `neo account --provider openai-codex login` and try again"
        ))
    })?;
    let mut inference =
        CodexOauthInference::hosted().map_err(|error| AgentError::Graph(error.to_string()))?;
    if let Some(account) = runtime
        .oauth_account_id(&OPENAI_CODEX)
        .map_err(|error| AgentError::Runtime(Box::new(error)))?
    {
        inference = inference.with_account(account);
    }
    let model = inference
        .model(&token, &id)
        .map_err(|error| AgentError::Graph(error.to_string()))?;
    Ok((
        Inference {
            system: "openai",
            id,
        },
        model,
    ))
}

/// What a turn on an inference runtime this path cannot serve is told.
///
/// `claude-subscription` gets its own sentence rather than the generic one,
/// because "choose another provider" reads like a configuration nit for what
/// is a structural fact: that runtime is the `claude` CLI, which is spawned
/// with `--disallowedTools '*'` and runs whatever tools it does have inside
/// its own sandbox. It never hands a `tool_use` back for Neo to run, so it
/// cannot drive the browser or an application, and a ReAct graph over it
/// would be a chat box that cannot act. The same Claude subscription *does*
/// work through `anthropic-oauth`, which is the thing worth saying.
fn unsupported(provider: &str) -> String {
    if provider == neo_core::PROVIDER_CLAUDE_SUBSCRIPTION {
        return "`claude-subscription` runs the `claude` CLI, which keeps its \
                tools inside its own sandbox and never hands one back for \
                Starkbot to run — so it cannot drive a browser or an \
                application. Select `anthropic-oauth` for the same Claude \
                subscription over Anthropic's own API, or `openai` for an \
                API key."
            .to_owned();
    }
    format!(
        "the agent loop runs on `openai` (an API key), `anthropic-oauth` (a \
         Claude Pro/Max plan) or `openai-codex` (a ChatGPT Plus/Pro plan), \
         and the selected inference runtime is `{provider}` — choose one of \
         those three as the inference provider"
    )
}

/// `{base}/v1`, the root rig appends `/responses` to.
///
/// Built by segments rather than `join`, which would drop the last segment of
/// a base like `http://127.0.0.1:1234/proxy`.
fn inference_base(base: &Url) -> Result<Url, AgentError> {
    let mut url = base.clone();
    {
        let mut segments = url.path_segments_mut().map_err(|()| {
            AgentError::Request("the OpenAI base URL cannot have a path".to_owned())
        })?;
        segments.pop_if_empty().push("v1");
    }
    Ok(url)
}

/// Keep a run reachable for as long as it is running.
///
/// A [`Drop`] guard rather than a call on each exit path: a run left in the
/// registry would accept steering forever, and there are five ways out of a
/// turn.
fn runtime_guard(reporter: &Arc<Reporter>, run: RunId, steering: Arc<Steering>) -> LiveRun {
    reporter.runtime.register_run(run, steering);
    LiveRun {
        runtime: Arc::clone(&reporter.runtime),
        run,
    }
}

pub(crate) struct LiveRun {
    runtime: Arc<Runtime>,
    run: RunId,
}

impl Drop for LiveRun {
    fn drop(&mut self) {
        self.runtime.forget_run(self.run);
    }
}

/// One classification for both ways a node failure reaches this turn.
///
/// metalcraft reports a failing node twice over: as an `Err(GraphError)` on
/// the event stream, and as `RunOutcome::Failed { node, error }` where
/// `error` is that same `GraphError` already stringified. A cancellation can
/// arrive on either, and only the stream was ever classified — so a stop that
/// came back as `Failed` was published as a red error card over a turn the
/// user had chosen to end. Both paths call this, which is the point: there is
/// one rule, and it reads a marker this crate wrote rather than prose a
/// reword would break. See [`CANCELLED_MARKER`].
fn node_failure(node: &str, message: &str) -> AgentError {
    if message.contains(CANCELLED_MARKER) {
        return AgentError::Core(CoreError::Cancelled);
    }
    AgentError::Graph(format!("{node}: {message}"))
}

/// A metalcraft failure as Neo's.
fn graph_error(error: GraphError) -> AgentError {
    match error {
        GraphError::Node { node, message } => node_failure(&node, &message),
        // Everything else is the graph itself refusing to run — no entry
        // point, no edge, a journal that would not write — and no node
        // produced it, so there is nothing to classify.
        other => AgentError::Graph(other.to_string()),
    }
}

/// A short stable name for a failure, so a report can count failures by cause
/// without matching on message text that will be reworded.
///
/// This is the `code` [`AppEvent::TurnFailed`] carries, so a front end can
/// tell a stopped turn from a broken one without matching on prose.
pub(crate) fn error_code(error: &AgentError) -> &'static str {
    match error {
        AgentError::Runtime(_) => "agent_runtime",
        AgentError::Tool(ToolError::Cancelled(_)) => "agent_cancelled",
        AgentError::Tool(_) => "agent_tool",
        AgentError::Request(_) => "agent_request",
        AgentError::Graph(_) => "agent_graph",
        AgentError::NoKey(_) => "agent_no_key",
        // Only `Cancelled` is a stop. The other `CoreError`s — an invalid
        // setting, a provider refusal — used to be counted as cancellations
        // here while `is_cancelled` correctly said they were not, so a report
        // read a configuration failure as a user pressing stop.
        AgentError::Core(CoreError::Cancelled) => "agent_cancelled",
        AgentError::Core(_) => "agent_core",
    }
}

// ---------------------------------------------------------------------------
// Cost
// ---------------------------------------------------------------------------

/// What the model round trips of one run added up to.
///
/// One `turns` row per run rather than per round trip: a run makes several
/// calls to reach one answer, and a cost view asks "what did that turn cost",
/// not "what did decision four cost". The count of round trips survives as
/// [`TurnUsage::requests`].
pub(crate) struct Tally {
    usage: TurnUsage,
    model: String,
    provider: ProviderId,
    started_at: TimestampMs,
    began: Instant,
}

impl Tally {
    pub(crate) fn new(settings: &Settings, started_at: TimestampMs) -> Self {
        Self {
            usage: TurnUsage::default(),
            model: settings.models.inference.id.clone(),
            provider: ProviderId::new(settings.models.inference.provider.as_str()),
            started_at,
            began: Instant::now(),
        }
    }

    /// One round trip.
    ///
    /// Cached and reasoning counts are breakouts of the parent numbers, not
    /// extra quantities: a cost view that added `cached_input_tokens` to
    /// `input_tokens` would double count what the vendor billed once.
    pub(crate) fn add(&mut self, usage: &LlmUsage) {
        self.usage.add(TurnUsage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cached_input_tokens: usage.cached_input_tokens,
            // Counted even when the vendor reported no tokens: the round trip
            // happened, and a turn with no usage object is not a free turn.
            requests: 1,
        });
    }

    /// Write the row, and hand back what to publish. `None` when the run
    /// never reached the model, in which case there is nothing to record.
    pub(crate) fn record(
        &self,
        runtime: &Runtime,
        conversation: ConversationId,
    ) -> Option<TurnUsage> {
        if self.usage.requests == 0 {
            return None;
        }
        runtime.record_turn(neo_store::NewTurn {
            conversation_id: conversation,
            model: self.model.clone(),
            provider: self.provider.clone(),
            duration_ms: u64::try_from(self.began.elapsed().as_millis()).unwrap_or(u64::MAX),
            usage: serde_json::to_value(self.usage).ok(),
            started_at: self.started_at,
        });
        Some(self.usage)
    }
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]

    use std::collections::VecDeque;

    use metalcraft::rig::completion::{CompletionError, CompletionRequest, CompletionResponse};
    use metalcraft::rig::streaming::{
        RawStreamingChoice, RawStreamingToolCall, StreamingCompletionResponse,
    };
    use neo_core::Envelope;
    use tokio::sync::broadcast::Receiver;
    use tokio_util::sync::CancellationToken;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

    use super::*;

    /// One piece of a scripted answer.
    #[derive(Clone)]
    enum Reply {
        Text(&'static str),
        Call {
            name: &'static str,
            arguments: Value,
        },
    }

    /// A model that answers from a script, and remembers what it was asked.
    ///
    /// The vendor is not reachable from a test — there is no key, and a turn
    /// must never leave the machine — but everything this module does with a
    /// model's answer is worth testing: the token events, the tool call, and
    /// the conversation a steered message lands in. So the script is the
    /// vendor, and the graph, the executor, the mailbox and the whole event
    /// mapping are the real ones.
    #[derive(Clone)]
    struct Scripted {
        turns: Arc<Mutex<VecDeque<Vec<Reply>>>>,
        /// What every call was sent: one entry per call, holding that call's
        /// messages in the order the provider sees them (the history, then
        /// the prompt). The *second* call is where a turn's own history is
        /// observable — a steered message arriving, and the model's own
        /// earlier sentence surviving.
        asked: Arc<Mutex<Vec<Vec<String>>>>,
        /// Which round trip the vendor refuses, counted from one. `None` —
        /// every other test — never refuses.
        fails_on: Option<usize>,
    }

    impl Scripted {
        fn new(turns: Vec<Vec<Reply>>) -> Self {
            Self {
                turns: Arc::new(Mutex::new(turns.into())),
                asked: Arc::new(Mutex::new(Vec::new())),
                fails_on: None,
            }
        }

        /// A model that answers from the script until its `nth` call, which
        /// it refuses — a vendor that broke mid-run.
        fn failing_on(turns: Vec<Vec<Reply>>, nth: usize) -> Self {
            Self {
                fails_on: Some(nth),
                ..Self::new(turns)
            }
        }

        fn asked(&self) -> Vec<Vec<String>> {
            self.asked.lock().expect("the log holds").clone()
        }
    }

    impl metalcraft::rig::completion::CompletionModel for Scripted {
        async fn completion(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, CompletionError> {
            Err(CompletionError::ProviderError(
                "the scripted model only streams".to_owned(),
            ))
        }

        async fn stream(
            &self,
            request: CompletionRequest,
        ) -> Result<StreamingCompletionResponse, CompletionError> {
            // `chat_history` is the whole conversation in order, prompt last:
            // rig's builder appends the prompt to it, so a positional
            // assertion reads exactly what the provider is sent.
            let messages: Vec<String> = request
                .chat_history
                .iter()
                .map(|message| serde_json::to_string(message).unwrap_or_default())
                .collect();
            let calls = {
                let mut asked = self.asked.lock().expect("the log holds");
                asked.push(messages);
                asked.len()
            };
            if self.fails_on == Some(calls) {
                return Err(CompletionError::ProviderError(
                    "the vendor broke mid-run".to_owned(),
                ));
            }
            let replies = self
                .turns
                .lock()
                .expect("the script holds")
                .pop_front()
                .unwrap_or_else(|| vec![Reply::Text("nothing left to say")]);
            let items = replies
                .into_iter()
                .map(|reply| {
                    Ok(match reply {
                        Reply::Text(text) => RawStreamingChoice::Message(text.to_owned()),
                        Reply::Call { name, arguments } => RawStreamingChoice::ToolCall(
                            RawStreamingToolCall::new(name, name.to_owned(), arguments),
                        ),
                    })
                })
                .collect::<Vec<_>>();
            Ok(StreamingCompletionResponse::stream(
                "scripted",
                Box::pin(futures_util::stream::iter(items)),
            ))
        }
    }

    /// A tool that drives nothing and reports through the same path the real
    /// surface tools use, so what a front end sees is what it would see for a
    /// real `app` run.
    struct Fake {
        reporter: Arc<Reporter>,
        observation: String,
        /// Run just before the observation is handed back — the seam a test
        /// uses to steer or stop a turn from *inside* it.
        during: Option<Box<dyn Fn() + Send + Sync>>,
    }

    #[async_trait::async_trait]
    impl Tool for Fake {
        fn name(&self) -> &str {
            "app"
        }

        fn description(&self) -> &str {
            "drive an application"
        }

        fn parameters_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": { "app": {"type": "string"}, "goal": {"type": "string"} },
                "required": ["app", "goal"]
            })
        }

        async fn call(&self, args: Value) -> metalcraft::Result<Value> {
            let summary = ActionSummary {
                kind: ActionKind::App,
                target: field(&args, "app"),
                goal: field(&args, "goal"),
                text: None,
            };
            let reporter = Arc::clone(&self.reporter);
            observed(&reporter, summary, async {
                if let Some(during) = &self.during {
                    during();
                }
                Ok(self.observation.clone())
            })
            .await
        }
    }

    /// A runtime with a thread, a turn to run in it, and a subscriber
    /// attached before anything is published.
    struct Fixture {
        _directory: tempfile::TempDir,
        runtime: Arc<Runtime>,
        request: ChatRequest,
        settings: Settings,
        events: Receiver<Envelope>,
    }

    impl Fixture {
        fn new(cancel: CancellationToken) -> Self {
            let directory = tempfile::tempdir().expect("a temporary data directory");
            let runtime = Arc::new(Runtime::open(directory.path()).expect("the store opens"));
            let conversation = runtime.new_conversation(None).expect("a conversation").id;
            let settings = runtime.settings().expect("settings");
            let events = runtime.subscribe();
            Self {
                _directory: directory,
                request: ChatRequest::new(
                    conversation,
                    vec![ChatMessage::user("make the deck say Q3")],
                )
                .with_cancel(cancel)
                .with_max_steps(4),
                runtime,
                settings,
                events,
            }
        }

        fn reporter(&self) -> (Arc<Reporter>, Cards) {
            let (reporter, cards) =
                Reporter::new(Arc::clone(&self.runtime), &self.request, &self.settings)
                    .expect("a reporter");
            (Arc::new(reporter), cards)
        }

        fn tools(&self, tool: Fake) -> ToolRegistry {
            ToolRegistry::new().register(tool)
        }

        /// What the scripted model is, for the hooks and the trace: a vendor
        /// name no real provider uses, and the id the settings hold.
        fn inference(&self) -> Inference {
            Inference::new("scripted", &self.settings.models.inference.id)
        }

        fn seen(&mut self) -> Vec<AppEvent> {
            let mut seen = Vec::new();
            while let Ok(envelope) = self.events.try_recv() {
                seen.push(envelope.event);
            }
            seen
        }

        fn thread(&self) -> Vec<Message> {
            self.runtime
                .thread(self.request.conversation, 100)
                .expect("the thread reads")
        }
    }

    /// The shape of the stream, so an ordering assertion reads as the
    /// sequence a front end sees rather than a pile of `matches!`.
    fn shape(events: &[AppEvent]) -> Vec<String> {
        events
            .iter()
            .map(|event| match event {
                AppEvent::TurnStarted { .. } => "started".to_owned(),
                AppEvent::TurnStep { action, .. } => format!("step {action}"),
                AppEvent::TurnStepDone { observation, .. } => format!("done {observation}"),
                AppEvent::TurnDelta { text, .. } => format!("delta {text}"),
                AppEvent::TurnCost { .. } => "cost".to_owned(),
                AppEvent::TurnNote { line, .. } => format!("note {line}"),
                AppEvent::TurnSteered { text, .. } => format!("steered {text}"),
                AppEvent::TurnFinished { text, .. } => format!("finished {text}"),
                AppEvent::TurnFailed { error, .. } => format!("failed {error}"),
                AppEvent::Message { message } => format!("message {:?}", message.role),
                other => format!("other {other:?}"),
            })
            .collect()
    }

    /// A model's own half of a multi-step turn has to survive into the next
    /// request.
    ///
    /// A response can carry text *and* tool calls — "let me open TextEdit and
    /// say hello in it" — and that sentence has already been streamed to the
    /// user. Dropping it left the model looking at a conversation in which it
    /// never spoke, so on the follow-up step it repeated itself or
    /// contradicted what the user was already reading.
    #[tokio::test]
    async fn what_the_model_said_before_a_tool_call_is_in_the_next_request() {
        let fixture = Fixture::new(CancellationToken::new());
        let (reporter, cards) = fixture.reporter();
        let model = Scripted::new(vec![
            vec![
                Reply::Text("Let me open TextEdit"),
                Reply::Text(" and say hello in it."),
                Reply::Call {
                    name: "app",
                    arguments: json!({"app": "TextEdit", "goal": "write hello"}),
                },
            ],
            vec![Reply::Text("Said hello.")],
        ]);
        let tools = fixture.tools(Fake {
            reporter: Arc::clone(&reporter),
            observation: "Done · typed hello".to_owned(),
            during: None,
        });

        drive(
            &reporter,
            cards,
            &fixture.request,
            &fixture.inference(),
            model.clone(),
            tools,
        )
        .await
        .expect("the turn answers");

        let asked = model.asked();
        assert_eq!(asked.len(), 2, "two round trips: {asked:?}");
        let second = &asked[1];
        let position = |needle: &str| {
            second
                .iter()
                .position(|message| message.contains(needle))
                .unwrap_or_else(|| panic!("expected {needle} in {second:?}"))
        };
        // The sentence is there, as the assistant's own message…
        let said = position("Let me open TextEdit and say hello in it.");
        assert!(
            second[said].contains("assistant"),
            "it is the model's own message, not quoted back as someone \
             else's: {}",
            second[said]
        );
        // …and in the right place: after the user's question, before the call
        // it introduced and the result of that call. A provider reading this
        // history sees the turn the user saw.
        assert!(position("make the deck say Q3") < said);
        // `write hello` is the call's own goal — the app name also appears in
        // the system preamble, which is the first message of every request.
        assert!(
            said < position("write hello"),
            "before the call: {second:?}"
        );
        assert!(
            said < position("Done · typed hello"),
            "before the tool result: {second:?}"
        );
    }

    /// The sequence a front end is built on. A card is opened by `TurnStep`,
    /// closed by `TurnStepDone` carrying what the tool observed, and the
    /// answer arrives as deltas before the turn is sealed — in that order,
    /// because a card closed before it opened or an answer after the finish
    /// cannot be rendered.
    #[tokio::test]
    async fn a_turn_with_a_tool_call_announces_the_call_the_result_and_the_answer() {
        let mut fixture = Fixture::new(CancellationToken::new());
        let (reporter, cards) = fixture.reporter();
        let model = Scripted::new(vec![
            vec![Reply::Call {
                name: "app",
                arguments: json!({"app": "Keynote", "goal": "retitle the deck"}),
            }],
            vec![Reply::Text("Retitled it"), Reply::Text(" to Q3.")],
        ]);
        let tools = fixture.tools(Fake {
            reporter: Arc::clone(&reporter),
            observation: "Done · the deck now reads Q3".to_owned(),
            during: None,
        });

        let outcome = drive(
            &reporter,
            cards,
            &fixture.request,
            &fixture.inference(),
            model,
            tools,
        )
        .await
        .expect("the turn answers");

        assert_eq!(outcome.text, "Retitled it to Q3.");
        assert!(!outcome.cancelled);
        assert!(!outcome.exhausted);
        assert_eq!(outcome.steps.len(), 1);
        assert_eq!(outcome.steps[0].observation, "Done · the deck now reads Q3");

        let seen = shape(&fixture.seen());
        let order = |needle: &str| {
            seen.iter()
                .position(|line| line.starts_with(needle))
                .unwrap_or_else(|| panic!("expected a {needle} event, got {seen:?}"))
        };
        assert_eq!(order("started"), 0);
        assert!(order("step app Keynote") < order("done Done · the deck"));
        assert!(order("done Done · the deck") < order("delta Retitled it"));
        assert!(order("delta Retitled it") < order("delta  to Q3."));
        assert!(order("delta  to Q3.") < order("finished Retitled it to Q3."));

        // The thread holds the answer as one row, not one per token, and the
        // observation as its own row the next turn's history replays.
        let thread = fixture.thread();
        let answers: Vec<&str> = thread
            .iter()
            .filter(|message| message.role == MessageRole::Assistant)
            .map(|message| message.text.as_str())
            .collect();
        assert_eq!(answers, vec!["Retitled it to Q3."]);
        assert!(
            thread
                .iter()
                .any(|message| message.role == MessageRole::Tool
                    && message.text == "Done · the deck now reads Q3"),
            "the observation is in the thread: {thread:?}"
        );
        // Every row a turn writes says which run wrote it, so a front end
        // correlates on the run and never on a timestamp.
        for message in &thread {
            assert_eq!(
                message.meta.as_ref().and_then(|meta| meta.get("run")),
                Some(&json!(fixture.request.run.to_string())),
                "{message:?}"
            );
        }
    }

    /// Steering is the whole reason the mailbox is wired: a message typed
    /// while the agent works has to reach *that* turn, and a message typed
    /// after it ends has to be refused so the caller sends it as a new turn.
    #[tokio::test]
    async fn a_message_typed_during_a_turn_reaches_the_running_turn() {
        let mut fixture = Fixture::new(CancellationToken::new());
        let (reporter, cards) = fixture.reporter();
        let run = fixture.request.run;
        let steerer = Arc::clone(&fixture.runtime);
        let model = Scripted::new(vec![
            vec![Reply::Call {
                name: "app",
                arguments: json!({"app": "Keynote", "goal": "retitle the deck"}),
            }],
            vec![Reply::Text("Used Pages instead.")],
        ]);
        let tools = fixture.tools(Fake {
            reporter: Arc::clone(&reporter),
            observation: "Done · retitled".to_owned(),
            during: Some(Box::new(move || {
                assert!(
                    steerer
                        .steer(run, "actually, do it in Pages")
                        .expect("steering a live run does not fail"),
                    "a run that is still going accepts a message"
                );
            })),
        });

        let outcome = drive(
            &reporter,
            cards,
            &fixture.request,
            &fixture.inference(),
            model.clone(),
            tools,
        )
        .await
        .expect("the turn answers");
        assert_eq!(outcome.text, "Used Pages instead.");

        // The model's second call knew about it: that is what "reached the
        // running turn" means, as opposed to "was recorded somewhere".
        let asked = model.asked();
        assert_eq!(asked.len(), 2, "two round trips: {asked:?}");
        assert!(
            asked[1]
                .iter()
                .any(|message| message.contains("actually, do it in Pages")),
            "the steered message is in the second call's conversation: {:?}",
            asked[1]
        );

        let seen = shape(&fixture.seen());
        assert!(
            seen.iter()
                .any(|line| line == "steered actually, do it in Pages"),
            "the message is announced as landing in the turn: {seen:?}"
        );
        assert!(
            fixture
                .thread()
                .iter()
                .any(|message| message.role == MessageRole::User
                    && message.text == "actually, do it in Pages"),
            "and it is in the thread"
        );

        // The run is over, so the same call now says "send it as a new turn"
        // rather than erroring.
        assert!(
            !fixture
                .runtime
                .steer(run, "and one more thing")
                .expect("steering a finished run is not a failure"),
            "a finished run refuses it, so the caller sends a new turn"
        );
    }

    /// Stopping a turn is not breaking it: what the model had already said
    /// stays on the screen and in the thread, and the turn ends as a turn
    /// rather than as an error.
    #[tokio::test]
    async fn a_stopped_turn_keeps_what_it_had_already_said() {
        let cancel = CancellationToken::new();
        let mut fixture = Fixture::new(cancel.clone());
        let (reporter, cards) = fixture.reporter();
        let model = Scripted::new(vec![
            vec![
                Reply::Text("Working on it"),
                Reply::Call {
                    name: "app",
                    arguments: json!({"app": "Keynote", "goal": "retitle the deck"}),
                },
            ],
            vec![Reply::Text("this must never be said")],
        ]);
        let stop = cancel.clone();
        let tools = fixture.tools(Fake {
            reporter: Arc::clone(&reporter),
            observation: "Done · retitled".to_owned(),
            during: Some(Box::new(move || stop.cancel())),
        });

        let outcome = drive(
            &reporter,
            cards,
            &fixture.request,
            &fixture.inference(),
            model,
            tools,
        )
        .await
        .expect("a stopped turn is an outcome, not an error");

        assert!(outcome.cancelled);
        assert_eq!(outcome.text, "Working on it");

        let seen = shape(&fixture.seen());
        // The model said something *before* asking for the tool, and that is
        // the order it has to arrive in: the graph runs in its own task, and
        // a tool that announced itself without waiting for the turn's own
        // queue published its card ahead of the sentence that introduced it.
        let position = |needle: &str| {
            seen.iter()
                .position(|line| line.starts_with(needle))
                .unwrap_or_else(|| panic!("expected {needle} in {seen:?}"))
        };
        assert!(
            position("delta Working on it") < position("step app"),
            "text precedes the card it introduced: {seen:?}"
        );
        let note = seen
            .iter()
            .position(|line| line == &format!("note {STOPPED}"))
            .unwrap_or_else(|| panic!("a stopped turn says so: {seen:?}"));
        let finished = seen
            .iter()
            .position(|line| line.starts_with("finished"))
            .unwrap_or_else(|| panic!("a stopped turn still finishes: {seen:?}"));
        assert!(note < finished, "the marker precedes the finish: {seen:?}");
        assert_eq!(seen[finished], "finished Working on it");
        assert!(
            !seen.iter().any(|line| line.starts_with("failed")),
            "a stopped turn is never published as a failure: {seen:?}"
        );

        assert!(
            fixture
                .thread()
                .iter()
                .any(|message| message.role == MessageRole::Assistant
                    && message.text == "Working on it"),
            "the partial answer survives in the thread"
        );
    }

    /// A node that blew up is not an answer.
    ///
    /// The `Failed` arm used to build the error, publish it, and then return
    /// a `TurnOutcome` anyway — under a comment claiming "the caller gets the
    /// error". `neo-tui` rendered the crashed turn as an answer and
    /// `neo-eval` scored it `error: None`, so `ExpectNoError` passed on every
    /// turn where a node failed. Every eval number produced before this was
    /// suspect.
    #[tokio::test]
    async fn a_turn_whose_node_failed_is_an_error_and_not_an_answer() {
        let mut fixture = Fixture::new(CancellationToken::new());
        let (reporter, cards) = fixture.reporter();
        // One good round trip, a tool call, then a vendor that refuses: the
        // failure lands mid-run, with a partial answer already on the screen.
        let model = Scripted::failing_on(
            vec![vec![
                Reply::Text("Opening it now."),
                Reply::Call {
                    name: "app",
                    arguments: json!({"app": "Keynote", "goal": "retitle the deck"}),
                },
            ]],
            2,
        );
        let tools = fixture.tools(Fake {
            reporter: Arc::clone(&reporter),
            observation: "Done · retitled".to_owned(),
            during: None,
        });

        let error = drive(
            &reporter,
            cards,
            &fixture.request,
            &fixture.inference(),
            model,
            tools,
        )
        .await
        .expect_err("a run whose node failed must not come back as a turn");

        assert!(!error.is_cancelled(), "nobody stopped this turn: {error}");
        assert_eq!(error_code(&error), "agent_graph");
        let seen = shape(&fixture.seen());
        assert!(
            seen.iter().any(|line| line.starts_with("failed")),
            "the failure is published as a failure: {seen:?}"
        );
        assert!(
            !seen.iter().any(|line| line.starts_with("finished")),
            "a failed turn must not also finish: {seen:?}"
        );
    }

    /// Both endings a stop can arrive on end the turn as a stop.
    ///
    /// metalcraft reports a cancellation as `RunOutcome::Cancelled` today —
    /// the executor races every node against the token — and the `Failed`
    /// arm is reached only by a node that returned `Err`. But the `Failed`
    /// arm classified nothing at all, and `graph_error`'s
    /// `error.to_string().contains("cancelled")` sat on a path node errors
    /// never take, so the moment a stop did surface as a node failure it
    /// would have been published as a red error card over a turn the user
    /// chose to end. [`finish`] is exercised directly because that is the
    /// contract: this arm, this marker, this disposition.
    #[tokio::test]
    async fn a_stop_that_surfaces_as_a_node_failure_is_still_a_stop() {
        let mut fixture = Fixture::new(CancellationToken::new());
        let (reporter, _cards) = fixture.reporter();
        let mut state = AgentState::new("make the deck say Q3");
        state
            .messages
            .push(AgentMessage::Assistant("Working on it".to_owned()));

        let outcome = finish(
            &reporter,
            &fixture.request,
            RunOutcome::Failed {
                state,
                node: "tools".to_owned(),
                // Exactly what `observed` hands the graph, stringified the
                // way the executor stringifies it.
                error: GraphError::Node {
                    node: "tools".to_owned(),
                    message: CANCELLED_MARKER.to_owned(),
                }
                .to_string(),
            },
        )
        .expect("a stop is an outcome, not an error, whichever arm it lands on");

        assert!(outcome.cancelled);
        assert_eq!(outcome.text, "Working on it");
        let seen = shape(&fixture.seen());
        assert!(
            !seen.iter().any(|line| line.starts_with("failed")),
            "a stopped turn is never published as a failure: {seen:?}"
        );
        assert!(
            seen.iter().any(|line| line == &format!("note {STOPPED}")),
            "and it says it was stopped: {seen:?}"
        );
    }

    /// The marker is the whole mechanism, so the two things that would break
    /// it silently are pinned: a vendor error that happens to use the word
    /// "cancelled" is a failure, and only `CoreError::Cancelled` is a stop.
    #[test]
    fn only_the_marker_classifies_a_stop() {
        let vendor = graph_error(GraphError::Node {
            node: "agent".to_owned(),
            message: "the request was cancelled by the upstream provider".to_owned(),
        });
        assert!(
            !vendor.is_cancelled(),
            "a vendor mentioning the word did not stop the run: {vendor}"
        );
        assert_eq!(error_code(&vendor), "agent_graph");

        let stop = node_failure("tools", CANCELLED_MARKER);
        assert!(stop.is_cancelled());
        assert_eq!(error_code(&stop), "agent_cancelled");

        // `is_cancelled` has always tested only `Cancelled`; `error_code`
        // used to call every `CoreError` a cancellation, so a report read an
        // invalid setting as a user pressing stop.
        let misconfigured = AgentError::Core(CoreError::InvalidSetting {
            field: "safety.on_task_floor",
            reason: "below the floor".to_owned(),
        });
        assert!(!misconfigured.is_cancelled());
        assert_eq!(error_code(&misconfigured), "agent_core");
    }

    /// A turn that never reaches the model records no cost: a row of zeroes
    /// is worse than a gap, because a cost view has to filter it out.
    #[tokio::test]
    async fn a_turn_stopped_before_it_started_costs_nothing() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut fixture = Fixture::new(cancel);
        let outcome = turn(&fixture.runtime, &fixture.request, &fixture.settings)
            .await
            .expect("a stopped turn is an outcome");

        assert!(outcome.cancelled);
        assert!(outcome.text.is_empty());
        assert_eq!(outcome.usage, None);
        assert!(
            fixture
                .runtime
                .turns(fixture.request.conversation, 10)
                .expect("the turns read")
                .is_empty(),
            "nothing reached the model, so there is nothing to bill"
        );
        let seen = shape(&fixture.seen());
        assert_eq!(
            seen,
            vec![
                "started".to_owned(),
                format!("note {STOPPED}"),
                "finished ".to_owned(),
            ]
        );
    }

    /// A turn that cannot run says how to make it run. The command is the
    /// contract here: "no key" on its own sends a user hunting through
    /// settings, and this path is the first thing a new install hits.
    #[tokio::test]
    async fn a_turn_with_no_key_names_the_command_that_sets_one() {
        let mut fixture = Fixture::new(CancellationToken::new());
        fixture
            .runtime
            .patch_settings(
                "models",
                json!({"inference": {"provider": "openai", "id": "gpt-5.1"}}),
            )
            .expect("the inference runtime is selectable");
        let settings = fixture.runtime.settings().expect("settings");

        let error = turn(&fixture.runtime, &fixture.request, &settings)
            .await
            .expect_err("a turn with no credential cannot run");
        assert!(
            error.to_string().contains("neo keys set openai"),
            "got {error}"
        );
        assert!(!error.is_cancelled());
        assert_eq!(error_code(&error), "agent_no_key");

        // And it ends as a failure on the stream, not as a silent hang.
        let seen = shape(&fixture.seen());
        assert!(
            seen.iter().any(|line| line.starts_with("failed")),
            "the run announces the failure: {seen:?}"
        );
    }

    /// The running cost and the sealed cost come from one accumulator, so the
    /// number a status line last showed is the number the turn is billed.
    #[test]
    fn one_accumulator_serves_the_running_total_and_the_final_one() {
        let directory = tempfile::tempdir().expect("a temporary data directory");
        let runtime = Runtime::open(directory.path()).expect("the store opens");
        let conversation = runtime.new_conversation(None).expect("a conversation").id;
        let settings = runtime.settings().expect("settings");

        let mut tally = Tally::new(&settings, 1_700_000_000_000);
        tally.add(&LlmUsage {
            input_tokens: 100,
            output_tokens: 20,
            total_tokens: 120,
            cached_input_tokens: 30,
            reasoning_tokens: 10,
        });
        let running = tally.usage;
        tally.add(&LlmUsage::default());

        let sealed = tally
            .record(&runtime, conversation)
            .expect("a turn that asked the model is recorded");
        // Cached tokens are a breakout of the input count, not an addition
        // to it: a view that summed them would double what was billed once.
        assert_eq!(sealed.input_tokens, 100);
        assert_eq!(sealed.cached_input_tokens, 30);
        assert_eq!(sealed.output_tokens, 20);
        // A round trip the vendor said nothing about still happened.
        assert_eq!(sealed.requests, 2);
        assert_eq!(running.requests, 1);

        let rows = runtime.turns(conversation, 10).expect("the turns read");
        assert_eq!(rows.len(), 1, "one row per run, not per round trip");
        let stored = rows[0].usage.as_ref().expect("usage was stored");
        assert_eq!(stored["input_tokens"], 100);
        assert_eq!(stored["requests"], 2);

        // A run that never reached the model records nothing at all.
        let untouched = Tally::new(&settings, 1_700_000_000_000);
        assert_eq!(untouched.record(&runtime, conversation), None);
        assert_eq!(
            runtime
                .turns(conversation, 10)
                .expect("the turns read")
                .len(),
            1
        );
    }

    /// Which arm of [`turn`] each provider id lands in, and what the three
    /// runtimes that cannot run an agent turn are told instead.
    ///
    /// An arm is identified by the credential it asks for first, because
    /// none of them gets past that without a real vendor behind it: the
    /// OpenAI arm wants an API key, each subscription arm wants its own
    /// connected OAuth credential, and the other three never build a model
    /// at all. A turn that fell through to the wrong arm would name the
    /// wrong missing credential, which is precisely the regression this
    /// covers — `anthropic-oauth` used to be told to choose `openai`.
    ///
    /// Every one of them also has to *settle*: the turn is announced before
    /// the model is built, so a failure that published nothing after
    /// `TurnStarted` leaves a front end on "thinking" for ever.
    #[tokio::test]
    async fn every_provider_id_reaches_its_own_arm_and_settles() {
        for (provider, expected) in [
            (neo_core::PROVIDER_OPENAI, "no openai key is stored"),
            (
                neo_core::PROVIDER_ANTHROPIC_OAUTH,
                "the Claude subscription is not connected",
            ),
            (
                neo_core::PROVIDER_OPENAI_CODEX,
                "the ChatGPT subscription is not connected",
            ),
            (neo_core::PROVIDER_ANTHROPIC, "choose one of those three"),
            (
                neo_core::PROVIDER_CHATGPT_CODEX,
                "choose one of those three",
            ),
            (
                neo_core::PROVIDER_CLAUDE_SUBSCRIPTION,
                "keeps its tools inside its own sandbox",
            ),
        ] {
            let mut fixture = Fixture::new(CancellationToken::new());
            fixture.settings.models.inference.provider = ProviderId::new(provider);

            let error = turn(&fixture.runtime, &fixture.request, &fixture.settings)
                .await
                .expect_err("no credential is configured for any runtime here");

            assert!(
                error.to_string().contains(expected),
                "`{provider}` reached the wrong arm: {error}",
            );
            // Announced, then settled, with nothing in between and nothing
            // after: a front end that has shown "thinking" on `started` has
            // exactly one event that takes it down again.
            let seen = shape(&fixture.seen());
            assert_eq!(seen.len(), 2, "`{provider}`: {seen:?}");
            assert_eq!(seen[0], "started", "`{provider}`: {seen:?}");
            assert!(
                seen[1].starts_with("failed"),
                "`{provider}` never settled: {seen:?}",
            );
        }
    }

    // -----------------------------------------------------------------
    // The Claude subscription over the real Messages wire
    // -----------------------------------------------------------------

    /// An Anthropic Messages endpoint that answers each call with the next
    /// scripted SSE stream.
    ///
    /// A responder rather than two mounted mocks with matchers on them: what
    /// a two-step turn has to prove is *what the second request contained*,
    /// and a fixture that also had to describe how the server tells the two
    /// calls apart would be asserting on itself.
    struct Script(Mutex<VecDeque<String>>);

    impl Respond for Script {
        fn respond(&self, _request: &wiremock::Request) -> ResponseTemplate {
            let body = self
                .0
                .lock()
                .ok()
                .and_then(|mut turns| turns.pop_front())
                .unwrap_or_default();
            ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
        }
    }

    /// Anthropic's SSE framing: one `event:`/`data:` pair per frame.
    fn sse(frames: &[Value]) -> String {
        frames
            .iter()
            .map(|frame| {
                let name = frame["type"].as_str().unwrap_or_default();
                format!("event: {name}\ndata: {frame}\n\n")
            })
            .collect()
    }

    /// One streamed message: what opens it, its content blocks, and the
    /// `message_delta` that closes it and carries what it cost.
    fn message(blocks: Vec<Value>, stop: &str, input: u64, output: u64) -> String {
        let mut frames = vec![json!({
            "type": "message_start",
            "message": {
                "id": "msg_test",
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-4-5-20250929",
                "content": [],
                "stop_reason": null,
                "usage": { "input_tokens": input, "output_tokens": 1 }
            }
        })];
        frames.extend(blocks);
        frames.push(json!({
            "type": "message_delta",
            "delta": { "stop_reason": stop, "stop_sequence": null },
            "usage": { "input_tokens": input, "output_tokens": output }
        }));
        frames.push(json!({ "type": "message_stop" }));
        sse(&frames)
    }

    /// One text block, in the deltas a front end renders one at a time.
    fn text_block(index: usize, deltas: &[&str]) -> Vec<Value> {
        let mut frames = vec![json!({
            "type": "content_block_start",
            "index": index,
            "content_block": { "type": "text", "text": "" }
        })];
        frames.extend(deltas.iter().map(|text| {
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "text_delta", "text": text }
            })
        }));
        frames.push(json!({ "type": "content_block_stop", "index": index }));
        frames
    }

    /// One tool call, with its arguments split across `input_json_delta`s the
    /// way a real stream sends them — mid-string, mid-key, wherever the
    /// vendor's chunking lands.
    fn tool_block(index: usize, name: &str, arguments: &[&str]) -> Vec<Value> {
        let mut frames = vec![json!({
            "type": "content_block_start",
            "index": index,
            "content_block": { "type": "tool_use", "id": "toolu_test", "name": name, "input": {} }
        })];
        frames.extend(arguments.iter().map(|partial_json| {
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "input_json_delta", "partial_json": partial_json }
            })
        }));
        frames.push(json!({ "type": "content_block_stop", "index": index }));
        frames
    }

    /// An endpoint answering `turns` in order.
    async fn anthropic(turns: Vec<String>) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(Script(Mutex::new(turns.into())))
            .mount(&server)
            .await;
        server
    }

    /// A subscription model pointed at `server`, on a token that is not one.
    fn claude(server: &MockServer) -> ClaudeSubscription {
        let base = Url::parse(&server.uri()).expect("wiremock hands out a URL");
        let token = neo_keys::Secret::new("test-access-token").expect("a non-empty secret");
        ClaudeSubscription::new(
            &base,
            crate::providers::anthropic_oauth_model::one_token(token),
            "claude-sonnet-4-5-20250929",
        )
        .expect("a model")
    }

    /// The bodies the graph sent, in order, so a test can read the
    /// conversation the vendor was shown.
    async fn sent(server: &MockServer) -> Vec<Value> {
        server
            .received_requests()
            .await
            .expect("recorded requests")
            .iter()
            .map(|request| request.body_json().expect("a JSON body"))
            .collect()
    }

    fn subscription() -> Inference {
        Inference::new("anthropic", "claude-sonnet-4-5-20250929")
    }

    /// Why the subscription path streams instead of reusing the single-shot
    /// `POST` this crate already had for it: the answer has to reach a front
    /// end in pieces, in the order the vendor produced them, or a turn is a
    /// spinner followed by a wall of text. What it cost has to arrive with
    /// it, or the status line and the `turns` row are blank on every Claude
    /// turn.
    #[tokio::test]
    async fn a_claude_turn_streams_its_text_and_reports_what_it_cost() {
        let server = anthropic(vec![message(
            text_block(0, &["Hello", ", world."]),
            "end_turn",
            41,
            7,
        )])
        .await;
        let mut fixture = Fixture::new(CancellationToken::new());
        let (reporter, cards) = fixture.reporter();
        let tools = fixture.tools(Fake {
            reporter: Arc::clone(&reporter),
            observation: "never asked for".to_owned(),
            during: None,
        });

        let outcome = drive(
            &reporter,
            cards,
            &fixture.request,
            &subscription(),
            claude(&server),
            tools,
        )
        .await
        .expect("the turn answers");

        assert_eq!(outcome.text, "Hello, world.");
        let seen = shape(&fixture.seen());
        let deltas: Vec<&str> = seen
            .iter()
            .filter(|line| line.starts_with("delta "))
            .map(String::as_str)
            .collect();
        assert_eq!(
            deltas,
            vec!["delta Hello", "delta , world."],
            "the answer arrives in the pieces the vendor sent, in order: {seen:?}"
        );

        // `message_delta` is the only frame carrying the final counts, so a
        // reader that stopped at `message_start` would bill every turn for
        // one output token.
        let usage = outcome
            .usage
            .expect("a turn that reached the model cost something");
        assert_eq!(usage.input_tokens, 41);
        assert_eq!(usage.output_tokens, 7);
        assert_eq!(usage.requests, 1);
    }

    /// A tool call over the Messages wire, end to end: a `tool_use` block
    /// becomes a real tool run, and what that run observed goes back as the
    /// `tool_result` of that same call.
    ///
    /// Beside it, the assertion that matters most. A response can carry text
    /// *and* a tool call — "let me open TextEdit and say hello in it" — and
    /// that sentence has already been streamed to the user. Dropping it left
    /// the model looking at a conversation in which it never spoke, so on the
    /// next step it repeated itself. That regression was fixed on the OpenAI
    /// path; it has to stay fixed here, where an assistant turn is a list of
    /// content blocks rather than a string.
    #[tokio::test]
    async fn a_claude_tool_call_returns_its_result_beside_the_text_that_asked_for_it() {
        let mut first = text_block(0, &["Let me open TextEdit", " and say hello in it."]);
        first.extend(tool_block(
            1,
            "app",
            &[r#"{"app": "TextE"#, r#"dit", "goal": "write hello"}"#],
        ));
        let server = anthropic(vec![
            message(first, "tool_use", 60, 24),
            message(text_block(0, &["Said hello."]), "end_turn", 90, 5),
        ])
        .await;

        let fixture = Fixture::new(CancellationToken::new());
        let (reporter, cards) = fixture.reporter();
        let tools = fixture.tools(Fake {
            reporter: Arc::clone(&reporter),
            observation: "Done · typed hello".to_owned(),
            during: None,
        });

        let outcome = drive(
            &reporter,
            cards,
            &fixture.request,
            &subscription(),
            claude(&server),
            tools,
        )
        .await
        .expect("the turn answers");

        // Both steps' text, because a turn's answer is everything it said —
        // the same accumulation the OpenAI path produces.
        assert_eq!(
            outcome.text,
            "Let me open TextEdit and say hello in it.Said hello."
        );
        assert_eq!(outcome.steps.len(), 1, "the tool ran once");
        assert_eq!(outcome.steps[0].observation, "Done · typed hello");
        // The fragmented `input_json_delta`s were reassembled, or the tool
        // would have been handed half a JSON object.
        assert_eq!(outcome.steps[0].action.target.as_deref(), Some("TextEdit"));

        let bodies = sent(&server).await;
        assert_eq!(bodies.len(), 2, "two round trips: {bodies:?}");
        let messages = bodies[1]["messages"]
            .as_array()
            .expect("the second request carries a conversation");
        // Every block the model itself produced this turn. metalcraft
        // replays the sentence and the call it introduced as two assistant
        // messages, and Anthropic merges consecutive same-role turns — so
        // what matters is that both blocks are there, on the model's own
        // side of the conversation, before the result.
        let spoken: Vec<&Value> = messages
            .iter()
            .filter(|message| message["role"] == json!("assistant"))
            .flat_map(|message| message["content"].as_array().into_iter().flatten())
            .collect();
        assert!(
            spoken.iter().any(|block| block["type"] == json!("text")
                && block["text"] == json!("Let me open TextEdit and say hello in it.")),
            "what the model said before the call survives: {messages:?}"
        );
        let call = spoken
            .iter()
            .find(|block| block["type"] == json!("tool_use"))
            .expect("the call it made survives beside the sentence");
        // A `tool_result` naming no call in the history is the orphaned-call
        // shape Anthropic answers with a 400.
        let result = messages
            .iter()
            .flat_map(|message| message["content"].as_array().into_iter().flatten())
            .find(|block| block["type"] == json!("tool_result"))
            .expect("the observation goes back as this call's result");
        assert_eq!(result["tool_use_id"], call["id"]);
        assert!(
            result.to_string().contains("Done · typed hello"),
            "the result carries what the tool observed: {result}"
        );
    }

    /// A user who selected the Claude subscription and has not connected it
    /// is told the command that connects it. `neo keys set` is the wrong
    /// answer and no key would work, so this failure has to name the sign-in.
    #[tokio::test]
    async fn an_unconnected_subscription_names_the_login_command() {
        let mut fixture = Fixture::new(CancellationToken::new());
        fixture.settings.models.inference.provider =
            ProviderId::new(neo_core::PROVIDER_ANTHROPIC_OAUTH);

        let error = turn(&fixture.runtime, &fixture.request, &fixture.settings)
            .await
            .expect_err("a turn with no subscription cannot run");
        assert!(
            error
                .to_string()
                .contains("neo account --provider anthropic-oauth login"),
            "got {error}"
        );
        assert!(!error.is_cancelled());
    }

    /// A machine with applications on it names them, id and all: the id is
    /// the half the model has to produce to reach the `app` tool, so a
    /// section that listed only human names would leave the guess it is
    /// there to prevent (A23).
    #[test]
    fn the_inventory_section_names_an_app_by_its_id() {
        let apps = vec![InstalledApp {
            id: "dev.degenpaint.studio".to_owned(),
            name: "degen-paint Studio".to_owned(),
            path: std::path::PathBuf::from("/usr/share/applications/dev.degenpaint.studio.desktop"),
        }];

        let section = render_installed_apps(&apps);
        assert!(
            section.contains("degen-paint Studio (dev.degenpaint.studio)"),
            "got {section}"
        );
    }

    /// A machine with no backend — no desktop entries, no bundles — adds
    /// nothing to the prompt. A bare heading would read as "there are none",
    /// which is a stronger claim than the empty inventory supports.
    #[test]
    fn an_empty_inventory_adds_nothing_to_the_prompt() {
        assert_eq!(render_installed_apps(&[]), "");
    }

    /// The one application the user installed themselves survives a machine
    /// full of distribution entries.
    ///
    /// This is the bug the cut had when it took the inventory's own order:
    /// that order is byte order, so a lowercase name sorts behind every
    /// capitalised one, and `degen-paint Studio` — the application the
    /// failing task named — fell off the end of a 40-entry list on a machine
    /// that had it installed.
    #[test]
    fn a_users_own_app_outranks_the_distributions() {
        let mut apps: Vec<InstalledApp> = (0..APP_LIMIT + 10)
            .map(|nth| InstalledApp {
                id: format!("org.distro.App{nth:03}"),
                name: format!("App {nth:03}"),
                path: PathBuf::from(format!("/usr/share/applications/app{nth:03}.desktop")),
            })
            .collect();
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        apps.push(InstalledApp {
            id: "dev.degenpaint.studio".to_owned(),
            name: "degen-paint Studio".to_owned(),
            path: home.join(".local/share/applications/dev.degenpaint.studio.desktop"),
        });

        let section = render_installed_apps(&apps);
        assert!(section.contains("dev.degenpaint.studio"), "got {section}");
        assert!(section.contains("list is partial"), "got {section}");
    }

    /// The preamble no longer asserts a platform (P16). The list of what is
    /// here is appended, so the constant itself must stay neutral.
    #[test]
    fn the_preamble_names_no_platform() {
        assert!(!PREAMBLE.contains("Mac"), "got {PREAMBLE}");
    }
}
