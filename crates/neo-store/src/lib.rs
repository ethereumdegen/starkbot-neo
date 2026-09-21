#![forbid(unsafe_code)]

mod accounts;
mod actor;
mod connection;
mod conversations;
mod presence;
mod models;
mod settings;

use std::path::{Path, PathBuf};

pub use accounts::ProviderAccountRepository;
pub use actor::{ReadPool, Writer};
pub use connection::{APPLICATION_ID, SCHEMA_VERSION, migrations, open_read_only};
pub use conversations::{ConversationRepository, NewMessage, NewTurn};
pub use presence::{Lease, PresenceRepository, Resource, Session, SessionKind};
pub use models::{CachedModel, GLOBAL_SCOPE, ModelRepository};
pub use settings::SettingsRepository;

use actor::{ReadPool as Pool, Writer as Actor};

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("migration failed: {0}")]
    Migration(String),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database schema {found} is newer than supported schema {supported}")]
    NewerSchema { found: i64, supported: i64 },
    #[error("database application id {found:#x} does not match {expected:#x}")]
    ApplicationId { found: i64, expected: i64 },
    #[error("database actor `{0}` stopped")]
    ActorStopped(&'static str),
    #[error("reader pool must contain at least one actor")]
    InvalidPoolSize,
    #[error("unknown settings section `{0}`")]
    UnknownSettingsSection(String),
    #[error("settings value is not an object")]
    InvalidSettingsShape,
    #[error(transparent)]
    InvalidSettings(neo_core::CoreError),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("registry cache row is unreadable: {0}")]
    InvalidModelRow(String),
    #[error("conversation row is unreadable: column `{0}`")]
    InvalidConversationRow(String),
    #[error("conversation `{0}` does not exist")]
    UnknownConversation(String),
    #[error("value for `{0}` does not fit in a SQLite integer")]
    ValueOverflow(&'static str),
    #[error("invalid provider account status `{0}`")]
    InvalidProviderAccountStatus(String),
    #[error("system clock error: {0}")]
    Clock(#[from] std::time::SystemTimeError),
    #[error("system clock cannot fit in a SQLite integer")]
    ClockOverflow,
    #[error("database thread failed to start: {0}")]
    Thread(std::io::Error),
}

pub struct Store {
    path: PathBuf,
    writer: Actor,
    readers: Pool,
}

impl Store {
    /// Open and migrate a store. Call during startup or from a blocking thread.
    pub fn open(path: impl AsRef<Path>, backup_directory: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_owned();
        connection::prepare_database(&path, backup_directory.as_ref())?;
        let writer = Actor::spawn(&path)?;
        let readers = Pool::spawn(&path, 4)?;
        Ok(Self {
            path,
            writer,
            readers,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn writer(&self) -> Writer {
        self.writer.clone()
    }
    pub fn readers(&self) -> ReadPool {
        self.readers.clone()
    }
    pub fn settings(&self) -> SettingsRepository {
        SettingsRepository::new(self.writer.clone(), self.readers.clone())
    }
    pub fn provider_accounts(&self) -> ProviderAccountRepository {
        ProviderAccountRepository::new(self.writer.clone(), self.readers.clone())
    }
    pub fn models(&self) -> ModelRepository {
        ModelRepository::new(self.writer.clone(), self.readers.clone())
    }
    /// Presence and leases: who else is running, and who may drive the
    /// keyboard (cross-process coordination).
    pub fn presence(&self) -> PresenceRepository {
        PresenceRepository::new(self.writer.clone(), self.readers.clone())
    }

    pub fn conversations(&self) -> ConversationRepository {
        ConversationRepository::new(self.writer.clone(), self.readers.clone())
    }
}

pub(crate) use connection::configure_connection;

#[cfg(test)]
mod tests;
