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
//! flags (`--headless`, `--profile`, `--attach`, `--no-safety`) the library
//! could not express. The fork meant a desktop window could not show what a
//! `neo nav` run shows, and a fix to one orchestration never reached the
//! other. There is now one implementation, it takes [`BrowserOptions`] /
//! [`AppOptions`] so every flag is reachable from a GUI, and progress is
//! published as [`AppEvent::NavStep`] — an event any number of front ends can
//! observe, instead of a callback exactly one caller can hold.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use jev_nav::ax::AxObserver;
use jev_nav::{Approval, Navigator, Outcome, RunConfig};
use neo_ax::{AppSel, AxHandle};
use neo_cdp::{Browser, LaunchOptions, Start};
use neo_core::{AppEvent, CoreError, NavDecision, NavStepKind, NavSurface, RunId, Settings};
use tokio_util::sync::CancellationToken;

use crate::ax::app_selector;
use crate::runtime::{Runtime, RuntimeError};

/// What a hand-over card offers instead of a text field: there is exactly one
/// thing to say when the user has done the part only they could do.
const READY: &str = "I'm ready";

/// What a refused action tells the next decision. A denial is information,
/// not an error: the run has to find another way, and it can only do that if
/// it knows why the obvious way is gone.
const REFUSED: &str = "the user did not approve this; find another way or stop";

/// Drive a navigator to an ending a model can act on, asking the person
/// watching whenever the run may not proceed alone (16 §5.3).
///
/// This is the difference between a confirm gate and a dead end. `jev-nav`
/// pauses with an escalation and a resume token, and the browser tab or the
/// app window is still sitting exactly where the decision was made. Here that
/// pause becomes a card, the answer becomes an [`Approval`], and the run
/// carries on — so a step that spends money, a login wall and a field the
/// goal never mentioned all end in the same place: the user decides, and the
/// work continues from where it stopped.
///
/// Nobody answering is not the same as a no to *everything*: an unanswered
/// confirm is a refusal of that one action, while an unanswered question
/// leaves the run with nothing to type, so it stops and says so.
async fn drive_with_cards<O: jev_nav::Observer>(
    navigator: &mut Navigator<O>,
    config: &RunConfig,
    runtime: &Arc<Runtime>,
    task_id: neo_core::TaskId,
    on_step: &mut impl FnMut(&jev_nav::StepEvent),
) -> Result<Outcome, jev_nav::NavError> {
    use neo_core::events::GateOutcome;

    let unanswered = || Outcome::Blocked {
        reason: jev_nav::BlockReason::Unanswered,
    };
    let mut outcome = navigator.run(config, &mut *on_step).await?;
    loop {
        let (resume, approval) = match outcome {
            Outcome::NeedsConfirm { escalation, resume } => {
                let asked = runtime
                    .confirm(
                        task_id,
                        cause_of(&escalation),
                        escalation.sentence.clone(),
                        Some(where_of(&escalation)),
                    )
                    .await;
                match asked {
                    Ok(GateOutcome::Confirmed) => (resume, Approval::Approve),
                    // A timeout refuses, and so does a cancellation:
                    // treating silence — or a user who walked away from the
                    // card — as a yes is the one reading of an unanswered
                    // question that spends money.
                    Ok(GateOutcome::Denied | GateOutcome::TimedOut | GateOutcome::Cancelled) => (
                        resume,
                        Approval::Deny {
                            note: REFUSED.to_owned(),
                        },
                    ),
                    Err(_) => return Ok(unanswered()),
                }
            }
            Outcome::NeedsUser { reason, resume, .. } => {
                let typed = matches!(reason, jev_nav::gate::NeedsUser::Value { .. });
                let options = if typed {
                    Vec::new()
                } else {
                    vec![READY.to_owned()]
                };
                match runtime.ask_user(task_id, reason.sentence(), options).await {
                    Ok(Some(answer)) if typed => (resume, Approval::Value(answer)),
                    Ok(Some(_)) => (resume, Approval::Ready),
                    Ok(None) | Err(_) => return Ok(unanswered()),
                }
            }
            terminal => return Ok(terminal),
        };
        outcome = navigator
            .resume(config, resume, approval, &mut *on_step)
            .await?;
    }
}

/// The machine-readable half of a card: which rule stopped the run, for a
/// trace, a test and later a calibration table.
fn cause_of(escalation: &jev_nav::Escalation) -> String {
    use jev_nav::gate::ConfirmReason;

    match &escalation.reason {
        Some(ConfirmReason::SafetyHead { head, .. }) => format!("safety:{head}"),
        Some(ConfirmReason::Label { word }) => format!("label:{word}"),
        Some(ConfirmReason::FirstUpload { origin }) => format!("upload:{origin}"),
        None => "ask".to_owned(),
    }
}

/// Where the run is, for the line under the card's sentence.
fn where_of(escalation: &jev_nav::Escalation) -> String {
    if escalation.title.is_empty() {
        escalation.url.clone()
    } else {
        format!("{} — {}", escalation.title, escalation.url)
    }
}

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

/// The app's own Chrome, under the data directory everything else in the
/// product lives in (10 §10). A directory name, not a path: it is joined
/// onto whichever data dir this process was opened with, so a `--data-dir`
/// run gets its own browser and cannot touch the real profile.
const CHROME_PROFILE: &str = "chrome";

/// How many tabs the bot leaves lying around before it starts closing the
/// oldest (10 §10). Every run leaves its tab open so the user can see what
/// was done, which without a cap is an unbounded window.
const OWNED_TABS: usize = 8;

/// The ledger of tabs runs on this profile opened, kept inside the profile
/// because that is exactly what it is keyed on: a target id means nothing
/// against a different Chrome, and Chrome ignores files it did not write.
const TAB_LEDGER: &str = "starkbot-tabs.json";

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
    /// Nothing can fill a field in, so the run is refused before it starts.
    ///
    /// This used to be a warning line and a launch: the run died at the
    /// first `TYPE_TEXT`, several steps and one Chrome later, looking like a
    /// navigator failure (16 §0, B5). The message names the one fix, because
    /// a refusal that does not is just a different dead end.
    #[error(
        "nothing can type field values yet — select an inference runtime that can answer: \
         `claude-subscription`, `anthropic-oauth` or `openai-codex`"
    )]
    NoTextHelper,
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
/// The defaults are the product's: the app's own headed Chrome, no
/// attachments and safety heads on. `neo nav` differs from an agent turn only
/// in the fields it overrides, which is the point — a GUI can offer the same
/// switches without reimplementing the run.
#[derive(Clone, Debug, PartialEq)]
pub struct BrowserOptions {
    pub url: String,
    pub goal: String,
    /// Run with no window, in a directory that dies with the run.
    ///
    /// The opt-in for eval and CI, where there is nobody to sign in and
    /// nothing should be left behind. Off by default: a browser the user
    /// cannot see is a browser they cannot sign into, and then everything
    /// behind a login is unreachable (16 §0, B4).
    pub headless: bool,
    /// A profile directory to use instead of the app's own. `None` is the
    /// managed profile under the data directory — the one the user's logins
    /// live in — unless `headless` asked for a throwaway.
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
            headless: false,
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
/// it. A stopped run leaves the tab exactly where it stopped — the work is
/// visible and the next run can pick it up — which is also why this takes a
/// token at all: `task.abort()` used to leave Chrome behind with nobody
/// holding it.
///
/// # Errors
///
/// Fails when there is no TypeSafe key, when nothing can type field values,
/// when Chrome will not start or drive, when the navigator errors, or when
/// `cancel` fires.
pub async fn run_browser(
    runtime: &Arc<Runtime>,
    options: &BrowserOptions,
    run: RunId,
    cancel: &CancellationToken,
) -> Result<BrowserRun, ToolError> {
    // A browser run does not take the screen. Every keystroke it makes is a
    // CDP event addressed to a tab, not a global one aimed at whatever is
    // frontmost, so it steals nothing from an app run happening beside it.
    // Now that a headed window is the default, keying the lease on that
    // would serialise every browse behind every app run for a focus change
    // that happens once, at the end — and that one moment takes the lease
    // itself (see the activation in `browse`).
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
    // Nothing is launched until something can type. A run that discovers
    // this at its first `TYPE_TEXT` has already opened a browser and burned
    // several steps, and then reads like a navigator failure (16 §4, B5).
    let Some(text) = runtime.text_helper(&settings) else {
        let error = ToolError::NoTextHelper;
        finish_run(0, Err(&error));
        return Err(error);
    };
    let mut progress = Progress::new(runtime, run);

    // The temp handle has to outlive the browser, or the profile is deleted
    // out from under Chrome.
    let temporary = match throwaway(options) {
        true => Some(tempfile::tempdir().map_err(|error| ToolError::Browser(error.to_string()))?),
        false => None,
    };
    let profile = profile_of(
        runtime.data_dir(),
        options,
        temporary.as_ref().map(tempfile::TempDir::path),
    );
    // A profile that outlives the run is shared state: two Chromes on one
    // directory either refuse to start or fight over the session, so it is
    // leased for as long as a run drives it. A throwaway is private to the
    // run and needs no lease.
    let _profile_lease = match &temporary {
        Some(_) => None,
        None => Some(runtime.hold(
            neo_store::Resource::Chrome,
            &format!("browsing with the profile at {}", profile.display()),
        )?),
    };
    let mut launch = match &temporary {
        Some(_) => LaunchOptions::ephemeral(&profile),
        None => LaunchOptions::new(&profile),
    };
    launch.headless = options.headless;

    let started = Instant::now();
    let timer = Instant::now();
    let (browser, start) = Browser::attach_or_launch(&launch)
        .await
        .map_err(|error| launch_failed(browser_error(error)))?;
    // Warm start or cold: the difference is the whole point of keeping the
    // browser alive, so the summary has to say which one happened.
    let how = match start {
        Start::Attached => "attach",
        Start::Launched => "launch + connect",
    };
    progress.launch(format!(
        "chrome {how:<19}{:>6} ms",
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

    let observer = jev_nav::web::CdpObserver::new(page).with_attachments(options.attach.clone());
    let mut navigator = Navigator::new(observer, jev, Some(text));
    let config = RunConfig {
        goal: options.goal.clone(),
        safety_heads: options.safety_heads,
        confirm_at: options.confirm_at,
        // The user's own denied list (10 §7). A refusal here is not a card:
        // no approval makes a denied host allowed.
        denied_origins: settings.safety.denied_origins.clone(),
    };
    progress.launch(format!("\ngoal: {}\n", options.goal));
    // One navigator run is one task, which is what a confirm card is filed
    // under (M4-lite, 16 §5.3). A queue with rows of its own comes only if
    // real use asks for one.
    let task_id = neo_core::TaskId::new();

    let running = Instant::now();
    let calls_before = browser.calls();
    let mut timings = Timings::default();
    let outcome = {
        let mut on_step = |step: &jev_nav::StepEvent| {
            timings.push(step);
            neo_otel::record(jev_step(step));
            progress.decision(NavSurface::Browser, step);
        };
        // `jev-nav` has no stop of its own, so the token races the whole run
        // and the step in flight is dropped at its next await point. The tab
        // is then left exactly as that step found it: a stopped run is
        // resumable, and the next run opens its own tab regardless.
        tokio::select! {
            biased;
            () = cancel.cancelled() => None,
            result = drive_with_cards(
                &mut navigator, &config, runtime, task_id, &mut on_step,
            ) => Some(result),
        }
    };
    let total = running.elapsed();

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
    let page = navigator.observer.page();

    // The work stays on screen. On `Done` the tab is brought to the front so
    // the user sees what was done; on every other ending it is left exactly
    // where it stopped, which is what a resume reattaches to (10 §10).
    // Either way the browser lives on — closing it would throw away the
    // logins that are the whole reason the profile persists.
    if matches!(outcome, Some(Ok(Outcome::Done))) && !options.headless {
        activate(runtime, run, page, &progress, &options.url).await;
    }
    match temporary {
        // A throwaway profile is about to be deleted, so its Chrome has to
        // go first or it is reading a directory that no longer exists.
        Some(temporary) => {
            browser.close().await;
            drop(temporary);
        }
        None => cap_owned_tabs(&browser, &profile, page.target_id()).await,
    }

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
        "total {} ms · {} · {} actions · {} protocol calls",
        total.as_millis(),
        timings.line(),
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
/// Fails when Accessibility is not granted, when nothing can type field
/// values, when the app cannot be brought to the front, when the navigator
/// errors, or when `cancel` fires.
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
    // Nothing is held and nothing is brought to the front until something
    // can type: the same refusal a browser run makes, for the same reason
    // (16 §4, B5). Taking the keyboard first would make a refusal look like
    // a busy machine.
    let settings = runtime.settings()?;
    let jev = runtime.jev().map_err(|_| ToolError::MissingJevKey)?;
    let Some(text) = runtime.text_helper(&settings) else {
        let error = ToolError::NoTextHelper;
        finish_run(0, Err(&error));
        return Err(error);
    };

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
    let _app = match runtime.hold(neo_store::Resource::App(options.app.clone()), &options.goal) {
        Ok(guard) => guard,
        Err(error) => {
            let error = ToolError::from(error);
            finish_run(0, Err(&error));
            return Err(error);
        }
    };
    let mut progress = Progress::new(runtime, run);

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
    let mut navigator = Navigator::new(observer, jev, Some(text));
    let config = RunConfig {
        goal: options.goal.clone(),
        safety_heads: options.safety_heads,
        confirm_at: options.confirm_at,
        denied_origins: settings.safety.denied_origins.clone(),
    };
    progress.launch(format!("\ngoal: {}\n", options.goal));
    let task_id = neo_core::TaskId::new();

    let run_started = Instant::now();
    let mut timings = Timings::default();
    let outcome = {
        let mut on_step = |step: &jev_nav::StepEvent| {
            timings.push(step);
            neo_otel::record(jev_step(step));
            progress.decision(NavSurface::App, step);
        };
        tokio::select! {
            biased;
            () = cancel.cancelled() => None,
            result = drive_with_cards(
                &mut navigator, &config, runtime, task_id, &mut on_step,
            ) => Some(result),
        }
    };

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
        "total {} ms · {} · {} actions",
        run_started.elapsed().as_millis(),
        timings.line(),
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

/// Does this run drive a directory that dies with it?
///
/// Only a headless run that named no profile. That is the unattended shape —
/// eval, CI — where there is nobody to sign in and nothing should be left
/// behind. A headless run that *was* given a profile is the opposite case:
/// somebody signed into that profile by hand and wants it used.
fn throwaway(options: &BrowserOptions) -> bool {
    options.profile.is_none() && options.headless
}

/// Which directory a browser run drives.
///
/// The default is the app's own Chrome under the data directory, because a
/// profile that survives the run is what makes a login survive it (10 §10,
/// B4). `temporary` is the throwaway [`throwaway`] asked for.
fn profile_of(data_dir: &Path, options: &BrowserOptions, temporary: Option<&Path>) -> PathBuf {
    match (&options.profile, temporary) {
        (Some(path), _) => path.clone(),
        (None, Some(path)) => path.to_owned(),
        (None, None) => data_dir.join(CHROME_PROFILE),
    }
}

/// Where a run's time went, per step, so a latency regression is seen in the
/// summary rather than felt (16 §4, B3).
///
/// The parity spike measured observe, decide and act separately because they
/// regress for different reasons — a slower snapshot, a slower Jev, a slower
/// helper — and one combined number hides all three.
#[derive(Default)]
struct Timings {
    jev: Vec<u128>,
    observe: Vec<u128>,
    act: Vec<u128>,
    text: Vec<u128>,
}

impl Timings {
    fn push(&mut self, step: &jev_nav::StepEvent) {
        self.jev.push(step.jev_ms);
        self.observe.push(step.observe_ms);
        self.act.push(step.act_ms);
        // Only a `TYPE_TEXT` step calls the helper. Counting the zeros from
        // every other step would put the p50 at zero on any run that is
        // mostly clicks, which is every run — and a slow helper would never
        // show up at all.
        if step.text_ms > 0 {
            self.text.push(step.text_ms);
        }
    }

    /// The timing half of the summary line.
    fn line(&mut self) -> String {
        self.jev.sort_unstable();
        self.observe.sort_unstable();
        self.act.sort_unstable();
        self.text.sort_unstable();
        format!(
            "{} jev requests (median {} ms) · observe {} ms · act {} ms · text p50 {} ms",
            self.jev.len(),
            median(&self.jev),
            median(&self.observe),
            median(&self.act),
            median(&self.text)
        )
    }
}

/// Bring the finished run's tab to the front, or leave it where it is.
///
/// Raising a window is the one moment a browser run touches the screen, so
/// it asks for the lease rather than assuming it: a run that finishes while
/// an app run is typing must not pull the frontmost window out from under
/// it. A tab left in the background is still open and still holds the work.
async fn activate(
    runtime: &Arc<Runtime>,
    run: RunId,
    page: &neo_cdp::Page,
    progress: &Progress<'_>,
    url: &str,
) {
    let left_behind = match runtime.acquire_screen(run, format!("show {url}")) {
        Ok(_screen) => page.activate().await.err().map(|error| error.to_string()),
        Err(busy) => Some(busy.to_string()),
    };
    if let Some(why) = left_behind {
        progress.summary(format!("the tab stayed in the background: {why}"));
    }
}

/// Record the tab this run opened, and close the oldest ones over the cap.
///
/// Best effort throughout: a ledger that cannot be read or written must not
/// fail a run that has already done its work, and an id Chrome no longer
/// knows is a tab the user closed themselves.
async fn cap_owned_tabs(browser: &Browser, profile: &Path, opened: &str) {
    let ledger = profile.join(TAB_LEDGER);
    let known = std::fs::read_to_string(&ledger)
        .ok()
        .and_then(|text| serde_json::from_str::<Vec<String>>(&text).ok())
        .unwrap_or_default();
    let (keep, close) = capped(known, opened);
    for target in close {
        let _ = browser.close_target(&target).await;
    }
    if let Ok(text) = serde_json::to_string(&keep) {
        let _ = std::fs::write(&ledger, text);
    }
}

/// The owned-tab ledger after one more tab was opened: what stays, oldest
/// first, and what to close.
fn capped(mut known: Vec<String>, opened: &str) -> (Vec<String>, Vec<String>) {
    // A tab already in the ledger moves to the end rather than appearing
    // twice, or a run that reused one would age out a tab per run.
    known.retain(|target| target != opened);
    known.push(opened.to_owned());
    let over = known.len().saturating_sub(OWNED_TABS);
    let closed = known.drain(..over).collect();
    (known, closed)
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
/// the three things that decide what to do next. A pause says who it is
/// waiting for and what for, and `Blocked` keeps the navigator's own reason
/// verbatim rather than softening it — a model told "done" about a run that
/// stopped will build on sand.
fn observe(outcome: &Outcome, steps: usize, ended_at: Option<&str>, text: Option<&str>) -> String {
    let where_it_ended = ended_at
        .map(|value| format!(" · ended at {value}"))
        .unwrap_or_default();
    let head = match outcome {
        Outcome::Done => format!("Done after {steps} action(s){where_it_ended}"),
        Outcome::NeedsConfirm { escalation, .. } => format!(
            "Waiting for a yes or no after {steps} action(s): {}{where_it_ended}",
            escalation.sentence
        ),
        Outcome::NeedsUser { reason, .. } => format!(
            "Waiting for you after {steps} action(s): {}{where_it_ended}",
            reason.sentence()
        ),
        Outcome::Blocked { reason } => {
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

    /// The whole Q2 chain in one place: a navigator run meets something it
    /// may not do alone, a card reaches a front end, the answer comes back,
    /// and the run does — or does not — the thing.
    ///
    /// The pieces are tested on their own (`jev_nav` pauses, `confirm`
    /// brokers), but this is the composition that is the product: before it,
    /// every one of these endings was a dead run with a sentence.
    mod cards {
        #![allow(clippy::expect_used)]

        use std::collections::VecDeque;
        use std::sync::Mutex;

        use jev_nav::policy::Action;
        use jev_nav::{ObserveError, Observer};
        use neo_core::events::{AppEvent, GateOutcome, ResolutionVia};
        use serde_json::{Value, json};
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

        use super::*;

        /// A surface that answers from one page and records what it executed.
        struct FakeSurface {
            page: Value,
            acted: std::sync::Arc<Mutex<Vec<String>>>,
        }

        #[async_trait::async_trait]
        impl Observer for FakeSurface {
            async fn observe(&mut self) -> Result<Value, ObserveError> {
                Ok(self.page.clone())
            }

            async fn fresh(
                &mut self,
                _observation: &Value,
                _action: Option<&Action>,
            ) -> Result<bool, ObserveError> {
                Ok(true)
            }

            async fn act(
                &mut self,
                action: &Action,
                _observation: &Value,
                _text: Option<&str>,
            ) -> Result<(), ObserveError> {
                let label = action
                    .get("label")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if let Ok(mut acted) = self.acted.lock() {
                    acted.push(label);
                }
                Ok(())
            }
        }

        struct Script(Mutex<VecDeque<Value>>);

        impl Respond for Script {
            fn respond(&self, _request: &Request) -> ResponseTemplate {
                match self.0.lock().ok().and_then(|mut queue| queue.pop_front()) {
                    Some(body) => ResponseTemplate::new(200).set_body_json(body),
                    None => ResponseTemplate::new(500),
                }
            }
        }

        /// A page offering one button whose label alone trips the gate.
        fn checkout() -> Value {
            json!({
                "url": "https://shop.test/cart", "title": "Checkout",
                "text": "One item in your cart", "page_key": "k", "marker": ["m", []],
                "guards": { "1": "g1" }, "scroll": { "y": 0 },
                "signals": { "password_fields": 0, "captcha": 0 },
                "actions": [
                    { "kind": "click", "node": 1, "id": "e1", "label": "Pay $42.00 now",
                      "role": "button" }
                ],
            })
        }

        /// Jev answers over exactly the operations this page offers, with the
        /// safety heads calm: the label is what must stop the run.
        fn answer(operation: &str) -> Value {
            let offered = ["CLICK", "DONE", "BLOCKED"];
            let mut probabilities = json!({});
            for id in offered {
                probabilities[id] = json!(if id == operation { 0.9 } else { 0.05 });
            }
            let mut body = json!({
                "model": "jev-test",
                "answers": { "operation": {
                    "choice": operation, "probabilities": probabilities, "confidence": 0.9,
                } },
                "usage": {},
            });
            for head in ["outward", "destructive", "spends", "on_task"] {
                let calm = if head == "on_task" { 0.95 } else { 0.02 };
                body["answers"][head] = json!({ "type": "noul", "noul": calm });
            }
            if operation == "CLICK" {
                body["answers"]["click_target"] =
                    json!({ "choice": "1", "probabilities": { "1": 1.0 }, "confidence": 0.9 });
            }
            body
        }

        struct Harness {
            runtime: Arc<Runtime>,
            acted: std::sync::Arc<Mutex<Vec<String>>>,
            _dir: tempfile::TempDir,
            _server: MockServer,
        }

        /// Run the card loop against the fake surface, letting `answer_card`
        /// play the person watching.
        async fn drive(
            answers: Vec<Value>,
        ) -> (
            Harness,
            tokio::task::JoinHandle<Result<Outcome, jev_nav::NavError>>,
        ) {
            let dir = tempfile::tempdir().expect("a temporary data directory");
            let runtime = Arc::new(Runtime::open(dir.path()).expect("the store opens"));
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/v1/systemone"))
                .respond_with(Script(Mutex::new(answers.into())))
                .mount(&server)
                .await;
            let jev = jev_nav::wire::TypeSafe::new(
                "ts-test-not-a-real-key",
                format!("{}/v1/systemone", server.uri()),
                "jev-test",
            );
            let acted = std::sync::Arc::new(Mutex::new(Vec::new()));
            let surface = FakeSurface {
                page: checkout(),
                acted: std::sync::Arc::clone(&acted),
            };
            let mut navigator = Navigator::new(surface, jev, None);
            let config = RunConfig {
                goal: "buy the item in the cart".into(),
                safety_heads: true,
                confirm_at: 0.4,
                denied_origins: Vec::new(),
            };
            let run = {
                let runtime = Arc::clone(&runtime);
                tokio::spawn(async move {
                    drive_with_cards(
                        &mut navigator,
                        &config,
                        &runtime,
                        neo_core::TaskId::new(),
                        &mut |_| {},
                    )
                    .await
                })
            };
            (
                Harness {
                    runtime,
                    acted,
                    _dir: dir,
                    _server: server,
                },
                run,
            )
        }

        /// The card the run is waiting on, and what it says.
        async fn next_card(
            events: &mut tokio::sync::broadcast::Receiver<neo_core::Envelope>,
        ) -> neo_core::events::ConfirmView {
            loop {
                let envelope = events.recv().await.expect("the channel stays open");
                if let AppEvent::ConfirmRequest { confirm } = envelope.event {
                    return confirm;
                }
            }
        }

        #[tokio::test]
        async fn approving_the_card_executes_the_action_the_card_described() {
            let (harness, run) =
                drive(vec![answer("CLICK"), answer("CLICK"), answer("DONE")]).await;
            let mut events = harness.runtime.subscribe();

            let card = next_card(&mut events).await;
            assert_eq!(card.cause, "label:pay", "the rule that stopped the run");
            assert!(
                card.action_sentence.contains("Pay $42.00 now"),
                "the card names what is about to happen: {}",
                card.action_sentence
            );
            assert_eq!(
                card.context.as_deref(),
                Some("Checkout — https://shop.test/cart")
            );
            assert!(!card.can_remember, "Q2 approvals are single-shot");
            harness
                .runtime
                .resolve_confirm(card.id, GateOutcome::Confirmed, ResolutionVia::Card)
                .expect("the card is waiting");

            let outcome = run.await.expect("the run task did not panic");
            assert_eq!(
                outcome.expect("the run completes"),
                Outcome::Done,
                "the approved run carried on to the end"
            );
            assert_eq!(
                *harness.acted.lock().expect("no panic"),
                vec!["Pay $42.00 now".to_owned()],
                "exactly the approved action ran"
            );
        }

        /// Denying must not execute, and must not end the run either: the
        /// refused action is withdrawn and the next decision has to cope.
        #[tokio::test]
        async fn denying_the_card_executes_nothing() {
            let done_without_the_button = {
                let mut body = answer("DONE");
                body["answers"]["operation"]["probabilities"] =
                    json!({ "DONE": 0.9, "BLOCKED": 0.1 });
                body
            };
            let (harness, run) = drive(vec![answer("CLICK"), done_without_the_button]).await;
            let mut events = harness.runtime.subscribe();

            let card = next_card(&mut events).await;
            harness
                .runtime
                .resolve_confirm(card.id, GateOutcome::Denied, ResolutionVia::Card)
                .expect("the card is waiting");

            let outcome = run.await.expect("the run task did not panic");
            assert_eq!(outcome.expect("the run completes"), Outcome::Done);
            assert!(
                harness.acted.lock().expect("no panic").is_empty(),
                "a denied action must never execute"
            );
        }
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
        assert!(
            (AppOptions::unattended(&settings, "TextEdit", "type").confirm_at - 0.25).abs() < 1e-6
        );
    }

    /// The default run is the user's own Chrome: headed, and on the profile
    /// their logins live in. It used to be headless on a directory thrown
    /// away at the end, which put every logged-in site out of reach (B4).
    #[test]
    fn the_default_browser_run_is_the_users_own_chrome() {
        let options = BrowserOptions::unattended(&settings(), "https://example.com", "read it");
        assert!(!options.headless);
        assert_eq!(
            options.profile, None,
            "no profile means the managed one, not a throwaway"
        );
        assert!(!throwaway(&options));
        assert_eq!(
            profile_of(Path::new("/data"), &options, None),
            Path::new("/data/chrome")
        );
        assert!(options.attach.is_empty());
        assert!(options.safety_heads);
        assert!(AppOptions::unattended(&settings(), "TextEdit", "type").safety_heads);
    }

    /// `--headless` is the eval and CI opt-in, and it has to leave nothing
    /// behind — but only when it was not also handed a profile. A headless
    /// run on a profile somebody signed into by hand must use that profile,
    /// not silently throw it away.
    #[test]
    fn headless_is_a_throwaway_only_when_no_profile_was_named() {
        let mut options = BrowserOptions::unattended(&settings(), "https://example.com", "read");
        options.headless = true;
        assert!(throwaway(&options));
        assert_eq!(
            profile_of(Path::new("/data"), &options, Some(Path::new("/tmp/t1"))),
            Path::new("/tmp/t1")
        );

        options.profile = Some(PathBuf::from("/home/me/chrome"));
        assert!(!throwaway(&options));
        assert_eq!(
            profile_of(Path::new("/data"), &options, None),
            Path::new("/home/me/chrome")
        );
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
            &Outcome::Blocked {
                reason: jev_nav::BlockReason::NoProgress,
            },
            1,
            None,
            None,
        );
        assert!(blocked.contains("Blocked"));
        // The navigator's own reason has to survive into the observation, or
        // the model cannot tell a timeout from a refusal.
        assert!(blocked.contains("three actions in a row changed nothing"));
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

    /// A goal that needs typing used to launch Chrome, run several steps and
    /// die at the first `TYPE_TEXT` with a warning nobody had read (B5). The
    /// refusal has to happen before anything starts, and it has to name the
    /// fix — a refusal that does not is just a different dead end.
    #[tokio::test]
    async fn a_run_with_nothing_to_type_with_is_refused_before_anything_launches() {
        let directory = match tempfile::TempDir::new() {
            Ok(directory) => directory,
            Err(error) => panic!("{error}"),
        };
        let runtime = match Runtime::open(directory.path()) {
            Ok(runtime) => Arc::new(runtime),
            Err(error) => panic!("{error}"),
        };
        // With a Jev key in place, the only thing missing is the helper: the
        // default settings select a runtime that cannot answer.
        if let Err(error) = runtime.set_key(neo_keys::ACCOUNT_TYPESAFE, "ts-fixture-key") {
            panic!("{error}");
        }

        let options = BrowserOptions::unattended_with(0.4, "https://example.com", "read it");
        let error =
            match run_browser(&runtime, &options, RunId::new(), &CancellationToken::new()).await {
                Err(error) => error,
                Ok(_) => panic!("a run that cannot type must not start"),
            };
        assert!(matches!(error, ToolError::NoTextHelper), "{error}");
        let message = error.to_string();
        assert!(
            message.contains("anthropic-oauth") && message.contains("runtime"),
            "the refusal must name the fix: {message}"
        );
        // Nothing launched: `Browser::launch` creates the profile directory
        // before it spawns anything, so its absence is proof.
        assert!(!directory.path().join(CHROME_PROFILE).exists());
    }

    /// Every run leaves its tab open so the user can see the work, which
    /// without a cap is an unbounded pile of windows (10 §10). The oldest go
    /// first, and a tab already in the ledger must not age one out for free.
    #[test]
    fn the_owned_tab_ledger_closes_the_oldest_over_the_cap() {
        let known: Vec<String> = (0..OWNED_TABS).map(|n| format!("t{n}")).collect();
        let (keep, close) = capped(known.clone(), "fresh");
        assert_eq!(close, vec!["t0".to_owned()]);
        assert_eq!(keep.len(), OWNED_TABS);
        assert_eq!(keep.last().map(String::as_str), Some("fresh"));

        // Re-opening a tab already owned moves it to the end and closes
        // nothing: the cap counts tabs, not runs.
        let (keep, close) = capped(known, "t0");
        assert!(close.is_empty());
        assert_eq!(keep.last().map(String::as_str), Some("t0"));
        assert_eq!(keep.len(), OWNED_TABS);
    }

    /// Observe, decide and act regress for different reasons, so the summary
    /// reports them apart (B3). The helper's p50 counts only the steps that
    /// called it: a run is mostly clicks, and folding their zeros in would
    /// report a slow helper as instant.
    #[test]
    fn the_summary_times_each_phase_and_the_helper_separately() {
        let mut timings = Timings::default();
        for (observe, jev, text, act) in [(10, 300, 0, 5), (30, 500, 900, 7), (20, 400, 1100, 9)] {
            timings.push(&step_event(observe, jev, text, act));
        }
        assert_eq!(
            timings.line(),
            "3 jev requests (median 400 ms) · observe 20 ms · act 7 ms · text p50 1100 ms"
        );

        // A run that never typed reports no helper time rather than a zero
        // that looks like a measurement.
        let mut clicks = Timings::default();
        clicks.push(&step_event(10, 300, 0, 5));
        assert!(clicks.line().ends_with("text p50 0 ms"));
    }

    fn step_event(
        observe_ms: u128,
        jev_ms: u128,
        text_ms: u128,
        act_ms: u128,
    ) -> jev_nav::StepEvent {
        jev_nav::StepEvent {
            elapsed: std::time::Duration::from_millis(1),
            decision: jev_nav::policy::Decision {
                operation: "CLICK".to_owned(),
                operation_confidence: 0.9,
                operation_probabilities: std::collections::BTreeMap::new(),
                action: None,
                target: None,
                target_confidence: None,
                safety: std::collections::BTreeMap::new(),
            },
            label: None,
            typed: None,
            observe_ms,
            jev_ms,
            text_ms,
            act_ms,
            candidates: 1,
            stale: false,
            usage: serde_json::Value::Null,
        }
    }
}
