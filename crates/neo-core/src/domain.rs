use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{ConversationId, MessageId, TaskId, TurnId};
use crate::providers::ProviderId;

/// Unix time in milliseconds. The wire representation stays language-neutral.
pub type TimestampMs = i64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
    /// The result of a tool call, fed back to the model as its own turn.
    Tool,
    System,
}

/// What a message *is* in the thread, so a front end can render it without
/// re-deriving intent from the text (04 §6). One variant per value the
/// `messages.kind` CHECK accepts; the two move together, and a kind nothing
/// constructs yet lives in neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    /// Plain conversation: what the user said or typed.
    Text,
    /// A question from the agent that parks the turn (`ask_user`, 03 §8).
    Ask,
    /// What an action produced, handed back to the model.
    Result,
    /// The agent's reply that ends a turn.
    Answer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageSource {
    Voice,
    Typed,
    System,
}

/// One thread. The digest columns that carry Sol's ~1.5k-token carry-over
/// (03 §5.2) stay in SQLite; a front end only needs these four fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    pub id: ConversationId,
    pub title: Option<String>,
    pub created_at: TimestampMs,
    /// Bumped by every append, so "the most recent thread" is one indexed read.
    pub updated_at: TimestampMs,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub conversation_id: ConversationId,
    pub role: MessageRole,
    pub source: MessageSource,
    pub kind: MessageKind,
    pub text: String,
    pub at: TimestampMs,
    pub task_id: Option<TaskId>,
    pub spoken: bool,
    /// Free-form per-message metadata (model, tool name, token counts). Never
    /// a credential: this is persisted and rendered.
    #[serde(default)]
    pub meta: Option<Value>,
}

/// One model round trip. Not a duplicate of the messages it produced: this is
/// what a cost view is built from (05 §4.3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub id: TurnId,
    pub conversation_id: ConversationId,
    pub model: String,
    pub provider: ProviderId,
    pub duration_ms: u64,
    /// The vendor's own usage object, stored verbatim — a normalised subset
    /// would silently drop fields like Anthropic's
    /// `cache_creation_input_tokens`.
    #[serde(default)]
    pub usage: Option<Value>,
    pub started_at: TimestampMs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Running,
    NeedsConfirm,
    WaitingUser,
    Done,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub goal: String,
    pub route: String,
    pub status: TaskStatus,
    pub progress: f32,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
    pub spend_usd: f64,
    pub failure: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Utterance {
    pub pcm_s16le: Vec<i16>,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub started_at: TimestampMs,
    pub ended_at: TimestampMs,
    #[serde(default)]
    pub metadata: Value,
}
