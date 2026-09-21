//! `jev-nav` — a goal goes in; each step observes the page's controls, asks Jev for
//! one operation + target in a single request, guards, executes, and repeats.
//!
//! Rust port of <https://github.com/browser-use/jev-ultrafast> (MIT).

#[cfg(feature = "ax")]
pub mod ax;
pub mod gate;
pub mod observer;
pub mod policy;
pub mod rules;
pub mod text;
pub mod web;
pub mod wire;

use std::time::{Duration, Instant};

use serde_json::{Value, json};

pub use observer::{ObserveError, Observer};

pub use gate::BlockReason;

use gate::{Gate, Verdict};
use policy::{Decision, action_space, build_request, resolve};
use rules::{MAX_ACTIONS, MAX_CONSECUTIVE_STALE, MAX_DECISIONS};
use text::{TextError, TextHelper, field_context};
use wire::{TypeSafe, WireError};

#[derive(Debug, thiserror::Error)]
pub enum NavError {
    #[error(transparent)]
    Wire(#[from] WireError),
    #[error(transparent)]
    Observe(#[from] ObserveError),
    #[error(transparent)]
    Text(#[from] TextError),
}

/// How a run ended — or paused.
///
/// Three of the four variants are recoverable by the person watching, which
/// is the whole point of the taxonomy (16 §5.1): before this, a confirm, a
/// login wall and an unknown field value all ended the run with a sentence
/// and no way back in.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Done,
    /// One action is waiting for a human yes or no.
    NeedsConfirm {
        escalation: Escalation,
        resume: ResumeToken,
    },
    /// Only the person at the keyboard can do the next thing.
    NeedsUser {
        reason: gate::NeedsUser,
        escalation: Escalation,
        resume: ResumeToken,
    },
    /// Nobody can carry this on from here.
    Blocked {
        reason: BlockReason,
    },
}

impl Outcome {
    /// Whether the run can be picked up again with [`Navigator::resume`].
    pub fn is_resumable(&self) -> bool {
        matches!(self, Self::NeedsConfirm { .. } | Self::NeedsUser { .. })
    }
}

/// Everything a confirm card — or Sol — needs to decide what happens next.
///
/// Deliberately data, not prose: the old `Blocked(String)` told the caller
/// what happened in a sentence it could only print.
#[derive(Debug, Clone, PartialEq)]
pub struct Escalation {
    /// What the run was about to do, when it was about to do something.
    pub action: Option<policy::Action>,
    pub label: Option<String>,
    pub operation: String,
    /// One sentence in the product's voice, for the card's headline.
    pub sentence: String,
    pub reason: Option<gate::ConfirmReason>,
    pub safety: std::collections::BTreeMap<String, f64>,
    pub url: String,
    pub title: String,
    /// The last few recorded actions, so a card can show how it got here.
    pub history: Vec<Value>,
}

/// What a paused run needs to carry on: the budget it already spent and the
/// thing it stopped on.
///
/// It is not a replay log. A resumed run re-observes and re-guards; the token
/// only says which action an approval was about, so an approval cannot be
/// spent on some other action the next observation happens to offer.
#[derive(Debug, Clone, PartialEq)]
pub struct ResumeToken {
    decisions: usize,
    pending: Option<policy::Action>,
    /// The field context a `NeedsUser::Value` was asked about.
    field: Option<Value>,
}

/// The human's answer to a pause.
#[derive(Debug, Clone)]
pub enum Approval {
    /// Do the thing that was asked about.
    Approve,
    /// Don't. The note becomes history the next decision can read, and the
    /// action itself is withdrawn from the run's action space.
    Deny { note: String },
    /// The user did the part only they could do (signed in, cleared a
    /// challenge); carry on from a fresh observation.
    Ready,
    /// The value for the field nothing else could fill.
    Value(String),
}

/// Everything that happened in one observe → decide → execute cycle, for the Steer ticker.
#[derive(Debug, Clone)]
pub struct StepEvent {
    pub elapsed: Duration,
    pub decision: Decision,
    pub label: Option<String>,
    pub typed: Option<String>,
    pub observe_ms: u128,
    pub jev_ms: u128,
    pub text_ms: u128,
    pub act_ms: u128,
    pub candidates: usize,
    pub stale: bool,
    pub usage: Value,
}

pub struct RunConfig {
    pub goal: String,
    pub safety_heads: bool,
    /// The probability at which a safety head becomes a confirm card.
    pub confirm_at: f64,
    /// Hosts this run may never act on (10 §7).
    pub denied_origins: Vec<String>,
}

pub struct Navigator<O: Observer> {
    pub observer: O,
    pub jev: TypeSafe,
    /// Whoever types field values, if anything does. Without one, the run
    /// stops at the first `TYPE_TEXT` rather than typing something invented.
    pub text: Option<Box<dyn TextHelper>>,
    history: Vec<Value>,
    pending_text: Option<(Value, String)>,
    /// The deterministic layer, kept across a pause so approvals, upload
    /// consent and off-task strikes survive a resume.
    gate: Option<Gate>,
    /// One action a human approved, spent on the next decision that names it.
    approved: Option<policy::Action>,
    /// Actions a human refused: withdrawn from the action space for the rest
    /// of the run, so a denial is not re-proposed on every observation.
    denied: Vec<Value>,
    /// The surface a `NeedsUser` was handed over on, so the same hand-over is
    /// not asked for twice on a page the user has not changed.
    handed_at: Option<String>,
}

impl<O: Observer> Navigator<O> {
    pub fn new(observer: O, jev: TypeSafe, text: Option<Box<dyn TextHelper>>) -> Self {
        Self {
            observer,
            jev,
            text,
            history: Vec::new(),
            pending_text: None,
            gate: None,
            approved: None,
            denied: Vec::new(),
            handed_at: None,
        }
    }

    pub fn history(&self) -> &[Value] {
        &self.history
    }

    /// Drive the goal from a fresh start.
    pub async fn run(
        &mut self,
        config: &RunConfig,
        on_step: impl FnMut(&StepEvent),
    ) -> Result<Outcome, NavError> {
        self.gate = Some(Gate::new(
            config.confirm_at,
            config.safety_heads,
            config.denied_origins.clone(),
        ));
        self.drive(config, 0, on_step).await
    }

    /// Carry on a paused run once the human has answered.
    ///
    /// Nothing is replayed: the surface is observed again and guarded again,
    /// because whatever the person did to it — signing in, clearing a
    /// challenge, or just taking a minute — has moved it. The token's only
    /// job is to keep an approval attached to the action it was given for,
    /// and to stop a resumed run from starting its budget over.
    pub async fn resume(
        &mut self,
        config: &RunConfig,
        token: ResumeToken,
        approval: Approval,
        on_step: impl FnMut(&StepEvent),
    ) -> Result<Outcome, NavError> {
        if self.gate.is_none() {
            self.gate = Some(Gate::new(
                config.confirm_at,
                config.safety_heads,
                config.denied_origins.clone(),
            ));
        }
        match approval {
            Approval::Approve => self.approved = token.pending,
            Approval::Deny { note } => {
                if let Some(action) = token.pending {
                    self.denied.push(identity_of(&action));
                    self.history.push(json!({
                        "action": action.get("label"), "kind": "refused",
                        "text": note, "page_changed": false,
                    }));
                }
            }
            Approval::Ready => {}
            Approval::Value(text) => {
                if let Some(field) = token.field {
                    self.pending_text = Some((field, text));
                }
            }
        }
        self.drive(config, token.decisions, on_step).await
    }

    async fn drive(
        &mut self,
        config: &RunConfig,
        spent: usize,
        mut on_step: impl FnMut(&StepEvent),
    ) -> Result<Outcome, NavError> {
        let started = Instant::now();
        let mut decisions = spent;
        let mut consecutive_stale = 0usize;
        let timer = Instant::now();
        let mut observation = self.observer.observe().await?;
        let mut observe_ms = timer.elapsed().as_millis();

        loop {
            if decisions >= MAX_DECISIONS {
                return Ok(self.blocked(BlockReason::Budget { what: "decision" }));
            }
            if self.history.len() >= MAX_ACTIONS {
                return Ok(self.blocked(BlockReason::Budget { what: "action" }));
            }
            // The page's own signals come first: a login wall or a challenge
            // is not something to ask a classifier about.
            let surface = fingerprint(&observation);
            if self.handed_at.as_deref() != Some(surface.as_str())
                && let Some(reason) = self.gate().page(&observation)
            {
                self.handed_at = Some(surface);
                return Ok(self.hand_over(reason, &observation, decisions));
            }
            let actions: Vec<policy::Action> = observation["actions"]
                .as_array()
                .map(|list| list.iter().filter_map(|a| a.as_object().cloned()).collect())
                .unwrap_or_default();
            // A refused action is withdrawn for the rest of the run: asking
            // again on every observation is how a confirm gate becomes the
            // thing users learn to ignore.
            let offered: Vec<policy::Action> = actions
                .iter()
                .filter(|action| !self.denied.contains(&identity_of(action)))
                .cloned()
                .collect();
            let space = action_space(&offered);
            let request = build_request(
                &observation,
                &space,
                &config.goal,
                &self.history,
                config.safety_heads,
            );
            let evaluation = self
                .jev
                .evaluate(&request.state, &request.questions)
                .await?;
            decisions += 1;
            let decision = resolve(&request, &space, &evaluation)?;

            let mut event = StepEvent {
                elapsed: started.elapsed(),
                label: decision
                    .action
                    .as_ref()
                    .and_then(|a| a.get("label"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                decision: decision.clone(),
                typed: None,
                observe_ms,
                jev_ms: evaluation.latency.as_millis(),
                text_ms: 0,
                act_ms: 0,
                candidates: offered.len(),
                stale: false,
                usage: evaluation.usage.clone(),
            };

            if matches!(decision.operation.as_str(), "DONE" | "BLOCKED") {
                if !self.observer.fresh(&observation, None).await? {
                    event.stale = true;
                    on_step(&event);
                    consecutive_stale += 1;
                    if consecutive_stale >= MAX_CONSECUTIVE_STALE {
                        return Ok(self.blocked(BlockReason::Unstable));
                    }
                    (observation, observe_ms) = self.reobserve().await?;
                    continue;
                }
                on_step(&event);
                return Ok(if decision.operation == "DONE" {
                    Outcome::Done
                } else {
                    self.blocked(BlockReason::NoOperation)
                });
            }

            // The run wandered off the goal, or page text talked the
            // classifier into somebody else's task.
            if let Some(reason) = self.gate().on_task(&decision.safety) {
                on_step(&event);
                return Ok(self.blocked(reason));
            }

            let Some(action) = decision.action.clone() else {
                return Ok(self.blocked(BlockReason::NoAction));
            };

            // An approval is spent only on the action it was given for.
            let pre_approved = self
                .approved
                .as_ref()
                .is_some_and(|approved| identity_of(approved) == identity_of(&action));
            let page_url = observation["url"].as_str().unwrap_or_default().to_owned();
            let verdict = if pre_approved {
                self.approved = None;
                self.gate().approve(&action, &page_url);
                Verdict::Allow
            } else {
                self.gate().action(&decision, &action, &page_url)
            };
            match verdict {
                Verdict::Allow => {}
                Verdict::Confirm(reason) => {
                    on_step(&event);
                    let escalation =
                        self.escalation(&decision, Some(&action), Some(reason), &observation);
                    return Ok(Outcome::NeedsConfirm {
                        escalation,
                        resume: ResumeToken {
                            decisions,
                            pending: Some(action),
                            field: None,
                        },
                    });
                }
                Verdict::Hand(reason) => {
                    on_step(&event);
                    return Ok(self.hand_over(reason, &observation, decisions));
                }
                Verdict::Refuse(reason) => {
                    on_step(&event);
                    return Ok(self.blocked(reason));
                }
            }

            let mut typed = None;
            if action.get("kind").and_then(Value::as_str) == Some("fill") {
                let context = field_context(&config.goal, &action, &observation, &self.history);
                let value = match &self.pending_text {
                    Some((cached, text)) if *cached == context => Ok(text.clone()),
                    _ => {
                        let helper = self.text.as_ref().ok_or(TextError::Unconfigured)?;
                        match helper.value(&context).await {
                            Ok(value) => {
                                event.text_ms = value.latency.as_millis();
                                self.pending_text = Some((context.clone(), value.text.clone()));
                                Ok(value.text)
                            }
                            // The goal does not say what this field wants:
                            // ask, rather than typing something invented or
                            // failing the whole run (10 §8).
                            Err(TextError::Unknown) => Err(context.clone()),
                            Err(other) => return Err(other.into()),
                        }
                    }
                };
                match value {
                    Ok(text) => typed = Some(text),
                    Err(field) => {
                        on_step(&event);
                        let reason = gate::NeedsUser::Value {
                            field: event.label.clone().unwrap_or_else(|| "this field".into()),
                        };
                        let escalation =
                            self.escalation(&decision, Some(&action), None, &observation);
                        return Ok(Outcome::NeedsUser {
                            reason,
                            escalation,
                            resume: ResumeToken {
                                decisions,
                                pending: Some(action),
                                field: Some(field),
                            },
                        });
                    }
                }
                event.typed = typed.clone();
            }

            let timer = Instant::now();
            match self
                .observer
                .act(&action, &observation, typed.as_deref())
                .await
            {
                Ok(()) => {}
                Err(ObserveError::Stale(_)) => {
                    event.stale = true;
                    on_step(&event);
                    consecutive_stale += 1;
                    if consecutive_stale >= MAX_CONSECUTIVE_STALE {
                        return Ok(self.blocked(BlockReason::Unstable));
                    }
                    (observation, observe_ms) = self.reobserve().await?;
                    continue;
                }
                Err(other) => return Err(other.into()),
            }
            event.act_ms = timer.elapsed().as_millis();
            self.pending_text = None;
            // Something executed, so the surface is holding still enough.
            consecutive_stale = 0;
            // Record execution before observing: a stale post-action read must not erase the action.
            self.history.push(json!({
                "action": event.label, "kind": action.get("kind"), "text": typed, "page_changed": Value::Null,
            }));
            on_step(&event);

            let before = fingerprint(&observation);
            (observation, observe_ms) = self.reobserve().await?;
            let changed = fingerprint(&observation) != before;
            if let Some(last) = self.history.last_mut() {
                last["page_changed"] = json!(changed);
            }
            let stuck = self.history.len() >= 3
                && self
                    .history
                    .iter()
                    .rev()
                    .take(3)
                    .all(|h| h["page_changed"] == json!(false) && h["kind"] != "wait");
            if stuck {
                return Ok(self.blocked(BlockReason::NoProgress));
            }
        }
    }

    /// The gate, which `run` and `resume` both install before driving.
    fn gate(&mut self) -> &mut Gate {
        self.gate
            .get_or_insert_with(|| Gate::new(0.4, true, Vec::new()))
    }

    fn blocked(&self, reason: BlockReason) -> Outcome {
        Outcome::Blocked { reason }
    }

    fn hand_over(
        &mut self,
        reason: gate::NeedsUser,
        observation: &Value,
        decisions: usize,
    ) -> Outcome {
        let escalation = Escalation {
            action: None,
            label: None,
            operation: "NEEDS_USER".into(),
            sentence: reason.sentence(),
            reason: None,
            safety: Default::default(),
            url: observation["url"].as_str().unwrap_or_default().to_owned(),
            title: observation["title"].as_str().unwrap_or_default().to_owned(),
            history: self.recent(),
        };
        Outcome::NeedsUser {
            reason,
            escalation,
            resume: ResumeToken {
                decisions,
                pending: None,
                field: None,
            },
        }
    }

    fn escalation(
        &self,
        decision: &Decision,
        action: Option<&policy::Action>,
        reason: Option<gate::ConfirmReason>,
        observation: &Value,
    ) -> Escalation {
        let label = action
            .and_then(|a| a.get("label"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let sentence = match &reason {
            Some(reason) => reason.sentence(label.as_deref().unwrap_or("this")),
            None => format!(
                "I don't know what to type in “{}”.",
                label.as_deref().unwrap_or("this field")
            ),
        };
        Escalation {
            action: action.cloned(),
            label,
            operation: decision.operation.clone(),
            sentence,
            reason,
            safety: decision.safety.clone(),
            url: observation["url"].as_str().unwrap_or_default().to_owned(),
            title: observation["title"].as_str().unwrap_or_default().to_owned(),
            history: self.recent(),
        }
    }

    /// The last few recorded actions, for a card's "how it got here".
    fn recent(&self) -> Vec<Value> {
        self.history.iter().rev().take(5).rev().cloned().collect()
    }

    async fn reobserve(&mut self) -> Result<(Value, u128), NavError> {
        let timer = Instant::now();
        let observation = self.observer.observe().await?;
        Ok((observation, timer.elapsed().as_millis()))
    }
}

/// What makes two offered actions the same action, for matching an approval
/// or a refusal to a later observation.
///
/// Node identity plus kind plus label, and not the rect: the button a person
/// approved is the same button when the page has reflowed under it, and is
/// *not* the same button if the label changed to something else.
fn identity_of(action: &policy::Action) -> Value {
    json!([
        action.get("kind"),
        action.get("node"),
        action.get("label"),
        action.get("value"),
    ])
}

/// A canonical, geometry-free digest of what the surface *says*: the input to
/// the no-progress tripwire and to `page_changed`.
///
/// Rects are deliberately excluded. They used to ride along inside
/// `actions`, which meant one animated banner moved every element by a pixel
/// and reported progress on a page where nothing had happened — defeating
/// the very tripwire this feeds (10 §2.14). Scroll position stays, because
/// scrolling *is* progress.
fn fingerprint(observation: &Value) -> String {
    let actions: Vec<Value> = observation["actions"]
        .as_array()
        .map(|list| list.iter().map(semantics).collect())
        .unwrap_or_default();
    json!([
        observation["url"],
        observation["text"],
        actions,
        observation["scroll"]
    ])
    .to_string()
}

/// One offered action reduced to the keys that carry its meaning.
fn semantics(action: &Value) -> Value {
    const MEANING: [&str; 11] = [
        "kind",
        "id",
        "node",
        "label",
        "role",
        "value",
        "current_value",
        "options",
        "checked",
        "selected",
        "expanded",
    ];
    Value::Object(
        MEANING
            .iter()
            .filter_map(|key| action.get(*key).map(|value| ((*key).into(), value.clone())))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn button(label: &str, x: f64) -> Value {
        json!({
            "kind": "click", "node": 1, "id": "click:1", "label": label,
            "role": "button", "rect": [x, 12.0, 80.0, 24.0], "x": x, "y": 12.0,
        })
    }

    /// A page whose only change is that something moved has not progressed;
    /// before this, an animation was enough to keep a stuck run running to
    /// its action budget.
    #[test]
    fn movement_alone_is_not_a_changed_page() {
        let still = json!({ "url": "https://fixture.test/", "text": "hi", "scroll": { "y": 0 },
                            "actions": [button("Continue", 40.0)] });
        let animated = json!({ "url": "https://fixture.test/", "text": "hi", "scroll": { "y": 0 },
                               "actions": [button("Continue", 41.0)] });

        assert_eq!(fingerprint(&still), fingerprint(&animated));
    }

    /// …while anything a user would call a change still registers.
    #[test]
    fn meaning_still_registers() {
        let before = json!({ "url": "https://fixture.test/", "text": "hi", "scroll": { "y": 0 },
                             "actions": [button("Continue", 40.0)] });
        for after in [
            json!({ "url": "https://fixture.test/next", "text": "hi", "scroll": { "y": 0 },
                    "actions": [button("Continue", 40.0)] }),
            json!({ "url": "https://fixture.test/", "text": "bye", "scroll": { "y": 0 },
                    "actions": [button("Continue", 40.0)] }),
            json!({ "url": "https://fixture.test/", "text": "hi", "scroll": { "y": 600 },
                    "actions": [button("Continue", 40.0)] }),
            json!({ "url": "https://fixture.test/", "text": "hi", "scroll": { "y": 0 },
                    "actions": [button("Submit", 40.0)] }),
            json!({ "url": "https://fixture.test/", "text": "hi", "scroll": { "y": 0 },
                    "actions": [] }),
        ] {
            assert_ne!(
                fingerprint(&before),
                fingerprint(&after),
                "a real change went unnoticed"
            );
        }
    }
}
