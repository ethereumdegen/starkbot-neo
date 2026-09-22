//! The model registry's pure logic: which use cases an id serves, which ids are
//! never offered, and what a symbolic id resolves to (05 §1, §7).
//!
//! No I/O and no vendor client lives here. A provider that reports capabilities
//! of its own (a Codex or StarkRouter catalogue) overrides these patterns; this
//! module is what a raw `GET /models` list is run through.
//!
//! Ids are parsed by structure rather than with a regex: both vendors' families
//! are hyphen-separated and a hand-written scan keeps `neo-core` dependency-free
//! and lets the parse report the version tuple the ordering needs.

use serde::{Deserialize, Serialize};

use crate::{
    ModelUseCase, PROVIDER_ANTHROPIC, PROVIDER_ANTHROPIC_OAUTH, PROVIDER_CHATGPT_CODEX,
    PROVIDER_CLAUDE_SUBSCRIPTION, PROVIDER_OPENAI, PROVIDER_OPENAI_CODEX,
};

/// The symbolic inference id settings hold instead of a concrete model (K3).
pub const SOL_LATEST: &str = "sol-latest";

/// What a model is for, independent of the vendor's naming.
///
/// OpenAI spells these `sol`/`terra`/`luna`; Anthropic spells them
/// `opus`/`sonnet`/`haiku` *(verify the tier names against the live Anthropic
/// catalogue)*. Starkbot reasons in tiers so `sol-latest` and the text-helper
/// default mean the same thing on either runtime (K6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    /// The orchestrator and creative: the most capable tier.
    Sol,
    /// The middle tier.
    Terra,
    /// The cheapest, fastest tier — the text helper's home.
    Luna,
}

/// One catalogue id, understood.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Classified {
    pub id: String,
    /// `None` for ids that are not an inference family member (speech models).
    pub tier: Option<ModelTier>,
    /// Version as a numeric tuple, so `5.10` sorts above `5.9`.
    pub version: Vec<u32>,
    /// A dated snapshot (`…-2026-09-01`) or a preview, rather than the
    /// evergreen alias.
    pub dated: bool,
    pub use_cases: Vec<ModelUseCase>,
}

/// Ids Starkbot never offers, whatever a catalogue says: deprecated speech
/// models (K3) and image generation, which Starkbot does not do (K4).
const HIDE_PREFIXES: [&str; 6] = [
    "whisper-1",
    "gpt-4o-transcribe",
    "gpt-4o-mini-transcribe",
    "tts-1",
    "gpt-image",
    "dall-e",
];

/// Is this id on the hide-list (05 §7)? A hidden id can never be selected,
/// even by editing the database.
#[must_use]
pub fn hidden(id: &str) -> bool {
    HIDE_PREFIXES
        .iter()
        .any(|prefix| id == *prefix || id.starts_with(prefix))
}

/// Whose catalogue a connection draws on.
///
/// A subscription is the same vendor reached through a different door:
/// `anthropic-oauth` and `claude-subscription` send Anthropic ids,
/// `openai-codex` and `chatgpt-codex` send OpenAI ids. Reading the
/// *connection* name as the vendor meant every id on a subscription
/// classified as nothing at all, so `sol-latest` resolved to nothing and the
/// turn silently ran on whatever the hardcoded fallback happened to be.
#[must_use]
pub fn vendor(provider: &str) -> Option<&'static str> {
    match provider {
        PROVIDER_OPENAI | PROVIDER_OPENAI_CODEX | PROVIDER_CHATGPT_CODEX => Some(PROVIDER_OPENAI),
        PROVIDER_ANTHROPIC | PROVIDER_ANTHROPIC_OAUTH | PROVIDER_CLAUDE_SUBSCRIPTION => {
            Some(PROVIDER_ANTHROPIC)
        }
        _ => None,
    }
}

/// What [`SOL_LATEST`] means on a connection whose catalogue has not been
/// read yet — the vendor's newest top-tier model, named here rather than in
/// each caller so "top tier" means one thing across the app.
///
/// A fresh install has no catalogue, so this is what the first turn actually
/// runs on. It is the *top* tier on purpose: a symbolic id that asks for the
/// best model and quietly delivers the middle one is the bug this replaced.
/// Anyone who wants a cheaper or pinned model types a concrete id, which is
/// returned untouched.
#[must_use]
pub fn sol_fallback(provider: &str) -> Option<&'static str> {
    match vendor(provider)? {
        PROVIDER_OPENAI => Some(OPENAI_SOL_FALLBACK),
        PROVIDER_ANTHROPIC => Some(ANTHROPIC_SOL_FALLBACK),
        _ => None,
    }
}

/// OpenAI's newest top tier *(verify against the live catalogue)*.
pub const OPENAI_SOL_FALLBACK: &str = "gpt-5.6-sol";
/// Anthropic's newest top tier *(verify against the live catalogue)*.
pub const ANTHROPIC_SOL_FALLBACK: &str = "claude-opus-5";

/// Understand one catalogue id, or answer `None` for "never offered" —
/// hide-listed or unclassified (05 §7).
#[must_use]
pub fn classify(provider: &str, id: &str) -> Option<Classified> {
    if hidden(id) {
        return None;
    }
    match vendor(provider)? {
        PROVIDER_OPENAI => classify_openai(id),
        PROVIDER_ANTHROPIC => classify_anthropic(id),
        // A runtime that reports its own capabilities does not come through
        // here; an unknown provider offers nothing rather than guessing.
        _ => None,
    }
}

/// `^gpt-<version>-(sol|terra|luna)(-yyyy-mm-dd)?$`, plus the speech families.
fn classify_openai(id: &str) -> Option<Classified> {
    if id == "gpt-transcribe" || id.starts_with("gpt-transcribe-") {
        return Some(speech(id, ModelUseCase::SpeechToText, false));
    }
    if id == "gpt-live-transcribe" || id.starts_with("gpt-live-transcribe-") {
        return Some(speech(id, ModelUseCase::SpeechToText, true));
    }
    if id.contains("-tts-") || id.ends_with("-tts") {
        return Some(speech(id, ModelUseCase::TextToSpeech, true));
    }

    let rest = id.strip_prefix("gpt-")?;
    let mut parts = rest.split('-');
    let version = numeric_tuple(parts.next()?)?;
    let tier = match parts.next()? {
        "sol" => ModelTier::Sol,
        "terra" => ModelTier::Terra,
        "luna" => ModelTier::Luna,
        _ => return None,
    };
    let tail: Vec<&str> = parts.collect();
    let dated = match tail.as_slice() {
        [] => false,
        // A dated snapshot is exactly `yyyy-mm-dd`.
        [year, month, day] if is_date(year, month, day) => true,
        // Anything else after the tier (a preview, a suffix we do not know)
        // is not the evergreen alias and is never what `sol-latest` picks.
        _ => true,
    };
    Some(Classified {
        id: id.to_owned(),
        tier: Some(tier),
        version,
        dated,
        use_cases: vec![ModelUseCase::Inference, ModelUseCase::TextHelper],
    })
}

/// Anthropic ids carry the tier as a word and the version around it, in more
/// than one order (`claude-sonnet-5`, `claude-3-5-sonnet-20241022`), so the
/// scan looks for a tier word anywhere and reads every numeric segment as the
/// version *(verify against the live catalogue)*. Anthropic has no speech
/// models: speech is OpenAI-key-only (K6).
fn classify_anthropic(id: &str) -> Option<Classified> {
    let rest = id.strip_prefix("claude-")?;
    let mut tier = None;
    let mut version = Vec::new();
    let mut dated = false;
    for segment in rest.split('-') {
        match segment {
            "opus" => tier = Some(ModelTier::Sol),
            "sonnet" => tier = Some(ModelTier::Terra),
            "haiku" => tier = Some(ModelTier::Luna),
            // An 8-digit run is a snapshot date, not a version.
            _ if segment.len() == 8 && segment.chars().all(|c| c.is_ascii_digit()) => dated = true,
            _ if segment.chars().all(|c| c.is_ascii_digit()) => match numeric_tuple(segment) {
                Some(mut parsed) => version.append(&mut parsed),
                None => return None,
            },
            // `latest`, `preview` and anything else means "not the evergreen
            // id we would resolve to".
            _ => dated = true,
        }
    }
    Some(Classified {
        id: id.to_owned(),
        tier: Some(tier?),
        version,
        dated,
        use_cases: vec![ModelUseCase::Inference, ModelUseCase::TextHelper],
    })
}

fn speech(id: &str, use_case: ModelUseCase, dated: bool) -> Classified {
    Classified {
        id: id.to_owned(),
        tier: None,
        version: Vec::new(),
        dated,
        use_cases: vec![use_case],
    }
}

fn is_date(year: &str, month: &str, day: &str) -> bool {
    let digits = |segment: &str, len: usize| {
        segment.len() == len && segment.chars().all(|c| c.is_ascii_digit())
    };
    digits(year, 4) && digits(month, 2) && digits(day, 2)
}

/// `5.10` → `[5, 10]`, so the tuple compares numerically rather than as text.
fn numeric_tuple(segment: &str) -> Option<Vec<u32>> {
    let mut parts = Vec::new();
    for piece in segment.split('.') {
        parts.push(piece.parse::<u32>().ok()?);
    }
    (!parts.is_empty()).then_some(parts)
}

/// The evergreen id of `tier` with the highest version in `ids`.
///
/// Dated snapshots and previews are excluded (05 §7): a run pinned to a
/// snapshot is something the user asked for explicitly, never what a symbolic
/// id resolves to.
#[must_use]
pub fn resolve_latest(provider: &str, tier: ModelTier, ids: &[String]) -> Option<String> {
    ids.iter()
        .filter_map(|id| classify(provider, id))
        .filter(|model| model.tier == Some(tier) && !model.dated)
        .max_by(|left, right| compare_versions(&left.version, &right.version))
        .map(|model| model.id)
}

/// What a saved model id means against a live catalogue: the symbolic
/// `sol-latest` resolves to the top tier's newest evergreen id; any other id
/// resolves to itself, but only while the catalogue still offers it and it is
/// not hidden (05 §7).
#[must_use]
pub fn resolve(provider: &str, requested: &str, ids: &[String]) -> Option<String> {
    if requested == SOL_LATEST {
        return resolve_latest(provider, ModelTier::Sol, ids);
    }
    if hidden(requested) {
        return None;
    }
    ids.iter()
        .find(|id| *id == requested)
        .map(|id| id.to_owned())
}

/// Numeric-tuple order: `[5, 10]` beats `[5, 9]`, and a longer tuple beats a
/// shorter prefix of itself.
fn compare_versions(left: &[u32], right: &[u32]) -> std::cmp::Ordering {
    left.iter().copied().cmp(right.iter().copied())
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]

    use super::*;

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn an_inference_id_carries_its_tier_and_version() {
        let model = classify(PROVIDER_OPENAI, "gpt-5.6-sol").expect("a known family");
        assert_eq!(model.tier, Some(ModelTier::Sol));
        assert_eq!(model.version, vec![5, 6]);
        assert!(!model.dated);
        assert_eq!(
            model.use_cases,
            vec![ModelUseCase::Inference, ModelUseCase::TextHelper]
        );
    }

    #[test]
    fn a_dated_snapshot_is_marked_dated() {
        let model = classify(PROVIDER_OPENAI, "gpt-5.6-sol-2026-09-01").expect("a known family");
        assert!(model.dated);
    }

    #[test]
    fn speech_families_classify_to_one_use_case_each() {
        for (id, use_case) in [
            ("gpt-transcribe", ModelUseCase::SpeechToText),
            ("gpt-live-transcribe", ModelUseCase::SpeechToText),
            ("gpt-4o-mini-tts", ModelUseCase::TextToSpeech),
        ] {
            let model = classify(PROVIDER_OPENAI, id).expect("a known family");
            assert_eq!(model.use_cases, vec![use_case], "{id}");
            assert_eq!(model.tier, None, "{id}");
        }
    }

    #[test]
    fn the_hide_list_is_never_offered() {
        for id in [
            "whisper-1",
            "gpt-4o-transcribe",
            "gpt-4o-mini-transcribe-2025-01-01",
            "tts-1-hd",
            "gpt-image-1",
            "dall-e-3",
        ] {
            assert!(hidden(id), "{id} must be hidden");
            assert!(classify(PROVIDER_OPENAI, id).is_none(), "{id}");
        }
    }

    #[test]
    fn an_unclassified_id_is_never_offered() {
        assert!(classify(PROVIDER_OPENAI, "gpt-4.1").is_none());
        assert!(classify(PROVIDER_OPENAI, "text-embedding-3-large").is_none());
        assert!(classify("starkrouter", "gpt-5.6-sol").is_none());
    }

    /// A subscription sends its vendor's ids. Reading the connection name as
    /// the vendor made every id on one unclassifiable, so `sol-latest`
    /// resolved to nothing and the turn ran on a hardcoded fallback instead
    /// of on the tier that was asked for.
    #[test]
    fn a_subscription_reads_its_vendors_catalogue() {
        let catalog = ids(&["claude-opus-5", "claude-sonnet-5"]);
        assert_eq!(
            resolve(PROVIDER_ANTHROPIC_OAUTH, SOL_LATEST, &catalog).as_deref(),
            Some("claude-opus-5"),
        );
        assert_eq!(
            resolve(PROVIDER_CLAUDE_SUBSCRIPTION, SOL_LATEST, &catalog).as_deref(),
            Some("claude-opus-5"),
        );
        let openai = ids(&["gpt-5.6-sol", "gpt-5.6-luna"]);
        assert_eq!(
            resolve(PROVIDER_OPENAI_CODEX, SOL_LATEST, &openai).as_deref(),
            Some("gpt-5.6-sol"),
        );
        assert_eq!(
            vendor("starkrouter"),
            None,
            "an unknown door is not a vendor"
        );
    }

    /// `sol-latest` asks for the top tier on both vendors, so both fallbacks
    /// have to *be* the top tier — Sol on OpenAI, Opus on Anthropic.
    #[test]
    fn the_no_catalogue_fallback_is_the_tier_the_alias_asks_for() {
        for provider in [
            PROVIDER_ANTHROPIC,
            PROVIDER_ANTHROPIC_OAUTH,
            PROVIDER_CLAUDE_SUBSCRIPTION,
            PROVIDER_OPENAI,
            PROVIDER_OPENAI_CODEX,
        ] {
            let fallback = sol_fallback(provider).expect("every vendor has a top tier");
            let classified = classify(provider, fallback).expect("the fallback is a known id");
            assert_eq!(
                classified.tier,
                Some(ModelTier::Sol),
                "{provider} falls back to {fallback}, which is not the top tier"
            );
            assert!(
                !classified.dated,
                "{fallback} is a snapshot, not an evergreen id"
            );
        }
        assert_eq!(sol_fallback("starkrouter"), None);
    }

    #[test]
    fn sol_latest_compares_versions_numerically() {
        let catalog = ids(&[
            "gpt-5.9-sol",
            "gpt-5.10-sol",
            "gpt-5.10-sol-2026-09-01",
            "gpt-6.0-terra",
        ]);
        assert_eq!(
            resolve(PROVIDER_OPENAI, SOL_LATEST, &catalog).as_deref(),
            Some("gpt-5.10-sol")
        );
    }

    #[test]
    fn sol_latest_ignores_snapshots_and_previews() {
        let catalog = ids(&["gpt-5.6-sol-2026-09-01", "gpt-5.7-sol-preview"]);
        assert_eq!(resolve(PROVIDER_OPENAI, SOL_LATEST, &catalog), None);
    }

    #[test]
    fn a_concrete_id_resolves_only_while_the_catalog_offers_it() {
        let catalog = ids(&["gpt-5.6-sol", "gpt-5.6-luna"]);
        assert_eq!(
            resolve(PROVIDER_OPENAI, "gpt-5.6-luna", &catalog).as_deref(),
            Some("gpt-5.6-luna")
        );
        assert_eq!(resolve(PROVIDER_OPENAI, "gpt-5.5-sol", &catalog), None);
    }

    #[test]
    fn a_hidden_id_cannot_be_resolved_even_if_the_catalog_lists_it() {
        let catalog = ids(&["whisper-1"]);
        assert_eq!(resolve(PROVIDER_OPENAI, "whisper-1", &catalog), None);
    }

    #[test]
    fn anthropic_tiers_map_onto_the_same_roles() {
        for (id, tier) in [
            ("claude-opus-5", ModelTier::Sol),
            ("claude-sonnet-5", ModelTier::Terra),
            ("claude-haiku-4-5", ModelTier::Luna),
        ] {
            let model = classify(PROVIDER_ANTHROPIC, id).expect("a known family");
            assert_eq!(model.tier, Some(tier), "{id}");
            assert!(!model.dated, "{id}");
        }
    }

    #[test]
    fn an_anthropic_snapshot_is_dated_and_never_resolved() {
        let model =
            classify(PROVIDER_ANTHROPIC, "claude-sonnet-4-5-20250929").expect("a known family");
        assert!(model.dated);
        assert_eq!(model.version, vec![4, 5]);

        let catalog = ids(&["claude-opus-4-1-20250805", "claude-opus-latest"]);
        assert_eq!(resolve(PROVIDER_ANTHROPIC, SOL_LATEST, &catalog), None);
    }

    #[test]
    fn sol_latest_on_anthropic_picks_the_newest_opus() {
        let catalog = ids(&[
            "claude-opus-4-5",
            "claude-opus-5",
            "claude-sonnet-5",
            "claude-opus-5-20260101",
        ]);
        assert_eq!(
            resolve(PROVIDER_ANTHROPIC, SOL_LATEST, &catalog).as_deref(),
            Some("claude-opus-5")
        );
    }

    #[test]
    fn the_text_helper_tier_is_the_cheapest_one() {
        let catalog = ids(&["gpt-5.6-sol", "gpt-5.6-luna", "gpt-5.7-luna"]);
        assert_eq!(
            resolve_latest(PROVIDER_OPENAI, ModelTier::Luna, &catalog).as_deref(),
            Some("gpt-5.7-luna")
        );
    }
}
