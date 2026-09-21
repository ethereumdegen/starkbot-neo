use std::fs;
use std::sync::{Arc, Barrier};
use std::time::Duration;

use neo_core::{
    Allowance, ConversationId, MessageKind, MessageRole, MessageSource, ModelCapabilities,
    ModelInfo, ModelRef, ModelUseCase, ProviderAccount, ProviderAccountStatus, ProviderId,
    RateLimitKind, RateLimitWindow, Settings,
};
use rusqlite::Connection;
use serde_json::json;
use tempfile::TempDir;

use crate::connection::prune_backups;
use crate::{
    APPLICATION_ID, GLOBAL_SCOPE, NewMessage, NewTurn, SCHEMA_VERSION, Store, StoreError,
    migrations, open_read_only,
};

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
fn provider_account_round_trips_without_credentials() {
    let (_temp, store) = test_store();
    let repository = store.provider_accounts();
    let provider = ProviderId::new("chatgpt-codex");
    assert!(
        repository
            .get(&provider)
            .unwrap_or_else(|error| panic!("{error}"))
            .is_none()
    );

    let account = ProviderAccount {
        provider: provider.clone(),
        status: ProviderAccountStatus::Connected,
        email: Some("user@example.com".into()),
        plan_type: Some("pro".into()),
        workspace: None,
        allowance: Some(Allowance {
            limits: vec![RateLimitWindow {
                limit_id: "codex".into(),
                limit_name: None,
                used_percent: 25.0,
                kind: RateLimitKind::Primary,
                window_duration_minutes: Some(15),
                resets_at: Some(1_730_947_200_000),
            }],
        }),
        updated_at: 1_730_946_300_000,
    };
    repository
        .put(account.clone())
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        repository
            .get(&provider)
            .unwrap_or_else(|error| panic!("{error}")),
        Some(account)
    );
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
fn released_v1_seed_migrates_with_backup() {
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
    assert!(
        store
            .provider_accounts()
            .get(&ProviderId::new("chatgpt-codex"))
            .unwrap_or_else(|error| panic!("{error}"))
            .is_none()
    );
    assert_eq!(
        fs::read_dir(temp.path().join("backups"))
            .unwrap_or_else(|error| panic!("{error}"))
            .count(),
        1
    );
}

#[test]
fn the_registry_cache_replaces_a_catalog_and_keeps_first_seen() {
    let (_temp, store) = test_store();
    let repository = store.models();
    let provider = ProviderId::new("openai");
    let model = |id: &str, use_cases: Vec<ModelUseCase>| ModelInfo {
        reference: ModelRef::new(provider.clone(), id),
        use_cases,
        capabilities: ModelCapabilities {
            reasoning: true,
            tools: true,
            image_input: true,
            streaming: true,
        },
        price: None,
        deprecated: false,
    };

    repository
        .replace(
            &provider,
            GLOBAL_SCOPE,
            vec![
                model("gpt-5.6-sol", vec![ModelUseCase::Inference, ModelUseCase::TextHelper]),
                model("gpt-transcribe", vec![ModelUseCase::SpeechToText]),
            ],
            1_000,
        )
        .unwrap_or_else(|error| panic!("{error}"));

    // A second refresh drops an id the vendor stopped listing, adds a new one,
    // and leaves the surviving id's first_seen where it was.
    repository
        .replace(
            &provider,
            GLOBAL_SCOPE,
            vec![
                model("gpt-5.6-sol", vec![ModelUseCase::Inference, ModelUseCase::TextHelper]),
                model("gpt-5.7-luna", vec![ModelUseCase::TextHelper]),
            ],
            2_000,
        )
        .unwrap_or_else(|error| panic!("{error}"));

    let cached = repository
        .list(&provider, GLOBAL_SCOPE)
        .unwrap_or_else(|error| panic!("{error}"));
    let ids: Vec<&str> = cached
        .iter()
        .map(|model| model.info.reference.id.as_str())
        .collect();
    assert_eq!(ids, vec!["gpt-5.6-sol", "gpt-5.7-luna"]);

    let sol = &cached[0];
    assert_eq!(sol.first_seen, 1_000, "a surviving id keeps its first sighting");
    assert_eq!(sol.last_seen, 2_000);
    assert_eq!(
        sol.info.use_cases,
        vec![ModelUseCase::Inference, ModelUseCase::TextHelper],
        "both use cases survive the round trip"
    );
    assert_eq!(cached[1].first_seen, 2_000);
}

#[test]
fn one_scope_never_reads_another_scopes_catalog() {
    let (_temp, store) = test_store();
    let repository = store.models();
    let provider = ProviderId::new("anthropic");
    let model = ModelInfo {
        reference: ModelRef::new(provider.clone(), "claude-opus-5"),
        use_cases: vec![ModelUseCase::Inference],
        capabilities: ModelCapabilities::default(),
        price: None,
        deprecated: false,
    };

    repository
        .replace(&provider, "workspace-a", vec![model], 1)
        .unwrap_or_else(|error| panic!("{error}"));

    assert!(
        repository
            .list(&provider, GLOBAL_SCOPE)
            .unwrap_or_else(|error| panic!("{error}"))
            .is_empty()
    );
    assert_eq!(
        repository
            .list(&provider, "workspace-a")
            .unwrap_or_else(|error| panic!("{error}"))
            .len(),
        1
    );
}

#[test]
fn a_thread_reads_back_in_the_order_it_was_written() {
    let (_temp, store) = test_store();
    let conversations = store.conversations();
    let thread = conversations
        .create(Some("launch note".into()), 1_000)
        .unwrap_or_else(|error| panic!("{error}"));

    let appended = [
        NewMessage::new(
            thread.id,
            MessageRole::User,
            MessageSource::Typed,
            "post the launch note",
            1_100,
        ),
        NewMessage::new(
            thread.id,
            MessageRole::Tool,
            MessageSource::System,
            "navigate: done, 7 steps",
            1_200,
        )
        .with_kind(MessageKind::Result)
        .with_meta(json!({"tool": "navigate", "steps": 7})),
        NewMessage::new(
            thread.id,
            MessageRole::Assistant,
            MessageSource::System,
            "Posted it.",
            1_300,
        )
        .with_kind(MessageKind::Answer),
    ]
    .map(|message| {
        conversations
            .append_message(message)
            .unwrap_or_else(|error| panic!("{error}"))
    });

    let thread_messages = conversations
        .messages(thread.id, 100)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        thread_messages.iter().map(|m| m.at).collect::<Vec<_>>(),
        vec![1_100, 1_200, 1_300],
        "the thread reads oldest first"
    );
    assert_eq!(
        thread_messages.iter().map(|m| m.id).collect::<Vec<_>>(),
        appended.iter().map(|m| m.id).collect::<Vec<_>>()
    );
    assert_eq!(thread_messages[1].role, MessageRole::Tool);
    assert_eq!(thread_messages[1].kind, MessageKind::Result);
    assert_eq!(
        thread_messages[1].meta,
        Some(json!({"tool": "navigate", "steps": 7})),
        "per-message metadata survives the round trip"
    );
    assert_eq!(thread_messages[2].role, MessageRole::Assistant);

    // Only the last two, still oldest first.
    let tail = conversations
        .messages(thread.id, 2)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        tail.iter().map(|m| m.at).collect::<Vec<_>>(),
        vec![1_200, 1_300]
    );

    // Appending made the thread the most recently active one.
    let latest = conversations
        .latest()
        .unwrap_or_else(|error| panic!("{error}"))
        .unwrap_or_else(|| panic!("a thread exists"));
    assert_eq!(latest.id, thread.id);
    assert_eq!(latest.updated_at, 1_300);
    assert_eq!(latest.created_at, 1_000);
}

/// An answer arrives in slices, and the thread must end up holding one
/// message, not one per slice: the row a front end is watching has to be the
/// row that grows.
#[test]
fn a_streaming_answer_grows_one_row() {
    let (_temp, store) = test_store();
    let conversations = store.conversations();
    let thread = conversations
        .create(None, 1_000)
        .unwrap_or_else(|error| panic!("{error}"));

    let growing = |at| {
        NewMessage::new(thread.id, MessageRole::Assistant, MessageSource::System, "", at)
            .with_kind(MessageKind::Answer)
            .with_meta(json!({"run": "run-1"}))
    };
    let first = conversations
        .upsert_streaming_message(&growing(1_100), "Posted ")
        .unwrap_or_else(|error| panic!("{error}"));
    for (at, slice) in [(1_150, "it "), (1_200, "twice.")] {
        let same = conversations
            .upsert_streaming_message(&growing(at), slice)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(same, first, "every slice of one run grows the same row");
    }

    // The next run is a different answer, so it gets its own row.
    let next = NewMessage::new(
        thread.id,
        MessageRole::Assistant,
        MessageSource::System,
        "",
        1_300,
    )
    .with_kind(MessageKind::Answer)
    .with_meta(json!({"run": "run-2"}));
    let second = conversations
        .upsert_streaming_message(&next, "And again.")
        .unwrap_or_else(|error| panic!("{error}"));
    assert_ne!(second, first);

    let thread_messages = conversations
        .messages(thread.id, 100)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        thread_messages
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        vec!["Posted it twice.", "And again."]
    );
    // A grown row keeps the timestamp and sequence it was inserted with, so
    // a slice arriving late never reorders the thread.
    assert_eq!(thread_messages[0].at, 1_100);
    // The thread's own activity does follow the newest slice.
    assert_eq!(
        conversations
            .latest()
            .unwrap_or_else(|error| panic!("{error}"))
            .unwrap_or_else(|| panic!("a thread exists"))
            .updated_at,
        1_300
    );
}

#[test]
fn a_recorded_turn_keeps_the_vendors_usage_object() {
    let (_temp, store) = test_store();
    let conversations = store.conversations();
    let thread = conversations
        .create(None, 1)
        .unwrap_or_else(|error| panic!("{error}"));
    // Anthropic's shape: a normalised subset would drop three of these.
    let usage = json!({
        "input_tokens": 1_200,
        "output_tokens": 340,
        "cache_creation_input_tokens": 900,
        "cache_read_input_tokens": 2_048,
        "service_tier": "standard"
    });

    let recorded = conversations
        .record_turn(NewTurn {
            conversation_id: thread.id,
            model: "claude-opus-5".into(),
            provider: ProviderId::new("anthropic-oauth"),
            duration_ms: 4_210,
            usage: Some(usage.clone()),
            started_at: 5_000,
        })
        .unwrap_or_else(|error| panic!("{error}"));

    let turns = conversations
        .turns(thread.id, 10)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(turns, vec![recorded]);
    assert_eq!(turns[0].usage, Some(usage), "usage is stored verbatim");
    assert_eq!(turns[0].duration_ms, 4_210);
    assert_eq!(turns[0].provider.as_str(), "anthropic-oauth");
    assert_eq!(
        conversations
            .latest()
            .unwrap_or_else(|error| panic!("{error}"))
            .map(|thread| thread.updated_at),
        Some(5_000),
        "a turn counts as activity even when it appended nothing"
    );
}

#[test]
fn the_thread_read_walks_its_index_instead_of_sorting() {
    let (_temp, store) = test_store();
    let plan = store
        .readers()
        .read(|connection| {
            let mut statement = connection.prepare(&format!(
                "EXPLAIN QUERY PLAN {}",
                crate::conversations::THREAD_QUERY
            ))?;
            let rows = statement.query_map(rusqlite::params![ConversationId::new().to_string(), 10_i64], |row| {
                row.get::<_, String>(3)
            })?;
            let mut lines = Vec::new();
            for row in rows {
                lines.push(row?);
            }
            Ok(lines.join(" | "))
        })
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(
        plan.contains("messages_thread"),
        "the thread read must use its index, plan was: {plan}"
    );
    assert!(
        !plan.contains("TEMP B-TREE"),
        "ordering by created_at must come from the index, plan was: {plan}"
    );
}

#[test]
fn renaming_and_deleting_a_thread() {
    let (_temp, store) = test_store();
    let conversations = store.conversations();
    let keep = conversations
        .create(None, 10)
        .unwrap_or_else(|error| panic!("{error}"));
    let drop = conversations
        .create(None, 20)
        .unwrap_or_else(|error| panic!("{error}"));
    conversations
        .append_message(NewMessage::new(
            drop.id,
            MessageRole::User,
            MessageSource::Voice,
            "heard something",
            30,
        ))
        .unwrap_or_else(|error| panic!("{error}"));

    conversations
        .rename(keep.id, "GTM week 3", 40)
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(matches!(
        conversations.rename(ConversationId::new(), "nowhere", 50),
        Err(StoreError::UnknownConversation(_))
    ));

    conversations
        .delete(drop.id)
        .unwrap_or_else(|error| panic!("{error}"));
    let listed = conversations
        .list(10)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, keep.id);
    assert_eq!(listed[0].title.as_deref(), Some("GTM week 3"));
    assert!(
        conversations
            .messages(drop.id, 10)
            .unwrap_or_else(|error| panic!("{error}"))
            .is_empty(),
        "deleting a thread takes its messages with it"
    );
    // Deleting a thread that is already gone is not an error.
    conversations
        .delete(drop.id)
        .unwrap_or_else(|error| panic!("{error}"));
}

#[test]
fn a_v3_database_upgrades_without_losing_its_thread() {
    let temp = TempDir::new().unwrap_or_else(|error| panic!("{error}"));
    let path = temp.path().join("neo.db");
    let conversation = "0199c0de-0000-7000-8000-00000000cafe";

    // A database as M1 shipped it: schema 3, with a thread already in it.
    {
        let mut connection = Connection::open(&path).unwrap_or_else(|error| panic!("{error}"));
        migrations()
            .to_version(&mut connection, 3)
            .unwrap_or_else(|error| panic!("{error}"));
        connection
            .execute(
                "INSERT INTO conversations(id, title, started_at, ended_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![conversation, "yesterday", 1_000_i64, 9_000_i64],
            )
            .unwrap_or_else(|error| panic!("{error}"));
        for (seq, role, text, at) in [
            (1_i64, "user", "book the venue", 1_100_i64),
            (2, "bot", "Booked it.", 1_200),
        ] {
            connection
                .execute(
                    "INSERT INTO messages(id, conversation_id, seq, role, source, kind, text, at) \
                     VALUES (?1, ?2, ?3, ?4, 'typed', 'text', ?5, ?6)",
                    rusqlite::params![
                        format!("0199c0de-0000-7000-8000-00000000000{seq}"),
                        conversation,
                        seq,
                        role,
                        text,
                        at
                    ],
                )
                .unwrap_or_else(|error| panic!("{error}"));
        }
    }

    let store =
        Store::open(&path, temp.path().join("backups")).unwrap_or_else(|error| panic!("{error}"));
    let id: ConversationId = conversation.parse().unwrap_or_else(|error| panic!("{error}"));
    let threads = store
        .conversations()
        .list(10)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].id, id);
    assert_eq!(threads[0].title.as_deref(), Some("yesterday"));
    assert_eq!(threads[0].created_at, 1_000, "started_at became created_at");
    assert_eq!(threads[0].updated_at, 9_000, "updated_at seeded from ended_at");

    let messages = store
        .conversations()
        .messages(id, 10)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        messages
            .iter()
            .map(|message| (message.role, message.text.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (MessageRole::User, "book the venue"),
            (MessageRole::Assistant, "Booked it."),
        ],
        "every message survives and `bot` becomes `assistant`"
    );
    assert!(messages.iter().all(|message| message.meta.is_none()));

    // The full-text index was rebuilt against the new table's rowids.
    let hits = store
        .readers()
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT count(*) FROM messages_fts WHERE messages_fts MATCH 'venue'",
                [],
                |row| row.get::<_, i64>(0),
            )?)
        })
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(hits, 1, "search still finds a message written before the upgrade");

    // And the upgraded database takes new work.
    store
        .conversations()
        .append_message(
            NewMessage::new(id, MessageRole::Assistant, MessageSource::System, "More.", 9_500)
                .with_kind(MessageKind::Answer),
        )
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        store
            .conversations()
            .messages(id, 10)
            .unwrap_or_else(|error| panic!("{error}"))
            .len(),
        3
    );
}
