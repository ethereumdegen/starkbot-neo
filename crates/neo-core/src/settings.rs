use serde::{Deserialize, Serialize};

use crate::{CoreError, ModelRef, ProviderId};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddressingMode {
    #[default]
    Open,
    NameRequired,
    PushToTalk,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeakMode {
    #[default]
    QuestionsOnly,
    QuestionsAndResults,
    Everything,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DuplexMode {
    #[default]
    Auto,
    Half,
    Full,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Minimal,
    #[default]
    Low,
    Medium,
    High,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserMode {
    #[default]
    Managed,
    Attach,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IdentitySettings {
    pub name: String,
}

impl Default for IdentitySettings {
    fn default() -> Self {
        Self {
            name: "Stark".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ListenSettings {
    pub enabled: bool,
    pub addressing: AddressingMode,
    pub push_to_talk: bool,
    pub mic_device: Option<String>,
}

impl Default for ListenSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            addressing: AddressingMode::Open,
            push_to_talk: false,
            mic_device: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VoiceSettings {
    pub tts_enabled: bool,
    pub tts_voice: String,
    pub speak: SpeakMode,
    pub duplex: DuplexMode,
    pub keep_recordings: bool,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            tts_enabled: false,
            tts_voice: "marin".into(),
            speak: SpeakMode::QuestionsOnly,
            duplex: DuplexMode::Auto,
            keep_recordings: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LiveSttSettings {
    pub enabled: bool,
    pub model: ModelRef,
}

impl Default for LiveSttSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            model: openai_model("gpt-live-transcribe"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelSettings {
    pub inference: ModelRef,
    pub text_helper: ModelRef,
    pub stt: ModelRef,
    pub stt_live: LiveSttSettings,
    pub tts: ModelRef,
    pub sol_effort: ReasoningEffort,
}

impl Default for ModelSettings {
    fn default() -> Self {
        Self {
            inference: openai_model("sol-latest"),
            text_helper: openai_model("gpt-5.6-luna"),
            stt: openai_model("gpt-transcribe"),
            stt_live: LiveSttSettings::default(),
            tts: openai_model("gpt-4o-mini-tts"),
            sol_effort: ReasoningEffort::Low,
        }
    }
}

fn openai_model(id: &str) -> ModelRef {
    ModelRef::new(ProviderId::new("openai"), id)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IntakeSettings {
    pub enqueue_at: f32,
    pub offer_at: f32,
}

impl Default for IntakeSettings {
    fn default() -> Self {
        Self {
            enqueue_at: 0.70,
            offer_at: 0.40,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConfirmThresholds {
    pub outward: f32,
    pub destructive: f32,
    pub spends: f32,
}

impl Default for ConfirmThresholds {
    fn default() -> Self {
        Self {
            outward: 0.40,
            destructive: 0.40,
            spends: 0.40,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SafetySettings {
    pub confirm_at: ConfirmThresholds,
    pub on_task_floor: f32,
    pub confirm_timeout_s: u32,
    pub confirm_labels: Vec<String>,
}

impl Default for SafetySettings {
    fn default() -> Self {
        Self {
            confirm_at: ConfirmThresholds::default(),
            on_task_floor: 0.30,
            confirm_timeout_s: 120,
            confirm_labels: [
                "send",
                "post",
                "publish",
                "submit",
                "share",
                "reply all",
                "tweet",
                "launch",
                "go live",
                "schedule",
                "delete",
                "remove",
                "erase",
                "discard",
                "empty trash",
                "format",
                "reset",
                "overwrite",
                "pay",
                "buy",
                "purchase",
                "order",
                "checkout",
                "subscribe",
                "upgrade",
                "donate",
                "transfer",
                "withdraw",
                "sign out",
                "log out",
                "deactivate",
                "unsubscribe",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CapSettings {
    pub usd_per_task: f64,
    pub usd_media_call_confirm: f64,
    pub usd_per_day: f64,
    pub sol_steps: u32,
    pub nav_actions: u32,
    pub nav_decisions: u32,
    pub wall_minutes: u32,
}

impl Default for CapSettings {
    fn default() -> Self {
        Self {
            usd_per_task: 1.0,
            usd_media_call_confirm: 0.25,
            usd_per_day: 10.0,
            sol_steps: 40,
            nav_actions: 60,
            nav_decisions: 120,
            wall_minutes: 10,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QueueSettings {
    pub idle_wait_s: f32,
    pub paused: bool,
}

impl Default for QueueSettings {
    fn default() -> Self {
        Self {
            idle_wait_s: 3.0,
            paused: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrowserSettings {
    pub mode: BrowserMode,
    pub keep_running: bool,
    pub close_task_tabs: bool,
}

impl Default for BrowserSettings {
    fn default() -> Self {
        Self {
            mode: BrowserMode::Managed,
            keep_running: true,
            close_task_tabs: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HotkeySettings {
    pub toggle_listen: String,
    pub quick_entry: String,
    pub kill: String,
}

impl Default for HotkeySettings {
    fn default() -> Self {
        Self {
            toggle_listen: "⌥Space".into(),
            quick_entry: "⌥⌘Space".into(),
            kill: "⌃⌥⌘.".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GeneralSettings {
    pub autostart: bool,
    pub update_check: bool,
    pub notifications: bool,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            autostart: false,
            update_check: true,
            notifications: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PrivacySettings {
    pub trace_days: u32,
    pub verdict_days: u32,
    pub ignored_text_days: u32,
}

impl Default for PrivacySettings {
    fn default() -> Self {
        Self {
            trace_days: 30,
            verdict_days: 180,
            ignored_text_days: 7,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub identity: IdentitySettings,
    pub listen: ListenSettings,
    pub voice: VoiceSettings,
    pub models: ModelSettings,
    pub intake: IntakeSettings,
    pub safety: SafetySettings,
    pub caps: CapSettings,
    pub queue: QueueSettings,
    pub browser: BrowserSettings,
    pub hotkeys: HotkeySettings,
    pub general: GeneralSettings,
    pub privacy: PrivacySettings,
}

impl Settings {
    pub fn validate(&self) -> Result<(), CoreError> {
        let name_len = self.identity.name.trim().chars().count();
        if !(1..=24).contains(&name_len) {
            return Err(invalid("identity.name", "must contain 1 to 24 characters"));
        }
        probability("intake.enqueue_at", self.intake.enqueue_at)?;
        probability("intake.offer_at", self.intake.offer_at)?;
        if self.intake.offer_at > self.intake.enqueue_at {
            return Err(invalid(
                "intake.offer_at",
                "cannot exceed intake.enqueue_at",
            ));
        }
        for (field, value) in [
            ("safety.confirm_at.outward", self.safety.confirm_at.outward),
            (
                "safety.confirm_at.destructive",
                self.safety.confirm_at.destructive,
            ),
            ("safety.confirm_at.spends", self.safety.confirm_at.spends),
        ] {
            probability(field, value)?;
            if value > 0.60 {
                return Err(invalid(field, "cannot exceed the 0.60 safety ceiling"));
            }
        }
        probability("safety.on_task_floor", self.safety.on_task_floor)?;
        if self.safety.on_task_floor < 0.15 {
            return Err(invalid(
                "safety.on_task_floor",
                "cannot be below the 0.15 safety floor",
            ));
        }
        if self.safety.confirm_timeout_s == 0 {
            return Err(invalid("safety.confirm_timeout_s", "must be positive"));
        }
        if self.caps.usd_per_task <= 0.0
            || self.caps.usd_media_call_confirm <= 0.0
            || self.caps.usd_per_day <= 0.0
        {
            return Err(invalid("caps", "dollar caps must be positive"));
        }
        if self.caps.sol_steps == 0
            || self.caps.nav_actions == 0
            || self.caps.nav_decisions == 0
            || self.caps.wall_minutes == 0
        {
            return Err(invalid("caps", "step and wall caps must be positive"));
        }
        Ok(())
    }
}

fn probability(field: &'static str, value: f32) -> Result<(), CoreError> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(invalid(field, "must be a finite probability from 0 to 1"))
    }
}

fn invalid(field: &'static str, reason: &str) -> CoreError {
    CoreError::InvalidSetting {
        field,
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_product_contract() {
        let settings = Settings::default();
        assert_eq!(settings.identity.name, "Stark");
        assert_eq!(settings.models.inference.id, "sol-latest");
        assert_eq!(settings.models.text_helper.id, "gpt-5.6-luna");
        assert_eq!(settings.caps.usd_per_task, 1.0);
        assert_eq!(settings.safety.confirm_at.outward, 0.40);
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn rejects_safety_floor_regressions() {
        let mut settings = Settings::default();
        settings.safety.on_task_floor = 0.14;
        assert!(matches!(
            settings.validate(),
            Err(CoreError::InvalidSetting {
                field: "safety.on_task_floor",
                ..
            })
        ));
    }

    #[test]
    fn missing_fields_take_defaults() {
        let settings: Settings =
            serde_json::from_value(serde_json::json!({"identity": {"name": "Nova"}}))
                .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(settings.identity.name, "Nova");
        assert_eq!(settings.caps.nav_actions, 60);
    }
}
