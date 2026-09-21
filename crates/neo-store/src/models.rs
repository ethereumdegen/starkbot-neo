//! The model-registry cache (05 §7): what a runtime's catalogue last looked
//! like, so the UI and a resolve have something to serve before a refresh
//! lands.
//!
//! Rows are scoped by provider *and* an opaque `scope` string that stands for
//! the account or workspace the catalogue came from, so one account's
//! catalogue never leaks into another's. A refresh is a whole-catalogue
//! replacement for that (provider, scope): an id the vendor stopped listing
//! disappears rather than lingering as selectable.

use std::collections::HashMap;

use neo_core::{ModelCapabilities, ModelInfo, ModelPrice, ModelRef, ModelUseCase, ProviderId};
use rusqlite::params;

use crate::{ReadPool, Result, StoreError, Writer};

/// The scope of a catalogue that belongs to no particular account.
pub const GLOBAL_SCOPE: &str = "global";

#[derive(Clone)]
pub struct ModelRepository {
    writer: Writer,
    readers: ReadPool,
}

impl ModelRepository {
    pub(crate) fn new(writer: Writer, readers: ReadPool) -> Self {
        Self { writer, readers }
    }

    /// Every cached model for one runtime and scope, hidden rows included so a
    /// caller can explain why an id is not selectable.
    pub fn list(&self, provider: &ProviderId, scope: &str) -> Result<Vec<CachedModel>> {
        let provider = provider.as_str().to_owned();
        let scope = scope.to_owned();
        self.readers.read(move |connection| {
            let mut statement = connection.prepare(
                "SELECT provider, id, use_case, capabilities, price, price_source, hidden, first_seen, last_seen \
                 FROM models WHERE provider = ?1 AND scope = ?2 ORDER BY id",
            )?;
            let rows = statement.query_map(params![provider, scope], |row| {
                Ok(ModelRow {
                    provider: row.get(0)?,
                    id: row.get(1)?,
                    use_case: row.get(2)?,
                    capabilities: row.get(3)?,
                    price: row.get(4)?,
                    price_source: row.get(5)?,
                    hidden: row.get::<_, i64>(6)? != 0,
                    first_seen: row.get(7)?,
                    last_seen: row.get(8)?,
                })
            })?;
            let mut models = Vec::new();
            for row in rows {
                models.push(decode(row?)?);
            }
            Ok(models)
        })
    }

    /// Replace one runtime and scope's catalogue with `models`, keeping each
    /// id's original `first_seen`.
    ///
    /// `at` is the refresh time in Unix milliseconds. The delete and the
    /// inserts share the writer's one transaction, so a reader never sees a
    /// half-replaced catalogue.
    pub fn replace(
        &self,
        provider: &ProviderId,
        scope: &str,
        models: Vec<ModelInfo>,
        at: i64,
    ) -> Result<()> {
        let provider = provider.as_str().to_owned();
        let scope = scope.to_owned();
        let rows = models
            .into_iter()
            .map(|model| encode(&provider, &model, at))
            .collect::<Result<Vec<_>>>()?;
        self.writer.execute(move |connection| {
            let transaction = connection.transaction()?;
            // Read the sightings before the delete: an id that survives a
            // refresh keeps the time this runtime first offered it.
            let mut first_seen: HashMap<String, i64> = HashMap::new();
            {
                let mut statement = transaction.prepare(
                    "SELECT id, first_seen FROM models WHERE provider = ?1 AND scope = ?2",
                )?;
                let rows = statement.query_map(params![provider, scope], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?;
                for row in rows {
                    let (id, seen) = row?;
                    first_seen.insert(id, seen);
                }
            }
            transaction.execute(
                "DELETE FROM models WHERE provider = ?1 AND scope = ?2",
                params![provider, scope],
            )?;
            for row in rows {
                let seen = first_seen.get(&row.id).copied().unwrap_or(at);
                transaction.execute(
                    "INSERT INTO models(provider, scope, id, use_case, capabilities, price, price_source, hidden, first_seen, last_seen) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        provider,
                        scope,
                        row.id,
                        row.use_case,
                        row.capabilities,
                        row.price,
                        row.price_source,
                        i64::from(row.hidden),
                        seen,
                        at
                    ],
                )?;
            }
            transaction.commit()?;
            Ok(())
        })
    }
}

/// A cached catalogue entry: the model plus when this runtime first and last
/// offered it.
#[derive(Clone, Debug, PartialEq)]
pub struct CachedModel {
    pub info: ModelInfo,
    pub hidden: bool,
    pub price_source: Option<String>,
    pub first_seen: i64,
    pub last_seen: i64,
}

struct ModelRow {
    provider: String,
    id: String,
    use_case: String,
    capabilities: String,
    price: Option<String>,
    price_source: Option<String>,
    hidden: bool,
    first_seen: i64,
    last_seen: i64,
}

/// `use_case` is stored as a comma-separated list because one inference model
/// serves both the inference and the text-helper use cases (05 §7).
fn decode(row: ModelRow) -> Result<CachedModel> {
    let mut use_cases = Vec::new();
    for value in row.use_case.split(',').filter(|value| !value.is_empty()) {
        use_cases.push(decode_use_case(value)?);
    }
    let capabilities: ModelCapabilities = serde_json::from_str(&row.capabilities)?;
    let price = row
        .price
        .map(|encoded| serde_json::from_str::<ModelPrice>(&encoded))
        .transpose()?;
    Ok(CachedModel {
        info: ModelInfo {
            reference: ModelRef::new(ProviderId::new(row.provider), row.id),
            use_cases,
            capabilities,
            price,
            // The cache does not remember a vendor's deprecation flag; a
            // deprecated id is simply absent from the next catalogue.
            deprecated: false,
        },
        hidden: row.hidden,
        price_source: row.price_source,
        first_seen: row.first_seen,
        last_seen: row.last_seen,
    })
}

struct EncodedModel {
    id: String,
    use_case: String,
    capabilities: String,
    price: Option<String>,
    price_source: Option<String>,
    hidden: bool,
}

fn encode(provider: &str, model: &ModelInfo, _at: i64) -> Result<EncodedModel> {
    if model.reference.provider.as_str() != provider {
        return Err(StoreError::InvalidSettingsShape);
    }
    let use_case = model
        .use_cases
        .iter()
        .map(|use_case| encode_use_case(*use_case))
        .collect::<Vec<_>>()
        .join(",");
    Ok(EncodedModel {
        id: model.reference.id.clone(),
        use_case,
        capabilities: serde_json::to_string(&model.capabilities)?,
        price: model
            .price
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?,
        price_source: None,
        hidden: model.deprecated,
    })
}

fn encode_use_case(use_case: ModelUseCase) -> &'static str {
    match use_case {
        ModelUseCase::Inference => "inference",
        ModelUseCase::TextHelper => "text_helper",
        ModelUseCase::SpeechToText => "stt",
        ModelUseCase::TextToSpeech => "tts",
    }
}

fn decode_use_case(value: &str) -> Result<ModelUseCase> {
    match value {
        "inference" => Ok(ModelUseCase::Inference),
        "text_helper" => Ok(ModelUseCase::TextHelper),
        "stt" => Ok(ModelUseCase::SpeechToText),
        "tts" => Ok(ModelUseCase::TextToSpeech),
        other => Err(StoreError::InvalidModelRow(format!(
            "unknown model use case `{other}` in the registry cache"
        ))),
    }
}
