use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{ConversationId, MessageId, TaskId};

/// Unix time in milliseconds. The wire representation stays language-neutral.
pub type TimestampMs = i64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageSource {
    Voice,
    Typed,
    System,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub conversation_id: ConversationId,
    pub role: MessageRole,
    pub source: MessageSource,
    pub text: String,
    pub at: TimestampMs,
    pub task_id: Option<TaskId>,
    pub spoken: bool,
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
