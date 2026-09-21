//! Agent-in-the-loop evaluation of app control, on `spice-framework`.
//!
//! This crate answers one question with evidence: **can Sol plus Jev actually
//! operate a real application?** Not "did the model produce plausible prose" —
//! whether the document, the spreadsheet cell, or the page is in the state the
//! task asked for.
//!
//! # Why `spice-framework`
//!
//! App control is nondeterministic twice over: the model picks a different
//! action sequence each run, and the application itself is a moving target
//! (layout, focus, timing). A normal `#[test]` is the wrong shape — one run,
//! pass or fail. `spice` is built for exactly this: `consensus_runs` /
//! `consensus_required` express "4 of 5 runs must pass", which is the
//! acceptance bar the plan already asks for (S8a), and the report carries
//! per-run latency and token cost so a regression in *cost* is visible too.
//!
//! Nothing in `spice` needed changing. Three seams carry all of it:
//! [`spice_framework::AgentUnderTest`] (implemented by [`NeoAgent`] over
//! `Runtime::chat`), [`spice_framework::Judge`] (implemented by
//! [`judge::JevJudge`] over TypeSafe's typed heads, on the same Jev
//! credential the navigator already needs, so judging costs no second
//! vendor), and `Assertion::ExpectToolArg`, which is how a probe's
//! observation becomes a hard assertion.
//!
//! # The part that makes this an eval and not a vibe check
//!
//! A model that says "I set A1 to 42" proves nothing. Every case ends with a
//! **probe**: after the turn, the harness reads the application's own state
//! back through the same accessibility and CDP paths the agent used, and
//! attaches it as a synthetic `probe` tool call whose *arguments are the
//! observed state*. Assertions then run against the app, not the transcript:
//!
//! ```text
//! Assertion::ExpectToolArg("probe".into(), "value".into(), json!("42"))
//! ```
//!
//! A run where the model claims success and the cell is empty fails.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use neo_agent::Runtime;
use neo_agent::agent::{ChatMessage, ChatRequest};
use neo_agent::screen::ScreenScope;
use neo_core::{ActionKind, ActionSummary, AppEvent, ConversationId, Envelope, RunId, TurnUsage};
use serde_json::{Value, json};
use spice_framework::agent::{AgentConfig, AgentOutput, AgentUnderTest, ToolCall, Turn, Usage};
use spice_framework::error::SpiceError;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

pub mod apps;
pub mod cards;
pub mod cases;
pub mod fixture;
pub mod judge;
pub mod pages;
pub mod probe;
pub mod suite;

pub use apps::{App, availability};
pub use cards::Cards;
pub use fixture::Fixture;
pub use judge::JevJudge;
pub use probe::{Probe, run_probe};
pub use spice_framework::report::{SuiteReport, TestReport};
pub use suite::{CASE_TIMEOUT, CaseListing, EvalError, Selection, list_cases, run_suite};

/// How many actions one eval turn may spend. Lower than the interactive
/// default: an eval task is one concrete outcome, and a run that needs eight
/// actions to set a spreadsheet cell has already failed the thing being
/// measured.
pub const EVAL_MAX_STEPS: usize = 5;

/// Starkbot under test.
///
/// Wraps the real [`Runtime`], so an eval exercises the same agent loop, the
/// same `jev-nav` policy, the same element budgets and the same safety heads
/// as a user typing into the TUI. There is no eval-only shortcut: if this
/// passes, the product does the thing.
pub struct NeoAgent {
    runtime: Arc<Runtime>,
    /// One conversation for the whole suite. `chat` records each turn against
    /// it, and `turns` has a foreign key on `conversations(id)`: an id the
    /// store never saw would lose every turn record the eval produces.
    conversation: ConversationId,
    /// The suite's stop signal, threaded into every turn so a cancelled eval
    /// does not leave a browser open and a document half typed.
    cancel: CancellationToken,
    /// The screen lease the suite holds for its whole run, so each case's turn
    /// can take the keyboard *inside* it.
    ///
    /// Without this every app case would be refused: the suite acquires the
    /// screen once under its own run id ([`suite::run`]), while each case mints
    /// a fresh run for its turn — `request.run` is what separates the per-case
    /// traces, the `turns` rows and the desktop's run list, so the cases cannot
    /// simply share one id. A lease that guessed at nesting from the run id
    /// would therefore have to refuse them; the scope says plainly that this
    /// turn runs inside a lease its caller already holds.
    screen: Option<ScreenScope>,
}

impl NeoAgent {
    #[must_use]
    pub fn new(
        runtime: Arc<Runtime>,
        conversation: ConversationId,
        cancel: CancellationToken,
        screen: Option<ScreenScope>,
    ) -> Self {
        Self {
            runtime,
            conversation,
            cancel,
            screen,
        }
    }
}

/// Exactly the actions the agent loop can take (P3: no shell, no files).
///
/// A constant rather than a literal inside
/// [`AgentUnderTest::available_tools`], so the test that guards the surface
/// reads the same list the agent reports. Asserting against a second copy
/// would only ever prove the copy right.
///
/// Three of these are the agent's registered tools and come from
/// `neo_agent::agent::metal::registry` — `browse`, `app`, `ax` — plus
/// `answer`, the terminal action every turn ends on. The remaining three are
/// the harness's own synthetic calls: `fixture` (the known starting state),
/// `probe` (the app's state read back) and `cards` (the confirm and ask cards
/// the run published, and what the person watching answered). A judge told
/// about a tool that is not registered marks a run down for "not using" it,
/// which is why this list must be the true surface and not the aspirational
/// one.
pub const ACTIONS: [&str; 7] = ["browse", "app", "ax", "answer", "fixture", "probe", "cards"];

#[async_trait]
impl AgentUnderTest for NeoAgent {
    async fn run(
        &self,
        user_message: &str,
        config: &AgentConfig,
    ) -> Result<AgentOutput, SpiceError> {
        let started = Instant::now();

        if self.cancel.is_cancelled() {
            return Ok(AgentOutput {
                final_text: String::new(),
                error: Some("the eval was cancelled".to_owned()),
                duration: started.elapsed(),
                ..Default::default()
            });
        }

        // Known starting state first: without it a case measures whatever the
        // last run left on screen, which is how the first TextEdit case
        // "failed".
        let mut prelude: Vec<Turn> = Vec::new();
        // What the fixture wants the model to know. The prelude turn below
        // is part of the *report*, not the conversation — the request is
        // built from the user message alone — so a note left only there
        // reaches the judge and never the agent. Three runs opened their own
        // project against a fixture that had already opened one, because the
        // sentence saying so was never sent.
        let mut note: Option<String> = None;
        if let Some(fixture) = Fixture::from_config(config) {
            match fixture::apply(&self.runtime, &fixture).await {
                Ok(applied) => {
                    note = applied
                        .get("note")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    prelude.push(Turn {
                        index: 0,
                        // The fixture's own words when it has any. A fixture that
                        // prepared something the task must not redo has to say so
                        // where the model reads prose, not only inside the tool
                        // arguments: with the note in the payload alone, a run
                        // opened its own project anyway and spent nine of ten
                        // actions in a file chooser.
                        output_text: Some(
                            applied
                                .get("note")
                                .and_then(Value::as_str)
                                .unwrap_or("fixture applied")
                                .to_owned(),
                        ),
                        tool_calls: vec![ToolCall {
                            id: "fixture".to_owned(),
                            name: "fixture".to_owned(),
                            arguments: applied.clone(),
                        }],
                        tool_results: vec![applied],
                        stop_reason: Some("fixture".to_owned()),
                        duration: Duration::ZERO,
                    });
                }
                // A broken fixture is a harness failure, not a model failure:
                // it is reported as the run's error so the case does not read
                // as "the agent could not do it".
                Err(error) => {
                    return Ok(AgentOutput {
                        final_text: String::new(),
                        error: Some(format!("fixture failed: {error}")),
                        duration: started.elapsed(),
                        ..Default::default()
                    });
                }
            }
        }

        // The environment the task starts in, then the task. Stated as its
        // own message rather than folded into the user's words, because the
        // case's `user_message` is the sentence a person actually said and
        // the run is only honest if that reaches the model unedited.
        let mut messages = Vec::new();
        if let Some(note) = note {
            messages.push(ChatMessage::user(format!("Before you start: {note}.")));
        }
        messages.push(ChatMessage::user(user_message));
        let mut request = ChatRequest::new(self.conversation, messages);
        // Most cases are one concrete outcome and get the tight default. A
        // case that legitimately needs more — a media task opens an app,
        // makes a document and exports it, which is three outcomes before
        // anything is drawn — says so in its own config rather than raising
        // the budget for the spreadsheet cases too.
        request.max_steps = config
            .data
            .get("max_steps")
            .and_then(Value::as_u64)
            .and_then(|steps| usize::try_from(steps).ok())
            .unwrap_or(EVAL_MAX_STEPS);
        request.cancel = self.cancel.clone();
        request.screen = self.screen;

        // `chat` has no progress callback any more: progress is events, so a
        // window, a terminal and this harness can all watch the same turn.
        // The eval subscribes *before* the call and keeps only the envelopes
        // carrying its own run — which is why the run id is minted here rather
        // than inside `chat`. The trace is not decoration: assertions such as
        // `ExpectToolArg("browse", "url", …)` are scored against it, and a
        // turn that errors keeps the steps it got through.
        //
        // The same subscription answers the run's cards. A confirm or an ask
        // is published *while* `chat` is still awaiting it, so there is
        // nobody else who could: a drain afterwards would find a run that had
        // already timed out (16 §5.3).
        let stand = cards::Stand::new(
            Arc::clone(&self.runtime),
            Cards::from_config(config).unwrap_or_default(),
        );
        let steps = Collector::attach(self.runtime.subscribe(), request.run, stand.clone());
        let outcome = self.runtime.chat(request).await;
        let collected = steps.finish().await;

        let (final_text, error) = match outcome {
            Ok(outcome) => (outcome.text, None),
            // A failed run is data, not a harness error: the case still gets
            // scored, and `ExpectNoError` is what fails it.
            Err(error) => (String::new(), Some(error.to_string())),
        };

        let offset = prelude.len();
        let mut turns: Vec<Turn> = prelude;
        turns.extend(
            collected
                .steps
                .into_iter()
                .enumerate()
                .map(|(index, step)| {
                    let observation = step.observation.unwrap_or_else(|| step.thought.clone());
                    Turn {
                        index: index + offset,
                        output_text: Some(observation.clone()),
                        tool_calls: vec![ToolCall {
                            id: format!("step-{index}"),
                            name: step.name,
                            arguments: step.arguments,
                        }],
                        tool_results: vec![json!({ "observation": observation })],
                        stop_reason: None,
                        duration: step.duration,
                    }
                }),
        );

        // What the run asked the person watching, and what they said. It goes
        // in before the probe, in the order it happened: the approval is what
        // the probe then finds the consequence of.
        let published = stand.published();
        if Cards::from_config(config).is_some() || !published.is_empty() {
            let described = published.describe();
            let index = turns.len();
            turns.push(Turn {
                index,
                output_text: None,
                tool_calls: vec![ToolCall {
                    id: "cards".to_owned(),
                    name: "cards".to_owned(),
                    arguments: described.clone(),
                }],
                tool_results: vec![described],
                stop_reason: Some("cards".to_owned()),
                duration: Duration::ZERO,
            });
        }

        // The probe: read the application's own state back, and attach it as a
        // tool call whose arguments *are* the observation. This is what makes
        // the assertions statements about the app rather than about the model.
        // A cancelled run is not probed: the app is mid-edit, and an
        // observation of that is worse than none.
        if let Some(probe) = Probe::from_config(config)
            && !self.cancel.is_cancelled()
        {
            let observed = run_probe(&self.runtime, &probe)
                .await
                .unwrap_or_else(|error| json!({ "error": error.to_string() }));
            let index = turns.len();
            turns.push(Turn {
                index,
                output_text: None,
                tool_calls: vec![ToolCall {
                    id: "probe".to_owned(),
                    name: "probe".to_owned(),
                    arguments: observed.clone(),
                }],
                tool_results: vec![observed],
                stop_reason: Some("probe".to_owned()),
                duration: Duration::ZERO,
            });
        }

        let tools_called = turns
            .iter()
            .flat_map(|turn| turn.tool_calls.iter().map(|call| call.name.clone()))
            .collect();
        Ok(AgentOutput {
            final_text,
            turns,
            tools_called,
            duration: started.elapsed(),
            error,
            usage: Some(
                collected
                    .usage
                    .as_ref()
                    .map_or_else(Usage::default, usage_of),
            ),
        })
    }

    fn available_tools(&self, _config: &AgentConfig) -> Vec<String> {
        ACTIONS.iter().map(|action| (*action).to_owned()).collect()
    }

    fn name(&self) -> &str {
        "starkbot-neo"
    }
}

/// One action as a spice tool call: the name is the action, the arguments are
/// what it was given, so `ExpectToolArg("browse", "url", …)` works. The
/// argument keys are a contract — every assertion in [`cases`] is written
/// against them.
fn describe(action: &ActionSummary) -> (String, Value) {
    match action.kind {
        ActionKind::Browse => (
            "browse".to_owned(),
            json!({ "url": action.target, "goal": action.goal }),
        ),
        ActionKind::App => (
            "app".to_owned(),
            json!({ "app": action.target, "goal": action.goal }),
        ),
        ActionKind::Answer => ("answer".to_owned(), json!({ "text": action.text })),
        ActionKind::Ask => ("ask".to_owned(), json!({ "question": action.text })),
    }
}

/// What a turn's usage costs, in spice's shape.
///
/// `cost_usd` stays empty on purpose: plan-backed work is `Usd::Unpriced`
/// (05 §7), and a report that invented a price would make a subscription run
/// look like a metered one.
fn usage_of(usage: &TurnUsage) -> Usage {
    Usage {
        input_tokens: Some(usage.input_tokens),
        output_tokens: Some(usage.output_tokens),
        total_tokens: Some(usage.total_tokens()),
        cost_usd: None,
    }
}

/// One step of a turn, as the events described it.
struct StepTrace {
    name: String,
    arguments: Value,
    /// The model's reason for the action, which stands in for the observation
    /// until the action resolves — a turn that errors mid-step still shows
    /// what it was trying to do.
    thought: String,
    observation: Option<String>,
    duration: Duration,
}

/// What one turn's events amounted to.
#[derive(Default)]
struct Collected {
    steps: Vec<StepTrace>,
    usage: Option<TurnUsage>,
}

/// A background subscriber that keeps one run's progress while `chat` runs.
///
/// It has to be a task rather than a drain afterwards: the broadcast buffer is
/// finite, and a turn that overran it would silently lose steps from the
/// trace, which weakens the very assertions the case is scored on.
struct Collector {
    stop: CancellationToken,
    task: tokio::task::JoinHandle<Collected>,
}

impl Collector {
    fn attach(mut events: broadcast::Receiver<Envelope>, run: RunId, stand: cards::Stand) -> Self {
        let stop = CancellationToken::new();
        let signal = stop.clone();
        let task = tokio::spawn(async move {
            let mut collected = Collected::default();
            loop {
                let envelope = tokio::select! {
                    received = events.recv() => match received {
                        Ok(envelope) => envelope,
                        // Lagging drops steps this trace needed; there is
                        // nothing to recover, so keep what still arrives.
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                    () = signal.cancelled() => break,
                };
                // A card is answered on the spot rather than absorbed: the
                // run is parked inside `chat` waiting for it, so nothing else
                // is arriving meanwhile and an answer deferred to the end of
                // the turn is an answer that never comes. Cards carry a task
                // id rather than a run id, and the suite drives one case at a
                // time (`suite::execute`), so a card in flight is this run's.
                match &envelope.event {
                    AppEvent::ConfirmRequest { confirm } => {
                        stand.confirm(confirm);
                        continue;
                    }
                    AppEvent::AskRequest { ask } => {
                        stand.ask(ask).await;
                        continue;
                    }
                    _ => {}
                }
                if absorb(&mut collected, run, envelope.event) {
                    return collected;
                }
            }
            // The stop arm can win the race against events already queued —
            // `TurnFinished` is published before `chat` returns — so take what
            // is left before answering.
            while let Ok(envelope) = events.try_recv() {
                if absorb(&mut collected, run, envelope.event) {
                    break;
                }
            }
            collected
        });
        Self { stop, task }
    }

    async fn finish(self) -> Collected {
        self.stop.cancel();
        // A collector that panicked would take the whole eval down with it if
        // this unwrapped; an empty trace fails the case instead, which is the
        // outcome a harness fault deserves.
        self.task.await.unwrap_or_default()
    }
}

/// Fold one event into the trace. Answers whether the turn ended.
fn absorb(collected: &mut Collected, run: RunId, event: AppEvent) -> bool {
    match event {
        AppEvent::TurnStep {
            run: theirs,
            action,
            thought,
            ..
        } if theirs == run => {
            let (name, arguments) = describe(&action);
            collected.steps.push(StepTrace {
                name,
                arguments,
                thought,
                observation: None,
                duration: Duration::ZERO,
            });
            false
        }
        AppEvent::TurnStepDone {
            run: theirs,
            observation,
            duration_ms,
            ..
        } if theirs == run => {
            if let Some(step) = collected.steps.last_mut() {
                step.observation = Some(observation);
                step.duration = Duration::from_millis(duration_ms);
            }
            false
        }
        AppEvent::TurnFinished {
            run: theirs, usage, ..
        } if theirs == run => {
            collected.usage = usage;
            true
        }
        AppEvent::TurnFailed { run: theirs, .. } => theirs == run,
        _ => false,
    }
}

/// The trace a judge is shown: what was done, and what each action produced.
pub(crate) fn trace_of(output: &AgentOutput) -> String {
    if output.turns.is_empty() {
        return "(nothing)".to_owned();
    }
    output
        .turns
        .iter()
        .map(|turn| {
            let calls = turn
                .tool_calls
                .iter()
                .map(|call| format!("{}({})", call.name, call.arguments))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "{}. {calls} -> {}",
                turn.index + 1,
                turn.output_text.as_deref().unwrap_or("(no observation)")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]

    use super::*;

    fn summary(kind: ActionKind, target: Option<&str>, goal: Option<&str>) -> ActionSummary {
        ActionSummary {
            kind,
            target: target.map(ToOwned::to_owned),
            goal: goal.map(ToOwned::to_owned),
            text: None,
        }
    }

    /// The action → tool-call mapping is what every assertion in the suite is
    /// written against, so the names and argument keys are a contract.
    #[test]
    fn an_action_becomes_a_tool_call_with_its_arguments() {
        let (name, arguments) = describe(&summary(
            ActionKind::Browse,
            Some("https://example.com"),
            Some("read the heading"),
        ));
        assert_eq!(name, "browse");
        assert_eq!(arguments["url"], json!("https://example.com"));
        assert_eq!(arguments["goal"], json!("read the heading"));

        let (name, arguments) = describe(&summary(
            ActionKind::App,
            Some("TextEdit"),
            Some("turn on bold"),
        ));
        assert_eq!(name, "app");
        assert_eq!(arguments["app"], json!("TextEdit"));
    }

    /// A turn's steps are rebuilt from events now, and the observation has to
    /// replace the thought once the action resolves — the judge and the
    /// `ExpectToolArg` assertions both read that text.
    #[test]
    fn an_observation_supersedes_the_thought_it_followed() {
        let run = RunId::new();
        let mut collected = Collected::default();
        assert!(!absorb(
            &mut collected,
            run,
            AppEvent::TurnStep {
                run,
                step: 1,
                thought: "open the page".to_owned(),
                action: summary(
                    ActionKind::Browse,
                    Some("https://example.com"),
                    Some("read it")
                ),
            }
        ));
        assert!(!absorb(
            &mut collected,
            run,
            AppEvent::TurnStepDone {
                run,
                step: 1,
                observation: "the page shows $29 per seat".to_owned(),
                duration_ms: 1_200,
            }
        ));
        // Another run's turn ending must not end this one's collection.
        assert!(!absorb(
            &mut collected,
            run,
            AppEvent::TurnFailed {
                run: RunId::new(),
                error: "someone else's turn".to_owned(),
                code: "agent_graph".to_owned(),
            }
        ));
        assert!(absorb(
            &mut collected,
            run,
            AppEvent::TurnFinished {
                run,
                text: "$29".to_owned(),
                steps: 1,
                exhausted: false,
                usage: Some(TurnUsage {
                    input_tokens: 10,
                    output_tokens: 5,
                    ..TurnUsage::default()
                }),
            }
        ));

        let step = collected.steps.first().expect("one step");
        assert_eq!(
            step.observation.as_deref(),
            Some("the page shows $29 per seat")
        );
        assert_eq!(step.duration, Duration::from_millis(1_200));
        assert_eq!(collected.steps.len(), 1);
        let usage = usage_of(&collected.usage.expect("the turn reported usage"));
        assert_eq!(usage.total_tokens, Some(15));
        // Plan-backed work has no price to report (05 §7).
        assert_eq!(usage.cost_usd, None);
    }

    /// The judge is shown what happened, not just what was claimed — a model
    /// that answers "done" with an empty trace has to be scoreable as wrong.
    #[test]
    fn the_judge_sees_the_trace_and_not_only_the_answer() {
        let output = AgentOutput {
            final_text: "I set the cell.".to_owned(),
            turns: vec![Turn {
                index: 0,
                output_text: Some("Blocked after 1 action(s): the grid never moved".to_owned()),
                tool_calls: vec![ToolCall {
                    id: "step-0".to_owned(),
                    name: "app".to_owned(),
                    arguments: json!({ "app": "LibreOffice", "goal": "type 42 into A1" }),
                }],
                tool_results: vec![],
                stop_reason: None,
                duration: Duration::ZERO,
            }],
            ..Default::default()
        };
        let trace = trace_of(&output);
        assert!(
            trace.contains("Blocked"),
            "the failure must reach the judge"
        );
        assert!(trace.contains("LibreOffice"));
    }

    #[test]
    fn the_tool_allowlist_contains_no_shell_or_file_action() {
        // Reads [`ACTIONS`] rather than standing up a `Runtime`: the list is
        // static, `available_tools` ignores `self` to produce it, and opening
        // a real runtime here made a test about a constant depend on the
        // store actor and the login keychain — which is how it failed once
        // under a loaded workspace run and never again.
        for forbidden in ["bash", "shell", "run", "read_file", "write_file", "exec"] {
            assert!(
                !ACTIONS.contains(&forbidden),
                "`{forbidden}` must not be an action (P3)"
            );
        }
    }

    /// Source of truth for the tool half of [`ACTIONS`]:
    /// `crates/neo-agent/src/agent/metal.rs` `registry()`, which registers
    /// `Browse` ("browse"), `App` ("app") and `Inspect` ("ax") — nothing
    /// else. It cannot be read from here: `registry` and the three tool
    /// structs are private, and widening neo-agent's API for a test would be
    /// a worse trade than this assertion.
    ///
    /// The loop half is cross-checked for real: every [`ActionKind`] a
    /// production step can carry is run through [`describe`], because that
    /// name is what lands in the judge's trace, and a name in the trace that
    /// the judge was never told about reads as an off-surface action.
    ///
    /// This drifted once: `ask` was advertised for a tool that is not
    /// registered (it arrives with the card path), while `ax` — which is
    /// registered, and which no `ActionKind` names, so nothing else here
    /// mentions it — was missing.
    #[test]
    fn every_action_the_judge_is_told_about_is_one_the_agent_can_take() {
        for kind in [ActionKind::Browse, ActionKind::App, ActionKind::Answer] {
            let (name, _) = describe(&summary(kind, Some("Numbers"), Some("set A1 to 42")));
            assert!(
                ACTIONS.contains(&name.as_str()),
                "`{name}` reaches the trace but is not advertised"
            );
        }
        assert!(
            ACTIONS.contains(&"ax"),
            "`ax` is a registered tool (metal.rs registry) the judge must know about"
        );
        assert!(
            !ACTIONS.contains(&"ask"),
            "`ask` is not a registered tool yet; advertising it invites the \
             judge to mark a run down for not using something it cannot call"
        );
    }
}
