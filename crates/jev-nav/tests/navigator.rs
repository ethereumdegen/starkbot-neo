#![allow(clippy::expect_used)]
//! The navigator loop over the `Observer` seam. No browser and no vendor: the
//! surface is a scripted fake observer, Jev is a `wiremock` server whose answers
//! this test writes.

use std::collections::VecDeque;
use std::sync::Mutex;

use jev_nav::gate::{ConfirmReason, NeedsUser};
use jev_nav::policy::Action;
use jev_nav::wire::TypeSafe;
use jev_nav::{
    Approval, BlockReason, Navigator, ObserveError, Observer, Outcome, RunConfig, StepEvent,
};
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
    ) -> Result<Option<std::borrow::Cow<'static, str>>, ObserveError> {
        let fresh = self.freshness.pop_front().unwrap_or(true);
        Ok((!fresh).then(|| "the scripted surface moved".into()))
    }

    async fn act(
        &mut self,
        action: &Action,
        observation: &Value,
        text: Option<&str>,
    ) -> Result<(), ObserveError> {
        // Same contract as `CdpObserver::act`: a stale surface executes nothing.
        if let Some(reason) = Observer::fresh(self, observation, Some(action)).await? {
            return Err(ObserveError::Stale(reason));
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

/// Jev's answer with all four safety heads attached.
fn guarded_answer(operation: &str, risk: f64) -> Value {
    let mut body = answer(operation);
    for head in ["outward", "destructive", "spends", "on_task"] {
        let value = if head == "on_task" { 0.95 } else { risk };
        body["answers"][head] = json!({ "type": "noul", "noul": value });
    }
    body
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
        denied_origins: Vec::new(),
        on_task_floor: 0.0,
    }
}

/// The same run with the safety heads on, which is every real run.
fn guarded_config() -> RunConfig {
    RunConfig {
        safety_heads: true,
        ..config()
    }
}

/// Drive one run to whatever it returns, retaining its events and wire requests.
async fn run_with(
    config: &RunConfig,
    observer: FakeObserver,
    answers: Vec<Value>,
) -> (
    Result<Outcome, jev_nav::NavError>,
    Navigator<FakeObserver>,
    Vec<StepEvent>,
    Vec<Value>,
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

async fn drive_guarded(
    observer: FakeObserver,
    answers: Vec<Value>,
) -> (Result<Outcome, jev_nav::NavError>, Navigator<FakeObserver>) {
    let (outcome, navigator, _, _) = run_with(&guarded_config(), observer, answers).await;
    (outcome, navigator)
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
        Outcome::Blocked {
            reason: BlockReason::NoProgress
        }
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
        Outcome::Blocked {
            reason: BlockReason::Unstable {
                reason: "the scripted surface moved".to_owned()
            }
        }
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

/// The P0 this change exists for: a response whose safety heads are missing
/// used to score every head 0.0 — "not risky" — and the mutation executed
/// unreviewed. A run that asked for the heads and did not get them now fails
/// the step, and above all executes nothing.
#[tokio::test]
async fn a_response_without_the_safety_heads_executes_nothing() {
    let observer = FakeObserver::new(vec![page("a checkout page")]);

    let (outcome, navigator) = drive_guarded(observer, vec![answer("CLICK")]).await;

    assert!(
        matches!(outcome, Err(jev_nav::NavError::Wire(_))),
        "an unanswered safety head is a wire error, not a decision"
    );
    assert!(navigator.observer.acted.is_empty());
    assert!(navigator.history().is_empty());
}

/// The same for a head that answers in some other shape — the failure mode
/// that would have silently disarmed every gate if TypeSafe's yes/no wire
/// format had differed from the `noul` field the client reads.
#[tokio::test]
async fn a_mis_shaped_safety_head_executes_nothing() {
    let observer = FakeObserver::new(vec![page("a checkout page")]);
    let mut mis_shaped = guarded_answer("CLICK", 0.01);
    mis_shaped["answers"]["spends"] = json!({ "type": "noul", "noul": "no" });

    let (outcome, navigator) = drive_guarded(observer, vec![mis_shaped]).await;

    assert!(matches!(outcome, Err(jev_nav::NavError::Wire(_))));
    assert!(navigator.observer.acted.is_empty());
}

/// And the gate still lets an answered, unrisky step through, so failing
/// closed did not turn every run into a refusal.
#[tokio::test]
async fn answered_and_unrisky_still_executes() {
    let observer = FakeObserver::new(vec![page("first screen"), page("second screen")]);

    let (outcome, navigator) = drive_guarded(
        observer,
        vec![guarded_answer("CLICK", 0.02), guarded_answer("DONE", 0.02)],
    )
    .await;

    assert_eq!(outcome.expect("the scripted run completes"), Outcome::Done);
    assert_eq!(navigator.observer.acted.len(), 1);
}

/// The pause that replaced the dead end: a head over the threshold stops
/// *before* acting and hands back an escalation the caller can put on a card,
/// plus a token to carry the run on with.
#[tokio::test]
async fn a_risky_step_pauses_with_an_escalation_instead_of_ending() {
    let observer = FakeObserver::new(vec![page("a checkout page")]);

    let (outcome, navigator) = drive_guarded(observer, vec![guarded_answer("CLICK", 0.93)]).await;

    let outcome = outcome.expect("the scripted run completes");
    assert!(outcome.is_resumable());
    let Outcome::NeedsConfirm { escalation, .. } = outcome else {
        panic!("a risky step must pause, not end: {outcome:?}");
    };
    assert_eq!(escalation.label.as_deref(), Some("Continue"));
    assert_eq!(escalation.url, "https://fixture.test/form");
    assert!(matches!(
        escalation.reason,
        Some(ConfirmReason::SafetyHead { .. })
    ));
    assert!(navigator.observer.acted.is_empty(), "nothing executed");
}

/// Approving the paused action executes exactly it, and the run carries on
/// from a fresh observation rather than replaying anything.
#[tokio::test]
async fn approving_a_confirm_executes_that_action_and_finishes() {
    let server = MockServer::start().await;
    let jev = jev(
        &server,
        vec![
            guarded_answer("CLICK", 0.93),
            guarded_answer("CLICK", 0.93),
            guarded_answer("DONE", 0.02),
        ],
    )
    .await;
    let observer = FakeObserver::new(vec![page("a checkout page"), page("thanks")]);
    let mut navigator = Navigator::new(observer, jev, None);

    let paused = navigator
        .run(&guarded_config(), |_| {})
        .await
        .expect("the scripted run pauses");
    let Outcome::NeedsConfirm { resume, .. } = paused else {
        panic!("expected a confirm");
    };
    let outcome = navigator
        .resume(&guarded_config(), resume, Approval::Approve, |_| {})
        .await
        .expect("the resumed run completes");

    assert_eq!(outcome, Outcome::Done);
    assert_eq!(
        navigator.observer.acted,
        vec![json!({ "label": "Continue", "text": Value::Null })],
        "the approved action ran once, and only it"
    );
}

/// Denying it withdraws that action from the run: the next decision cannot
/// offer it again, so a refusal is not re-asked on every observation.
#[tokio::test]
async fn denying_a_confirm_withdraws_the_action_and_records_why() {
    let server = MockServer::start().await;
    let jev = jev(
        &server,
        vec![
            guarded_answer("CLICK", 0.93),
            guarded_answer_over("DONE", 0.02, &["DONE", "BLOCKED"]),
        ],
    )
    .await;
    let observer = FakeObserver::new(vec![page("a checkout page")]);
    let mut navigator = Navigator::new(observer, jev, None);

    let paused = navigator
        .run(&guarded_config(), |_| {})
        .await
        .expect("the scripted run pauses");
    let Outcome::NeedsConfirm { resume, .. } = paused else {
        panic!("expected a confirm");
    };
    let outcome = navigator
        .resume(
            &guarded_config(),
            resume,
            Approval::Deny {
                note: "don't pay for anything".into(),
            },
            |_| {},
        )
        .await
        .expect("the resumed run completes");

    assert_eq!(outcome, Outcome::Done);
    assert!(navigator.observer.acted.is_empty());
    let refusal = navigator
        .history()
        .iter()
        .find(|entry| entry["kind"] == "refused")
        .expect("the refusal is in the history the next decision reads");
    assert_eq!(refusal["text"], json!("don't pay for anything"));
}

/// A visible password field is not something to ask a classifier about: the
/// page is handed over, and the run picks up after the user signs in.
#[tokio::test]
async fn a_login_wall_is_handed_to_the_user_and_resumes() {
    let mut login = page("sign in to continue");
    login["signals"] = json!({ "password_fields": 1, "captcha": 0 });
    let server = MockServer::start().await;
    let jev = jev(&server, vec![guarded_answer("DONE", 0.02)]).await;
    let observer = FakeObserver::new(vec![login, page("signed in")]);
    let mut navigator = Navigator::new(observer, jev, None);

    let handed = navigator
        .run(&guarded_config(), |_| {})
        .await
        .expect("the run reaches the login wall");
    let Outcome::NeedsUser { reason, resume, .. } = handed else {
        panic!("expected a hand-over, got {handed:?}");
    };
    assert_eq!(reason, NeedsUser::SignIn);

    let outcome = navigator
        .resume(&guarded_config(), resume, Approval::Ready, |_| {})
        .await
        .expect("the resumed run completes");

    assert_eq!(outcome, Outcome::Done, "no Jev call was spent on the wall");
}

/// A challenge is never clicked at.
#[tokio::test]
async fn a_captcha_is_handed_over_without_asking_jev() {
    let mut challenge = page("prove you are human");
    challenge["signals"] = json!({ "password_fields": 0, "captcha": 1 });
    let server = MockServer::start().await;
    let jev = jev(&server, Vec::new()).await;
    let observer = FakeObserver::new(vec![challenge]);
    let mut navigator = Navigator::new(observer, jev, None);

    let handed = navigator
        .run(&guarded_config(), |_| {})
        .await
        .expect("the run reaches the challenge");

    assert!(matches!(
        handed,
        Outcome::NeedsUser {
            reason: NeedsUser::Captcha,
            ..
        }
    ));
    assert!(
        server
            .received_requests()
            .await
            .expect("recording is on")
            .is_empty(),
        "a challenge costs no classifier call"
    );
}

/// A denied host is refused outright — not confirmed, because no approval
/// makes it allowed.
#[tokio::test]
async fn a_click_into_a_denied_host_is_refused() {
    let mut tracker = page("an article");
    tracker["actions"] = json!([
        { "kind": "click", "node": 1, "id": "click:1", "label": "Continue",
          "role": "link", "href": "https://pixel.ads.example/track" }
    ]);
    let server = MockServer::start().await;
    let jev = jev(&server, vec![guarded_answer("CLICK", 0.02)]).await;
    let observer = FakeObserver::new(vec![tracker]);
    let mut navigator = Navigator::new(observer, jev, None);
    let config = RunConfig {
        denied_origins: vec!["ads.example".into()],
        ..guarded_config()
    };

    let outcome = navigator
        .run(&config, |_| {})
        .await
        .expect("the scripted run completes");

    assert_eq!(
        outcome,
        Outcome::Blocked {
            reason: BlockReason::DeniedOrigin {
                host: "pixel.ads.example".into()
            }
        }
    );
    assert!(navigator.observer.acted.is_empty());
}

/// A field whose value the goal does not carry becomes a question, not a
/// failed run: the helper says `{"text": null}`, the navigator asks, and the
/// answer is typed on resume.
#[tokio::test]
async fn an_unknown_field_value_asks_the_user_and_types_the_answer() {
    let mut form = page("a form");
    form["actions"] = json!([
        { "kind": "fill", "node": 1, "id": "e1", "label": "Invoice number",
          "role": "textbox", "value": "" }
    ]);
    let server = MockServer::start().await;
    let jev = jev(
        &server,
        vec![
            guarded_answer_over("TYPE_TEXT", 0.02, &["TYPE_TEXT", "DONE", "BLOCKED"]),
            guarded_answer_over("TYPE_TEXT", 0.02, &["TYPE_TEXT", "DONE", "BLOCKED"]),
            guarded_answer_over("DONE", 0.02, &["TYPE_TEXT", "DONE", "BLOCKED"]),
        ],
    )
    .await;
    // One observation, repeated: the field is still there when the user's
    // answer comes back, which is the case the resume has to handle.
    let observer = FakeObserver::new(vec![form]);
    let mut navigator = Navigator::new(observer, jev, Some(Box::new(UnknownHelper)));

    let asked = navigator
        .run(&guarded_config(), |_| {})
        .await
        .expect("the run reaches the field");
    let Outcome::NeedsUser { reason, resume, .. } = asked else {
        panic!("expected a question, got {asked:?}");
    };
    assert_eq!(
        reason,
        NeedsUser::Value {
            field: "Invoice number".into()
        }
    );
    assert!(navigator.observer.acted.is_empty());

    let outcome = navigator
        .resume(
            &guarded_config(),
            resume,
            Approval::Value("INV-4417".into()),
            |_| {},
        )
        .await
        .expect("the resumed run completes");

    assert_eq!(outcome, Outcome::Done);
    assert_eq!(
        navigator.observer.acted,
        vec![json!({ "label": "Invoice number", "text": "INV-4417" })],
        "the user's own value was typed, and the helper was not asked again"
    );
}

/// A helper that always answers "the goal does not say".
struct UnknownHelper;

#[async_trait::async_trait]
impl jev_nav::text::TextHelper for UnknownHelper {
    async fn value(
        &self,
        _context: &Value,
    ) -> Result<jev_nav::text::TextValue, jev_nav::text::TextError> {
        Err(jev_nav::text::TextError::Unknown)
    }
}

/// A guarded answer over exactly the operations the page offers.
///
/// `choice` validation checks the probability keys against the offered ids, so
/// a scripted answer that names an operation this page does not offer is a
/// wire error — which is the check working, not a test fixture quirk.
fn guarded_answer_over(operation: &str, risk: f64, offered: &[&str]) -> Value {
    // Probabilities must sum to 1 within 0.02 or the client rejects the
    // answer, so the spread is computed from however many were offered.
    let mut probabilities = json!({});
    let rest = 0.1 / (offered.len().max(2) - 1) as f64;
    for id in offered {
        probabilities[*id] = json!(if *id == operation { 0.9 } else { rest });
    }
    let mut body = json!({
        "model": "jev-test",
        "answers": { "operation": {
            "choice": operation, "probabilities": probabilities, "confidence": 0.9,
        } },
        "usage": { "input_tokens": 11 },
    });
    for head in ["outward", "destructive", "spends", "on_task"] {
        let value = if head == "on_task" { 0.95 } else { risk };
        body["answers"][head] = json!({ "type": "noul", "noul": value });
    }
    if !matches!(operation, "DONE" | "BLOCKED") {
        body["answers"][format!("{}_target", operation.to_lowercase())] =
            json!({ "choice": "1", "probabilities": { "1": 1.0 }, "confidence": 0.9 });
    }
    body
}
