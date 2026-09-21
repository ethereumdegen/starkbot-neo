#![allow(clippy::expect_used)]
//! The navigator loop over the `Observer` seam. No browser and no vendor: the
//! surface is a scripted fake observer, Jev is a `wiremock` server whose answers
//! this test writes.

use std::collections::VecDeque;
use std::sync::Mutex;

use jev_nav::policy::Action;
use jev_nav::wire::{TypeSafe, WireError};
use jev_nav::{NavError, Navigator, ObserveError, Observer, Outcome, RunConfig, StepEvent};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// A surface that answers from a script and remembers what it was told to do.
struct FakeObserver {
    /// Observations in order; the last one repeats once the script runs out.
    script: Vec<Value>,
    observed: usize,
    /// One answer per `fresh` call, oldest first; exhausted means "still fresh".
    freshness: VecDeque<bool>,
    /// `{ label, text }` per executed action.
    acted: Vec<Value>,
}

impl FakeObserver {
    fn new(script: Vec<Value>) -> Self {
        Self {
            script,
            observed: 0,
            freshness: VecDeque::new(),
            acted: Vec::new(),
        }
    }

    /// Script the next `fresh` answers; anything after them is fresh.
    fn freshness(mut self, answers: impl IntoIterator<Item = bool>) -> Self {
        self.freshness = answers.into_iter().collect();
        self
    }
}

#[async_trait::async_trait]
impl Observer for FakeObserver {
    async fn observe(&mut self) -> Result<Value, ObserveError> {
        let index = self.observed.min(self.script.len() - 1);
        self.observed += 1;
        Ok(self.script[index].clone())
    }

    async fn fresh(
        &mut self,
        _observation: &Value,
        _action: Option<&Action>,
    ) -> Result<bool, ObserveError> {
        Ok(self.freshness.pop_front().unwrap_or(true))
    }

    async fn act(
        &mut self,
        action: &Action,
        observation: &Value,
        text: Option<&str>,
    ) -> Result<(), ObserveError> {
        // Same contract as `CdpObserver::act`: a stale surface executes nothing.
        if !Observer::fresh(self, observation, Some(action)).await? {
            return Err(ObserveError::Stale("target changed since this decision"));
        }
        self.acted
            .push(json!({ "label": action.get("label"), "text": text }));
        Ok(())
    }
}

/// Jev's answers in order; a step the test did not script is a 500, which the
/// navigator reports as a wire error rather than silently ending the run.
struct Script(Mutex<VecDeque<Value>>);

impl Respond for Script {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let answer = self
            .0
            .lock()
            .ok()
            .and_then(|mut answers| answers.pop_front());
        match answer {
            Some(body) => ResponseTemplate::new(200).set_body_json(body),
            None => ResponseTemplate::new(500),
        }
    }
}

/// One page offering exactly one click target, so the offered operations are
/// `CLICK`, `DONE`, `BLOCKED` and the only click target is `1`.
fn page(text: &str) -> Value {
    json!({
        "url": "https://fixture.test/form",
        "title": "Fixture",
        "text": text,
        "page_key": "page-1",
        "marker": ["m", []],
        "guards": { "1": "guard-1" },
        "scroll": { "y": 0 },
        "actions": [
            { "kind": "click", "node": 1, "id": "click:1", "label": "Continue", "role": "button" }
        ],
    })
}

fn answer(operation: &str) -> Value {
    let weight = |name: &str| if name == operation { 0.9 } else { 0.05 };
    let mut answers = json!({
        "operation": {
            "choice": operation,
            "probabilities": {
                "CLICK": weight("CLICK"), "DONE": weight("DONE"), "BLOCKED": weight("BLOCKED"),
            },
            "confidence": 0.9,
        }
    });
    if operation == "CLICK" {
        answers["click_target"] =
            json!({ "choice": "1", "probabilities": { "1": 1.0 }, "confidence": 0.88 });
    }
    json!({ "model": "jev-test", "answers": answers, "usage": { "input_tokens": 11 } })
}

/// The same answer with yes/no heads attached. A head the caller does not
/// name is absent from the response, which is what a truncated or partially
/// parsed provider answer looks like.
fn with_heads(mut answer: Value, heads: &[(&str, f64)]) -> Value {
    for (name, probability) in heads {
        answer["answers"][*name] = json!({ "noul": probability });
    }
    answer
}

async fn jev(server: &MockServer, answers: Vec<Value>) -> TypeSafe {
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(Script(Mutex::new(answers.into())))
        .mount(server)
        .await;
    TypeSafe::new(
        "ts-test-not-a-real-key",
        format!("{}/v1/systemone", server.uri()),
        "jev-test",
    )
}

fn config() -> RunConfig {
    RunConfig {
        goal: "continue past the first screen".into(),
        safety_heads: false,
        confirm_at: 0.4,
        on_task_floor: 0.0,
    }
}

/// Drive one run to whatever it returns — including a wire error, which is
/// what a safety head that came back unreadable now produces.
async fn run_with(
    config: &RunConfig,
    observer: FakeObserver,
    answers: Vec<Value>,
) -> (
    Result<Outcome, NavError>,
    Navigator<FakeObserver>,
    Vec<StepEvent>,
    Vec<Value>, // the Jev request bodies, in order
) {
    let server = MockServer::start().await;
    let jev = jev(&server, answers).await;
    let mut navigator = Navigator::new(observer, jev, None);
    let mut steps = Vec::new();
    let outcome = navigator.run(config, |step| steps.push(step.clone())).await;
    let requests = server
        .received_requests()
        .await
        .expect("recording is on")
        .iter()
        .map(|request| request.body_json().unwrap_or(Value::Null))
        .collect();
    (outcome, navigator, steps, requests)
}

async fn drive(
    observer: FakeObserver,
    answers: Vec<Value>,
) -> (
    Outcome,
    Navigator<FakeObserver>,
    Vec<StepEvent>,
    usize, // Jev requests
) {
    let (outcome, navigator, steps, requests) = run_with(&config(), observer, answers).await;
    (
        outcome.expect("the scripted run completes"),
        navigator,
        steps,
        requests.len(),
    )
}

#[tokio::test]
async fn the_loop_executes_the_chosen_action_and_finishes_on_done() {
    let observer = FakeObserver::new(vec![page("first screen"), page("second screen")]);

    let (outcome, navigator, steps, requests) =
        drive(observer, vec![answer("CLICK"), answer("DONE")]).await;

    assert_eq!(outcome, Outcome::Done);
    assert_eq!(requests, 2);
    assert_eq!(
        navigator.observer.acted,
        vec![json!({ "label": "Continue", "text": Value::Null })]
    );
    assert_eq!(navigator.history().len(), 1);
    let recorded = &navigator.history()[0];
    assert_eq!(recorded["action"], json!("Continue"));
    assert_eq!(recorded["kind"], json!("click"));
    // The second observation differs, so the step is recorded as having changed the page.
    assert_eq!(recorded["page_changed"], json!(true));
    let operations: Vec<&str> = steps
        .iter()
        .map(|s| s.decision.operation.as_str())
        .collect();
    assert_eq!(operations, ["CLICK", "DONE"]);
    assert!(steps.iter().all(|step| !step.stale));
}

#[tokio::test]
async fn a_stale_target_re_observes_instead_of_recording_an_action() {
    // The first `act` finds the target moved; the second decision executes.
    let observer = FakeObserver::new(vec![
        page("first screen"),
        page("re-observed"),
        page("after the click"),
    ])
    .freshness([false]);

    let (outcome, navigator, steps, requests) = drive(
        observer,
        vec![answer("CLICK"), answer("CLICK"), answer("DONE")],
    )
    .await;

    assert_eq!(outcome, Outcome::Done);
    // Decide → stale → observe again → decide → act → observe → decide DONE.
    assert_eq!(requests, 3);
    assert_eq!(navigator.observer.observed, 3);
    // The stale attempt executed nothing and left no history behind.
    assert_eq!(navigator.observer.acted.len(), 1);
    assert_eq!(navigator.history().len(), 1);
    assert_eq!(
        steps.iter().filter(|step| step.stale).count(),
        1,
        "the stale step is reported to the caller"
    );
}

#[tokio::test]
async fn done_on_a_stale_page_re_observes_before_finishing() {
    // Jev says DONE, but the page it judged is gone: the answer must not stand.
    let observer =
        FakeObserver::new(vec![page("first screen"), page("second screen")]).freshness([false]);

    let (outcome, navigator, steps, requests) =
        drive(observer, vec![answer("DONE"), answer("DONE")]).await;

    assert_eq!(outcome, Outcome::Done);
    assert_eq!(requests, 2, "DONE was re-decided on the fresh observation");
    assert_eq!(navigator.observer.observed, 2);
    assert!(navigator.observer.acted.is_empty());
    assert!(steps[0].stale);
    assert!(!steps[1].stale);
}

#[tokio::test]
async fn three_actions_that_change_nothing_end_the_run_as_blocked() {
    // One observation, repeated: every action leaves the same page behind.
    let observer = FakeObserver::new(vec![page("unchanging screen")]);

    let (outcome, navigator, steps, _requests) = drive(
        observer,
        vec![answer("CLICK"), answer("CLICK"), answer("CLICK")],
    )
    .await;

    assert_eq!(
        outcome,
        Outcome::Blocked("three actions in a row changed nothing".into())
    );
    assert_eq!(navigator.observer.acted.len(), 3);
    assert_eq!(navigator.history().len(), 3);
    assert!(
        navigator
            .history()
            .iter()
            .all(|entry| entry["page_changed"] == json!(false))
    );
    assert_eq!(steps.len(), 3);
}

/// A surface that is never fresh must not spend the decision budget asking Jev
/// the same question: five stale decisions in a row end the run.
///
/// This is the LibreOffice failure that motivated the cap — every guard came
/// back stale, nothing executed, and one run burned 120 Jev requests.
#[tokio::test]
async fn a_surface_that_is_never_fresh_stops_the_run() {
    let observer = FakeObserver::new(vec![page("a window that will not hold still")]).freshness(
        std::iter::repeat_n(false, jev_nav::rules::MAX_CONSECUTIVE_STALE * 2),
    );
    let answers = std::iter::repeat_with(|| answer("CLICK"))
        .take(jev_nav::rules::MAX_CONSECUTIVE_STALE + 2)
        .collect();

    let (outcome, navigator, steps, requests) = drive(observer, answers).await;

    assert_eq!(
        outcome,
        Outcome::Blocked(
            "the surface changed under every decision; nothing could be executed".into()
        )
    );
    assert_eq!(steps.len(), jev_nav::rules::MAX_CONSECUTIVE_STALE);
    assert_eq!(requests, jev_nav::rules::MAX_CONSECUTIVE_STALE);
    assert!(steps.iter().all(|step| step.stale));
    assert!(
        navigator.observer.acted.is_empty(),
        "nothing may execute on a stale surface"
    );
    assert!(navigator.history().is_empty());
}

/// A risky operation stops for confirmation instead of executing.
#[tokio::test]
async fn a_safety_head_over_the_threshold_blocks_the_action() {
    let observer = FakeObserver::new(vec![page("a composed message")]);
    let answers = vec![with_heads(
        answer("CLICK"),
        &[("outward", 0.9), ("destructive", 0.02), ("spends", 0.01)],
    )];

    let (outcome, navigator, steps, _requests) = run_with(
        &RunConfig {
            safety_heads: true,
            ..config()
        },
        observer,
        answers,
    )
    .await;

    assert_eq!(
        outcome.expect("the scripted run completes"),
        Outcome::Blocked("needs confirmation before `Continue`".into())
    );
    assert!(navigator.observer.acted.is_empty());
    assert_eq!(steps.len(), 1);
}

/// The same answer with `outward` omitted entirely — a truncated or partially
/// parsed provider response — must not send the message.
///
/// This is R1.1: the head used to be absent from `Decision::safety`, the loop
/// read absent as `0.0`, and the send executed unconfirmed and unevented.
#[tokio::test]
async fn a_missing_safety_head_fails_the_step_instead_of_executing_it() {
    let observer = FakeObserver::new(vec![page("a composed message")]);
    let answers = vec![with_heads(
        answer("CLICK"),
        &[("destructive", 0.02), ("spends", 0.01)],
    )];

    let (outcome, navigator, steps, _requests) = run_with(
        &RunConfig {
            safety_heads: true,
            ..config()
        },
        observer,
        answers,
    )
    .await;

    assert!(
        matches!(&outcome, Err(NavError::Wire(WireError::Invalid(head))) if head == "outward"),
        "an unreadable head fails the step, got {outcome:?}"
    );
    assert!(
        navigator.observer.acted.is_empty(),
        "a head that came back unreadable must not execute the action"
    );
    assert!(navigator.history().is_empty());
    assert!(steps.is_empty());
}

/// `on_task` is a floor: low confidence that the page still serves the goal
/// stops the run (R1.2).
#[tokio::test]
async fn a_run_that_drifts_off_the_goal_stops_before_acting() {
    let observer = FakeObserver::new(vec![page("an unrelated page")]);
    let answers = vec![with_heads(answer("CLICK"), &[("on_task", 0.1)])];

    let (outcome, navigator, _steps, _requests) = run_with(
        &RunConfig {
            on_task_floor: 0.3,
            ..config()
        },
        observer,
        answers,
    )
    .await;

    assert_eq!(
        outcome.expect("the scripted run completes"),
        Outcome::Blocked("the page drifted off the goal; stopped before `Continue`".into())
    );
    assert!(navigator.observer.acted.is_empty());
}

/// The same drift with the floor at zero: the check is off, and the head is
/// not even asked for.
#[tokio::test]
async fn a_zero_on_task_floor_lets_the_same_run_proceed() {
    let observer = FakeObserver::new(vec![page("an unrelated page"), page("the next page")]);
    let answers = vec![
        with_heads(answer("CLICK"), &[("on_task", 0.1)]),
        answer("DONE"),
    ];

    let (outcome, navigator, _steps, requests) = run_with(&config(), observer, answers).await;

    assert_eq!(outcome.expect("the scripted run completes"), Outcome::Done);
    assert_eq!(navigator.observer.acted.len(), 1);
    assert!(
        requests
            .iter()
            .all(|request| request["questions"]["on_task"].is_null()),
        "a floor of zero must not pay for the head, got {requests:?}"
    );
}
