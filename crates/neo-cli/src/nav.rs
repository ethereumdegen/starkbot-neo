//! `neo nav`, `neo app` and `neo ax` — arguments in, JSON out.
//!
//! Every one of these three used to be implemented here: a second copy of
//! `neo_agent::agent`'s navigator orchestration with extra flags, and the
//! whole accessibility surface with no library twin at all. A desktop window
//! cannot shell out to itself, so both capabilities were reachable only by
//! typing. They now live in `neo-agent`; what is left here is the mapping
//! from clap's types onto the library's, and printing.
//!
//! A run's live trace is no longer printed by the code that produces it — it
//! is published as [`AppEvent::NavStep`], and this module is simply one
//! subscriber that renders it to stderr. stdout stays JSON for whatever
//! called us.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use neo_agent::Runtime;
use neo_agent::agent::{
    AppOptions, BrowserOptions, ToolError, run_app as drive_app, run_browser,
};
use neo_agent::ax::{AxRequest, AxResponse};
use neo_core::{AppEvent, Envelope, RunId};
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;

/// Options `neo nav` accepts.
pub struct NavOptions {
    pub url: String,
    pub goal: String,
    /// Show the browser window. A headless run is the default.
    pub headed: bool,
    /// Turn off the Jev safety heads. Off means *no* head is asked, which also
    /// means nothing can trip the confirm gate: only for fixtures.
    pub no_safety: bool,
    /// Files a file input may be given (05 §8: nothing else is readable).
    pub attach: Vec<PathBuf>,
    /// A profile directory to reuse, so a logged-in session survives runs.
    pub profile: Option<PathBuf>,
}

/// Options `neo nav app` accepts.
pub struct AppNavOptions {
    /// Which app: a bundle id, a pid, or a name substring.
    pub app: String,
    pub goal: String,
}

/// Drive a web page to a goal with Jev (10, spike S1 promoted).
///
/// # Errors
///
/// Fails when settings cannot be read, or when the run itself fails.
pub async fn run(runtime: Arc<Runtime>, options: NavOptions) -> Result<()> {
    let settings = tokio::task::block_in_place(|| runtime.settings())?;
    let mut request = BrowserOptions::unattended(&settings, options.url, options.goal);
    request.headed = options.headed;
    request.safety_heads = !options.no_safety;
    request.attach = options.attach;
    request.profile = options.profile;

    let id = RunId::new();
    let cancel = CancellationToken::new();
    let outcome = traced(&runtime, id, run_browser(&runtime, &request, id, &cancel))
        .await
        .map_err(in_terminal)?
        .outcome;
    print_json(&serde_json::json!({ "outcome": format!("{outcome:?}") }))
}

/// Drive a native macOS app to a goal with Jev (01, A23).
///
/// # Errors
///
/// Fails when settings cannot be read, or when the run itself fails.
pub async fn run_app(runtime: Arc<Runtime>, options: AppNavOptions) -> Result<()> {
    let settings = tokio::task::block_in_place(|| runtime.settings())?;
    let request = AppOptions::unattended(&settings, options.app, options.goal);

    let id = RunId::new();
    let cancel = CancellationToken::new();
    let outcome = traced(&runtime, id, drive_app(&runtime, &request, id, &cancel))
        .await
        .map_err(in_terminal)?
        .outcome;
    print_json(&serde_json::json!({ "outcome": format!("{outcome:?}") }))
}

/// `neo ax …` — the direct accessibility surface.
///
/// # Errors
///
/// Fails when Accessibility is not granted, or when the request does.
pub async fn run_ax(runtime: Arc<Runtime>, request: AxRequest) -> Result<()> {
    let response: AxResponse =
        neo_agent::ax::ax(&runtime, request, RunId::new(), &CancellationToken::new()).await?;
    print_json(&response)
}

/// The library's failures, worded for whoever is reading them here.
///
/// `ToolError` points a desktop user at Connections, which does not exist in
/// a terminal: a headless user needs the command that fixes it. Everything
/// else already reads the same either way.
fn in_terminal(error: ToolError) -> anyhow::Error {
    match error {
        ToolError::MissingJevKey => {
            anyhow!("the navigator needs a TypeSafe key: `neo keys set typesafe`")
        }
        other => other.into(),
    }
}

/// Run `work`, rendering its [`AppEvent::NavStep`] events to stderr as they
/// are published.
///
/// The subscription is opened before the work starts and drained after it
/// finishes, so no line is lost either end; polling the events first
/// (`biased`) keeps the trace live rather than arriving in a burst at the end.
async fn traced<T, E>(
    runtime: &Arc<Runtime>,
    id: RunId,
    work: impl Future<Output = Result<T, E>>,
) -> Result<T, E> {
    let mut events = runtime.subscribe();
    let mut running = std::pin::pin!(work);
    let mut listening = true;
    let result = loop {
        tokio::select! {
            biased;
            received = events.recv(), if listening => match received {
                Ok(envelope) => report(id, &envelope),
                // A front end that fell behind re-bootstraps; a terminal
                // trace just says so and keeps going.
                Err(RecvError::Lagged(missed)) => {
                    report_line(&format!("… {missed} lines dropped"));
                }
                // The runtime outlives this call, so this cannot happen —
                // but a closed channel is always ready, and polling one in a
                // loop would burn a core until the run ended.
                Err(RecvError::Closed) => listening = false,
            },
            result = &mut running => break result,
        }
    };
    while let Ok(envelope) = events.try_recv() {
        report(id, &envelope);
    }
    result
}

/// One event, if it belongs to this run. Another window's run publishes onto
/// the same bus, and a terminal trace that interleaved two of them would be
/// unreadable.
fn report(id: RunId, envelope: &Envelope) {
    if let AppEvent::NavStep { run, line, .. } = &envelope.event
        && *run == id
    {
        report_line(line);
    }
}

/// A navigator run is a live trace, so it goes to stderr: stdout stays JSON
/// for whatever called us.
fn report_line(line: &str) {
    eprintln!("{line}");
}

#[allow(clippy::print_stdout)]
fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
