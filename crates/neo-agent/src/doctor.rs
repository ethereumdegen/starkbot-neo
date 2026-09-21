//! `neo doctor` (05 §10): is this machine actually able to run a task?
//!
//! Every check is local and cheap — no vendor call, no key validation, no
//! browser launch — so it can run at startup and in Settings → Doctor. A check
//! reports what it saw, never a guess: a thing that cannot be determined is
//! `Unknown`, not `Ok`.

use std::path::Path;

use neo_core::{InferenceConnection, KeyState, ProviderAccountStatus, Settings};
use neo_keys::{ACCOUNT_ANTHROPIC, ACCOUNT_OPENAI, ACCOUNT_TYPESAFE};
use serde::{Deserialize, Serialize};

use crate::runtime::{Runtime, RuntimeError};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    /// Ready to use.
    Ok,
    /// Usable, but something is missing that limits what Starkbot can do.
    Warn,
    /// A task cannot run until this is fixed.
    Fail,
    /// Not knowable from here.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub health: Health,
    pub detail: String,
    /// What to do about it, when there is something to do.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl Check {
    fn new(name: &str, health: Health, detail: impl Into<String>) -> Self {
        Self {
            name: name.to_owned(),
            health,
            detail: detail.into(),
            fix: None,
        }
    }

    fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DoctorReport {
    pub checks: Vec<Check>,
}

impl DoctorReport {
    /// The worst health in the report — what an exit code should follow.
    #[must_use]
    pub fn health(&self) -> Health {
        if self.checks.iter().any(|check| check.health == Health::Fail) {
            Health::Fail
        } else if self.checks.iter().any(|check| check.health == Health::Warn) {
            Health::Warn
        } else {
            Health::Ok
        }
    }

    /// Can a task run at all?
    #[must_use]
    pub fn ready(&self) -> bool {
        self.health() != Health::Fail
    }
}

/// Where the managed Chrome profile's browser comes from (10). Checked as a
/// file so a missing Chrome is reported rather than discovered mid-task.
const CHROME: &str = neo_cdp::DEFAULT_CHROME;

impl Runtime {
    /// Every local readiness check, in the order the Doctor tab shows them.
    pub fn doctor(&self) -> Result<DoctorReport, RuntimeError> {
        let settings = self.settings()?;
        let keys = self.key_status()?;
        let account = self.subscription_accounts()?;
        let connection = InferenceConnection::detect(
            &settings,
            &keys,
            account
                .iter()
                .find(|row| row.provider.as_str() == settings.models.inference.provider.as_str()),
        );

        let mut checks = vec![self.store_check()];
        checks.push(inference_check(&settings, connection));
        checks.push(navigator_check(&keys));
        checks.push(speech_check(&keys));
        checks.push(dictation_check(&keys));
        checks.push(self.sessions_check());
        for row in &account {
            checks.push(subscription_check(row));
        }
        checks.push(chrome_check(Path::new(CHROME)));
        Ok(DoctorReport { checks })
    }

    /// Who else is running on this laptop.
    ///
    /// Not a failure — two Starkbots are allowed — but the user has to be able
    /// to see it, because the other one may be holding the keyboard and this
    /// one's next app action will say so.
    fn sessions_check(&self) -> Check {
        let mine = self.announced_session().unwrap_or_default();
        let others: Vec<neo_store::Session> = match self.sessions() {
            Ok(sessions) => sessions
                .into_iter()
                .filter(|session| session.id != mine)
                .collect(),
            Err(error) => {
                return Check::new("sessions", Health::Unknown, error.to_string());
            }
        };
        if others.is_empty() {
            return Check::new("sessions", Health::Ok, "this is the only Starkbot running");
        }
        let detail = others
            .iter()
            .map(|session| {
                let doing = session.activity.as_deref().unwrap_or("idle");
                format!("{} (pid {}) · {doing}", session.kind.label(), session.pid)
            })
            .collect::<Vec<_>>()
            .join(" · ");
        Check::new(
            "sessions",
            Health::Warn,
            format!("{} other Starkbot(s): {detail}", others.len()),
        )
    }

    fn store_check(&self) -> Check {
        let info = self.store_info();
        Check::new(
            "store",
            Health::Ok,
            format!(
                "{} · schema v{} · application id {:#x}",
                info.path.display(),
                info.schema_version,
                info.application_id
            ),
        )
    }
}

fn state_of(keys: &[neo_core::KeyStatus], account: &str) -> KeyState {
    keys.iter()
        .find(|key| key.account == account)
        .map(|key| key.state)
        .unwrap_or(KeyState::Missing)
}

/// Is there an inference connection at all (K6)? Without one, Sol cannot run;
/// the navigator still can, which is why this is the *inference* row.
fn inference_check(settings: &Settings, connection: InferenceConnection) -> Check {
    let selected = settings.models.inference.provider.as_str();
    let model = settings.models.inference.id.as_str();
    match connection {
        InferenceConnection::None => Check::new(
            "inference",
            Health::Fail,
            format!("`{selected}` is selected but has no usable credential"),
        )
        .with_fix(
            "`neo keys set openai|anthropic`, or \
             `neo account --provider anthropic-oauth|openai-codex login`, then \
             `neo settings use-runtime <provider>`",
        ),
        connected => Check::new(
            "inference",
            Health::Ok,
            format!("{connected:?} · {selected}/{model}"),
        ),
    }
}

/// Jev is the navigator's inner loop (A6): no TypeSafe key, no browser or app
/// automation at all.
fn navigator_check(keys: &[neo_core::KeyStatus]) -> Check {
    match state_of(keys, ACCOUNT_TYPESAFE) {
        KeyState::Present => Check::new("navigator (jev)", Health::Ok, "typesafe key present"),
        KeyState::Invalid => Check::new("navigator (jev)", Health::Fail, "typesafe key rejected")
            .with_fix("`neo keys set typesafe`"),
        KeyState::Limited => Check::new(
            "navigator (jev)",
            Health::Warn,
            "typesafe key is limited for this use case",
        ),
        KeyState::Unchecked => Check::new(
            "navigator (jev)",
            Health::Unknown,
            "typesafe key present but never checked",
        )
        .with_fix("`neo keys check typesafe`"),
        KeyState::Missing => Check::new("navigator (jev)", Health::Fail, "no typesafe key")
            .with_fix("`neo keys set typesafe`"),
    }
}

/// Can the user dictate right now (K6 as amended: on-device dictation is a
/// first-class backend, so voice does not need an OpenAI key)?
///
/// Three separate gates, each with its own fix, because "voice does not work"
/// is useless to a user who has to know *which* switch is off.
fn dictation_check(keys: &[neo_core::KeyStatus]) -> Check {
    use neo_voice::{MicrophoneAuth, SpeechAuth};

    if state_of(keys, ACCOUNT_OPENAI) == KeyState::Present {
        return Check::new(
            "dictation",
            Health::Ok,
            "openai gpt-transcribe · on-device available as a fallback",
        );
    }
    match neo_voice::microphone_status() {
        MicrophoneAuth::Denied => {
            return Check::new("dictation", Health::Fail, "microphone access is denied")
                .with_fix(neo_voice::MICROPHONE_SETTINGS_URL);
        }
        MicrophoneAuth::NotDetermined => {
            return Check::new(
                "dictation",
                Health::Unknown,
                "microphone not authorised yet — the first dictation asks",
            );
        }
        MicrophoneAuth::Authorized => {}
    }
    if !neo_voice::dictation_enabled() {
        // Starkbot never flips this itself: it is the user's System Settings.
        return Check::new(
            "dictation",
            Health::Warn,
            "Siri & Dictation is off, so on-device transcription cannot run",
        )
        .with_fix(neo_voice::DICTATION_SETTINGS_URL);
    }
    match neo_voice::speech_status() {
        SpeechAuth::Authorized => {
            Check::new("dictation", Health::Ok, "on-device · no key, no network")
        }
        SpeechAuth::Denied => Check::new("dictation", Health::Fail, "speech recognition is denied")
            .with_fix(neo_voice::SPEECH_SETTINGS_URL),
        SpeechAuth::NotDetermined => Check::new(
            "dictation",
            Health::Unknown,
            "speech recognition not authorised yet — the first dictation asks",
        ),
    }
}

/// Speech is OpenAI-API-key only (K6), so a subscription-only user is
/// typed-only until they add one. That is a warning, never a failure.
/// Spoken *replies* (TTS), which really are OpenAI-key-only.
///
/// Voice **in** is a separate row: on-device dictation needs no key (K6 as
/// amended), so a user without an OpenAI key can talk to Starkbot — it just
/// cannot talk back.
fn speech_check(keys: &[neo_core::KeyStatus]) -> Check {
    match state_of(keys, ACCOUNT_OPENAI) {
        KeyState::Present | KeyState::Limited | KeyState::Unchecked => {
            Check::new("speech out", Health::Ok, "openai gpt-4o-mini-tts")
        }
        KeyState::Invalid => Check::new(
            "speech out",
            Health::Warn,
            "openai key rejected — no spoken replies",
        )
        .with_fix("`neo keys set openai`"),
        KeyState::Missing => Check::new(
            "speech out",
            Health::Warn,
            "no openai key — no spoken replies (dictation still works on-device)",
        )
        .with_fix("`neo keys set openai`"),
    }
}

fn subscription_check(account: &neo_core::ProviderAccount) -> Check {
    let name = format!("subscription ({})", account.provider.as_str());
    let plan = account.plan_type.as_deref().unwrap_or("plan unknown");
    match account.status {
        ProviderAccountStatus::Connected => {
            Check::new(&name, Health::Ok, format!("connected · {plan}"))
        }
        ProviderAccountStatus::RateLimited => {
            Check::new(&name, Health::Warn, format!("rate limited · {plan}"))
        }
        ProviderAccountStatus::SignedOut => {
            Check::new(&name, Health::Warn, "signed out").with_fix(format!(
                "`neo account --provider {} login`",
                account.provider.as_str()
            ))
        }
        ProviderAccountStatus::Unavailable => {
            Check::new(&name, Health::Warn, "the helper is unavailable")
        }
    }
}

fn chrome_check(chrome: &Path) -> Check {
    if chrome.is_file() {
        Check::new("chrome", Health::Ok, chrome.display().to_string())
    } else {
        Check::new(
            "chrome",
            Health::Fail,
            format!("{} is not there", chrome.display()),
        )
        .with_fix("install Google Chrome — the managed profile needs it (10)")
    }
}

/// Also used by `Anthropic`-key users: which accounts a report mentions.
#[must_use]
pub fn credential_accounts() -> [&'static str; 3] {
    [ACCOUNT_OPENAI, ACCOUNT_ANTHROPIC, ACCOUNT_TYPESAFE]
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]

    use super::*;
    use neo_core::{KeyStatus, ProviderId};

    fn keys(rows: &[(&str, KeyState)]) -> Vec<KeyStatus> {
        rows.iter()
            .map(|(account, state)| KeyStatus::new(*account, *state))
            .collect()
    }

    #[test]
    fn a_report_takes_the_worst_health() {
        let report = DoctorReport {
            checks: vec![
                Check::new("a", Health::Ok, ""),
                Check::new("b", Health::Warn, ""),
            ],
        };
        assert_eq!(report.health(), Health::Warn);
        assert!(report.ready(), "a warning does not stop a task");

        let report = DoctorReport {
            checks: vec![
                Check::new("a", Health::Ok, ""),
                Check::new("b", Health::Fail, ""),
            ],
        };
        assert_eq!(report.health(), Health::Fail);
        assert!(!report.ready());
    }

    #[test]
    fn no_typesafe_key_fails_the_navigator_and_names_the_fix() {
        let check = navigator_check(&keys(&[]));
        assert_eq!(check.health, Health::Fail);
        assert_eq!(check.fix.as_deref(), Some("`neo keys set typesafe`"));
    }

    /// A missing OpenAI key costs the user spoken *replies* and nothing else
    /// (K8): dictation is on-device, so voice in still works, and neither is
    /// allowed to block a task.
    #[test]
    fn a_missing_openai_key_only_costs_spoken_replies() {
        let check = speech_check(&keys(&[]));
        assert_eq!(check.health, Health::Warn);
        assert!(
            check.detail.contains("no spoken replies"),
            "the row must say what is actually lost, got {:?}",
            check.detail
        );
        assert!(check.fix.is_some(), "a warning must name its fix");
    }

    #[test]
    fn a_selected_runtime_without_a_credential_fails() {
        let mut settings = Settings::default();
        settings.models.inference.provider = ProviderId::new(neo_core::PROVIDER_ANTHROPIC);
        let check = inference_check(&settings, InferenceConnection::None);
        assert_eq!(check.health, Health::Fail);
        assert!(check.detail.contains("anthropic"));
        assert!(check.fix.is_some());
    }

    #[test]
    fn a_connected_runtime_passes_and_names_the_model() {
        let settings = Settings::default();
        let check = inference_check(&settings, InferenceConnection::OpenAiKey);
        assert_eq!(check.health, Health::Ok);
        assert!(check.detail.contains("sol-latest"));
    }

    #[test]
    fn a_missing_chrome_is_a_failure_with_a_fix() {
        let check = chrome_check(Path::new("/nope/Google Chrome"));
        assert_eq!(check.health, Health::Fail);
        assert!(check.fix.is_some());
    }
}
