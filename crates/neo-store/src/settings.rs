use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use neo_core::Settings;
use rusqlite::params;
use serde_json::{Map, Value};

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
            let transaction = connection.transaction()?;
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

fn load_values(connection: &rusqlite::Connection) -> Result<HashMap<String, Value>> {
    let mut statement = connection.prepare("SELECT section, value FROM settings")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut values = HashMap::new();
    for row in rows {
        let (section, encoded) = row?;
        values.insert(section, serde_json::from_str::<Value>(&encoded)?);
    }
    Ok(values)
}

fn decode_settings(values: HashMap<String, Value>) -> Result<Settings> {
    let mut root = default_object()?;
    for (section, value) in values {
        if SECTIONS.contains(&section.as_str()) {
            root.insert(section, value);
        }
    }
    let settings: Settings = serde_json::from_value(Value::Object(root))?;
    settings.validate().map_err(StoreError::InvalidSettings)?;
    Ok(settings)
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
