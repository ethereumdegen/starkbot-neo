// Each test binary links its own copy of this module and uses part of it.
#![allow(dead_code)]

//! Hand-written fixtures and a deterministic buffer dump shared by the TUI
//! tests. No clock, no TTY, no terminal-capability probing (14 §6).

use std::path::PathBuf;

use neo_agent::runtime::{BRIDGE_VERSION, Bootstrap, StoreInfo};
use neo_core::{
    ActionKind, ActionSummary, AppEvent, ConversationId, Envelope, InferenceConnection, KeyState,
    KeyStatus, NavDecision, NavSurface, PROVIDER_ANTHROPIC, PROVIDER_OPENAI, RunId, Settings,
};
use neo_tui::{Painted, State};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier, Style};

/// A store that never exists on disk: the path is a fixture, so the snapshots
/// are the same on every machine.
pub fn store_info() -> StoreInfo {
    StoreInfo {
        path: PathBuf::from("/fixtures/neo/neo.db"),
        schema_version: 3,
        application_id: 0x4E45_4F31,
    }
}

/// A run id that is the same on every machine, so a runs-pane snapshot is
/// about the run and not about a v7 UUID's clock.
pub fn run_id(nth: u8) -> RunId {
    format!("00000000-0000-7000-8000-0000000000{nth:02}")
        .parse()
        .unwrap_or_else(|error| panic!("fixture run id {nth} is not a uuid: {error}"))
}

pub fn conversation_id(nth: u8) -> ConversationId {
    format!("00000000-0000-7000-9000-0000000000{nth:02}")
        .parse()
        .unwrap_or_else(|error| panic!("fixture conversation id {nth} is not a uuid: {error}"))
}

pub fn project() -> neo_core::Project {
    neo_core::Project {
        slug: "q4-launch".into(),
        name: "Q4 Launch".into(),
        root: "/fixtures/projects/q4-launch".into(),
        heartbeat_enabled: true,
        heartbeat_every_seconds: 14_400,
        on_gate: neo_core::HeartbeatGate::Hold,
        last_tick_at: Some(1_780_000_000_000),
        next_due_at: Some(1_780_014_400_000),
        consecutive_failures: 0,
        created_at: 1_779_000_000_000,
        updated_at: 1_780_000_000_000,
    }
}

/// What the model says it is about to do, in the shape `TurnStep` carries.
pub fn browse(target: &str, goal: &str) -> ActionSummary {
    ActionSummary {
        kind: ActionKind::Browse,
        target: Some(target.to_owned()),
        goal: Some(goal.to_owned()),
        text: None,
    }
}

/// One navigator decision with every timing pinned, so the line the Mind
/// pane renders is exactly `NavDecision`'s own `Display` and a change to
/// that formatting fails here rather than silently reshaping the pane.
pub fn decision() -> NavDecision {
    NavDecision {
        surface: NavSurface::Browser,
        operation: "CLICK".to_owned(),
        label: Some("Compose post".to_owned()),
        operation_confidence: 0.97,
        target_confidence: Some(0.91),
        candidates: 12,
        stale: false,
        typed_chars: None,
        observe_ms: 41,
        jev_ms: 164,
        text_ms: 0,
        act_ms: 22,
        elapsed_ms: 1_284,
        safety: vec![
            ("outward".to_owned(), 0.08),
            ("destructive".to_owned(), 0.01),
        ],
    }
}

/// The state a fresh install reaches after adding an OpenAI key and TypeSafe:
/// one usable inference connection, no Anthropic key, no subscription.
pub fn bootstrap() -> Bootstrap {
    Bootstrap {
        doctor: doctor_report(),
        bridge_version: BRIDGE_VERSION,
        settings: Settings::default(),
        // What the default `sol-latest` means on the OpenAI key this fixture
        // has: the goldens render the id a turn would really send.
        inference_model: neo_core::OPENAI_SOL_FALLBACK.to_owned(),
        keys: vec![
            KeyStatus::new(PROVIDER_OPENAI, KeyState::Present),
            KeyStatus::new(PROVIDER_ANTHROPIC, KeyState::Missing),
            KeyStatus::new("typesafe", KeyState::Present),
        ],
        inference: InferenceConnection::OpenAiKey,
        account: None,
        accounts: Vec::new(),
        projects: Vec::new(),
        store: store_info(),
    }
}

/// A truly fresh install: nothing in the Keychain, so no inference connection
/// and the K6 typed-only notice.
pub fn empty_bootstrap() -> Bootstrap {
    Bootstrap {
        keys: vec![
            KeyStatus::new(PROVIDER_OPENAI, KeyState::Missing),
            KeyStatus::new(PROVIDER_ANTHROPIC, KeyState::Missing),
            KeyStatus::new("typesafe", KeyState::Missing),
        ],
        inference: InferenceConnection::None,
        ..bootstrap()
    }
}

pub fn state() -> State {
    State::new(bootstrap())
}

pub fn render(state: &State, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => panic!("test backend refused a terminal: {error}"),
    };
    // The renderer's report is what the loop folds back into the state; a
    // snapshot only wants the cells, so `paint` is the one that keeps it.
    if let Err(error) = terminal.draw(|frame| {
        neo_tui::draw(frame, state);
    }) {
        panic!("draw failed: {error}");
    }
    dump(terminal.backend().buffer())
}

/// Draw a frame the way the event loop does: the renderer's report goes back
/// into the state, so `Card::rendered` — and therefore the arming rule —
/// behaves here exactly as it does at runtime (04 §13).
pub fn paint(state: &mut State, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => panic!("test backend refused a terminal: {error}"),
    };
    let mut painted = Painted::default();
    let frame_state = &*state;
    if let Err(error) = terminal.draw(|frame| painted = neo_tui::draw(frame, frame_state)) {
        panic!("draw failed: {error}");
    }
    state.painted(painted);
    dump(terminal.backend().buffer())
}

/// The shared card corpus (16 §5.5): one JSONL of envelopes that drives both
/// these goldens and the webview's reducer tests, so the two front ends
/// cannot drift apart without one of the two suites failing.
pub fn corpus() -> Vec<AppEvent> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/cards/envelopes.jsonl");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => panic!("the card corpus is missing at {}: {error}", path.display()),
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| match serde_json::from_str::<Envelope>(line) {
            Ok(envelope) => envelope.event,
            Err(error) => {
                panic!("the card corpus has a line this front end cannot read: {error}\n{line}")
            }
        })
        .collect()
}

/// One envelope from the corpus by its `seq`, which is how the two suites
/// name the same case without copying its contents into either of them.
pub fn envelope(seq: usize) -> AppEvent {
    let mut events = corpus();
    if seq == 0 || seq > events.len() {
        panic!("the card corpus has no envelope {seq}");
    }
    events.remove(seq - 1)
}

/// The frame as text plus a style legend, so a style-plumbing regression fails
/// loudly instead of silently (14 §6, 14 §8).
pub fn dump(buffer: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out.push_str(
                buffer
                    .cell((x, y))
                    .map_or(" ", ratatui::buffer::Cell::symbol),
            );
        }
        while out.ends_with(' ') {
            out.pop();
        }
        out.push('\n');
    }
    let mut legend: Vec<String> = Vec::new();
    for y in 0..buffer.area.height {
        let mut run = String::new();
        let mut run_style: Option<Style> = None;
        for x in 0..buffer.area.width {
            let Some(cell) = buffer.cell((x, y)) else {
                continue;
            };
            let style = Style::new()
                .fg(cell.fg)
                .bg(cell.bg)
                .add_modifier(cell.modifier);
            if Some(style) != run_style {
                push_run(&mut legend, run_style, &run);
                run.clear();
                run_style = Some(style);
            }
            run.push_str(cell.symbol());
        }
        push_run(&mut legend, run_style, &run);
    }
    legend.sort_unstable();
    legend.dedup();
    out.push_str("--- styles ---\n");
    for entry in legend {
        out.push_str(&entry);
        out.push('\n');
    }
    out
}

fn push_run(legend: &mut Vec<String>, style: Option<Style>, run: &str) {
    let Some(style) = style else { return };
    let text = run.trim();
    if text.is_empty() {
        return;
    }
    let plain = style.fg.unwrap_or(Color::Reset) == Color::Reset
        && style.bg.unwrap_or(Color::Reset) == Color::Reset
        && style.add_modifier == Modifier::empty();
    if plain {
        return;
    }
    legend.push(format!(
        "fg={:?} bg={:?} mod={:?}  {text}",
        style.fg.unwrap_or(Color::Reset),
        style.bg.unwrap_or(Color::Reset),
        style.add_modifier
    ));
}

/// A fixed readiness report, so the Doctor rows are part of the rendered
/// snapshot rather than an empty section (05 §10).
fn doctor_report() -> neo_agent::doctor::DoctorReport {
    use neo_agent::doctor::{Check, DoctorReport, Health};
    DoctorReport {
        checks: vec![
            Check {
                name: "navigator (jev)".to_owned(),
                health: Health::Ok,
                detail: "typesafe key present".to_owned(),
                fix: None,
            },
            Check {
                name: "speech".to_owned(),
                health: Health::Warn,
                detail: "no openai key — typed-only, no speech in or out".to_owned(),
                fix: Some("`neo keys set openai`".to_owned()),
            },
        ],
    }
}
