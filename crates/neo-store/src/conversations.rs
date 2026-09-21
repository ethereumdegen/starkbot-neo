//! The conversation thread (03 §1, 05 §4.3): the messages a turn appends and
//! the round trips that produced them.
//!
//! `messages` is the thread a front end renders; `turns` is one row per model
//! round trip, which is what a cost view is built from — a turn that appended
//! three messages is still one call, and a turn that failed appended none.
//!
//! Nothing here holds a credential. `meta` and `usage` are model and token
//! metadata written by the agent loop; a key never reaches SQLite (K1, 05 §6).

use std::str::FromStr;

use neo_core::{
    Conversation, ConversationId, Message, MessageId, MessageKind, MessageRole, MessageSource,
    ProviderId, TaskId, TimestampMs, Turn, TurnId,
};
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::Value;

use crate::{ReadPool, Result, StoreError, Writer};

/// A message on its way into the thread. The store assigns the id and the
/// per-conversation sequence; the caller owns the clock, as it does for the
/// registry cache, so a test never races a real one.
#[derive(Clone, Debug, PartialEq)]
pub struct NewMessage {
    pub conversation_id: ConversationId,
    pub role: MessageRole,
    pub source: MessageSource,
    pub kind: MessageKind,
    pub text: String,
    pub task_id: Option<TaskId>,
    pub meta: Option<Value>,
    pub at: TimestampMs,
}

impl NewMessage {
    /// A plain [`MessageKind::Text`] message with no task and no metadata.
    #[must_use]
    pub fn new(
        conversation_id: ConversationId,
        role: MessageRole,
        source: MessageSource,
        text: impl Into<String>,
        at: TimestampMs,
    ) -> Self {
        Self {
            conversation_id,
            role,
            source,
            kind: MessageKind::Text,
            text: text.into(),
            task_id: None,
            meta: None,
            at,
        }
    }

    #[must_use]
    pub fn with_kind(mut self, kind: MessageKind) -> Self {
        self.kind = kind;
        self
    }

    #[must_use]
    pub fn with_task(mut self, task_id: TaskId) -> Self {
        self.task_id = Some(task_id);
        self
    }

    #[must_use]
    pub fn with_meta(mut self, meta: Value) -> Self {
        self.meta = Some(meta);
        self
    }
}

/// One finished model round trip. `usage` is the vendor's own object, stored
/// verbatim.
#[derive(Clone, Debug, PartialEq)]
pub struct NewTurn {
    pub conversation_id: ConversationId,
    pub model: String,
    pub provider: ProviderId,
    pub duration_ms: u64,
    pub usage: Option<Value>,
    pub started_at: TimestampMs,
}

/// The thread read, named so the query-plan test asserts against the query
/// the repository actually runs. `ORDER BY … DESC` walks `messages_thread`
/// backwards: newest `limit` rows, no temporary b-tree, reversed in Rust.
pub(crate) const THREAD_QUERY: &str = "SELECT id, conversation_id, role, source, kind, text, meta, task_id, spoken, created_at \
     FROM messages WHERE conversation_id = ?1 ORDER BY created_at DESC, seq DESC LIMIT ?2";

#[derive(Clone)]
pub struct ConversationRepository {
    writer: Writer,
    readers: ReadPool,
}

impl ConversationRepository {
    pub(crate) fn new(writer: Writer, readers: ReadPool) -> Self {
        Self { writer, readers }
    }

    /// Start a thread. `at` is the creation time in Unix milliseconds and
    /// seeds `updated_at`, so a brand-new conversation already sorts first.
    pub fn create(&self, title: Option<String>, at: TimestampMs) -> Result<Conversation> {
        let conversation = Conversation {
            id: ConversationId::new(),
            title,
            created_at: at,
            updated_at: at,
        };
        let row = conversation.clone();
        self.writer.execute(move |connection| {
            connection.execute(
                "INSERT INTO conversations(id, title, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    row.id.to_string(),
                    row.title,
                    row.created_at,
                    row.updated_at
                ],
            )?;
            Ok(())
        })?;
        Ok(conversation)
    }

    /// The `limit` most recently active threads, newest first.
    pub fn list(&self, limit: u32) -> Result<Vec<Conversation>> {
        self.readers.read(move |connection| {
            let mut statement = connection.prepare(
                "SELECT id, title, created_at, updated_at FROM conversations \
                 ORDER BY updated_at DESC, id DESC LIMIT ?1",
            )?;
            let rows = statement.query_map([i64::from(limit)], conversation_row)?;
            let mut conversations = Vec::new();
            for row in rows {
                conversations.push(decode_conversation(row?)?);
            }
            Ok(conversations)
        })
    }

    /// The thread the user was last in, if there is one.
    pub fn latest(&self) -> Result<Option<Conversation>> {
        self.readers.read(|connection| {
            let row = connection
                .query_row(
                    "SELECT id, title, created_at, updated_at FROM conversations \
                     ORDER BY updated_at DESC, id DESC LIMIT 1",
                    [],
                    conversation_row,
                )
                .optional()?;
            row.map(decode_conversation).transpose()
        })
    }

    /// Append one message and bump the thread's `updated_at`, in one
    /// transaction: a reader never sees a message whose conversation still
    /// looks idle. The conversation must exist — the foreign key says so.
    pub fn append_message(&self, message: NewMessage) -> Result<Message> {
        let stored = Message {
            id: MessageId::new(),
            conversation_id: message.conversation_id,
            role: message.role,
            source: message.source,
            kind: message.kind,
            text: message.text,
            at: message.at,
            task_id: message.task_id,
            spoken: false,
            meta: message.meta,
        };
        let row = stored.clone();
        self.writer.execute(move |connection| {
            let transaction = connection.transaction()?;
            let conversation = row.conversation_id.to_string();
            let seq: i64 = transaction.query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM messages WHERE conversation_id = ?1",
                [&conversation],
                |row| row.get(0),
            )?;
            let meta = row.meta.as_ref().map(serde_json::to_string).transpose()?;
            transaction.execute(
                "INSERT INTO messages(id, conversation_id, seq, role, source, kind, text, meta, \
                 task_id, spoken, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10)",
                params![
                    row.id.to_string(),
                    conversation,
                    seq,
                    encode_role(row.role),
                    encode_source(row.source),
                    encode_kind(row.kind),
                    row.text,
                    meta,
                    row.task_id.map(|id| id.to_string()),
                    row.at,
                ],
            )?;
            touch(&transaction, &conversation, row.at)?;
            transaction.commit()?;
            Ok(())
        })?;
        Ok(stored)
    }

    /// Grow an assistant message that is still being written, or insert it on
    /// the first slice. Returns the message id, so a front end can update one
    /// row.
    ///
    /// An answer arrives as a stream of slices, and the thread has to hold
    /// what has arrived so far: a turn stopped halfway through its sentence
    /// used to leave nothing at all in the store, because the row was written
    /// once, at the end, from the finished text.
    ///
    /// **The row's identity while it grows is `meta.run`.** One agent run
    /// writes one assistant row, so the run id is what distinguishes "more of
    /// the answer I am already writing" from "the answer to the next
    /// question" — a caller needs no handle, and two processes watching the
    /// same thread agree about which row grew. A message with no `run` in its
    /// `meta` has no such identity and is simply inserted.
    pub fn upsert_streaming_message(
        &self,
        message: &NewMessage,
        append: &str,
    ) -> Result<MessageId> {
        let run = message
            .meta
            .as_ref()
            .and_then(|meta| meta.get("run"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let fresh = MessageId::new();
        let row = message.clone();
        let append = append.to_owned();
        self.writer.execute(move |connection| {
            let transaction = connection.transaction()?;
            let conversation = row.conversation_id.to_string();
            let existing: Option<String> = match &run {
                Some(run) => transaction
                    .query_row(
                        "SELECT id FROM messages WHERE conversation_id = ?1 \
                         AND json_extract(meta, '$.run') = ?2 AND role = ?3 \
                         ORDER BY seq DESC LIMIT 1",
                        params![&conversation, run, encode_role(row.role)],
                        |row| row.get(0),
                    )
                    .optional()?,
                None => None,
            };
            let id = match existing {
                // `text || ?` rather than a read-modify-write: the append is
                // one statement, so a slice cannot be lost to a reader that
                // saw the row between the two halves.
                Some(id) => {
                    transaction.execute(
                        "UPDATE messages SET text = text || ?1 WHERE id = ?2",
                        params![append, id],
                    )?;
                    id
                }
                None => {
                    let id = fresh.to_string();
                    let seq: i64 = transaction.query_row(
                        "SELECT COALESCE(MAX(seq), 0) + 1 FROM messages WHERE conversation_id = ?1",
                        [&conversation],
                        |row| row.get(0),
                    )?;
                    let meta = row.meta.as_ref().map(serde_json::to_string).transpose()?;
                    let mut text = row.text.clone();
                    text.push_str(&append);
                    transaction.execute(
                        "INSERT INTO messages(id, conversation_id, seq, role, source, kind, text, \
                         meta, task_id, spoken, created_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10)",
                        params![
                            id,
                            conversation,
                            seq,
                            encode_role(row.role),
                            encode_source(row.source),
                            encode_kind(row.kind),
                            text,
                            meta,
                            row.task_id.map(|id| id.to_string()),
                            row.at,
                        ],
                    )?;
                    id
                }
            };
            touch(&transaction, &conversation, row.at)?;
            transaction.commit()?;
            parse_id(&id, "id")
        })
    }

    /// The `limit` most recent messages of one thread, oldest first — the
    /// order a front end renders and a prompt replays. Served by
    /// `messages_thread (conversation_id, created_at, seq)`.
    pub fn messages(&self, conversation: ConversationId, limit: u32) -> Result<Vec<Message>> {
        let conversation = conversation.to_string();
        self.readers.read(move |connection| {
            let mut statement = connection.prepare(THREAD_QUERY)?;
            let rows = statement.query_map(params![conversation, i64::from(limit)], message_row)?;
            let mut messages = Vec::new();
            for row in rows {
                messages.push(decode_message(row?)?);
            }
            messages.reverse();
            Ok(messages)
        })
    }

    /// Record one model round trip. The `usage` object is stored as the vendor
    /// sent it; nothing here normalises or drops fields.
    pub fn record_turn(&self, turn: NewTurn) -> Result<Turn> {
        let stored = Turn {
            id: TurnId::new(),
            conversation_id: turn.conversation_id,
            model: turn.model,
            provider: turn.provider,
            duration_ms: turn.duration_ms,
            usage: turn.usage,
            started_at: turn.started_at,
        };
        let row = stored.clone();
        self.writer.execute(move |connection| {
            let transaction = connection.transaction()?;
            let conversation = row.conversation_id.to_string();
            let duration = i64::try_from(row.duration_ms)
                .map_err(|_| StoreError::ValueOverflow("turns.duration_ms"))?;
            let usage = row.usage.as_ref().map(serde_json::to_string).transpose()?;
            transaction.execute(
                "INSERT INTO turns(id, conversation_id, model, provider, duration_ms, usage, \
                 started_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    row.id.to_string(),
                    conversation,
                    row.model,
                    row.provider.as_str(),
                    duration,
                    usage,
                    row.started_at,
                ],
            )?;
            touch(&transaction, &conversation, row.started_at)?;
            transaction.commit()?;
            Ok(())
        })?;
        Ok(stored)
    }

    /// The `limit` most recent round trips of one thread, oldest first.
    pub fn turns(&self, conversation: ConversationId, limit: u32) -> Result<Vec<Turn>> {
        let conversation = conversation.to_string();
        self.readers.read(move |connection| {
            let mut statement = connection.prepare(
                "SELECT id, conversation_id, model, provider, duration_ms, usage, started_at \
                 FROM turns WHERE conversation_id = ?1 ORDER BY started_at DESC, id DESC LIMIT ?2",
            )?;
            let rows = statement.query_map(params![conversation, i64::from(limit)], turn_row)?;
            let mut turns = Vec::new();
            for row in rows {
                turns.push(decode_turn(row?)?);
            }
            turns.reverse();
            Ok(turns)
        })
    }

    /// Give a thread a title. Renaming a thread that is not there is an error,
    /// not a silent no-op.
    pub fn rename(&self, id: ConversationId, title: &str, at: TimestampMs) -> Result<()> {
        let title = title.to_owned();
        self.writer.execute(move |connection| {
            let changed = connection.execute(
                "UPDATE conversations SET title = ?2, updated_at = MAX(updated_at, ?3) \
                 WHERE id = ?1",
                params![id.to_string(), title, at],
            )?;
            if changed == 0 {
                return Err(StoreError::UnknownConversation(id.to_string()));
            }
            Ok(())
        })
    }

    /// Delete a thread and everything hanging off it — messages, turns and
    /// utterances cascade; a task that referenced it keeps its own row with a
    /// null conversation. Deleting a thread that is already gone is fine.
    pub fn delete(&self, id: ConversationId) -> Result<()> {
        self.writer.execute(move |connection| {
            connection.execute("DELETE FROM conversations WHERE id = ?1", [id.to_string()])?;
            Ok(())
        })
    }
}

/// A thread is "active" at the moment of its newest message or turn; an
/// out-of-order write never drags that backwards.
fn touch(transaction: &Transaction<'_>, conversation: &str, at: TimestampMs) -> Result<()> {
    transaction.execute(
        "UPDATE conversations SET updated_at = MAX(updated_at, ?2) WHERE id = ?1",
        params![conversation, at],
    )?;
    Ok(())
}

type ConversationRow = (String, Option<String>, i64, i64);
type MessageRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    i64,
    i64,
);
type TurnRow = (String, String, String, String, i64, Option<String>, i64);

fn conversation_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConversationRow> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

fn message_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MessageRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
    ))
}

fn turn_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TurnRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
    ))
}

fn decode_conversation(row: ConversationRow) -> Result<Conversation> {
    Ok(Conversation {
        id: parse_id(&row.0, "conversations.id")?,
        title: row.1,
        created_at: row.2,
        updated_at: row.3,
    })
}

fn decode_message(row: MessageRow) -> Result<Message> {
    let task_id = row
        .7
        .as_deref()
        .map(|id| parse_id(id, "messages.task_id"))
        .transpose()?;
    Ok(Message {
        id: parse_id(&row.0, "messages.id")?,
        conversation_id: parse_id(&row.1, "messages.conversation_id")?,
        role: decode_role(&row.2)?,
        source: decode_source(&row.3)?,
        kind: decode_kind(&row.4)?,
        text: row.5,
        meta: row.6.as_deref().map(serde_json::from_str).transpose()?,
        task_id,
        spoken: row.8 != 0,
        at: row.9,
    })
}

fn decode_turn(row: TurnRow) -> Result<Turn> {
    Ok(Turn {
        id: parse_id(&row.0, "turns.id")?,
        conversation_id: parse_id(&row.1, "turns.conversation_id")?,
        model: row.2,
        provider: ProviderId::new(row.3),
        duration_ms: u64::try_from(row.4)
            .map_err(|_| StoreError::InvalidConversationRow("turns.duration_ms".into()))?,
        usage: row.5.as_deref().map(serde_json::from_str).transpose()?,
        started_at: row.6,
    })
}

/// A malformed row is reported by column, never by content: a message's text
/// does not belong in an error string.
fn parse_id<T: FromStr>(value: &str, column: &str) -> Result<T> {
    T::from_str(value).map_err(|_| StoreError::InvalidConversationRow(column.to_owned()))
}

fn encode_role(role: MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
        MessageRole::System => "system",
    }
}

fn decode_role(value: &str) -> Result<MessageRole> {
    match value {
        "user" => Ok(MessageRole::User),
        "assistant" => Ok(MessageRole::Assistant),
        "tool" => Ok(MessageRole::Tool),
        "system" => Ok(MessageRole::System),
        _ => Err(StoreError::InvalidConversationRow("messages.role".into())),
    }
}

fn encode_source(source: MessageSource) -> &'static str {
    match source {
        MessageSource::Voice => "voice",
        MessageSource::Typed => "typed",
        MessageSource::System => "system",
    }
}

fn decode_source(value: &str) -> Result<MessageSource> {
    match value {
        "voice" => Ok(MessageSource::Voice),
        "typed" => Ok(MessageSource::Typed),
        "system" => Ok(MessageSource::System),
        _ => Err(StoreError::InvalidConversationRow("messages.source".into())),
    }
}

fn encode_kind(kind: MessageKind) -> &'static str {
    match kind {
        MessageKind::Text => "text",
        MessageKind::Ask => "ask",
        MessageKind::Result => "result",
        MessageKind::Answer => "answer",
    }
}

fn decode_kind(value: &str) -> Result<MessageKind> {
    match value {
        "text" => Ok(MessageKind::Text),
        "ask" => Ok(MessageKind::Ask),
        "result" => Ok(MessageKind::Result),
        "answer" => Ok(MessageKind::Answer),
        _ => Err(StoreError::InvalidConversationRow("messages.kind".into())),
    }
}
