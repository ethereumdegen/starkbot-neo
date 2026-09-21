//! `jev-nav` — a goal goes in; each step observes the page's controls, asks Jev for
//! one operation + target in a single request, guards, executes, and repeats.
//!
//! Rust port of <https://github.com/browser-use/jev-ultrafast> (MIT).

#[cfg(feature = "ax")]
pub mod ax;
pub mod observer;
pub mod policy;
pub mod rules;
pub mod text;
pub mod web;
pub mod wire;

use std::time::{Duration, Instant};

use serde_json::{Value, json};

pub use observer::{ObserveError, Observer};

use policy::{Decision, action_space, build_request, resolve};
use rules::{MAX_ACTIONS, MAX_CONSECUTIVE_STALE, MAX_DECISIONS, ON_TASK, SAFETY};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Done,
    Blocked(String),
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

/// Why a run stops when the surface will not hold still.
const STALE_LIMIT: &str = "the surface changed under every decision; nothing could be executed";

pub struct RunConfig {
    pub goal: String,
    /// Ask the risk heads (`rules::SAFETY`) on every step.
    pub safety_heads: bool,
    /// Return instead of executing when a safety head crosses this (the caller shows a confirm card).
    pub confirm_at: f64,
    /// Stop the run when Jev's confidence that the page still serves the goal
    /// falls below this.
    ///
    /// A floor, not a ceiling: `on_task` answers "still on task?", so low is
    /// the dangerous direction — the opposite of the three risk heads. `0.0`
    /// disables the check, and the head is then not asked for at all.
    pub on_task_floor: f32,
}

pub struct Navigator<O: Observer> {
    pub observer: O,
    pub jev: TypeSafe,
    /// Whoever types field values, if anything does. Without one, the run
    /// stops at the first `TYPE_TEXT` rather than typing something invented.
    pub text: Option<Box<dyn TextHelper>>,
    history: Vec<Value>,
    pending_text: Option<(Value, String)>,
}

impl<O: Observer> Navigator<O> {
    pub fn new(observer: O, jev: TypeSafe, text: Option<Box<dyn TextHelper>>) -> Self {
        Self {
            observer,
            jev,
            text,
            history: Vec::new(),
            pending_text: None,
        }
    }

    pub fn history(&self) -> &[Value] {
        &self.history
    }

    pub async fn run(
        &mut self,
        config: &RunConfig,
        mut on_step: impl FnMut(&StepEvent),
    ) -> Result<Outcome, NavError> {
        let started = Instant::now();
        let mut decisions = 0usize;
        let mut consecutive_stale = 0usize;
        let timer = Instant::now();
        let mut observation = self.observer.observe().await?;
        let mut observe_ms = timer.elapsed().as_millis();

        loop {
            if decisions >= MAX_DECISIONS {
                return Ok(Outcome::Blocked("decision budget reached".into()));
            }
            if self.history.len() >= MAX_ACTIONS {
                return Ok(Outcome::Blocked("action budget reached".into()));
            }
            let actions: Vec<policy::Action> = observation["actions"]
                .as_array()
                .map(|list| list.iter().filter_map(|a| a.as_object().cloned()).collect())
                .unwrap_or_default();
            let space = action_space(&actions);
            let request = build_request(
                &observation,
                &space,
                &config.goal,
                &self.history,
                config.safety_heads,
                config.on_task_floor,
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
                candidates: actions.len(),
                stale: false,
                usage: evaluation.usage.clone(),
            };

            if matches!(decision.operation.as_str(), "DONE" | "BLOCKED") {
                if !self.observer.fresh(&observation, None).await? {
                    event.stale = true;
                    on_step(&event);
                    consecutive_stale += 1;
                    if consecutive_stale >= MAX_CONSECUTIVE_STALE {
                        return Ok(Outcome::Blocked(STALE_LIMIT.into()));
                    }
                    (observation, observe_ms) = self.reobserve().await?;
                    continue;
                }
                on_step(&event);
                return Ok(if decision.operation == "DONE" {
                    Outcome::Done
                } else {
                    Outcome::Blocked("Jev reported no operation can progress".into())
                });
            }

            // An absent head means "not asked": `policy::resolve` fails the
            // step when a head the request carried came back missing or
            // malformed, so there is no reading of a truncated answer that
            // scores a send at `0.0` and then executes it (R1.1).
            let risky = SAFETY.iter().any(|(head, _)| {
                decision
                    .safety
                    .get(*head)
                    .is_some_and(|probability| *probability >= config.confirm_at)
            });
            // The drift head is a floor: low confidence that the page still
            // serves the goal is the dangerous direction (R1.2).
            let drifted = config.on_task_floor > 0.0
                && decision
                    .safety
                    .get(ON_TASK.0)
                    .is_some_and(|probability| *probability < f64::from(config.on_task_floor));
            let Some(action) = decision.action.clone() else {
                return Ok(Outcome::Blocked("decision had no executable action".into()));
            };
            // A WAIT mutates nothing, so neither gate applies to one: stopping
            // a run for pausing while a sheet loads is a pure false positive,
            // and a page that is still loading is exactly the page whose
            // on-task score is about to recover.
            let waiting = action.get("kind").and_then(Value::as_str) == Some("wait");
            if risky && !waiting {
                on_step(&event);
                return Ok(Outcome::Blocked(format!(
                    "needs confirmation before `{}`",
                    event.label.clone().unwrap_or_default()
                )));
            }
            if drifted && !waiting {
                on_step(&event);
                return Ok(Outcome::Blocked(format!(
                    "the page drifted off the goal; stopped before `{}`",
                    event.label.clone().unwrap_or_default()
                )));
            }

            let mut typed = None;
            if action.get("kind").and_then(Value::as_str) == Some("fill") {
                let context = field_context(&config.goal, &action, &observation, &self.history);
                typed = Some(match &self.pending_text {
                    Some((cached, text)) if *cached == context => text.clone(),
                    _ => {
                        let helper = self.text.as_ref().ok_or(TextError::Unconfigured)?;
                        let value = helper.value(&context).await?;
                        event.text_ms = value.latency.as_millis();
                        self.pending_text = Some((context, value.text.clone()));
                        value.text
                    }
                });
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
                        return Ok(Outcome::Blocked(STALE_LIMIT.into()));
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
                return Ok(Outcome::Blocked(
                    "three actions in a row changed nothing".into(),
                ));
            }
        }
    }

    async fn reobserve(&mut self) -> Result<(Value, u128), NavError> {
        let timer = Instant::now();
        let observation = self.observer.observe().await?;
        Ok((observation, timer.elapsed().as_millis()))
    }
}

fn fingerprint(observation: &Value) -> String {
    json!([
        observation["url"],
        observation["text"],
        observation["actions"],
        observation["scroll"]
    ])
    .to_string()
}
