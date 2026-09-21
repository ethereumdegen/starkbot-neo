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
use neo_agent::agent::{AppOptions, BrowserOptions, ToolError, run_app as drive_app, run_browser};
use neo_agent::ax::{AxRequest, AxResponse};
use neo_core::events::{GateOutcome, ResolutionVia};
use neo_core::{AppEvent, Envelope, RunId};
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;

/// Options `neo nav` accepts.
pub struct NavOptions {
    pub url: String,
    pub goal: String,
    /// Run without a window, in a profile that dies with the run — the
    /// fixture and CI shape. Since P17 an ordinary run is headless anyway, so
    /// what this flag still decides is the *profile*: asking for it here is
    /// asking to leave nothing behind, and without it the run uses the app's
    /// own Chrome where the user's logins persist (B4).
    pub headless: bool,
    /// Turn off the Jev safety heads. Off means *no* head is asked, which also
    /// means nothing can trip the confirm gate: only for fixtures.
    pub no_safety: bool,
    /// Files a file input may be given (05 §8: nothing else is readable).
    pub attach: Vec<PathBuf>,
    /// A Chrome profile directory to use instead of the app's own.
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
    request.headless = options.headless || request.headless;
    // The flag's whole remaining meaning: discard the profile afterwards.
    request.throwaway = options.headless;
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
        ToolError::NoTextHelper => anyhow!(
            "nothing can type field values yet: \
             `neo settings patch models '{{\"inference\":{{\"provider\":\"anthropic-oauth\",\"id\":\"sol-latest\"}}}}'` \
             (or `openai-codex`, or `claude-subscription`)"
        ),
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
                Ok(envelope) => {
                    report(id, &envelope);
                    offer_card(runtime, &envelope);
                }
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

/// Put a confirm or a question to whoever is at the terminal.
///
/// Without this a terminal run is a dead end of a new kind: the navigator
/// pauses correctly, publishes its card, and then waits for a front end that
/// a piped `neo nav` does not have — so the run hangs until the card times
/// out. Measured, on the fixture that trips `spends`: 180 s of nothing.
///
/// The answer is read on a blocking thread so the trace keeps streaming
/// underneath the prompt, and a run with no terminal attached (a pipe, CI,
/// a cron) is refused immediately rather than waiting for a person who is
/// not there.
fn offer_card(runtime: &Arc<Runtime>, envelope: &Envelope) {
    use std::io::IsTerminal;

    let runtime = Arc::clone(runtime);
    match envelope.event.clone() {
        AppEvent::ConfirmRequest { confirm } => {
            let interactive = std::io::stdin().is_terminal();
            report_line(&format!("\n⏸  {}", confirm.action_sentence));
            if let Some(context) = &confirm.context {
                report_line(&format!("   {context}"));
            }
            tokio::spawn(async move {
                let outcome = if interactive {
                    match ask_line("   [y] do it · [n] don't → ").await.as_deref() {
                        Some("y" | "Y" | "yes") => GateOutcome::Confirmed,
                        _ => GateOutcome::Denied,
                    }
                } else {
                    report_line("   no terminal to ask: refused");
                    GateOutcome::Denied
                };
                let _ = runtime.resolve_confirm(confirm.id, outcome, ResolutionVia::Card);
            });
        }
        AppEvent::AskRequest { ask } => {
            let interactive = std::io::stdin().is_terminal();
            report_line(&format!("\n⏸  {}", ask.question));
            for (index, option) in ask.options.iter().enumerate() {
                report_line(&format!("   {}) {option}", index + 1));
            }
            tokio::spawn(async move {
                if !interactive {
                    report_line("   no terminal to ask: stopping here");
                    let _ = runtime.cancel_ask(ask.id);
                    return;
                }
                match ask_line("   → ").await {
                    // A number picks an offered option; anything else is the
                    // answer itself, which is what a field value needs.
                    Some(answer) => {
                        let chosen = answer
                            .parse::<usize>()
                            .ok()
                            .and_then(|n| ask.options.get(n.saturating_sub(1)).cloned())
                            .unwrap_or(answer);
                        let _ = runtime.answer_ask(ask.id, chosen, ResolutionVia::Card);
                    }
                    None => {
                        let _ = runtime.cancel_ask(ask.id);
                    }
                }
            });
        }
        _ => {}
    }
}

/// One line from the person at the terminal, off the async runtime.
async fn ask_line(prompt: &str) -> Option<String> {
    let prompt = prompt.to_owned();
    tokio::task::spawn_blocking(move || {
        use std::io::{BufRead, Write};

        let mut stderr = std::io::stderr();
        write!(stderr, "{prompt}").ok()?;
        stderr.flush().ok()?;
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).ok()?;
        let line = line.trim().to_owned();
        (!line.is_empty()).then_some(line)
    })
    .await
    .ok()
    .flatten()
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
