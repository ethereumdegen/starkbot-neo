use std::fs;
use std::sync::{Arc, Barrier};
use std::time::Duration;

use neo_core::Settings;
use rusqlite::Connection;
use serde_json::json;
use tempfile::TempDir;

use crate::connection::prune_backups;
use crate::{APPLICATION_ID, SCHEMA_VERSION, Store, StoreError, migrations, open_read_only};

fn test_store() -> (TempDir, Store) {
    let temp = TempDir::new().unwrap_or_else(|error| panic!("{error}"));
    let store = Store::open(temp.path().join("neo.db"), temp.path().join("backups"))
        .unwrap_or_else(|error| panic!("{error}"));
    (temp, store)
}

#[test]
fn migration_is_valid_and_builds_full_schema() {
    migrations()
        .validate()
        .unwrap_or_else(|error| panic!("{error}"));
    let (_temp, store) = test_store();
    let (version, application_id, table_count) = store
        .readers()
        .read(|connection| {
            let version = connection.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?;
            let application_id = connection.pragma_query_value(None, "application_id", |row| row.get::<_, i64>(0))?;
            let table_count = connection.query_row("SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'", [], |row| row.get::<_, i64>(0))?;
            Ok((version, application_id, table_count))
        })
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(version, SCHEMA_VERSION);
    assert_eq!(application_id, APPLICATION_ID);
    assert!(table_count >= 20);
}

#[test]
fn existing_database_is_backed_up_before_migration() {
    let temp = TempDir::new().unwrap_or_else(|error| panic!("{error}"));
    let path = temp.path().join("neo.db");
    Connection::open(&path)
        .and_then(|connection| connection.execute("CREATE TABLE legacy(value TEXT)", []))
        .unwrap_or_else(|error| panic!("{error}"));
    let backups = temp.path().join("backups");
    let store = Store::open(&path, &backups).unwrap_or_else(|error| panic!("{error}"));
    let (application_id, auto_vacuum) = store
        .readers()
        .read(|connection| {
            Ok((
                connection
                    .pragma_query_value(None, "application_id", |row| row.get::<_, i64>(0))?,
                connection.pragma_query_value(None, "auto_vacuum", |row| row.get::<_, i64>(0))?,
            ))
        })
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(application_id, APPLICATION_ID);
    assert_eq!(auto_vacuum, 2);
    let entries = fs::read_dir(backups)
        .unwrap_or_else(|error| panic!("{error}"))
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(entries.len(), 1);
    let backup = open_read_only(&entries[0].path()).unwrap_or_else(|error| panic!("{error}"));
    let legacy: i64 = backup
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name = 'legacy'",
            [],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(legacy, 1);
}

#[test]
fn newer_database_is_refused() {
    let temp = TempDir::new().unwrap_or_else(|error| panic!("{error}"));
    let path = temp.path().join("neo.db");
    let connection = Connection::open(&path).unwrap_or_else(|error| panic!("{error}"));
    connection
        .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap_or_else(|error| panic!("{error}"));
    drop(connection);
    let result = Store::open(path, temp.path().join("backups"));
    assert!(matches!(result, Err(StoreError::NewerSchema { .. })));
}

#[test]
fn another_applications_database_is_refused() {
    let temp = TempDir::new().unwrap_or_else(|error| panic!("{error}"));
    let path = temp.path().join("neo.db");
    let connection = Connection::open(&path).unwrap_or_else(|error| panic!("{error}"));
    connection
        .pragma_update(None, "application_id", 0x1234_i64)
        .unwrap_or_else(|error| panic!("{error}"));
    drop(connection);
    let result = Store::open(path, temp.path().join("backups"));
    assert!(matches!(result, Err(StoreError::ApplicationId { .. })));
}

#[test]
fn backup_retention_keeps_the_latest_three_snapshots() {
    let temp = TempDir::new().unwrap_or_else(|error| panic!("{error}"));
    for name in [
        "neo-v9-old.db",
        "neo-v10-a.db",
        "neo-v10-b.db",
        "neo-v10-c.db",
    ] {
        fs::write(temp.path().join(name), name).unwrap_or_else(|error| panic!("{error}"));
        std::thread::sleep(Duration::from_millis(5));
    }
    fs::write(temp.path().join("notes.txt"), "keep").unwrap_or_else(|error| panic!("{error}"));
    prune_backups(temp.path()).unwrap_or_else(|error| panic!("{error}"));
    assert!(!temp.path().join("neo-v9-old.db").exists());
    assert!(temp.path().join("neo-v10-a.db").exists());
    assert!(temp.path().join("neo-v10-b.db").exists());
    assert!(temp.path().join("neo-v10-c.db").exists());
    assert!(temp.path().join("notes.txt").exists());
}

#[test]
fn settings_patch_is_atomic_validated_and_resettable() {
    let (_temp, store) = test_store();
    let repository = store.settings();
    let initial = repository.load().unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(initial, Settings::default());

    let updated = repository
        .patch("identity", json!({"name": "Nova"}))
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(updated.identity.name, "Nova");

    let invalid = repository.patch("safety", json!({"on_task_floor": 0.1}));
    assert!(matches!(invalid, Err(StoreError::InvalidSettings(_))));
    assert_eq!(
        repository
            .load()
            .unwrap_or_else(|error| panic!("{error}"))
            .safety
            .on_task_floor,
        0.30
    );

    let reset = repository
        .patch("identity", json!({"name": null}))
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(reset.identity.name, "Stark");

    let reset_section = repository
        .patch("identity", json!(null))
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(reset_section.identity, Settings::default().identity);
}

#[test]
fn concurrent_patches_to_one_section_do_not_lose_fields() {
    let (_temp, store) = test_store();
    let barrier = Arc::new(Barrier::new(3));
    let first_repository = store.settings();
    let first_barrier = Arc::clone(&barrier);
    let first = std::thread::spawn(move || {
        first_barrier.wait();
        first_repository.patch("listen", json!({"enabled": false}))
    });
    let second_repository = store.settings();
    let second_barrier = Arc::clone(&barrier);
    let second = std::thread::spawn(move || {
        second_barrier.wait();
        second_repository.patch("listen", json!({"push_to_talk": true}))
    });
    barrier.wait();
    first
        .join()
        .unwrap_or_else(|_| panic!("first settings patch panicked"))
        .unwrap_or_else(|error| panic!("{error}"));
    second
        .join()
        .unwrap_or_else(|_| panic!("second settings patch panicked"))
        .unwrap_or_else(|error| panic!("{error}"));
    let settings = store
        .settings()
        .load()
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(!settings.listen.enabled);
    assert!(settings.listen.push_to_talk);
}

#[test]
fn readers_are_query_only_and_writer_preserves_order() {
    let (_temp, store) = test_store();
    let write_error = store.readers().read(|connection| {
        connection.execute(
            "INSERT INTO schema_meta(key, value) VALUES ('bad', 'bad')",
            [],
        )?;
        Ok(())
    });
    assert!(write_error.is_err());

    let writer = store.writer();
    for value in 0..10 {
        writer
            .execute(move |connection| {
                connection.execute(
                    "INSERT INTO schema_meta(key, value) VALUES (?1, ?2)",
                    [format!("k{value}"), value.to_string()],
                )?;
                Ok(())
            })
            .unwrap_or_else(|error| panic!("{error}"));
    }
    let count = store
        .readers()
        .read(|connection| {
            connection
                .query_row(
                    "SELECT count(*) FROM schema_meta WHERE key LIKE 'k%'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(StoreError::from)
        })
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(count, 10);
}

#[test]
fn released_v1_seed_opens_without_migration() {
    let temp = TempDir::new().unwrap_or_else(|error| panic!("{error}"));
    let path = temp.path().join("neo.db");
    fs::write(&path, include_bytes!("../../../fixtures/db/v1.db"))
        .unwrap_or_else(|error| panic!("{error}"));
    let store =
        Store::open(&path, temp.path().join("backups")).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        store
            .settings()
            .load()
            .unwrap_or_else(|error| panic!("{error}")),
        Settings::default()
    );
    assert_eq!(
        fs::read_dir(temp.path().join("backups"))
            .unwrap_or_else(|error| panic!("{error}"))
            .count(),
        0
    );
}
