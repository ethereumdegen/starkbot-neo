use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use neo_core::Settings;
use rusqlite::params;
use serde_json::{Map, Value};

use crate::connection::write_transaction;
use crate::{ReadPool, Result, StoreError, Writer};

const SECTIONS: &[&str] = &[
    "identity", "listen", "voice", "models", "intake", "safety", "caps", "queue", "browser",
    "hotkeys", "general", "privacy",
];

#[derive(Clone)]
pub struct SettingsRepository {
    writer: Writer,
    readers: ReadPool,
}

impl SettingsRepository {
    pub(crate) fn new(writer: Writer, readers: ReadPool) -> Self {
        Self { writer, readers }
    }

    pub fn load(&self) -> Result<Settings> {
        self.readers
            .read(|connection| decode_settings(load_values(connection)?))
    }

    pub fn patch(&self, section: &str, patch: Value) -> Result<Settings> {
        if !SECTIONS.contains(&section) {
            return Err(StoreError::UnknownSettingsSection(section.into()));
        }
        let section = section.to_owned();
        self.writer.execute(move |connection| {
            let transaction = write_transaction(connection)?;
            let current = decode_settings(load_values(&transaction)?)?;
            let mut all_value = serde_json::to_value(current)?;
            let root = all_value
                .as_object_mut()
                .ok_or(StoreError::InvalidSettingsShape)?;
            let default_section = default_object()?
                .remove(&section)
                .ok_or_else(|| StoreError::UnknownSettingsSection(section.clone()))?;
            let updated = if patch.is_null() {
                default_section
            } else {
                let target = root
                    .get_mut(&section)
                    .ok_or_else(|| StoreError::UnknownSettingsSection(section.clone()))?;
                merge_patch(target, &patch);
                target.clone()
            };
            root.insert(section.clone(), updated.clone());

            let settings: Settings = serde_json::from_value(all_value)?;
            settings.validate().map_err(StoreError::InvalidSettings)?;
            let encoded = serde_json::to_string(&updated)?;
            let updated_at = now_ms()?;
            transaction.execute(
                "INSERT INTO settings(section, value, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT(section) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
                params![section, encoded, updated_at],
            )?;
            transaction.commit()?;
            Ok(settings)
        })
    }
}

/// Every stored section, as JSON.
///
/// `settings.value` carries `CHECK (json_valid(value))`, so a blob that is
/// not JSON at all arrives only past the check — a hand-restored `.dump`, an
/// outside tool, a corrupt page. It cannot be repaired key by key, so it is
/// dropped and the section comes back as defaults. Returning an error here
/// meant `load()` failed, `Runtime::settings()` propagated it, `bootstrap()`
/// refused to start, and the user had no app in which to fix the setting.
/// Note that `json_valid` permits `[1,2,3]` and `null`, which is the shape
/// [`decode_settings`] then has to survive.
fn load_values(connection: &rusqlite::Connection) -> Result<HashMap<String, Value>> {
    let mut statement = connection.prepare("SELECT section, value FROM settings")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut values = HashMap::new();
    for row in rows {
        let (section, encoded) = row?;
        match serde_json::from_str::<Value>(&encoded) {
            Ok(value) => {
                values.insert(section, value);
            }
            Err(error) => tracing::warn!(
                section,
                %error,
                "stored settings section is not JSON; using its defaults"
            ),
        }
    }
    Ok(values)
}

/// Rebuild [`Settings`] from the stored blobs, keeping whatever is readable.
///
/// Every section and `Settings` itself carry `deny_unknown_fields`. That is
/// right for [`SettingsRepository::patch`] — a caller naming a field that
/// does not exist has a bug and should hear about it — and wrong here. A blob
/// written by a newer build, a downgrade, a rollback or a field rename made
/// this return `StoreError::Json`, which `Runtime::settings()` propagates
/// into `bootstrap()`: **every surface refused to start, with no repair
/// path**, because the migrations version `user_version` and never touch the
/// settings JSON. So a stored blob is repaired rather than rejected, and
/// every repair names what it dropped.
///
/// Three nets, narrowest first: drop keys the current shape does not have;
/// then, for a section that still will not deserialize, use that section's
/// defaults and keep the others; then reset whatever [`Settings::validate`]
/// rejects. The result always deserializes and always validates.
fn decode_settings(values: HashMap<String, Value>) -> Result<Settings> {
    let defaults = default_object()?;
    let mut root = defaults.clone();
    for (section, stored) in values {
        let Some(section_defaults) = defaults.get(&section) else {
            tracing::warn!(
                section,
                "stored settings section does not exist in this build; ignoring it"
            );
            continue;
        };
        let kept = retain_known(&section, stored, section_defaults);
        // Try the section in isolation, so one unreadable section costs its
        // own defaults rather than everybody's.
        let mut trial = root.clone();
        trial.insert(section.clone(), kept.clone());
        if serde_json::from_value::<Settings>(Value::Object(trial)).is_ok() {
            root.insert(section, kept);
        } else {
            tracing::warn!(
                section,
                "stored settings section is unreadable; using its defaults"
            );
        }
    }
    // Cannot fail: every section in `root` is either a default or one that
    // just deserialized against the same defaults.
    let settings: Settings = serde_json::from_value(Value::Object(root))?;
    repair_invalid(settings, &defaults)
}

/// Drop stored keys the current [`Settings`] shape does not have, naming each.
///
/// The default serialization is the schema: nothing in the settings tree uses
/// `skip_serializing_if`, so `Settings::default()` emits every key exactly
/// once and a key missing from it is a key this build does not know. A
/// non-object default (a string, a number, `null` for an `Option`, the
/// `confirm_labels` array) is passed through untouched — there is no field
/// list to check it against.
fn retain_known(path: &str, stored: Value, defaults: &Value) -> Value {
    let (Value::Object(stored_fields), Value::Object(default_fields)) = (&stored, defaults) else {
        return stored;
    };
    let mut kept = Map::new();
    for (key, value) in stored_fields {
        let field = format!("{path}.{key}");
        match default_fields.get(key) {
            Some(default) => {
                kept.insert(key.clone(), retain_known(&field, value.clone(), default));
            }
            None => tracing::warn!(field, "dropping a stored setting this build does not have"),
        }
    }
    Value::Object(kept)
}

/// Reset whatever [`Settings::validate`] now rejects, section by section.
///
/// A value that was valid when it was written can stop being valid: the 0.15
/// `safety.on_task_floor` did not always exist. `load()` used to return
/// `InvalidSettings` for that, which leaves the user with an app that will
/// not start and therefore no way to edit the setting. Resetting the section
/// the error names is the smallest repair that gives them one back.
fn repair_invalid(mut settings: Settings, defaults: &Map<String, Value>) -> Result<Settings> {
    // Each pass resets one section and `Settings::default()` validates, so
    // this cannot loop more times than there are sections.
    for _ in 0..defaults.len() {
        let error = match settings.validate() {
            Ok(()) => return Ok(settings),
            Err(error) => error,
        };
        let neo_core::CoreError::InvalidSetting { field, .. } = &error else {
            return Err(StoreError::InvalidSettings(error));
        };
        let section = field.split('.').next().unwrap_or(field);
        let Some(section_defaults) = defaults.get(section) else {
            return Err(StoreError::InvalidSettings(error));
        };
        tracing::warn!(
            section,
            field,
            %error,
            "stored settings are no longer valid; resetting the section to its defaults"
        );
        let mut root = serde_json::to_value(&settings)?
            .as_object()
            .cloned()
            .ok_or(StoreError::InvalidSettingsShape)?;
        root.insert(section.to_owned(), section_defaults.clone());
        settings = serde_json::from_value(Value::Object(root))?;
    }
    settings
        .validate()
        .map_err(StoreError::InvalidSettings)
        .map(|()| settings)
}

fn default_object() -> Result<Map<String, Value>> {
    serde_json::to_value(Settings::default())?
        .as_object()
        .cloned()
        .ok_or(StoreError::InvalidSettingsShape)
}

fn merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(patch) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    if let Value::Object(target) = target {
        for (key, value) in patch {
            if value.is_null() {
                target.remove(key);
            } else {
                merge_patch(target.entry(key.clone()).or_insert(Value::Null), value);
            }
        }
    }
}

fn now_ms() -> Result<i64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(StoreError::Clock)?
        .as_millis();
    i64::try_from(millis).map_err(|_| StoreError::ClockOverflow)
}
