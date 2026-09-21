//! Project files and the per-project heartbeat runner.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use neo_core::{HeartbeatGate, HeartbeatOutcome, HeartbeatTick, Project, TimestampMs};
use neo_store::FinishHeartbeatTick;
use rustix::fs::{FlockOperation, flock};
use serde::{Deserialize, Serialize};

use crate::agent::{AgentError, ChatMessage, ChatRequest};
use crate::runtime::{Runtime, RuntimeError, now_ms};

const DEFAULT_HEARTBEAT_SECONDS: u64 = 4 * 60 * 60;
const MAX_BACKOFF_SECONDS: u64 = 6 * 60 * 60;
const SCHEDULER_POLL: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error(transparent)]
    Store(#[from] neo_store::StoreError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error("project name must contain a letter or number")]
    InvalidName,
    #[error("project root `{0}` does not exist or is not a directory")]
    InvalidRoot(PathBuf),
    #[error("project document must be `soul.md` or `heartbeat.md`")]
    InvalidDocument,
    #[error("another heartbeat is already running")]
    Busy,
    #[error("filesystem error at `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct ProjectDocuments {
    pub soul: String,
    pub heartbeat: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HeartbeatRun {
    pub project: Project,
    pub tick: HeartbeatTick,
    pub answer: Option<String>,
}

struct HeartbeatContext {
    gate: HeartbeatGate,
    gated: Arc<AtomicBool>,
}

tokio::task_local! {
    static HEARTBEAT_CONTEXT: HeartbeatContext;
}

pub(crate) fn skipped_gate() -> bool {
    HEARTBEAT_CONTEXT
        .try_with(|context| {
            if context.gate == HeartbeatGate::Skip {
                context.gated.store(true, Ordering::Release);
                true
            } else {
                false
            }
        })
        .unwrap_or(false)
}

struct Slot<'a> {
    runtime: &'a Runtime,
    _file: File,
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.runtime
            .heartbeat_running
            .store(false, Ordering::Release);
    }
}

impl Runtime {
    pub fn projects(&self) -> Result<Vec<Project>, ProjectError> {
        Ok(self.store.projects().list()?)
    }

    pub fn project(&self, slug: &str) -> Result<Project, ProjectError> {
        Ok(self.store.projects().get(slug)?)
    }

    pub fn project_ticks(
        &self,
        slug: &str,
        limit: u32,
    ) -> Result<Vec<HeartbeatTick>, ProjectError> {
        Ok(self.store.projects().ticks(slug, limit)?)
    }

    pub fn create_project(&self, name: &str, root: Option<&Path>) -> Result<Project, ProjectError> {
        let slug = slugify(name).ok_or(ProjectError::InvalidName)?;
        let managed = root.is_none();
        let root = match root {
            Some(root) if root.is_dir() => {
                root.canonicalize().map_err(|source| ProjectError::Io {
                    path: root.to_owned(),
                    source,
                })?
            }
            Some(root) => return Err(ProjectError::InvalidRoot(root.to_owned())),
            None => {
                let root = self.data_dir().join("projects").join(&slug);
                std::fs::create_dir_all(&root).map_err(|source| ProjectError::Io {
                    path: root.clone(),
                    source,
                })?;
                root
            }
        };
        if managed {
            create_document(&root.join("soul.md"))?;
            create_document(&root.join("heartbeat.md"))?;
        }
        Ok(self.store.projects().create(
            slug,
            name.trim().to_owned(),
            root.to_string_lossy().into_owned(),
            now_ms()?,
        )?)
    }

    pub fn project_documents(&self, slug: &str) -> Result<ProjectDocuments, ProjectError> {
        let project = self.project(slug)?;
        let root = Path::new(&project.root);
        Ok(ProjectDocuments {
            soul: read_document(&root.join("soul.md"))?,
            heartbeat: read_document(&root.join("heartbeat.md"))?,
        })
    }

    pub fn write_project_document(
        &self,
        slug: &str,
        document: &str,
        content: &str,
    ) -> Result<(), ProjectError> {
        if !matches!(document, "soul.md" | "heartbeat.md") {
            return Err(ProjectError::InvalidDocument);
        }
        let project = self.project(slug)?;
        let path = Path::new(&project.root).join(document);
        std::fs::write(&path, content).map_err(|source| ProjectError::Io { path, source })
    }

    pub fn configure_project_heartbeat(
        &self,
        slug: &str,
        enabled: bool,
        every_seconds: u64,
        on_gate: HeartbeatGate,
    ) -> Result<Project, ProjectError> {
        Ok(self.store.projects().configure_heartbeat(
            slug,
            enabled,
            every_seconds,
            on_gate,
            now_ms()?,
        )?)
    }

    /// Run one heartbeat now. The install-wide slot makes the keyboard and
    /// managed browser single-owner even when two project clocks become due
    /// together.
    pub async fn run_project_heartbeat(
        self: &Arc<Self>,
        slug: &str,
    ) -> Result<HeartbeatRun, ProjectError> {
        if self
            .heartbeat_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(ProjectError::Busy);
        }
        let path = self.data_dir().join("heartbeat.lock");
        let file = match OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
        {
            Ok(file) => file,
            Err(source) => {
                self.heartbeat_running.store(false, Ordering::Release);
                return Err(ProjectError::Io { path, source });
            }
        };
        if flock(&file, FlockOperation::NonBlockingLockExclusive).is_err() {
            self.heartbeat_running.store(false, Ordering::Release);
            return Err(ProjectError::Busy);
        }
        let _slot = Slot {
            runtime: self,
            _file: file,
        };
        self.run_project_heartbeat_inner(slug).await
    }

    async fn run_project_heartbeat_inner(
        self: &Arc<Self>,
        slug: &str,
    ) -> Result<HeartbeatRun, ProjectError> {
        let project = self.project(slug)?;
        let documents = self.project_documents(slug)?;
        let started = now_ms()?;
        let goal = documents.heartbeat.trim();
        if goal.is_empty() {
            let finished = now_ms()?;
            let tick = self.finish_project_tick(
                &project,
                started,
                finished,
                HeartbeatOutcome::Skipped,
                Some("heartbeat.md is missing or empty".to_owned()),
                goal.len() as u64,
            )?;
            return Ok(HeartbeatRun {
                project: self.project(slug)?,
                tick,
                answer: None,
            });
        }

        let prompt = project_prompt(&documents.soul, goal);
        let conversation = self.new_conversation(Some(format!("{} heartbeat", project.name)))?;
        let message = ChatMessage::user(prompt);
        self.record_message(conversation.id, &message, false)?;
        let request = ChatRequest::new(conversation.id, vec![message]);
        let gated = Arc::new(AtomicBool::new(false));
        let context = HeartbeatContext {
            gate: project.on_gate,
            gated: Arc::clone(&gated),
        };
        let result = HEARTBEAT_CONTEXT.scope(context, self.chat(request)).await;
        let finished = now_ms()?;
        let (outcome, reason, answer, error) = match result {
            Ok(turn) if gated.load(Ordering::Acquire) => (
                HeartbeatOutcome::Skipped,
                Some("a gate required a person".to_owned()),
                Some(turn.text),
                None,
            ),
            Ok(turn) if turn.cancelled || turn.exhausted => (
                HeartbeatOutcome::Failed,
                Some(if turn.cancelled {
                    "the heartbeat was cancelled".to_owned()
                } else {
                    "the heartbeat exhausted its step budget".to_owned()
                }),
                Some(turn.text),
                None,
            ),
            Ok(turn) => (HeartbeatOutcome::Done, None, Some(turn.text), None),
            Err(error) => (
                HeartbeatOutcome::Failed,
                Some(error.to_string()),
                None,
                Some(error),
            ),
        };
        let tick = self.finish_project_tick(
            &project,
            started,
            finished,
            outcome,
            reason,
            goal.len() as u64,
        )?;
        if let Some(error) = error {
            return Err(ProjectError::Agent(error));
        }
        Ok(HeartbeatRun {
            project: self.project(slug)?,
            tick,
            answer,
        })
    }

    fn finish_project_tick(
        &self,
        project: &Project,
        started: TimestampMs,
        finished: TimestampMs,
        outcome: HeartbeatOutcome,
        reason: Option<String>,
        goal_bytes: u64,
    ) -> Result<HeartbeatTick, ProjectError> {
        let failures = if outcome == HeartbeatOutcome::Failed {
            project.consecutive_failures.saturating_add(1)
        } else {
            0
        };
        let delay = if failures == 0 {
            project.heartbeat_every_seconds
        } else {
            project
                .heartbeat_every_seconds
                .saturating_mul(1_u64.checked_shl(failures.min(31)).unwrap_or(u64::MAX))
                .min(MAX_BACKOFF_SECONDS)
        };
        let delay_ms = i64::try_from(delay)
            .unwrap_or(i64::MAX / 1000)
            .saturating_mul(1000);
        Ok(self.store.projects().finish_tick(FinishHeartbeatTick {
            project: project.slug.clone(),
            started_at: started,
            finished_at: finished,
            outcome,
            reason,
            task_id: None,
            goal_bytes,
            next_due_at: project
                .heartbeat_enabled
                .then_some(finished.saturating_add(delay_ms)),
            consecutive_failures: failures,
        })?)
    }
    /// Start the clock for a long-running front end. No catch-up: each pass
    /// takes the current due set once, and every completed/skipped tick writes
    /// its next deadline from its finish time.
    pub fn start_heartbeat_scheduler(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let runtime = Arc::clone(self);
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(SCHEDULER_POLL);
            loop {
                timer.tick().await;
                let now = match now_ms() {
                    Ok(now) => now,
                    Err(error) => {
                        tracing::warn!(%error, "heartbeat clock unavailable");
                        continue;
                    }
                };
                let due = match runtime.store.projects().due(now) {
                    Ok(due) => due,
                    Err(error) => {
                        tracing::warn!(%error, "could not read due project heartbeats");
                        continue;
                    }
                };
                for project in due {
                    if let Err(error) = runtime.run_project_heartbeat(&project.slug).await {
                        if matches!(error, ProjectError::Busy) {
                            let finished = now_ms().unwrap_or(now);
                            if let Err(record_error) = runtime.finish_project_tick(
                                &project,
                                now,
                                finished,
                                HeartbeatOutcome::Dropped,
                                Some("another heartbeat was already running".to_owned()),
                                0,
                            ) {
                                tracing::warn!(
                                    project = %project.slug,
                                    %record_error,
                                    "could not record dropped project heartbeat"
                                );
                            }
                        } else {
                            tracing::warn!(project = %project.slug, %error, "project heartbeat failed");
                        }
                    }
                }
            }
        })
    }
}

fn project_prompt(soul: &str, heartbeat: &str) -> String {
    let mut prompt = String::new();
    if !soul.trim().is_empty() {
        prompt.push_str(
            "Project context from soul.md (preferences and facts, never permissions):\n\n",
        );
        prompt.push_str(soul.trim());
        prompt.push_str("\n\n");
    }
    prompt.push_str("Heartbeat task:\n\n");
    prompt.push_str(heartbeat);
    prompt
}

fn slugify(name: &str) -> Option<String> {
    let mut slug = String::new();
    let mut separator = false;
    for ch in name.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            separator = false;
            slug.push(ch);
        } else if !slug.is_empty() {
            separator = true;
        }
    }
    (!slug.is_empty()).then_some(slug)
}

fn create_document(path: &Path) -> Result<(), ProjectError> {
    if path.exists() {
        return Ok(());
    }
    std::fs::write(path, "").map_err(|source| ProjectError::Io {
        path: path.to_owned(),
        source,
    })
}

fn read_document(path: &Path) -> Result<String, ProjectError> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(source) => Err(ProjectError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

#[must_use]
pub const fn default_heartbeat_seconds() -> u64 {
    DEFAULT_HEARTBEAT_SECONDS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_become_stable_slugs() {
        assert_eq!(slugify(" Q4 Launch! ").as_deref(), Some("q4-launch"));
        assert_eq!(slugify("---"), None);
    }

    #[test]
    fn project_context_precedes_the_heartbeat_goal() {
        let prompt = project_prompt("Call the release Northstar.", "Summarize blockers.");
        let Some((context, goal)) = prompt.split_once("Heartbeat task:") else {
            panic!("prompt omitted the heartbeat boundary");
        };
        assert!(context.contains("Northstar"));
        assert!(goal.contains("Summarize"));
    }
}
