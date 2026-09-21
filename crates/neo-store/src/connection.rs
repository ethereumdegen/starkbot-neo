use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, params};
use rusqlite_migration::{M, Migrations};

use crate::{Result, StoreError};

pub const APPLICATION_ID: i64 = 0x4E45_4F31;
pub const SCHEMA_VERSION: i64 = 5;

pub fn migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(include_str!("../migrations/0001_init.sql")),
        M::up(include_str!("../migrations/0002_provider_accounts.sql")),
        M::up(include_str!("../migrations/0003_runtime_scoped_usage.sql")),
        M::up(include_str!("../migrations/0004_conversation_threads.sql")),
        M::up(include_str!("../migrations/0005_presence_and_leases.sql")),
    ])
}

pub(crate) fn prepare_database(path: &Path, backups: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir_all(backups)?;
    let existed = path.exists() && fs::metadata(path)?.len() > 0;
    let mut connection = Connection::open(path)?;
    configure_connection(&connection, false)?;
    if !existed {
        connection.pragma_update(None, "application_id", APPLICATION_ID)?;
    }
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let application_id: i64 =
        connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(StoreError::NewerSchema {
            found: version,
            supported: SCHEMA_VERSION,
        });
    }
    if application_id != 0 && application_id != APPLICATION_ID {
        return Err(StoreError::ApplicationId {
            found: application_id,
            expected: APPLICATION_ID,
        });
    }
    if version == SCHEMA_VERSION && application_id != APPLICATION_ID {
        return Err(StoreError::ApplicationId {
            found: application_id,
            expected: APPLICATION_ID,
        });
    }
    if version < SCHEMA_VERSION {
        if existed {
            backup(&connection, backups, version)?;
        }
        let auto_vacuum: i64 =
            connection.pragma_query_value(None, "auto_vacuum", |row| row.get(0))?;
        if auto_vacuum != 2 {
            connection.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
        }
        connection.pragma_update(None, "application_id", APPLICATION_ID)?;
        migrations()
            .to_latest(&mut connection)
            .map_err(|error| StoreError::Migration(error.to_string()))?;
        if auto_vacuum != 2 {
            connection.execute_batch("VACUUM")?;
        }
        prune_backups(backups)?;
    }
    Ok(())
}

pub(crate) fn configure_connection(connection: &Connection, query_only: bool) -> Result<()> {
    // First, always: switching to WAL takes an exclusive lock, and the writer
    // and the four readers open concurrently. Without a busy timeout already
    // in force, one of them loses that race with SQLITE_BUSY.
    connection.pragma_update(None, "busy_timeout", 5_000_i64)?;
    if !query_only {
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "wal_autocheckpoint", 1_000_i64)?;
        connection.pragma_update(None, "journal_size_limit", 67_108_864_i64)?;
    }
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update(None, "temp_store", "MEMORY")?;
    if query_only {
        connection.pragma_update(None, "query_only", "ON")?;
    }
    Ok(())
}

fn backup(connection: &Connection, directory: &Path, version: i64) -> Result<PathBuf> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(StoreError::Clock)?
        .as_millis();
    let path = directory.join(format!("neo-v{version}-{millis}.db"));
    connection.execute("VACUUM INTO ?1", params![path.to_string_lossy().as_ref()])?;
    Ok(path)
}

pub(crate) fn prune_backups(directory: &Path) -> Result<()> {
    let mut backups = fs::read_dir(directory)?
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("neo-v"))
        .collect::<Vec<_>>();
    backups.sort_by_key(|entry| {
        entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
    });
    let remove_count = backups.len().saturating_sub(3);
    for entry in backups.into_iter().take(remove_count) {
        fs::remove_file(entry.path())?;
    }
    Ok(())
}

pub fn open_read_only(path: &Path) -> Result<Connection> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    configure_connection(&connection, true)?;
    Ok(connection)
}
