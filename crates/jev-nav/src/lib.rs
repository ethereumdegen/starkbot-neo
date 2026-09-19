//! `jev-nav` — a goal goes in; each step observes the page's controls, asks Jev for
//! one operation + target in a single request, guards, executes, and repeats.
//!
//! Rust port of <https://github.com/browser-use/jev-ultrafast> (MIT).

pub mod policy;
pub mod rules;
pub mod text;
pub mod web;
pub mod wire;

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use policy::{Decision, action_space, build_request, resolve};
use rules::{MAX_ACTIONS, MAX_DECISIONS};
use text::{OpenAiTextHelper, TextError, field_context};
use web::{CdpObserver, ObserveError};
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

pub struct RunConfig {
    pub goal: String,
    pub safety_heads: bool,
    /// Return instead of executing when a safety head crosses this (the caller shows a confirm card).
    pub confirm_at: f64,
}

pub struct Navigator {
    pub observer: CdpObserver,
    pub jev: TypeSafe,
    pub text: Option<OpenAiTextHelper>,
    history: Vec<Value>,
    pending_text: Option<(Value, String)>,
}

impl Navigator {
    pub fn new(observer: CdpObserver, jev: TypeSafe, text: Option<OpenAiTextHelper>) -> Self {
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

            let risky = ["outward", "destructive", "spends"].iter().any(|head| {
                decision.safety.get(*head).copied().unwrap_or(0.0) >= config.confirm_at
            });
            let Some(action) = decision.action.clone() else {
                return Ok(Outcome::Blocked("decision had no executable action".into()));
            };
            if risky && action.get("kind").and_then(Value::as_str) != Some("wait") {
                on_step(&event);
                return Ok(Outcome::Blocked(format!(
                    "needs confirmation before `{}`",
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
                    (observation, observe_ms) = self.reobserve().await?;
                    continue;
                }
                Err(other) => return Err(other.into()),
            }
            event.act_ms = timer.elapsed().as_millis();
            self.pending_text = None;
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
