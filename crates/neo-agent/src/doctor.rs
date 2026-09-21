//! `neo doctor` (05 §10): is this machine actually able to run a task?
//!
//! Every check is local and cheap — no vendor call, no key validation, no
//! browser launch — so it can run at startup and in Settings → Doctor. A check
//! reports what it saw, never a guess: a thing that cannot be determined is
//! `Unknown`, not `Ok`.

use std::path::PathBuf;

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
        checks.push(credential_storage_check(self.keychain()));
        checks.push(inference_check(&settings, connection));
        checks.push(navigator_check(&keys));
        checks.push(speech_check(&keys));
        checks.push(dictation_check(&keys));
        checks.push(self.sessions_check());
        for row in &account {
            checks.push(subscription_check(row));
        }
        checks.push(chrome_check(neo_cdp::chrome_path()));
        #[cfg(target_os = "linux")]
        checks.extend(linux::checks());
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
///
/// The three answers are asked **once per process**. Each is a macOS
/// authorisation query that takes over a second on a machine where nothing
/// has warmed the frameworks, and `Bootstrap` embeds this report — so a fresh
/// install, which is exactly the path with no OpenAI key and therefore no
/// early return, paid ~4.6 s before `neo tui` could draw its first frame. The
/// values are also stable for a process's life in every way that matters: a
/// user who flips a System Settings switch mid-session has already been told
/// which switch to flip, and re-running the app re-asks.
fn dictation_check(keys: &[neo_core::KeyStatus]) -> Check {
    use neo_voice::{MicrophoneAuth, SpeechAuth};

    if state_of(keys, ACCOUNT_OPENAI) == KeyState::Present {
        return Check::new(
            "dictation",
            Health::Ok,
            "openai gpt-transcribe · on-device available as a fallback",
        );
    }
    let (microphone, dictation, speech) = *VOICE_AUTHORISATION;
    match microphone {
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
    // Before the macOS switches, because neither exists where there is no
    // on-device recogniser at all: a Linux user told "Siri & Dictation is
    // off" would go looking for a setting that is not there, when what they
    // actually need is the OpenAI key.
    if speech == SpeechAuth::Unsupported {
        return no_on_device_dictation();
    }
    if !dictation {
        // Starkbot never flips this itself: it is the user's System Settings.
        return Check::new(
            "dictation",
            Health::Warn,
            "Siri & Dictation is off, so on-device transcription cannot run",
        )
        .with_fix(neo_voice::DICTATION_SETTINGS_URL);
    }
    match speech {
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
        SpeechAuth::Unsupported => no_on_device_dictation(),
    }
}

/// The three macOS voice answers, asked once and kept.
///
/// See [`dictation_check`] for why this is cached rather than probed per
/// report. Off macOS each of the three is a compile-time constant in
/// `neo-voice`, so nothing is probed and the cache costs nothing.
static VOICE_AUTHORISATION: std::sync::LazyLock<(
    neo_voice::MicrophoneAuth,
    bool,
    neo_voice::SpeechAuth,
)> = std::sync::LazyLock::new(|| {
    (
        neo_voice::microphone_status(),
        neo_voice::dictation_enabled(),
        neo_voice::speech_status(),
    )
});

/// Where there is no on-device recogniser, `gpt-transcribe` is the whole of
/// dictation — a warning and never a failure, because typing still works.
fn no_on_device_dictation() -> Check {
    Check::new(
        "dictation",
        Health::Warn,
        "no on-device speech recogniser on this platform — dictation needs openai gpt-transcribe",
    )
    .with_fix("`neo keys set openai`")
}

/// Speech is OpenAI-API-key only (K6), so a subscription-only user is
/// typed-only until they add one. That is a warning, never a failure.
/// Spoken *replies* (TTS), which really are OpenAI-key-only.
///
/// Voice **in** is a separate row: on-device dictation needs no key (K6 as
/// amended), so a user without an OpenAI key can talk to Starkbot — it just
/// cannot talk back. Where there is no on-device recogniser that is no
/// longer true, and this row says so rather than promising dictation the
/// machine cannot do.
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
            format!(
                "no openai key — no spoken replies ({})",
                voice_in_without_a_key()
            ),
        )
        .with_fix("`neo keys set openai`"),
    }
}

/// What a missing OpenAI key costs *besides* spoken replies.
fn voice_in_without_a_key() -> &'static str {
    if neo_voice::speech_status() == neo_voice::SpeechAuth::Unsupported {
        "and no dictation either"
    } else {
        "dictation still works on-device"
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

/// Where the managed Chrome profile's browser comes from (10). Resolved
/// rather than assumed, so a missing browser is reported here instead of
/// discovered mid-task.
fn chrome_check(chrome: Option<PathBuf>) -> Check {
    match chrome {
        Some(path) => Check::new("chrome", Health::Ok, path.display().to_string()),
        None => Check::new("chrome", Health::Fail, "no Chrome-family browser found")
            .with_fix("install Google Chrome — the managed profile needs it (10)"),
    }
}

/// Where the credentials actually are, and whether this session could have
/// done better.
///
/// The backend alone does not answer the question: a debug build picks the
/// file deliberately (signing prompts on macOS; a test suite must not write
/// into the user's own keyring on Linux), and that is not a problem. The
/// warning exists exactly when the keyring was wanted and the session has
/// none, so the row keys off it rather than off the backend.
fn credential_storage_check(keychain: &neo_keys::Keychain) -> Check {
    let detail = format!(
        "{} · keyring service {}",
        if keychain.is_login_keychain() {
            "os keyring"
        } else {
            "clear-text file"
        },
        if neo_keys::os_keyring_available() {
            "present"
        } else {
            "absent"
        },
    );
    match keychain.fallback_warning() {
        Some(warning) => Check::new(
            "credential storage",
            Health::Warn,
            format!("{detail} — {warning}"),
        )
        .with_fix(
            "start a keyring service — gnome-keyring, KWallet or KeePassXC — and sign in again",
        ),
        None => Check::new("credential storage", Health::Ok, detail),
    }
}

/// The Linux facts that stand where macOS has TCC (starkbot.md §11).
///
/// Nothing here is a permission: Linux grants none and asks for none. What
/// can be missing is *capability* — an accessibility bus that no one
/// started, a toolkit switch that leaves Chromium and Electron publishing no
/// tree at all, a session with no compositor — and each of those is a fact
/// this can read.
#[cfg(target_os = "linux")]
mod linux {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::{Check, Health};

    /// How long the session bus gets to answer both questions. Doctor is a
    /// startup screen: a wedged bus costs a row, never the report.
    const BUS_TIMEOUT: Duration = Duration::from_secs(2);

    /// The accessibility bus service, and the object publishing its status.
    const A11Y_BUS: &str = "org.a11y.Bus";
    const A11Y_PATH: &str = "/org/a11y/bus";
    const A11Y_STATUS: &str = "org.a11y.Status";

    pub(super) fn checks() -> Vec<Check> {
        let (bus, enabled) = match session_bus() {
            Ok(facts) => facts,
            Err(error) => {
                let detail = format!("the session bus did not answer: {error}");
                return vec![
                    Check::new("a11y bus", Health::Unknown, detail.clone()),
                    Check::new("a11y enabled", Health::Unknown, detail),
                    compositor(),
                ];
            }
        };
        vec![bus, enabled, compositor()]
    }

    /// Both a11y rows, from one connection.
    ///
    /// The queries run on their own thread with their own current-thread
    /// runtime because `doctor` is synchronous and every caller reaches it
    /// from inside a Tokio runtime, where blocking on a future panics.
    fn session_bus() -> Result<(Check, Check), String> {
        std::thread::spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())?;
            runtime.block_on(async {
                tokio::time::timeout(BUS_TIMEOUT, a11y())
                    .await
                    .map_err(|_| format!("no answer within {BUS_TIMEOUT:?}"))?
            })
        })
        .join()
        .map_err(|_| "the probe thread panicked".to_owned())?
    }

    async fn a11y() -> Result<(Check, Check), String> {
        let connection = zbus::Connection::session()
            .await
            .map_err(|error| error.to_string())?;
        // `NameHasOwner` rather than a call on the service itself: the name
        // is D-Bus activatable, so calling it would start `at-spi-bus-
        // launcher` and then report the daemon Doctor had just launched.
        let dbus = zbus::fdo::DBusProxy::new(&connection)
            .await
            .map_err(|error| error.to_string())?;
        let owned = dbus
            .name_has_owner(A11Y_BUS.try_into().map_err(|_| "bad bus name".to_owned())?)
            .await
            .map_err(|error| error.to_string())?;
        if !owned {
            return Ok((
                Check::new(
                    "a11y bus",
                    Health::Fail,
                    format!("nothing owns {A11Y_BUS} on the session bus"),
                )
                .with_fix("install at-spi2-core and log in again"),
                Check::new(
                    "a11y enabled",
                    Health::Unknown,
                    "unreadable while the a11y bus is down",
                ),
            ));
        }
        let bus = Check::new(
            "a11y bus",
            Health::Ok,
            format!("{A11Y_BUS} answers on the session bus"),
        );
        let status = zbus::Proxy::new(&connection, A11Y_BUS, A11Y_PATH, A11Y_STATUS)
            .await
            .map_err(|error| error.to_string())?;
        let enabled = match status.get_property::<bool>("IsEnabled").await {
            Ok(true) => Check::new("a11y enabled", Health::Ok, "org.a11y.Status.IsEnabled = true"),
            // Chromium and Electron read this switch and publish nothing
            // while it is false, so a native run against them observes an
            // empty tree and blames the app.
            Ok(false) => Check::new(
                "a11y enabled",
                Health::Warn,
                "org.a11y.Status.IsEnabled = false — Chromium and Electron apps publish no tree",
            )
            .with_fix("start the app with --force-renderer-accessibility, or turn accessibility on in the desktop settings"),
            Err(error) => Check::new("a11y enabled", Health::Unknown, error.to_string()),
        };
        Ok((bus, enabled))
    }

    /// The display server this session runs on.
    ///
    /// Reading the tree needs neither, but the input fallback does, and it
    /// is Wayland-only: X11 is reported rather than refused because AT-SPI
    /// works there and the actions come first anyway.
    fn compositor() -> Check {
        let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
        let wayland = std::env::var_os("WAYLAND_DISPLAY").map(PathBuf::from);
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "unknown".to_owned());
        match (runtime_dir, wayland) {
            (Some(dir), Some(display)) => {
                let socket = if display.is_absolute() {
                    display
                } else {
                    dir.join(display)
                };
                if socket.exists() {
                    Check::new(
                        "compositor",
                        Health::Ok,
                        format!("wayland · {desktop} · {}", socket.display()),
                    )
                } else {
                    Check::new(
                        "compositor",
                        Health::Fail,
                        format!(
                            "WAYLAND_DISPLAY names {}, which is not there",
                            socket.display()
                        ),
                    )
                }
            }
            _ if std::env::var_os("DISPLAY").is_some() => Check::new(
                "compositor",
                Health::Warn,
                format!("x11 · {desktop} · synthetic input on Wayland is unavailable here"),
            ),
            _ => Check::new(
                "compositor",
                Health::Fail,
                "no WAYLAND_DISPLAY and no DISPLAY — there is no session to drive",
            ),
        }
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
        let check = chrome_check(None);
        assert_eq!(check.health, Health::Fail);
        assert!(check.fix.is_some());
    }
}
