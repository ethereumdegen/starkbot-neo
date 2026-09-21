//! `neo eval` — run the app-control evaluation suite.
//!
//! An eval is not a unit test: it drives real applications with a real model,
//! so it costs time and plan quota and it cannot run in CI. It lives behind
//! its own command for that reason, and it reports the application's state
//! rather than the model's prose ([`neo_eval`]).
//!
//! The run itself is [`neo_eval::run_suite`], not this file. The command is a
//! renderer: it turns flags into a [`Selection`], prints what the library
//! answers, and chooses the exit code. The desktop app calls the same function
//! and draws the same report in a window.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use neo_agent::Runtime;
use neo_core::RunId;
use neo_eval::{CaseListing, Selection, SuiteReport, availability, cases, list_cases};
use tokio_util::sync::CancellationToken;

pub struct EvalOptions {
    pub filter: Option<String>,
    pub tags: Vec<String>,
    pub once: bool,
    pub report: Option<PathBuf>,
    pub baseline: Option<PathBuf>,
    pub list: bool,
}

pub async fn run(data_dir: Option<PathBuf>, options: EvalOptions) -> Result<()> {
    if options.list {
        list();
        return Ok(());
    }

    let data_dir = match data_dir {
        Some(path) => path,
        None => crate::default_data_dir()?,
    };
    let runtime = Arc::new(
        tokio::task::block_in_place(|| Runtime::open(&data_dir))
            .context("could not open Starkbot Neo's runtime")?,
    );

    let selection = Selection {
        filter: options.filter,
        tags: options.tags,
        once: options.once,
    };
    report_plan(&selection);

    // Ctrl-C would otherwise abort the process with a browser still open and a
    // document half typed; the token unwinds the turn instead.
    let cancel = CancellationToken::new();
    let interrupt = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            interrupt.cancel();
        }
    });

    // The library's messages already name the fix (and the cancelled case
    // says how far it got); re-wording them here would make the desktop app
    // and the terminal disagree about the same failure.
    let report = neo_eval::run_suite(&runtime, selection, RunId::new(), &cancel).await?;

    render(&report, options.report.as_deref(), options.baseline.as_deref());

    if report.failed > 0 {
        anyhow::bail!(
            "{} of {} cases failed — see the report above",
            report.failed,
            report.total
        );
    }
    Ok(())
}

/// The report, plus the baseline diff and the saved copy the flags asked for.
///
/// `run_suite` deliberately returns the report rather than printing it, so
/// every decision about a terminal is made here.
#[allow(clippy::print_stdout)]
fn render(report: &SuiteReport, save_to: Option<&std::path::Path>, baseline: Option<&std::path::Path>) {
    report.print_console();

    if let Some(path) = save_to
        && let Err(error) = report.save_to_file(path)
    {
        eprintln!("could not save the report: {error}");
    }

    if let Some(path) = baseline {
        match SuiteReport::load_from_file(path) {
            Ok(previous) => report.diff_against(&previous).print_console(),
            Err(error) => eprintln!("  (no baseline diff: {error})"),
        }
    }
}

/// What would run, and what is missing, without spending a token.
#[allow(clippy::print_stdout)]
fn list() {
    println!("target applications");
    for (app, installed) in availability() {
        let mark = if installed.is_some() { "ok  " } else { "none" };
        let detail = installed
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "not installed — its cases are skipped".to_owned());
        println!("  {mark} {:<20} {detail}", app.label());
    }
    println!("\ncases");
    for case in list_cases() {
        let runnable = if case.runnable() { "run " } else { "skip" };
        println!(
            "  {runnable} {:<26} {:<12} [{}]",
            case.id,
            case.app.label(),
            case.tags.join(" ")
        );
    }
}

#[allow(clippy::print_stdout)]
fn report_plan(selection: &Selection) {
    let runs = if selection.once {
        "1 run each".to_owned()
    } else {
        format!(
            "{} runs each, {} must pass",
            cases::CONSENSUS_RUNS,
            cases::CONSENSUS_REQUIRED
        )
    };
    // The same predicate `run_suite` applies, over the same listing `--list`
    // shows, so the count printed here is the count that runs.
    let listed = list_cases();
    let selected = listed
        .iter()
        .filter(|case| selection.selects(case) && case.runnable())
        .count();
    println!("running {selected} case(s) · {runs} · one at a time (they share the keyboard)");

    let skipped: Vec<&str> = listed
        .iter()
        .filter(|case: &&CaseListing| !case.runnable())
        .map(|case| case.app.label())
        .collect();
    if !skipped.is_empty() {
        println!("skipping cases for: {}", skipped.join(", "));
    }
    println!();
}
