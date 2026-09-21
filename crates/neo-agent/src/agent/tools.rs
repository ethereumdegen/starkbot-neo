//! The two surfaces an agent action can drive, behind one shape.
//!
//! Both go through `jev-nav`: the same policy, the same ≤250-element table,
//! the same safety heads, the same confirm threshold. The only difference is
//! the [`jev_nav::observer::Observer`] behind them — `CdpObserver` for a web
//! page, `AxObserver` for a native application (A22/A23). Nothing here
//! chooses *how* to act; it launches a surface, hands the goal to the
//! navigator, and turns the outcome into one sentence the model can read.
//!
//! # One run, any number of watchers
//!
//! These functions used to exist twice: once here, taking a `note` closure,
//! and once in `neo-cli`'s `nav` module, printing to stderr and carrying the
//! flags (`--headed`, `--profile`, `--attach`, `--no-safety`) the library
//! could not express. The fork meant a desktop window could not show what a
//! `neo nav` run shows, and a fix to one orchestration never reached the
//! other. There is now one implementation, it takes [`BrowserOptions`] /
//! [`AppOptions`] so every flag is reachable from a GUI, and progress is
//! published as [`AppEvent::NavStep`] — an event any number of front ends can
//! observe, instead of a callback exactly one caller can hold.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use jev_nav::ax::AxObserver;
use jev_nav::{Navigator, Outcome, RunConfig};
use neo_ax::{AppSel, AxHandle};
use neo_cdp::{Browser, LaunchOptions};
use neo_core::{AppEvent, CoreError, NavDecision, NavStepKind, NavSurface, RunId, Settings};
use tokio_util::sync::CancellationToken;

use crate::ax::app_selector;
use crate::runtime::{Runtime, RuntimeError};

/// Viewport the managed Chrome uses. Fixed, so a page renders the same way on
/// every machine and the element table is stable between runs.
const VIEWPORT: (u32, u32) = (1120, 780);

/// How much of a surface's text comes back with an observation.
///
/// The agent has to be able to *answer* from what it read, not just report
/// that it looked: an eval case that asked for a fact off a page had the model
/// come back with a question, because the observation said `Done · ended at
/// <url>` and nothing about the contents. Two thousand characters covers a
/// page's readable content without turning every observation into a document.
const OBSERVED_TEXT: usize = 2_000;

/// Said once, before the first step, when nothing can fill a field in.
///
/// A run that stops at the first `TYPE_TEXT` looks like a navigator failure
/// unless the missing piece is named up front.
const NO_TEXT_HELPER: &str =
    "no inference runtime can type field values yet — the run will stop at the first TYPE_TEXT";

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("the navigator needs a TypeSafe key — add it in Connections")]
    MissingJevKey,
    #[error(
        "Starkbot is not trusted for Accessibility yet — grant it in System Settings › Privacy & Security › Accessibility"
    )]
    NotTrusted,
    /// Another run already has the keyboard. Its own error rather than a
    /// generic failure: nothing is broken, the work simply cannot start yet,
    /// and the message names who to stop.
    #[error(transparent)]
    ScreenBusy(#[from] crate::screen::ScreenBusy),
    #[error("could not drive the browser: {0}")]
    Browser(String),
    #[error("could not drive `{app}`: {detail}")]
    App { app: String, detail: String },
    /// The run was stopped on purpose. Distinct from every other failure
    /// because a front end must not show a stopped run as a broken one.
    #[error(transparent)]
    Cancelled(#[from] CoreError),
    #[error(transparent)]
    Runtime(#[from] Box<RuntimeError>),
}

impl From<RuntimeError> for ToolError {
    fn from(error: RuntimeError) -> Self {
        Self::Runtime(Box::new(error))
    }
}

/// The one failure a stopped run reports, so the whole product agrees on it.
fn cancelled() -> ToolError {
    ToolError::Cancelled(CoreError::Cancelled)
}

/// The single confirm threshold a [`RunConfig`] carries.
///
/// Settings hold one threshold per safety head — `outward`, `destructive`,
/// `spends`, the same three heads `jev-nav` asks Jev for — but a run config
/// compares every head against one number. The lowest of the three is the
/// only mapping that cannot let a head through: a run stops at the strictest
/// threshold the user configured, never at a laxer one (A9 — the gate fails
/// closed). This replaced a `const CONFIRM_AT: f64 = 0.4` that existed twice
/// and ignored settings entirely.
#[must_use]
pub fn confirm_at(settings: &Settings) -> f64 {
    let thresholds = &settings.safety.confirm_at;
    f64::from(
        thresholds
            .outward
            .min(thresholds.destructive)
            .min(thresholds.spends),
    )
}

/// Everything a browser run can be asked for.
///
/// The defaults are an unattended run's: headless, a throwaway profile, no
/// attachments and safety heads on. `neo nav` differs from an agent turn only
/// in the fields it overrides, which is the point — a GUI can offer the same
/// switches without reimplementing the run.
#[derive(Clone, Debug, PartialEq)]
pub struct BrowserOptions {
    pub url: String,
    pub goal: String,
    /// Show the browser window. A headless run is the default.
    pub headed: bool,
    /// A profile directory to reuse, so a logged-in session survives runs.
    /// `None` is a throwaway directory: an unattended run neither inherits
    /// nor leaves a login.
    pub profile: Option<PathBuf>,
    /// Files a file input may be given (05 §8: nothing else is readable).
    pub attach: Vec<PathBuf>,
    /// Ask the Jev safety heads at all. Off means *no* head is asked, which
    /// also means nothing can trip the confirm gate: fixtures only.
    pub safety_heads: bool,
    /// The probability at which a safety head stops the run.
    pub confirm_at: f64,
}

impl BrowserOptions {
    /// What a run with nobody watching takes, with the confirm threshold the
    /// user's settings carry.
    #[must_use]
    pub fn unattended(
        settings: &Settings,
        url: impl Into<String>,
        goal: impl Into<String>,
    ) -> Self {
        Self::unattended_with(confirm_at(settings), url, goal)
    }

    /// [`BrowserOptions::unattended`] for a caller that already resolved the
    /// threshold and has no [`Settings`] in hand.
    #[must_use]
    pub fn unattended_with(
        confirm_at: f64,
        url: impl Into<String>,
        goal: impl Into<String>,
    ) -> Self {
        Self {
            url: url.into(),
            goal: goal.into(),
            headed: false,
            profile: None,
            attach: Vec::new(),
            safety_heads: true,
            confirm_at,
        }
    }
}

/// Everything a native-application run can be asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct AppOptions {
    /// Which app: a bundle id, a pid, or a name substring.
    pub app: String,
    pub goal: String,
    pub safety_heads: bool,
    pub confirm_at: f64,
}

impl AppOptions {
    /// What a run with nobody watching takes, with the confirm threshold the
    /// user's settings carry.
    #[must_use]
    pub fn unattended(
        settings: &Settings,
        app: impl Into<String>,
        goal: impl Into<String>,
    ) -> Self {
        Self::unattended_with(confirm_at(settings), app, goal)
    }

    /// [`AppOptions::unattended`] for a caller that already resolved the
    /// threshold and has no [`Settings`] in hand.
    #[must_use]
    pub fn unattended_with(
        confirm_at: f64,
        app: impl Into<String>,
        goal: impl Into<String>,
    ) -> Self {
        Self {
            app: app.into(),
            goal: goal.into(),
            safety_heads: true,
            confirm_at,
        }
    }
}

/// What a browser action produced.
pub struct BrowserRun {
    pub outcome: Outcome,
    /// One sentence for the model and the thread.
    pub observation: String,
    pub steps: usize,
    pub duration_ms: u64,
}

/// What a native-application action produced.
pub struct AppRun {
    pub outcome: Outcome,
    pub observation: String,
    pub steps: usize,
    pub duration_ms: u64,
}

/// Open `options.url` in a managed Chrome and pursue `options.goal`.
///
/// `run` names this run in every [`AppEvent::NavStep`] it publishes, so a
/// front end watching two runs at once can tell them apart; `cancel` stops
/// it. A cancelled run still closes its browser before returning
/// [`ToolError::Cancelled`] — the reason this takes a token at all is that
/// `task.abort()` left Chrome behind.
///
/// # Errors
///
/// Fails when there is no TypeSafe key, when Chrome will not start or drive,
/// when the navigator errors, or when `cancel` fires.
pub async fn run_browser(
    runtime: &Arc<Runtime>,
    options: &BrowserOptions,
    run: RunId,
    cancel: &CancellationToken,
) -> Result<BrowserRun, ToolError> {
    // Only a headed run takes the screen. Headless steals no focus and types
    // into nothing the user can see, and making it queue behind an app run
    // would be a lie about what it needs.
    let _screen = if options.headed {
        Some(runtime.acquire_screen(run, format!("navigate {}", options.url))?)
    } else {
        None
    };
    neo_otel::in_span(
        surface_run("browser", &options.url, &options.goal),
        browse(runtime, options, run, cancel),
    )
    .await
}

/// The run itself, split out so the span above covers all of it — including
/// the launch, which is where a run most often dies.
async fn browse(
    runtime: &Arc<Runtime>,
    options: &BrowserOptions,
    run: RunId,
    cancel: &CancellationToken,
) -> Result<BrowserRun, ToolError> {
    if cancel.is_cancelled() {
        let error = cancelled();
        finish_run(0, Err(&error));
        return Err(error);
    }
    let settings = runtime.settings()?;
    let jev = runtime.jev().map_err(|_| ToolError::MissingJevKey)?;
    let text = runtime.text_helper(&settings);
    let mut progress = Progress::new(runtime, run);
    if text.is_none() {
        progress.launch(NO_TEXT_HELPER.to_owned());
    }

    // A named profile is shared state: two Chromes on one profile directory
    // either refuse to start or steal each other's session, so it is leased.
    // A throwaway profile is private to this run and needs no lease.
    let _profile_lease = match &options.profile {
        Some(path) => Some(runtime.hold(
            neo_store::Resource::Chrome,
            &format!("browsing with the profile at {}", path.display()),
        )?),
        None => None,
    };

    // A run that was given no profile gets a directory that dies with it, so
    // it neither inherits nor leaves a login. The handle has to outlive the
    // browser, or the profile is deleted out from under Chrome.
    let temporary = match options.profile {
        Some(_) => None,
        None => Some(tempfile::tempdir().map_err(|error| ToolError::Browser(error.to_string()))?),
    };
    let profile = match (&options.profile, &temporary) {
        (Some(path), _) => path.clone(),
        (None, Some(temporary)) => temporary.path().to_owned(),
        (None, None) => return Err(ToolError::Browser("no Chrome profile directory".to_owned())),
    };
    let mut launch = LaunchOptions::new(profile);
    launch.headless = !options.headed;

    let started = Instant::now();
    let timer = Instant::now();
    let browser = Browser::launch(&launch)
        .await
        .map_err(|error| launch_failed(browser_error(error)))?;
    progress.launch(format!(
        "chrome launch + connect   {:>6} ms",
        timer.elapsed().as_millis()
    ));

    let timer = Instant::now();
    let page = async {
        let page = browser
            .new_page("about:blank")
            .await
            .map_err(browser_error)?;
        page.set_viewport(VIEWPORT.0, VIEWPORT.1, 1.0)
            .await
            .map_err(browser_error)?;
        page.navigate(&options.url).await.map_err(browser_error)?;
        Ok::<_, ToolError>(page)
    }
    .await
    .map_err(launch_failed)?;
    progress.launch(format!(
        "new tab + navigate        {:>6} ms   {}",
        timer.elapsed().as_millis(),
        options.url
    ));

    let observer =
        jev_nav::web::CdpObserver::new(page).with_attachments(options.attach.clone());
    let mut navigator = Navigator::new(observer, jev, text);
    let config = RunConfig {
        goal: options.goal.clone(),
        safety_heads: options.safety_heads,
        confirm_at: options.confirm_at,
    };
    progress.launch(format!("\ngoal: {}\n", options.goal));

    let running = Instant::now();
    let calls_before = browser.calls();
    let mut latencies = Vec::new();
    let outcome = {
        let mut on_step = |step: &jev_nav::StepEvent| {
            latencies.push(step.jev_ms);
            neo_otel::record(jev_step(step));
            progress.decision(NavSurface::Browser, step);
        };
        // `jev-nav` has no stop of its own, so the token races the whole run
        // and the step in flight is dropped at its next await point. What
        // matters is what happens after: the browser is closed below on every
        // path, which is exactly what aborting the task did not do.
        tokio::select! {
            biased;
            () = cancel.cancelled() => None,
            result = navigator.run(&config, &mut on_step) => Some(result),
        }
    };
    let total = running.elapsed();
    latencies.sort_unstable();

    // Where the run ended, and what the page says. Both are read before the
    // browser goes away, because the next decision is made from them.
    let ended_at = navigator
        .observer
        .page()
        .evaluate("[location.href, document.title]")
        .await
        .ok()
        .map(|value| value.to_string());
    let text = navigator
        .observer
        .page()
        .evaluate("document.body ? document.body.innerText : ''")
        .await
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .map(|text| clamp(&text));
    let steps = navigator.history().len();
    let protocol_calls = browser.calls().saturating_sub(calls_before);
    browser.close().await;
    drop(temporary);

    // A run that ended in an error is the one worth having in a report, so
    // the span is closed out before the failure leaves this function.
    let Some(outcome) = outcome else {
        let error = cancelled();
        finish_run(steps, Err(&error));
        return Err(error);
    };
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            let error = browser_error(error);
            finish_run(steps, Err(&error));
            return Err(error);
        }
    };
    finish_run(steps, Ok(&outcome));

    progress.outcome(format!("\noutcome: {outcome:?}"));
    progress.summary(format!(
        "total {} ms · {} jev requests (median {} ms) · {} actions · {} protocol calls",
        total.as_millis(),
        latencies.len(),
        median(&latencies),
        steps,
        protocol_calls
    ));
    if let Some(ended_at) = &ended_at {
        progress.summary(format!("ended at: {ended_at}"));
    }

    Ok(BrowserRun {
        observation: observe(&outcome, steps, ended_at.as_deref(), text.as_deref()),
        outcome,
        steps,
        duration_ms: millis(started.elapsed().as_millis()),
    })
}

/// Bring `options.app` to the front and pursue `options.goal` through the
/// accessibility path.
///
/// The app is activated first — a backgrounded app exposes little more than
/// its menu bar — and every step goes through the same policy, budgets and
/// guards as a web page.
///
/// # Errors
///
/// Fails when Accessibility is not granted, when the app cannot be brought to
/// the front, when the navigator errors, or when `cancel` fires.
pub async fn run_app(
    runtime: &Arc<Runtime>,
    options: &AppOptions,
    run: RunId,
    cancel: &CancellationToken,
) -> Result<AppRun, ToolError> {
    // Held for the whole run: driving an app is keystrokes into the frontmost
    // window, and a second run typing into a different window mid-goal does
    // not produce two results, it produces one wrong one.
    let _screen = runtime.acquire_screen(run, format!("drive {}", options.app))?;
    neo_otel::in_span(
        surface_run("app", &options.app, &options.goal),
        drive_app(runtime, options, run, cancel),
    )
    .await
}

/// The run itself, split out for the same reason [`browse`] is: the span has
/// to cover activation, which is where an app run most often dies.
async fn drive_app(
    runtime: &Arc<Runtime>,
    options: &AppOptions,
    run: RunId,
    cancel: &CancellationToken,
) -> Result<AppRun, ToolError> {
    if cancel.is_cancelled() {
        let error = cancelled();
        finish_run(0, Err(&error));
        return Err(error);
    }
    if !AxHandle::trusted() {
        let error = ToolError::NotTrusted;
        finish_run(0, Err(&error));
        return Err(error);
    }
    // Exclusive use of the keyboard and of this app, for as long as the run
    // lasts. Every action here is a global CGEvent or an `AXPress` on whatever
    // is frontmost, so a second Starkbot driving another app at the same
    // moment would type into it. The guard releases on every exit path.
    let _keyboard = match runtime.hold(
        neo_store::Resource::Keyboard,
        &format!("driving {}", options.app),
    ) {
        Ok(guard) => guard,
        Err(error) => {
            let error = ToolError::from(error);
            finish_run(0, Err(&error));
            return Err(error);
        }
    };
    let _app = match runtime.hold(
        neo_store::Resource::App(options.app.clone()),
        &options.goal,
    ) {
        Ok(guard) => guard,
        Err(error) => {
            let error = ToolError::from(error);
            finish_run(0, Err(&error));
            return Err(error);
        }
    };

    let settings = runtime.settings()?;
    let jev = runtime.jev().map_err(|_| ToolError::MissingJevKey)?;
    let text = runtime.text_helper(&settings);
    let mut progress = Progress::new(runtime, run);
    if text.is_none() {
        progress.launch(NO_TEXT_HELPER.to_owned());
    }

    let started = Instant::now();
    // The handle and the activation are the run's launch: a failure in either
    // is a surface run that produced nothing, and is reported as one.
    let activated = async {
        let ax = AxHandle::spawn().map_err(|error| app_error(&options.app, &error))?;
        let running = ax
            .activate(&app_selector(&options.app))
            .await
            .map_err(|error| app_error(&options.app, &error))?;
        Ok::<_, ToolError>((ax, running))
    }
    .await;
    let (ax, running) = match activated {
        Ok(activated) => activated,
        Err(error) => {
            finish_run(0, Err(&error));
            return Err(error);
        }
    };
    progress.launch(format!(
        "activated                 {} · pid {}{}",
        running.name,
        running.pid,
        running
            .bundle_id
            .as_deref()
            .map(|id| format!(" · {id}"))
            .unwrap_or_default()
    ));

    // The application's own name, not the selector that was typed: a trace
    // read later should say which application answered, not `com.apple.…`.
    // It arrives as its own attribute because the span was opened, with the
    // selector as its target, before anything knew what would answer.
    let target = running.name.clone();
    neo_otel::annotate(vec![(
        "starkbot.app",
        serde_json::Value::String(target.clone()),
    )]);
    let stop = ax.clone();
    let observer = AxObserver::new(ax, AppSel::Pid(running.pid));
    let mut navigator = Navigator::new(observer, jev, text);
    let config = RunConfig {
        goal: options.goal.clone(),
        safety_heads: options.safety_heads,
        confirm_at: options.confirm_at,
    };
    progress.launch(format!("\ngoal: {}\n", options.goal));

    let run_started = Instant::now();
    let mut latencies = Vec::new();
    let outcome = {
        let mut on_step = |step: &jev_nav::StepEvent| {
            latencies.push(step.jev_ms);
            neo_otel::record(jev_step(step));
            progress.decision(NavSurface::App, step);
        };
        tokio::select! {
            biased;
            () = cancel.cancelled() => None,
            result = navigator.run(&config, &mut on_step) => Some(result),
        }
    };
    latencies.sort_unstable();
    let steps = navigator.history().len();

    let Some(outcome) = outcome else {
        // Typing is the one action that leaves the machine in a state the
        // user can see. The actor's kill switch posts key-ups for every held
        // modifier and stops mid-chunk, so a stopped run does not leave a
        // Command key down in someone else's app.
        stop.stop();
        let error = cancelled();
        finish_run(steps, Err(&error));
        return Err(error);
    };

    // What the window now says: the same reason as the browser's page text —
    // the agent must be able to answer from what it saw.
    let text = navigator
        .observer
        .table()
        .await
        .ok()
        .map(|table| clamp(&table.text));
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            let error = app_error(&options.app, &error);
            finish_run(steps, Err(&error));
            return Err(error);
        }
    };
    finish_run(steps, Ok(&outcome));

    progress.outcome(format!("\noutcome: {outcome:?}"));
    progress.summary(format!(
        "total {} ms · {} jev requests (median {} ms) · {} actions",
        run_started.elapsed().as_millis(),
        latencies.len(),
        median(&latencies),
        steps
    ));

    Ok(AppRun {
        observation: observe(&outcome, steps, Some(&target), text.as_deref()),
        outcome,
        steps,
        duration_ms: millis(started.elapsed().as_millis()),
    })
}

/// One run's progress, numbered as it goes.
///
/// Every line a run used to print now leaves as an [`AppEvent::NavStep`]. The
/// event carries both the structure (`kind`) a GUI wants to lay out and the
/// rendered `line` a terminal wants to print, produced by one `Display` so
/// the two can never disagree about what happened.
struct Progress<'a> {
    runtime: &'a Arc<Runtime>,
    run: RunId,
    /// Decisions published so far. Launch lines carry `0`: they happen before
    /// the navigator has made a single call.
    step: u32,
}

impl<'a> Progress<'a> {
    fn new(runtime: &'a Arc<Runtime>, run: RunId) -> Self {
        Self {
            runtime,
            run,
            step: 0,
        }
    }

    fn launch(&self, line: String) {
        self.publish(line, NavStepKind::Launch);
    }

    fn outcome(&self, line: String) {
        self.publish(line, NavStepKind::Outcome);
    }

    fn summary(&self, line: String) {
        self.publish(line, NavStepKind::Summary);
    }

    fn decision(&mut self, surface: NavSurface, step: &jev_nav::StepEvent) {
        self.step = self.step.saturating_add(1);
        let decision = nav_decision(surface, step);
        let line = decision.to_string();
        self.publish(line, NavStepKind::Decision(Box::new(decision)));
    }

    fn publish(&self, line: String, kind: NavStepKind) {
        self.runtime.publish(AppEvent::NavStep {
            run: self.run,
            step: self.step,
            line,
            kind,
        });
    }
}

/// One navigator step as the event a front end reads.
///
/// The typed text is counted and thrown away, for the same reason
/// [`jev_step`] does it: a field being typed into may hold a password or a
/// one-time code, and an event bus that carried the characters would be a
/// credential store nobody asked for.
fn nav_decision(surface: NavSurface, step: &jev_nav::StepEvent) -> NavDecision {
    NavDecision {
        surface,
        operation: step.decision.operation.clone(),
        label: step.label.clone(),
        operation_confidence: step.decision.operation_confidence as f32,
        target_confidence: step.decision.target_confidence.map(|value| value as f32),
        candidates: step.candidates,
        stale: step.stale,
        typed_chars: step.typed.as_ref().map(|text| text.chars().count()),
        observe_ms: millis(step.observe_ms),
        jev_ms: millis(step.jev_ms),
        text_ms: millis(step.text_ms),
        act_ms: millis(step.act_ms),
        elapsed_ms: millis(step.elapsed.as_millis()),
        safety: step
            .decision
            .safety
            .iter()
            .map(|(head, probability)| (head.clone(), *probability as f32))
            .collect(),
    }
}

/// The median of an already-sorted list of latencies, or zero when nothing
/// ran. Reported rather than a mean because one cold request would otherwise
/// describe the whole run.
fn median(sorted: &[u128]) -> u128 {
    sorted.get(sorted.len() / 2).copied().unwrap_or(0)
}

/// Report a launch failure once, as the run that never started.
fn launch_failed(error: ToolError) -> ToolError {
    finish_run(0, Err(&error));
    error
}

/// Everything that went wrong while driving a page is the same failure to the
/// caller, and naming it once keeps the launch sequence readable.
fn browser_error(error: impl std::fmt::Display) -> ToolError {
    ToolError::Browser(error.to_string())
}

fn app_error(app: &str, error: &impl std::fmt::Display) -> ToolError {
    ToolError::App {
        app: app.to_owned(),
        detail: error.to_string(),
    }
}

/// One navigator step as the span a report reads.
///
/// The operation is unquoted — it already *is* `CLICK`/`DONE`/`BLOCKED`, and
/// `{:?}` stored it as `"CLICK"`, which made every `group by operation` in a
/// report read one level of quoting deep.
///
/// The typed text is counted and thrown away. A field being typed into may
/// hold a password or a one-time code, and a trace that kept the characters
/// would be a credential store nobody asked for; the length is enough to see
/// that something was typed and how much.
///
/// Public, and used by both the agent's tool runs and a hand-run `neo nav`,
/// so one navigator step produces the same span however it was started.
#[must_use]
pub fn jev_step(step: &jev_nav::StepEvent) -> neo_otel::SpanBuilder {
    // `StepEvent::elapsed` counts from the start of the run, not the start of
    // this decision. Using it made each step's span longer than the last and
    // the jev total exceed the wall time of the run containing it, so the span
    // is timed by the work this step actually did.
    let spent = step.observe_ms + step.jev_ms + step.text_ms + step.act_ms;
    neo_otel::SpanBuilder::internal(format!("jev {}", step.decision.operation))
        .elapsed(std::time::Duration::from_millis(
            u64::try_from(spent).unwrap_or(u64::MAX),
        ))
        .text("starkbot.operation", step.decision.operation.clone())
        .float("starkbot.confidence", step.decision.operation_confidence)
        .int("starkbot.candidates", step.candidates)
        .flag("starkbot.stale", step.stale)
        .maybe_text("starkbot.label", step.label.clone())
        .maybe_int(
            "starkbot.typed_chars",
            step.typed.as_ref().map(|text| text.chars().count()),
        )
        .int("starkbot.observe_ms", step.observe_ms)
        .int("starkbot.jev_ms", step.jev_ms)
        .int("starkbot.text_ms", step.text_ms)
        .int("starkbot.act_ms", step.act_ms)
}

/// The span one whole surface run happens inside.
///
/// It is opened before the run rather than recorded after it, because every
/// [`jev_step`] the run produces is parented to it: a waterfall of a turn
/// reads `invoke_agent` → `execute_tool` → `navigate app` → `jev CLICK`
/// without anything having been threaded through `jev-nav`'s callbacks. How
/// it ended is added on the way out by [`finish_run`].
#[must_use]
pub fn surface_run(surface: &str, target: &str, goal: &str) -> neo_otel::SpanBuilder {
    neo_otel::SpanBuilder::internal(format!("navigate {surface}"))
        .text("starkbot.surface", surface)
        .text("starkbot.target", target)
        .text("starkbot.goal", goal)
}

/// How the run ended, onto the span it is still inside.
///
/// `Blocked` goes through `Debug`, so the navigator's own reason survives
/// into the trace the way it survives into the observation the model reads.
/// A run that ended in an error is the one worth having in a report, so this
/// is called on every exit, including the ones that never launched.
fn finish_run(steps: usize, result: Result<&Outcome, &ToolError>) {
    let outcome = match result {
        Ok(outcome) => format!("{outcome:?}"),
        Err(_) => "error".to_owned(),
    };
    neo_otel::annotate(vec![
        ("starkbot.outcome", serde_json::Value::String(outcome)),
        (
            "starkbot.steps",
            serde_json::Value::from(u64::try_from(steps).unwrap_or(u64::MAX)),
        ),
    ]);
    if let Err(error) = result {
        neo_otel::fail(&error.to_string());
    }
}

/// A millisecond count the wire carries as a `u64`, saturating: a duration
/// that will not fit is a clock the trace cannot describe anyway.
fn millis(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Turn an outcome into the sentence the model reads next.
///
/// It names the outcome, the work done and where it ended, because those are
/// the three things that decide what to do next. `Blocked` keeps the
/// navigator's own reason verbatim rather than softening it — a model told
/// "done" about a failed run will build on sand.
fn observe(
    outcome: &Outcome,
    steps: usize,
    ended_at: Option<&str>,
    text: Option<&str>,
) -> String {
    let where_it_ended = ended_at
        .map(|value| format!(" · ended at {value}"))
        .unwrap_or_default();
    let head = match outcome {
        Outcome::Done => format!("Done after {steps} action(s){where_it_ended}"),
        Outcome::Blocked(reason) => {
            format!("Blocked after {steps} action(s): {reason}{where_it_ended}")
        }
    };
    match text.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => format!("{head}\n\nWhat the surface says:\n{text}"),
        None => head,
    }
}

/// Keep an observation bounded without cutting a word in half mid-character.
fn clamp(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= OBSERVED_TEXT {
        return trimmed.to_owned();
    }
    let kept: String = trimmed.chars().take(OBSERVED_TEXT).collect();
    format!("{kept}\n… (truncated)")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings::default()
    }

    /// The threshold used to be a `const 0.4` in two files, so changing
    /// `safety.confirm_at` in settings changed nothing at all.
    #[test]
    fn the_confirm_threshold_comes_from_settings() {
        let mut settings = settings();
        settings.safety.confirm_at.outward = 0.9;
        settings.safety.confirm_at.destructive = 0.25;
        settings.safety.confirm_at.spends = 0.8;
        // The strictest head wins: one number has to stand in for three, and
        // the gate must fail closed.
        assert!((confirm_at(&settings) - 0.25).abs() < 1e-6);

        let options = BrowserOptions::unattended(&settings, "https://example.com", "read it");
        assert!((options.confirm_at - 0.25).abs() < 1e-6);
        assert!((AppOptions::unattended(&settings, "TextEdit", "type").confirm_at - 0.25).abs() < 1e-6);
    }

    /// An unattended run is headless, throwaway and guarded. A GUI that wants
    /// otherwise has to say so field by field.
    #[test]
    fn unattended_defaults_are_the_safe_ones() {
        let options = BrowserOptions::unattended(&settings(), "https://example.com", "read it");
        assert!(!options.headed);
        assert_eq!(options.profile, None);
        assert!(options.attach.is_empty());
        assert!(options.safety_heads);
        assert!(AppOptions::unattended(&settings(), "TextEdit", "type").safety_heads);
    }

    /// The observation is what the next decision is made from, so a failed run
    /// must not read like a successful one.
    #[test]
    fn an_observation_states_the_outcome_plainly() {
        let done = observe(
            &Outcome::Done,
            3,
            Some("[\"https://example.com/\",\"Example\"]"),
            None,
        );
        assert!(done.contains("Done"));
        assert!(done.contains("3 action(s)"));
        assert!(done.contains("example.com"));

        let blocked = observe(
            &Outcome::Blocked("the page never settled".into()),
            1,
            None,
            None,
        );
        assert!(blocked.contains("Blocked"));
        // The navigator's own reason has to survive into the observation, or
        // the model cannot tell a timeout from a refusal.
        assert!(blocked.contains("the page never settled"));
        assert!(!blocked.contains("ended at"));
    }

    /// The observation has to carry what the surface said, or the model cannot
    /// answer a question about the page it just read — it asks the user
    /// instead, which an eval case caught.
    #[test]
    fn an_observation_carries_the_text_the_surface_showed() {
        let observation = observe(
            &Outcome::Done,
            1,
            Some("[\"https://example.com/\",\"Example Domain\"]"),
            Some("Example Domain\nThis domain is for use in examples."),
        );
        assert!(observation.contains("This domain is for use in examples."));
    }

    /// A long page is bounded, and says that it was.
    #[test]
    fn long_text_is_truncated_and_says_so() {
        let clamped = clamp(&"x".repeat(OBSERVED_TEXT + 500));
        assert!(clamped.contains("truncated"));
        assert!(clamped.chars().count() < OBSERVED_TEXT + 100);
    }

    /// One slow request must not be allowed to describe a whole run.
    #[test]
    fn the_median_of_no_requests_is_zero() {
        assert_eq!(median(&[]), 0);
        assert_eq!(median(&[10, 20, 900]), 20);
    }

    /// `neo nav`'s live trace is parsed by scripts and read by eye in fixed
    /// columns, and this refactor moved the renderer into `neo-core`. The
    /// line has to come out of the new path exactly as the old `step_line`
    /// produced it — same widths, same separators, same order — with the one
    /// deliberate change: the typed text is counted, never echoed.
    #[test]
    fn a_decision_renders_the_line_the_cli_has_always_printed() {
        let mut safety = std::collections::BTreeMap::new();
        safety.insert("destructive".to_owned(), 0.125);
        safety.insert("outward".to_owned(), 0.9);
        let step = jev_nav::StepEvent {
            elapsed: std::time::Duration::from_millis(1234),
            decision: jev_nav::policy::Decision {
                operation: "TYPE_TEXT".to_owned(),
                operation_confidence: 0.875,
                operation_probabilities: std::collections::BTreeMap::new(),
                action: None,
                target: None,
                target_confidence: Some(0.5),
                safety,
            },
            label: Some("Search the catalogue".to_owned()),
            typed: Some("hunter2".to_owned()),
            observe_ms: 12,
            jev_ms: 340,
            text_ms: 56,
            act_ms: 7,
            candidates: 42,
            stale: true,
            usage: serde_json::Value::Null,
        };
        let line = nav_decision(NavSurface::Browser, &step).to_string();
        assert_eq!(
            line,
            "  1234 ms  TYPE_TEXT   Search the catalogue                         p=0.88 tgt=0.50  \
             obs  12 · jev  340 · text   56 · act   7 ms  [42 cands]  typed 7 chars  STALE  \
             destructive=0.12 outward=0.90"
        );
        // The keystrokes never reach the bus, only their count.
        assert!(!line.contains("hunter2"));
    }

    /// A stopped run must be distinguishable from a broken one: wave 2 shows
    /// a cancelled turn differently from a failed turn, and the whole reason
    /// these entry points take a token is that `task.abort()` could not say
    /// which had happened.
    #[tokio::test]
    async fn a_run_that_starts_cancelled_reports_cancellation_not_failure() {
        let directory = match tempfile::TempDir::new() {
            Ok(directory) => directory,
            Err(error) => panic!("{error}"),
        };
        let runtime = match Runtime::open(directory.path()) {
            Ok(runtime) => Arc::new(runtime),
            Err(error) => panic!("{error}"),
        };
        let cancel = CancellationToken::new();
        cancel.cancel();

        // No TypeSafe key is stored here, so a run that ignored the token
        // would fail with `MissingJevKey` instead — the token is checked
        // first, before anything is launched.
        let options = BrowserOptions::unattended_with(0.4, "https://example.com", "read it");
        match run_browser(&runtime, &options, RunId::new(), &cancel).await {
            Err(ToolError::Cancelled(CoreError::Cancelled)) => (),
            Err(other) => panic!("{other}"),
            Ok(_) => panic!("a cancelled run must not succeed"),
        }

        let options = AppOptions::unattended_with(0.4, "Finder", "look");
        match run_app(&runtime, &options, RunId::new(), &cancel).await {
            Err(ToolError::Cancelled(CoreError::Cancelled)) => (),
            Err(other) => panic!("{other}"),
            Ok(_) => panic!("a cancelled run must not succeed"),
        }
    }
}
