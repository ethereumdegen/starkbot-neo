//! What this front end started, and what each of those runs is doing.
//!
//! A run is one thing the user asked for — a chat turn, a `/nav`, an `/app`, an
//! `/ax` call, an eval suite — identified by the [`RunId`] the loop minted
//! before it spawned the work. Everything below is reduced from the event
//! stream: [`AppEvent::TurnStep`] and friends, [`AppEvent::NavStep`],
//! [`AppEvent::EvalCase`]. Nothing here polls, and nothing here is derived
//! from a callback, which is what lets a second observer follow a run it did
//! not start.
//!
//! Time is not read here. The loop hands the reducer a monotonic millisecond
//! count (`State::tick`) and a run records the value it saw when it started
//! and when it ended, so elapsed is computable in a unit test without a clock.

use std::collections::VecDeque;

use neo_core::{EvalCaseState, NavDecision, NavStepKind, RunId};

/// How many trace lines one run keeps. A navigator run publishes a decision
/// every few hundred milliseconds for minutes; the pane shows the tail, and
/// the whole trace is persisted core-side regardless of what the TUI holds.
pub const TRACE_CAP: usize = 300;

/// How many finished runs the pane keeps before the oldest is dropped. A
/// running run is never dropped: it is the one thing `x` has to be able to
/// reach.
pub const RUNS_CAP: usize = 50;

/// Which capability a run is exercising. The pane groups by this and the
/// trace styles by it, so it is a value rather than a prefix on a string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunKind {
    Chat,
    Nav,
    App,
    Ax,
    Eval,
}

impl RunKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Nav => "nav",
            Self::App => "app",
            Self::Ax => "ax",
            Self::Eval => "eval",
        }
    }
}

/// Where a run got to.
///
/// [`RunState::Stopping`] exists because cancellation is not instant: the
/// token has to reach the navigator, which closes its Chrome on the way out.
/// Rendering `cancelled` the moment `x` is pressed would claim a stop that
/// has not happened yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunState {
    Running,
    /// The token was cancelled; the run has not unwound yet.
    Stopping,
    Done,
    Failed(String),
    Cancelled,
}

impl RunState {
    pub const fn word(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Done => "done",
            Self::Failed(_) => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub const fn glyph(&self) -> char {
        match self {
            Self::Running => '▶',
            Self::Stopping => '◐',
            Self::Done => '✓',
            Self::Failed(_) => '×',
            Self::Cancelled => '⊘',
        }
    }

    pub const fn live(&self) -> bool {
        matches!(self, Self::Running | Self::Stopping)
    }
}

/// What a trace line is, so the renderer can style it without parsing prose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceKind {
    /// The model's chosen action, published before it runs.
    Step,
    /// The model's own reason for that action.
    Thought,
    /// What the action produced.
    Observation,
    /// Prose from inside a step.
    Note,
    /// A surface was opened or brought forward.
    Launch,
    /// One observe → decide → act cycle.
    Decision,
    /// How a navigator run ended.
    Outcome,
    /// What the navigator made of the whole run.
    Summary,
    /// One eval case moving.
    Case,
    /// How the whole run ended, as the loop saw it return.
    Result,
}

impl TraceKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Step => "step",
            Self::Thought => "why",
            Self::Observation => "saw",
            Self::Note => "note",
            Self::Launch => "launch",
            Self::Decision => "decide",
            Self::Outcome => "outcome",
            Self::Summary => "summary",
            Self::Case => "case",
            Self::Result => "result",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceLine {
    pub kind: TraceKind,
    pub text: String,
}

/// One thing the user started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub id: RunId,
    pub kind: RunKind,
    /// What was asked for, in one line — the goal, the message, the filter.
    pub title: String,
    pub state: RunState,
    /// The loop's monotonic millisecond count when this was registered.
    pub started_ms: u64,
    /// The same count when it settled. `None` while it is live.
    pub ended_ms: Option<u64>,
    /// Steps completed, or eval cases seen.
    pub steps: u32,
    /// The most recent line, for the one-line pane row.
    pub last: String,
    pub trace: VecDeque<TraceLine>,
}

impl Run {
    #[must_use]
    pub fn new(id: RunId, kind: RunKind, title: String, now_ms: u64) -> Self {
        Self {
            id,
            kind,
            title,
            state: RunState::Running,
            started_ms: now_ms,
            ended_ms: None,
            steps: 0,
            last: String::new(),
            trace: VecDeque::new(),
        }
    }

    /// Whether this run can still produce events.
    #[must_use]
    pub const fn is_live(&self) -> bool {
        self.state.live()
    }

    /// How long this has been going, in milliseconds of the loop's clock.
    #[must_use]
    pub const fn elapsed_ms(&self, now_ms: u64) -> u64 {
        let end = match self.ended_ms {
            Some(end) => end,
            None => now_ms,
        };
        end.saturating_sub(self.started_ms)
    }

    /// `m:ss`, which is what a run that takes minutes needs and what a run
    /// that takes seconds still reads correctly as.
    #[must_use]
    pub fn elapsed(&self, now_ms: u64) -> String {
        let seconds = self.elapsed_ms(now_ms) / 1000;
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }

    pub fn push(&mut self, kind: TraceKind, text: impl Into<String>) {
        let text = text.into();
        if self.trace.len() == TRACE_CAP {
            self.trace.pop_front();
        }
        self.trace.push_back(TraceLine { kind, text });
    }

    /// Settle the run, unless it already settled. A cancelled run reports
    /// `TurnFailed` as well as returning an error, and whichever arrives
    /// first is the one that decided.
    pub fn settle(&mut self, state: RunState, now_ms: u64) {
        if !self.state.live() {
            return;
        }
        self.state = state;
        self.ended_ms = Some(now_ms);
    }
}

/// Reduce one navigator step into a trace line.
///
/// A [`NavStepKind::Decision`] is rendered through [`NavDecision`]'s own
/// `Display` rather than through the event's pre-rendered `line`: the two are
/// the same today, and going through `Display` is what keeps them the same
/// when the producer changes its mind.
#[must_use]
pub fn nav_trace(line: &str, kind: &NavStepKind) -> TraceLine {
    match kind {
        NavStepKind::Launch => TraceLine {
            kind: TraceKind::Launch,
            text: line.to_owned(),
        },
        NavStepKind::Decision(decision) => TraceLine {
            kind: TraceKind::Decision,
            text: decision_line(decision),
        },
        NavStepKind::Outcome => TraceLine {
            kind: TraceKind::Outcome,
            text: line.to_owned(),
        },
        NavStepKind::Summary => TraceLine {
            kind: TraceKind::Summary,
            text: line.to_owned(),
        },
    }
}

/// One decision, in the CLI's exact words.
#[must_use]
pub fn decision_line(decision: &NavDecision) -> String {
    decision.to_string().trim_end().to_owned()
}

/// Reduce one eval case event into a trace line.
#[must_use]
pub fn eval_line(index: u32, total: u32, case: &str, state: &EvalCaseState) -> String {
    let position = format!("{}/{total}", index.saturating_add(1));
    match state {
        EvalCaseState::Started => format!("{position} {case} · started"),
        EvalCaseState::Passed { runs } => format!("{position} {case} · passed ({runs} run(s))"),
        EvalCaseState::Failed { runs, detail } => {
            format!("{position} {case} · FAILED ({runs} run(s)) — {detail}")
        }
        EvalCaseState::Skipped { reason } => format!("{position} {case} · skipped — {reason}"),
    }
}
