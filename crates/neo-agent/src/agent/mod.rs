//! The agent loop: a message from the user becomes work in real applications.
//!
//! One turn is a ReAct loop — show the model the conversation, take back
//! either an answer or tool calls, run them, show it what they produced,
//! repeat — and it is metalcraft's loop, not one written here. This module
//! owns the *contract*: [`ChatRequest`] in, [`TurnOutcome`] out, and every
//! step of the middle published as an [`AppEvent`] any number of front ends
//! can watch. [`metal`] owns the mechanism.
//!
//! # Why metalcraft rather than one JSON action per step
//!
//! It used to be one strict-JSON action per step through `Runtime::ask_json`,
//! which was the only primitive both subscription runtimes shared. That loop
//! could not stream: the answer arrived whole, in one round trip, so a turn
//! was a spinner followed by a wall of text. It also had no seam to reach a
//! run that was already going, which is what made "let me correct that while
//! you work" impossible, and it re-expressed every vendor's tool calling as a
//! schema Neo maintained. metalcraft supplies all three — token events, a
//! mailbox polled at every step boundary, and native tool calls — so the
//! hand-rolled loop is gone rather than kept beside it.
//!
//! # What the model may do
//!
//! Only what P2′ and P3 allow: operate the browser, operate a native macOS
//! application, read what an application is showing, or answer. There is no
//! shell action, no file action and no "run this command" action. Every tool
//! that touches an application goes through the same `jev-nav` policy, the
//! same element budget and the same safety heads as `neo nav` — this module
//! chooses *which* surface to drive, never *how* to drive it.

use std::sync::Arc;

use neo_core::{ActionSummary, AppEvent, ConversationId, CoreError, RunId, TurnUsage};
use neo_otel::SpanBuilder;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::runtime::{Runtime, RuntimeError};

pub(crate) mod metal;
mod tools;

pub use metal::STOPPED;
pub use tools::{
    AppOptions, AppRun, BrowserOptions, BrowserRun, ToolError, jev_step, run_app, run_browser,
    surface_run,
};

/// How many actions one user message may cost before the loop stops and says
/// so. A wedged plan must end in bounded time, and a user watching a TUI needs
/// an upper bound they can predict.
pub const DEFAULT_MAX_STEPS: usize = 8;

/// One entry in the conversation the model is shown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub text: String,
}

impl ChatMessage {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            text: text.into(),
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            text: text.into(),
        }
    }

    /// The agent's view of a stored message, or `None` for the roles a model
    /// must not be shown — a stored `system` message is configuration, not
    /// conversation.
    #[must_use]
    pub fn from_stored(message: &neo_core::Message) -> Option<Self> {
        let role = match message.role {
            neo_core::MessageRole::User => Role::User,
            neo_core::MessageRole::Assistant => Role::Assistant,
            neo_core::MessageRole::Tool => Role::Tool,
            neo_core::MessageRole::System => return None,
        };
        Some(Self {
            role,
            text: message.text.clone(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    /// The result of a tool call, fed back to the model.
    Tool,
}

/// What one tool call produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepRecord {
    /// The call as a front end saw it — the same summary
    /// [`AppEvent::TurnStep`] carried, so a card and this record can never
    /// disagree about what the step did.
    pub action: ActionSummary,
    /// What it produced, in the words fed back to the model.
    pub observation: String,
    pub duration_ms: u64,
}

/// One agent turn, asked for as a whole.
///
/// A request rather than a parameter list because every field is something a
/// second observer needs to know about: `run` is what it filters events by,
/// `conversation` is what it renders into, and `cancel` is how it stops the
/// turn from a different thread than the one awaiting it.
#[derive(Clone, Debug)]
pub struct ChatRequest {
    /// Minted by the caller, before the call, so a front end can subscribe
    /// and filter by it without a handshake — a run id handed back at the end
    /// is useless to something that wanted to watch the middle. It is also
    /// what [`Runtime::steer`] addresses.
    pub run: RunId,
    /// The thread this turn belongs to. It must exist in the store: the turn
    /// is recorded against it.
    pub conversation: ConversationId,
    pub history: Vec<ChatMessage>,
    pub max_steps: usize,
    pub cancel: CancellationToken,
}

impl ChatRequest {
    /// A turn with a fresh run id, the default step budget and a token nobody
    /// has cancelled.
    #[must_use]
    pub fn new(conversation: ConversationId, history: Vec<ChatMessage>) -> Self {
        Self {
            run: RunId::new(),
            conversation,
            history,
            max_steps: DEFAULT_MAX_STEPS,
            cancel: CancellationToken::new(),
        }
    }

    #[must_use]
    pub fn with_max_steps(mut self, max_steps: usize) -> Self {
        self.max_steps = max_steps;
        self
    }

    /// Use a token the caller already holds, so a stop button can fire it.
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = cancel;
        self
    }
}

/// The end of a turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnOutcome {
    /// The run this was, echoing [`ChatRequest::run`].
    pub run: RunId,
    /// What to show the user. A stopped turn carries what the model had
    /// already said, which may be nothing.
    pub text: String,
    pub steps: Vec<StepRecord>,
    /// True when the step budget ran out before the model answered.
    pub exhausted: bool,
    /// True when the run was stopped on purpose.
    ///
    /// A stopped turn is an outcome, not an error: it has a partial answer,
    /// steps that really happened and tokens that were really spent, and a
    /// caller that received `Err` had to reconstruct all three from the event
    /// stream. [`AgentError::is_cancelled`] remains for the failures that
    /// happen *inside* a tool.
    pub cancelled: bool,
    /// What the model round trips of this run added up to. `None` when the
    /// turn made none — a stopped turn can end before it asks anything.
    pub usage: Option<TurnUsage>,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Runtime(#[from] Box<RuntimeError>),
    #[error(transparent)]
    Tool(#[from] ToolError),
    /// The turn cannot be run as asked — no message to answer, or an
    /// inference runtime this path does not serve.
    #[error("this turn cannot run: {0}")]
    Request(String),
    /// A node of the agent graph failed: the vendor refused the request, or a
    /// tool could not be dispatched.
    #[error("the agent could not finish: {0}")]
    Graph(String),
    /// The credential the selected path needs is not stored. Names the
    /// command that fixes it, because "missing key" sends a user hunting.
    #[error("no {0} key is stored — run `neo keys set {0}` and try again")]
    NoKey(&'static str),
    /// The run was stopped on purpose. Its own variant, and its own
    /// [`CoreError::Cancelled`] underneath, because a front end must not show
    /// a stopped run the way it shows a broken one.
    #[error(transparent)]
    Core(#[from] CoreError),
}

impl AgentError {
    /// Whether this failure is "the caller asked us to stop", including a
    /// cancellation that happened inside a tool.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        matches!(
            self,
            Self::Core(CoreError::Cancelled)
                | Self::Tool(ToolError::Cancelled(CoreError::Cancelled))
        )
    }
}

impl From<RuntimeError> for AgentError {
    fn from(error: RuntimeError) -> Self {
        Self::Runtime(Box::new(error))
    }
}

impl Runtime {
    /// Run one agent turn over `req.history`.
    ///
    /// Progress is published, never handed to a callback:
    /// [`AppEvent::TurnStarted`], then `TurnDelta` as the answer is produced,
    /// `TurnStep`/`TurnStepDone` per tool call, `NavStep` from inside the
    /// tools, `TurnCost` per model round trip, and finally `TurnFinished` or
    /// `TurnFailed` — every one of them carrying `req.run`. A callback has
    /// exactly one owner, and a turn has as many watchers as the user has
    /// windows open.
    ///
    /// The turn's own rows are written as it goes: the assistant message
    /// grows with the answer, and each tool observation is appended when it
    /// happens. The run is recorded in the `turns` table on the way out
    /// whether it answered, was stopped or failed — the tokens were spent
    /// either way.
    ///
    /// # Steering
    ///
    /// The run is reachable while it lasts: [`Runtime::steer`] delivers a
    /// message into it at the next step boundary.
    ///
    /// # Cancellation
    ///
    /// `req.cancel` stops the turn: metalcraft drops the in-flight node and
    /// hands back what the model had already said, so a stopped turn resolves
    /// to `Ok` with [`TurnOutcome::cancelled`] set and the partial answer
    /// kept, both on the stream and in the thread.
    ///
    /// **What cancellation cannot reclaim.** It stops further work; it is not
    /// an undo. The model round trip in flight is dropped, but the vendor has
    /// already served and counted it. Keystrokes and clicks already delivered
    /// to a real application cannot be un-typed, and a document the app saved
    /// stays saved. A Chrome process already launched is closed by the tool
    /// on its way out — that is the reason for the token, since `task.abort()`
    /// left the browser running — but a page it navigated stays navigated, and
    /// a form it submitted stays submitted.
    ///
    /// # Errors
    ///
    /// Fails when there is no key for the selected runtime, when the model
    /// cannot be reached, or when a node of the graph fails.
    pub async fn chat(self: &Arc<Self>, req: ChatRequest) -> Result<TurnOutcome, AgentError> {
        let settings = self.settings()?;
        let runtime = Arc::clone(self);
        // The whole turn runs inside one span, so every model call, tool run
        // and navigator step underneath it is a child of this turn without
        // any of them being told that a trace exists.
        neo_otel::in_span(turn_span(&req), async move {
            metal::turn(&runtime, &req, &settings).await
        })
        .await
    }

    /// Deliver a message into a turn that is already running.
    ///
    /// `Ok(true)` when the run accepted it, `Ok(false)` when that run has
    /// already finished — the caller then sends it as a new turn instead.
    ///
    /// Accepted means *queued*: the run absorbs it at its next step boundary,
    /// which is the first moment its conversation is coherent enough to add
    /// to. The message is recorded in the thread and announced as
    /// [`AppEvent::TurnSteered`] immediately, so it is visible before the run
    /// has read it — and if the run happens to end before the next boundary,
    /// the row is already in the thread and the next turn's history carries
    /// it. Nothing is dropped silently.
    ///
    /// # Errors
    ///
    /// Fails when the message cannot be recorded in the thread.
    pub fn steer(&self, run: RunId, text: &str) -> Result<bool, AgentError> {
        let Some(steering) = self.live_run(run) else {
            return Ok(false);
        };
        steering.post(text);
        self.record_agent_message(
            neo_store::NewMessage::new(
                steering.conversation(),
                neo_core::MessageRole::User,
                neo_core::MessageSource::Typed,
                text,
                crate::runtime::now_ms()?,
            )
            .with_meta(json!({"run": run.to_string(), "steered": true})),
        )?;
        self.publish(AppEvent::TurnSteered {
            run,
            text: text.to_owned(),
        });
        Ok(true)
    }
}

/// The span one whole turn happens inside.
///
/// `invoke_agent` is the GenAI semantic conventions' name for exactly this:
/// one invocation of an agent, with the steps, model calls and tool runs it
/// caused underneath it. What the user asked is recorded because a turn
/// nobody can read the question of is a turn nobody can diagnose.
fn turn_span(req: &ChatRequest) -> SpanBuilder {
    SpanBuilder::internal("invoke_agent")
        .text("gen_ai.operation.name", "invoke_agent")
        .text("starkbot.turn", req.run.to_string())
        .text(
            "starkbot.user_text",
            req.history
                .iter()
                .rev()
                .find(|message| message.role == Role::User)
                .map(|message| message.text.clone())
                .unwrap_or_default(),
        )
        .int("starkbot.history_len", req.history.len())
        .int("starkbot.max_steps", req.max_steps)
}
