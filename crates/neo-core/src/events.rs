use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AddressingMode, AskId, ConfirmId, ConversationId, DisplayId, MediaJobId, Message, MessageId,
    Settings, Task, TaskId, TaskStatus, TimestampMs, Usage,
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
pub enum KeyState {
    Missing,
    Present,
    Invalid,
    Unchecked,
    Limited,
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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppEventEnvelope {
    pub seq: u64,
    pub event: AppEvent,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_wire_format_is_tagged_snake_case() {
        let event = AppEventEnvelope {
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
}
