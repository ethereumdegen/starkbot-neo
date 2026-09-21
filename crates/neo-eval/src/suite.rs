//! Running the suite as a library call, so something other than a terminal can
//! start it and watch it.
//!
//! The CLI used to own all of this: it filtered the cases, gated on `doctor`,
//! printed a plan, handed the runner `console_output: true` and then threw the
//! [`SuiteReport`] away in favour of an exit code. An exit code is not a
//! surface a window can render, so the run lives here now and the report comes
//! back to whoever asked for it. `neo eval` is one caller of this function,
//! not its owner.
//!
//! # One case at a time, forever
//!
//! [`RunnerConfig::concurrency`] is pinned to `1` and must stay there. These
//! cases drive *the machine*: they type on the one keyboard, they raise
//! windows to the one frontmost position, and a probe reads whatever is on
//! screen when it runs. Two cases in parallel type into each other's windows
//! and then assert about it. The same rule is why a front end must not offer
//! two concurrent eval runs — [`run_suite`] has no lock of its own, and a
//! second call while one is in flight produces two agents fighting over the
//! keyboard, not two results.
//!
//! # Progress
//!
//! `spice`'s runner has no per-case hook, so the suite is driven one case per
//! [`Runner::run`] and the per-case reports are merged at the end. That is
//! also what makes [`AppEvent::EvalCase`] and the cancellation check between
//! cases possible: a front end sees every case start and settle, and a stop
//! button takes effect at the next case boundary (and, inside a case, through
//! the [`CancellationToken`] the agent threads into `Runtime::chat`).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use neo_agent::Runtime;
use neo_core::{AppEvent, EvalCaseState, RunId};
use serde::{Deserialize, Serialize};
use spice_framework::agent::AgentUnderTest;
use spice_framework::judge::Judge;
use spice_framework::report::{SuiteReport, TestReport};
use spice_framework::runner::{Runner, RunnerConfig};
use tokio_util::sync::CancellationToken;

use crate::apps::{self, App};
use crate::cases::{self, Case};
use crate::{NeoAgent, NeoJudge};

/// One case may drive an app through several navigator steps, each a Jev round
/// trip; three minutes is generous but finite.
pub const CASE_TIMEOUT: Duration = Duration::from_secs(180);

/// Why a suite run could not produce a report.
///
/// A case that *fails* is not an error — it is the measurement, and it comes
/// back inside the [`SuiteReport`]. These are the states where there is no
/// measurement to return.
#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    /// Without an inference connection every case would be reported as a model
    /// failure, which is a lie about the agent.
    #[error("the eval needs a working inference connection: {detail} ({fix})")]
    NoInference { detail: String, fix: String },
    /// Nothing matched, or everything that matched needs an app this machine
    /// does not have.
    #[error("no runnable cases matched — try `neo eval --list`")]
    NoCases,
    /// Stopped on request. Distinguishable from a failing suite on purpose: a
    /// front end says "stopped", not "broken", and reports how far it got.
    #[error("the eval was cancelled after {completed} of {total} case(s)")]
    Cancelled { completed: usize, total: usize },
    /// Another run already has the keyboard. The suite drives real apps, so
    /// it cannot share the screen with anything, including a run started from
    /// another window.
    #[error(transparent)]
    ScreenBusy(#[from] neo_agent::screen::ScreenBusy),
    /// The store or the doctor could not be reached at all.
    #[error(transparent)]
    Runtime(#[from] neo_agent::RuntimeError),
}

/// Which cases to run, and how many times each.
///
/// The same rule serves a run and a preview: a front end filters
/// [`list_cases`] with [`Selection::selects`] to show what *would* run, and
/// [`run_suite`] applies the identical predicate, so the preview cannot
/// disagree with the run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    /// Substring of a case id or name. Matches `spice`'s own filter semantics,
    /// which this replaces — filtering happens before the runner now, so the
    /// count a front end shows is the count that runs.
    pub filter: Option<String>,
    /// Any-of over case tags (`browser`, `spreadsheet`, `known-gap`, …).
    pub tags: Vec<String>,
    /// One run per case instead of the [`cases::CONSENSUS_RUNS`] consensus.
    /// A quick look, never evidence that a case passes.
    pub once: bool,
}

impl Selection {
    /// Whether a case is in this selection, ignoring whether its app exists.
    #[must_use]
    pub fn selects(&self, case: &CaseListing) -> bool {
        self.matches(&case.id, case.name.as_deref(), &case.tags)
    }

    /// The selected cases, with their run count already adjusted for `once`.
    #[must_use]
    pub fn apply(&self, cases: Vec<Case>) -> Vec<Case> {
        cases
            .into_iter()
            .filter(|case| self.matches(&case.test.id, case.test.name.as_deref(), &case.test.tags))
            .map(|mut case| {
                if self.once {
                    case.test.consensus_runs = None;
                    case.test.consensus_required = None;
                }
                case
            })
            .collect()
    }

    fn matches(&self, id: &str, name: Option<&str>, tags: &[String]) -> bool {
        if let Some(filter) = &self.filter
            && !id.contains(filter.as_str())
            && !name.is_some_and(|name| name.contains(filter.as_str()))
        {
            return false;
        }
        self.tags.is_empty() || tags.iter().any(|tag| self.tags.contains(tag))
    }
}

/// One case as a front end needs to show it before anything runs: what it is,
/// what it needs, and whether this machine has that.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseListing {
    pub id: String,
    pub name: Option<String>,
    pub tags: Vec<String>,
    pub app: App,
    /// The app bundle, when it is installed. `None` means this case is skipped
    /// with a reason rather than run and failed.
    pub installed: Option<PathBuf>,
}

impl CaseListing {
    /// Whether this case would run on this machine.
    #[must_use]
    pub const fn runnable(&self) -> bool {
        self.installed.is_some()
    }
}

/// Every case and whether this machine can run it, without spending a token.
///
/// Availability is resolved once for the whole app list rather than per case:
/// [`App::installed`] walks candidate bundle paths, and several cases share an
/// app.
#[must_use]
pub fn list_cases() -> Vec<CaseListing> {
    let availability = apps::availability();
    cases::all()
        .into_iter()
        .map(|case| CaseListing {
            id: case.test.id,
            name: case.test.name,
            tags: case.test.tags,
            app: case.app,
            installed: availability
                .iter()
                .find(|(app, _)| *app == case.app)
                .and_then(|(_, path)| path.clone()),
        })
        .collect()
}

/// Run the selected cases and answer with the report.
///
/// `run` attributes every [`AppEvent::EvalCase`] this emits, so a window that
/// started the suite can tell its own progress from anyone else's. `cancel`
/// is checked between cases and is threaded into every `Runtime::chat` the
/// agent makes, so a stop button does not leave a browser and a half-typed
/// document behind.
pub async fn run_suite(
    runtime: &Arc<Runtime>,
    selection: Selection,
    run: RunId,
    cancel: &CancellationToken,
) -> Result<SuiteReport, EvalError> {
    require_inference(runtime)?;

    let planned = plan(&selection);
    if !planned.iter().any(|(_, skip)| skip.is_none()) {
        return Err(EvalError::NoCases);
    }

    // Held across every case, not per case: a suite that released the screen
    // between cases would let another window in halfway through and report the
    // interference as a model failure.
    //
    // Each case's turn takes the keyboard again inside this hold. It does so
    // under its *own* run id, so the nesting is declared with `screen.scope()`
    // rather than guessed at — see the `NeoAgent::screen` doc for why the
    // cases cannot simply share the suite's run id.
    let screen = runtime.acquire_screen(run, format!("eval: {}", cases::SUITE_NAME))?;

    // One conversation for the whole suite: `chat` records each turn against
    // it, and the `turns` table has a foreign key on `conversations(id)`, so an
    // id the store never saw would lose every turn record.
    let conversation = tokio::task::block_in_place(|| {
        runtime.new_conversation(Some(format!("eval: {}", cases::SUITE_NAME)))
    })?;

    let agent: Arc<dyn AgentUnderTest> = Arc::new(NeoAgent::new(
        Arc::clone(runtime),
        conversation.id,
        cancel.clone(),
        Some(screen.scope()),
    ));
    let judge: Arc<dyn Judge> = Arc::new(NeoJudge::new(Arc::clone(runtime)));
    let trace_dir = runtime.data_dir().join("eval-traces");

    execute(
        planned,
        agent,
        Some(judge),
        Some(trace_dir),
        run,
        cancel,
        &|event| runtime.publish(event),
    )
    .await
}

/// The selected cases in suite order, each with the reason it will be skipped
/// if its app is absent. Skipped cases stay in the list so a front end's
/// progress reads `3 of 8` against the same set it previewed.
fn plan(selection: &Selection) -> Vec<(Case, Option<String>)> {
    selection
        .apply(cases::all())
        .into_iter()
        .map(|case| {
            let skip = (case.app.installed().is_none())
                .then(|| format!("{} is not installed", case.app.label()));
            (case, skip)
        })
        .collect()
}

/// An eval with no inference connection measures the connection, not the
/// agent, so it refuses to start rather than reporting eight model failures.
fn require_inference(runtime: &Arc<Runtime>) -> Result<(), EvalError> {
    let doctor = tokio::task::block_in_place(|| runtime.doctor())?;
    match doctor
        .checks
        .iter()
        .find(|check| check.name == "inference" && check.health == neo_agent::doctor::Health::Fail)
    {
        Some(check) => Err(EvalError::NoInference {
            detail: check.detail.clone(),
            fix: check
                .fix
                .clone()
                .unwrap_or_else(|| "see `neo doctor`".to_owned()),
        }),
        None => Ok(()),
    }
}

/// The run loop, over anything that implements `spice`'s agent trait.
///
/// Taking the agent, the judge and the publisher as parameters is what makes
/// the loop testable without a model or a real application: the tests drive it
/// with `MockAgent` and a collecting publisher.
async fn execute(
    planned: Vec<(Case, Option<String>)>,
    agent: Arc<dyn AgentUnderTest>,
    judge: Option<Arc<dyn Judge>>,
    trace_dir: Option<PathBuf>,
    run: RunId,
    cancel: &CancellationToken,
    publish: &(dyn Fn(AppEvent) + Send + Sync),
) -> Result<SuiteReport, EvalError> {
    let started = Instant::now();
    let total = planned.len();
    let mut reports: Vec<TestReport> = Vec::with_capacity(total);
    let mut timestamp = None;

    for (index, (case, skip)) in planned.into_iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(EvalError::Cancelled {
                completed: reports.len(),
                total,
            });
        }

        let id = case.test.id.clone();
        let at = Progress {
            run,
            index,
            total,
            case: &id,
        };
        if let Some(reason) = skip {
            publish(at.event(EvalCaseState::Skipped { reason }));
            continue;
        }

        publish(at.event(EvalCaseState::Started));

        let mut runner = Runner::new(RunnerConfig {
            // See the module docs: this is a property of the machine, not a
            // tuning knob.
            concurrency: 1,
            default_timeout: CASE_TIMEOUT,
            // Selection already happened, and it happened over the case list a
            // front end was shown.
            filter: None,
            tag_filter: None,
            trace_dir: trace_dir.clone(),
            // The caller owns persistence and the baseline diff: it has the
            // report, and it knows whether it has a terminal to print to.
            report_path: None,
            baseline_path: None,
            console_output: false,
        });
        if let Some(judge) = &judge {
            runner = runner.with_judge(Arc::clone(judge));
        }

        let one = runner
            .run(cases::suite(vec![case]), Arc::clone(&agent))
            .await;
        timestamp.get_or_insert(one.timestamp);
        let report = one
            .tests
            .into_iter()
            .next()
            .unwrap_or_else(|| lost_report(&id));

        publish(at.event(settled(&report)));
        reports.push(report);
    }

    // The clock reading `spice` already took when the first case ran: minting
    // one here would mean depending on `chrono` for a `now()` this crate
    // otherwise never needs. There is always one — `NoCases` is checked before
    // the loop, so a plan with nothing runnable never reaches here, and that
    // is the honest answer if it somehow did.
    let Some(timestamp) = timestamp else {
        return Err(EvalError::NoCases);
    };
    Ok(SuiteReport::new(
        cases::SUITE_NAME.to_owned(),
        reports,
        total,
        started.elapsed(),
        timestamp,
    ))
}

/// Where a case sits in the run, so the three publish sites cannot disagree
/// about its index.
struct Progress<'a> {
    run: RunId,
    index: usize,
    total: usize,
    case: &'a str,
}

impl Progress<'_> {
    fn event(&self, state: EvalCaseState) -> AppEvent {
        AppEvent::EvalCase {
            run: self.run,
            index: u32::try_from(self.index).unwrap_or(u32::MAX),
            total: u32::try_from(self.total).unwrap_or(u32::MAX),
            case: self.case.to_owned(),
            state,
        }
    }
}

/// How a finished case reads to a front end: the consensus distribution is the
/// interesting number, not the boolean.
fn settled(report: &TestReport) -> EvalCaseState {
    let runs = u32::try_from(
        report
            .consensus
            .as_ref()
            .map_or(report.attempts, |consensus| consensus.runs),
    )
    .unwrap_or(u32::MAX);
    if report.passed {
        return EvalCaseState::Passed { runs };
    }
    let detail = report.error.clone().unwrap_or_else(|| {
        report
            .assertion_results
            .iter()
            .find(|result| !result.passed)
            .map(|result| {
                result
                    .message
                    .clone()
                    .unwrap_or_else(|| result.description.clone())
            })
            .unwrap_or_else(|| "the case failed without saying why".to_owned())
    });
    EvalCaseState::Failed { runs, detail }
}

/// `spice` returns one report per test; a suite of one that comes back empty
/// means the runner itself lost the case, which must not read as a pass.
fn lost_report(id: &str) -> TestReport {
    TestReport {
        test_id: id.to_owned(),
        test_name: None,
        tags: vec![],
        passed: false,
        attempts: 0,
        assertion_results: vec![],
        judge_results: vec![],
        score: 0.0,
        consensus: None,
        usage: None,
        run_duration: None,
        duration: Duration::ZERO,
        error: Some("the runner returned no report for this case".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]

    use std::sync::Mutex;

    use spice_framework::assertion::Assertion;
    use spice_framework::mock::{MockAgent, MockResponse};

    use super::*;

    /// `run_suite`'s future must be `Send`, or no front end can drive it:
    /// `tokio::spawn` and a Tauri async command both require it, and a bare
    /// `&dyn Fn(AppEvent)` publisher held across an await is enough to lose
    /// the bound — which is exactly how it was lost once already. This never
    /// runs; it fails at compile time, which is when it matters.
    #[allow(dead_code)]
    fn the_suite_future_is_send() {
        fn require_send<T: Send>(_: T) {}
        let _ = |runtime: &Arc<Runtime>,
                 selection: Selection,
                 run: RunId,
                 cancel: &CancellationToken| {
            require_send(run_suite(runtime, selection, run, cancel));
        };
    }

    fn selection(filter: Option<&str>, tags: &[&str], once: bool) -> Selection {
        Selection {
            filter: filter.map(ToOwned::to_owned),
            tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
            once,
        }
    }

    fn ids(cases: &[Case]) -> Vec<&str> {
        cases.iter().map(|case| case.test.id.as_str()).collect()
    }

    /// The filter is a substring of the id *or* the name — `spice`'s own rule,
    /// which this replaced by filtering before the runner instead of inside
    /// it. A front end that shows "3 cases" must run exactly those three.
    #[test]
    fn a_filter_matches_an_id_or_a_name() {
        let by_id = selection(Some("calc"), &[], false).apply(cases::all());
        assert_eq!(ids(&by_id), ["write-a-calc-cell", "read-a-calc-cell"]);

        // Names are "<app label>: <id>", so the app label only matches by name.
        let by_name = selection(Some("TextEdit"), &[], false).apply(cases::all());
        assert!(!by_name.is_empty(), "the app label must match by name");
        assert!(
            by_name.iter().all(|case| case.app == App::TextEdit),
            "a name filter must not drag in other apps"
        );

        assert!(
            selection(Some("no-such-case"), &[], false)
                .apply(cases::all())
                .is_empty()
        );
    }

    /// Tags are any-of, and they compose with the filter rather than
    /// replacing it.
    #[test]
    fn tags_are_any_of_and_compose_with_the_filter() {
        let tagged = selection(None, &["spreadsheet", "media"], false).apply(cases::all());
        assert!(tagged.iter().all(|case| {
            case.test
                .tags
                .iter()
                .any(|tag| tag == "spreadsheet" || tag == "media")
        }));
        assert!(tagged.len() > 1, "any-of must not behave like all-of");

        // `read-a` alone also matches `read-a-page`; the tag is what narrows
        // it, which is the composition being pinned here. Asserted as a
        // predicate rather than a census of ids: an exhaustive list fails
        // whenever anyone adds a case, which is churn without protection —
        // adding the Numbers cases broke exactly that.
        let matched = selection(Some("read-a"), &[], false).apply(cases::all());
        let filter_only = ids(&matched);
        assert!(filter_only.contains(&"read-a-page"));
        assert!(filter_only.contains(&"read-a-calc-cell"));
        assert!(
            filter_only.iter().all(|id| id.contains("read-a")),
            "the filter must match the id, got {filter_only:?}"
        );

        let both = selection(Some("read-a"), &["spreadsheet"], false).apply(cases::all());
        assert!(!both.is_empty(), "the composition must not empty the set");
        assert!(both.iter().all(|case| {
            case.test.id.contains("read-a") && case.test.tags.iter().any(|tag| tag == "spreadsheet")
        }));
        assert!(
            ids(&both).contains(&"read-a-calc-cell"),
            "the spreadsheet read case must survive both"
        );
        assert!(
            !ids(&both).contains(&"read-a-page"),
            "the tag must exclude the browser case"
        );
    }

    /// `--once` is the quick look: one run, no consensus. Leaving
    /// `consensus_required` behind would make `spice` score one run against a
    /// bar of four and report every case as failed.
    #[test]
    fn once_drops_the_consensus_and_the_default_keeps_it() {
        for case in selection(None, &[], true).apply(cases::all()) {
            assert_eq!(case.test.consensus_runs, None, "{}", case.test.id);
            assert_eq!(case.test.consensus_required, None, "{}", case.test.id);
        }
        for case in selection(None, &[], false).apply(cases::all()) {
            assert_eq!(case.test.consensus_runs, Some(cases::CONSENSUS_RUNS));
            assert_eq!(
                case.test.consensus_required,
                Some(cases::CONSENSUS_REQUIRED)
            );
        }
    }

    /// A case whose app is absent is listed and skipped, never run and failed
    /// — an absent dependency says nothing about the agent.
    #[test]
    fn a_listing_says_what_this_machine_can_run() {
        let listed = list_cases();
        assert_eq!(listed.len(), cases::all().len());

        // macOS-only: what "installed" means on Linux is a desktop entry,
        // and that arrives with L3.
        #[cfg(target_os = "macos")]
        {
            let textedit = listed
                .iter()
                .find(|case| case.id == "type-into-textedit")
                .expect("TextEdit ships on every Mac");
            assert!(textedit.runnable());
            assert_eq!(textedit.app, App::TextEdit);
        }

        for case in &listed {
            assert_eq!(case.runnable(), case.app.installed().is_some());
        }
    }

    /// The report a front end gets back has to be about the cases that ran:
    /// one failing case in, one failure out, and the skipped case neither
    /// passes nor fails.
    #[tokio::test]
    async fn the_report_counts_the_cases_that_ran() {
        let passing = probe_case("passes", "say yes");
        let failing = probe_case("fails", "say no");
        let agent = MockAgent::new("mock").default_response(MockResponse::text("yes"));

        let events = Mutex::new(Vec::new());
        let run = RunId::new();
        let report = execute(
            vec![
                (passing, None),
                (failing, None),
                (probe_case("absent", "never"), Some("no app".to_owned())),
            ],
            Arc::new(agent),
            None,
            None,
            run,
            &CancellationToken::new(),
            &|event| {
                events
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(event)
            },
        )
        .await
        .expect("a report");

        assert_eq!(report.suite_name, cases::SUITE_NAME);
        assert_eq!(report.tests.len(), 2, "the skipped case has no result");
        assert_eq!(report.passed, 1);
        assert_eq!(report.failed, 1);
        assert!(
            report
                .tests
                .iter()
                .any(|test| test.test_id == "fails" && !test.passed)
        );

        let seen = events
            .into_inner()
            .unwrap_or_else(|error| error.into_inner());
        let states: Vec<(String, EvalCaseState)> = seen
            .into_iter()
            .filter_map(|event| match event {
                AppEvent::EvalCase {
                    run: theirs,
                    case,
                    state,
                    total,
                    ..
                } if theirs == run => {
                    assert_eq!(total, 3, "progress counts the skipped case too");
                    Some((case, state))
                }
                _ => None,
            })
            .collect();
        assert!(
            matches!(
                states.as_slice(),
                [
                    (first, EvalCaseState::Started),
                    (_, EvalCaseState::Passed { .. }),
                    (_, EvalCaseState::Started),
                    (_, EvalCaseState::Failed { .. }),
                    (last, EvalCaseState::Skipped { .. }),
                ] if first.as_str() == "passes" && last.as_str() == "absent"
            ),
            "unexpected progress: {states:?}"
        );
    }

    /// Cancelling stops at the next case boundary and says so, rather than
    /// returning a report that looks like a suite which merely failed.
    #[tokio::test]
    async fn cancelling_stops_the_run_and_is_not_a_failed_report() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let outcome = execute(
            vec![(probe_case("passes", "say yes"), None)],
            Arc::new(MockAgent::new("mock")),
            None,
            None,
            RunId::new(),
            &cancel,
            &|_| {},
        )
        .await;
        match outcome {
            Err(EvalError::Cancelled { completed, total }) => {
                assert_eq!((completed, total), (0, 1));
            }
            other => panic!(
                "expected a cancellation, got {:?}",
                other.map(|_| "a report")
            ),
        }
    }

    /// A case the mock agent answers "yes" to, asserted on the answer so the
    /// pass/fail is decided by the test and not by a real application.
    fn probe_case(id: &str, message: &str) -> Case {
        let expected = if id == "fails" { "no" } else { "yes" };
        Case {
            app: App::TextEdit,
            test: spice_framework::test_case::TestCase {
                id: id.to_owned(),
                name: None,
                user_message: message.to_owned(),
                config: spice_framework::agent::AgentConfig::default(),
                assertions: vec![Assertion::ExpectTextContains(expected.to_owned())],
                judges: vec![],
                tags: vec![],
                retries: 0,
                consensus_runs: None,
                consensus_required: None,
                timeout: Some(Duration::from_secs(5)),
            },
        }
    }
}
