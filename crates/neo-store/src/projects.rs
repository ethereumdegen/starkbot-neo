use std::str::FromStr;

use neo_core::{HeartbeatGate, HeartbeatOutcome, HeartbeatTick, Project, TaskId, TimestampMs};
use rusqlite::{OptionalExtension, Row, params};

use crate::connection::write_transaction;
use crate::{ReadPool, Result, StoreError, Writer};

pub struct FinishHeartbeatTick {
    pub project: String,
    pub started_at: TimestampMs,
    pub finished_at: TimestampMs,
    pub outcome: HeartbeatOutcome,
    pub reason: Option<String>,
    pub task_id: Option<TaskId>,
    pub goal_bytes: u64,
    pub next_due_at: Option<TimestampMs>,
    pub consecutive_failures: u32,
}

#[derive(Clone)]
pub struct ProjectRepository {
    writer: Writer,
    readers: ReadPool,
}

impl ProjectRepository {
    pub(crate) fn new(writer: Writer, readers: ReadPool) -> Self {
        Self { writer, readers }
    }

    pub fn create(
        &self,
        slug: String,
        name: String,
        root: String,
        at: TimestampMs,
    ) -> Result<Project> {
        let project = Project {
            slug,
            name,
            root,
            heartbeat_enabled: false,
            heartbeat_every_seconds: 14_400,
            on_gate: HeartbeatGate::Hold,
            last_tick_at: None,
            next_due_at: None,
            consecutive_failures: 0,
            created_at: at,
            updated_at: at,
        };
        let row = project.clone();
        self.writer.execute(move |connection| {
            connection.execute(
                "INSERT INTO projects(slug, name, root, heartbeat_enabled, \
                 heartbeat_every_seconds, on_gate, last_tick_at, next_due_at, \
                 consecutive_failures, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, 0, ?4, 'hold', NULL, NULL, 0, ?5, ?5)",
                params![
                    row.slug,
                    row.name,
                    row.root,
                    seconds(row.heartbeat_every_seconds)?,
                    row.created_at,
                ],
            )?;
            Ok(())
        })?;
        Ok(project)
    }

    pub fn get(&self, slug: &str) -> Result<Project> {
        let slug = slug.to_owned();
        self.readers.read(move |connection| {
            connection
                .query_row(
                    &format!("{PROJECT_SELECT} WHERE slug = ?1"),
                    [&slug],
                    project_row,
                )
                .optional()?
                .ok_or(StoreError::UnknownProject(slug))
        })
    }

    pub fn list(&self) -> Result<Vec<Project>> {
        self.readers.read(|connection| {
            let mut statement = connection.prepare(&format!(
                "{PROJECT_SELECT} ORDER BY updated_at DESC, slug ASC"
            ))?;
            let rows = statement.query_map([], project_row)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(StoreError::from)
        })
    }

    /// Enabled projects whose clock has reached `now`, oldest deadline first.
    pub fn due(&self, now: TimestampMs) -> Result<Vec<Project>> {
        self.readers.read(move |connection| {
            let mut statement = connection.prepare(&format!(
                "{PROJECT_SELECT} WHERE heartbeat_enabled = 1 AND next_due_at <= ?1 \
                 ORDER BY next_due_at ASC, slug ASC"
            ))?;
            let rows = statement.query_map([now], project_row)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(StoreError::from)
        })
    }

    pub fn configure_heartbeat(
        &self,
        slug: &str,
        enabled: bool,
        every_seconds: u64,
        on_gate: HeartbeatGate,
        now: TimestampMs,
    ) -> Result<Project> {
        if !(300..=604_800).contains(&every_seconds) {
            return Err(StoreError::InvalidHeartbeatInterval(every_seconds));
        }
        let slug = slug.to_owned();
        let due =
            enabled.then_some(now.saturating_add(seconds(every_seconds)?.saturating_mul(1000)));
        let gate = gate_name(on_gate);
        self.writer.execute({
            let slug = slug.clone();
            move |connection| {
                let changed = connection.execute(
                    "UPDATE projects SET heartbeat_enabled = ?1, heartbeat_every_seconds = ?2, \
                     on_gate = ?3, next_due_at = ?4, updated_at = ?5 WHERE slug = ?6",
                    params![enabled, seconds(every_seconds)?, gate, due, now, slug],
                )?;
                if changed == 0 {
                    return Err(StoreError::UnknownProject(slug));
                }
                Ok(())
            }
        })?;
        self.get(&slug)
    }

    /// Record a tick and move that project's clock in the same transaction.
    pub fn finish_tick(&self, tick: FinishHeartbeatTick) -> Result<HeartbeatTick> {
        let FinishHeartbeatTick {
            project: slug,
            started_at,
            finished_at,
            outcome,
            reason,
            task_id,
            goal_bytes,
            next_due_at,
            consecutive_failures,
        } = tick;
        let slug = slug.to_owned();
        let outcome_name = outcome_name(outcome);
        let task = task_id.map(|id| id.to_string());
        let bytes = i64::try_from(goal_bytes)
            .map_err(|_| StoreError::ValueOverflow("heartbeat_ticks.goal_bytes"))?;
        let failures = i64::from(consecutive_failures);
        let stored_slug = slug.clone();
        let stored_reason = reason.clone();
        let inserted = self.writer.execute(move |connection| {
            let transaction = write_transaction(connection)?;
            let changed = transaction.execute(
                "UPDATE projects SET last_tick_at = ?1, next_due_at = ?2, \
                 consecutive_failures = ?3, updated_at = ?1 WHERE slug = ?4",
                params![finished_at, next_due_at, failures, stored_slug],
            )?;
            if changed == 0 {
                return Err(StoreError::UnknownProject(stored_slug));
            }
            transaction.execute(
                "INSERT INTO heartbeat_ticks(project, started_at, finished_at, outcome, reason, \
                     task_id, goal_bytes) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    stored_slug,
                    started_at,
                    finished_at,
                    outcome_name,
                    stored_reason,
                    task,
                    bytes
                ],
            )?;
            let id = transaction.last_insert_rowid();
            transaction.commit()?;
            Ok(id)
        })?;
        Ok(HeartbeatTick {
            id: inserted,
            project: slug,
            started_at,
            finished_at,
            outcome,
            reason,
            task_id,
            goal_bytes,
        })
    }

    pub fn ticks(&self, slug: &str, limit: u32) -> Result<Vec<HeartbeatTick>> {
        let slug = slug.to_owned();
        self.readers.read(move |connection| {
            let mut statement = connection.prepare(
                "SELECT id, project, started_at, finished_at, outcome, reason, task_id, goal_bytes \
                 FROM heartbeat_ticks WHERE project = ?1 ORDER BY started_at DESC, id DESC LIMIT ?2",
            )?;
            let rows = statement.query_map(params![slug, i64::from(limit)], tick_row)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(StoreError::from)
        })
    }
}

const PROJECT_SELECT: &str = "SELECT slug, name, root, heartbeat_enabled, heartbeat_every_seconds, \
    on_gate, last_tick_at, next_due_at, consecutive_failures, created_at, updated_at FROM projects";

fn project_row(row: &Row<'_>) -> rusqlite::Result<Project> {
    let every: i64 = row.get(4)?;
    let failures: i64 = row.get(8)?;
    Ok(Project {
        slug: row.get(0)?,
        name: row.get(1)?,
        root: row.get(2)?,
        heartbeat_enabled: row.get(3)?,
        heartbeat_every_seconds: u64::try_from(every).map_err(|_| invalid_column(4, every))?,
        on_gate: parse_gate(row.get::<_, String>(5)?.as_str()).map_err(|_| invalid_text(5))?,
        last_tick_at: row.get(6)?,
        next_due_at: row.get(7)?,
        consecutive_failures: u32::try_from(failures).map_err(|_| invalid_column(8, failures))?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn tick_row(row: &Row<'_>) -> rusqlite::Result<HeartbeatTick> {
    let outcome = row.get::<_, String>(4)?;
    let task = row.get::<_, Option<String>>(6)?;
    let bytes: i64 = row.get(7)?;
    Ok(HeartbeatTick {
        id: row.get(0)?,
        project: row.get(1)?,
        started_at: row.get(2)?,
        finished_at: row.get(3)?,
        outcome: parse_outcome(&outcome).map_err(|_| invalid_text(4))?,
        reason: row.get(5)?,
        task_id: task
            .map(|id| TaskId::from_str(&id).map_err(|_| invalid_text(6)))
            .transpose()?,
        goal_bytes: u64::try_from(bytes).map_err(|_| invalid_column(7, bytes))?,
    })
}

fn seconds(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| StoreError::ValueOverflow("projects.heartbeat_every_seconds"))
}

fn gate_name(gate: HeartbeatGate) -> &'static str {
    match gate {
        HeartbeatGate::Hold => "hold",
        HeartbeatGate::Skip => "skip",
    }
}

fn parse_gate(value: &str) -> std::result::Result<HeartbeatGate, ()> {
    match value {
        "hold" => Ok(HeartbeatGate::Hold),
        "skip" => Ok(HeartbeatGate::Skip),
        _ => Err(()),
    }
}

fn outcome_name(outcome: HeartbeatOutcome) -> &'static str {
    match outcome {
        HeartbeatOutcome::Done => "done",
        HeartbeatOutcome::Skipped => "skipped",
        HeartbeatOutcome::Held => "held",
        HeartbeatOutcome::Failed => "failed",
        HeartbeatOutcome::Dropped => "dropped",
    }
}

fn parse_outcome(value: &str) -> std::result::Result<HeartbeatOutcome, ()> {
    match value {
        "done" => Ok(HeartbeatOutcome::Done),
        "skipped" => Ok(HeartbeatOutcome::Skipped),
        "held" => Ok(HeartbeatOutcome::Held),
        "failed" => Ok(HeartbeatOutcome::Failed),
        "dropped" => Ok(HeartbeatOutcome::Dropped),
        _ => Err(()),
    }
}

fn invalid_text(column: usize) -> rusqlite::Error {
    rusqlite::Error::InvalidColumnType(column, String::new(), rusqlite::types::Type::Text)
}

fn invalid_column(column: usize, value: i64) -> rusqlite::Error {
    rusqlite::Error::IntegralValueOutOfRange(column, value)
}
