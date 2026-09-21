//! Deterministic buffer snapshots (14 §6, 05 §13). Every frame below is drawn
//! from a hand-written `Bootstrap` and a scripted event/key stream through a
//! `TestBackend`; no clock, no TTY, no capability probing.

mod common;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use neo_core::{
    AppEvent, EvalCaseState, KeyState, ListenState, NavStepKind, PROVIDER_ANTHROPIC, TurnUsage,
};
use neo_tui::{KeyMap, RunKind, SessionRow, State, TraceKind, View};

const SECRET: &str = "sk-live-do-not-render-1234";

fn press(state: &mut State, code: KeyCode) {
    let key = KeyEvent::new(code, KeyModifiers::NONE);
    let action = KeyMap.resolve(key, state);
    state.apply_action(action);
}

fn open_settings(state: &mut State) {
    press(state, KeyCode::Char(','));
}

fn select_row(state: &mut State, label: &str) {
    let index = state
        .rows()
        .iter()
        .position(|row| row.label == label)
        .unwrap_or_else(|| panic!("no {label} row"));
    state.row = index;
}

#[test]
fn panes_at_120x40() {
    let mut state = common::state();
    state.apply(AppEvent::ListenState {
        state: ListenState::Muted,
        device: Some("MacBook Pro Microphone".into()),
        addressing: neo_core::AddressingMode::Open,
    });
    state.apply(AppEvent::KeyStatus {
        account: PROVIDER_ANTHROPIC.into(),
        status: KeyState::Present,
    });
    insta::assert_snapshot!("panes_120x40", common::render(&state, 120, 40));
}

#[test]
fn panes_at_100x30_keep_three_panes() {
    let state = common::state();
    insta::assert_snapshot!("panes_100x30", common::render(&state, 100, 30));
}

#[test]
fn panes_at_80x24_drop_to_two() {
    let mut state = common::state();
    press(&mut state, KeyCode::Char('3'));
    insta::assert_snapshot!("panes_80x24", common::render(&state, 80, 24));
}

#[test]
fn panes_at_60x20_collapse_to_one_with_a_tab_bar() {
    let state = common::state();
    insta::assert_snapshot!("panes_60x20", common::render(&state, 60, 20));
}

#[test]
fn below_the_minimum_one_line_is_drawn_and_events_keep_flowing() {
    let mut state = common::state();
    insta::assert_snapshot!("too_small_59x19", common::render(&state, 59, 19));
    // The frame is a single sentence, but the reducer still consumed the event.
    state.apply(AppEvent::Latency {
        step_ms_p50: 12,
        jev_ms_p50: 30,
    });
    assert_eq!(state.activity.len(), 1);
}

#[test]
fn the_activity_pane_shows_the_event_ring() {
    let mut state = common::state();
    state.apply(AppEvent::KeyStatus {
        account: PROVIDER_ANTHROPIC.into(),
        status: KeyState::Invalid,
    });
    state.apply(AppEvent::Latency {
        step_ms_p50: 41,
        jev_ms_p50: 168,
    });
    state.apply(AppEvent::Notice {
        level: neo_core::NoticeLevel::Info,
        code: "store.opened".into(),
        text: "store ready".into(),
    });
    press(&mut state, KeyCode::Char('3'));
    insta::assert_snapshot!("activity_pane", common::render(&state, 120, 40));
}

/// The runs pane is the list of what this front end started: one live run and
/// one that failed, each with its state word, its elapsed and its last line.
/// The words are what a monochrome terminal reads, so they are in the
/// snapshot rather than only the colours (14 §2).
#[test]
fn the_runs_pane_shows_a_running_and_a_failed_run() {
    let mut state = common::state();
    state.tick(0);
    state.start_run(
        common::run_id(1),
        RunKind::Nav,
        "https://example.com — read the headline",
    );
    state.apply(AppEvent::NavStep {
        run: common::run_id(1),
        step: 3,
        line: "opened https://example.com".into(),
        kind: NavStepKind::Launch,
    });

    state.tick(4_000);
    state.start_run(common::run_id(2), RunKind::Chat, "post the launch note");
    state.apply(AppEvent::TurnStep {
        run: common::run_id(2),
        step: 0,
        thought: "the composer is the only place a post can be written".into(),
        action: common::browse("https://x.com/compose", "post the launch note"),
    });
    state.tick(19_000);
    state.apply(AppEvent::TurnFailed {
        run: common::run_id(2),
        error: "the model could not be reached".into(),
    });

    state.tick(23_000);
    press(&mut state, KeyCode::Char('2'));
    let frame = common::render(&state, 120, 40);
    assert!(frame.contains("running"), "no live run on screen");
    assert!(frame.contains("failed"), "no failed run on screen");
    // Elapsed keeps moving while a run is live and freezes when it settles:
    // 23 s for the nav that started at 0, 15 s for the turn that started at
    // 4 s and failed at 19 s.
    assert!(
        frame.contains("0:23"),
        "the live run's elapsed is not ticking"
    );
    assert!(
        frame.contains("0:15"),
        "the failed run's elapsed did not freeze"
    );
    insta::assert_snapshot!("runs_pane", frame);
}

/// The Mind pane traces the selected run. A navigator decision is rendered
/// through `NavDecision`'s own `Display`, not through a second formatter
/// here — so the terminal and `neo nav`'s stderr say the same thing.
#[test]
fn the_mind_pane_traces_the_selected_run_through_nav_decision_display() {
    let mut state = common::state();
    state.tick(0);
    state.start_run(
        common::run_id(1),
        RunKind::Nav,
        "https://x.com — post the launch note",
    );
    let decision = common::decision();
    state.apply(AppEvent::NavStep {
        run: common::run_id(1),
        step: 1,
        line: "a line the pane must not prefer over Display".into(),
        kind: NavStepKind::Decision(Box::new(decision.clone())),
    });
    state.tick(1_300);

    // The contract is the trace line, not the pixels: at three panes the
    // Mind column is ~43 cells and a decision wraps across several rows, so
    // asserting on the frame would really be asserting on where the wrap
    // fell. The rendered frame is still snapshotted below.
    let traced = state
        .selected_run()
        .map(|run| run.trace.clone())
        .unwrap_or_default();
    assert_eq!(
        traced
            .iter()
            .find(|entry| entry.kind == TraceKind::Decision)
            .map(|entry| entry.text.clone()),
        Some(decision.to_string().trim_end().to_owned()),
        "the trace must be NavDecision's own Display, not a second formatter"
    );

    press(&mut state, KeyCode::Char('3'));
    let frame = common::render(&state, 120, 40);
    assert!(
        !frame.contains("must not prefer"),
        "the pane rendered the pre-rendered line instead of NavDecision::Display"
    );
    assert!(frame.contains("CLICK"), "no operation in the trace");
    assert!(
        frame.contains("outward=0.08"),
        "no safety head in the trace"
    );
    insta::assert_snapshot!("mind_pane_nav_decision", frame);
}

/// An eval case renders as a row of the same trace, so a suite and a
/// navigator run are read the same way.
#[test]
fn the_mind_pane_traces_eval_cases() {
    let mut state = common::state();
    state.tick(0);
    state.start_run(common::run_id(3), RunKind::Eval, "suite · once");
    for (index, case, outcome) in [
        (0, "numbers-sum", EvalCaseState::Passed { runs: 1 }),
        (
            1,
            "pages-heading",
            EvalCaseState::Failed {
                runs: 1,
                detail: "the heading still read “Untitled”".into(),
            },
        ),
        (
            2,
            "keynote-slide",
            EvalCaseState::Skipped {
                reason: "Keynote is not installed".into(),
            },
        ),
    ] {
        state.apply(AppEvent::EvalCase {
            run: common::run_id(3),
            index,
            total: 3,
            case: case.into(),
            state: outcome,
        });
    }
    state.tick(12_000);
    press(&mut state, KeyCode::Char('3'));
    let frame = common::render(&state, 120, 40);
    assert!(frame.contains("1/3 numbers-sum · passed"));
    assert!(frame.contains("3/3 keynote-slide · skipped"));
    insta::assert_snapshot!("mind_pane_eval_cases", frame);
}

/// A turn in flight reaches the conversation itself, not only the Activity
/// pane: a card per action with its outcome, and the answer as the model
/// writes it. Asserted on the text rather than snapshotted, because what
/// matters is that the words are on screen, not where the wrap fell.
#[test]
fn a_running_turn_shows_its_cards_and_its_answer_as_it_arrives() {
    let mut state = common::state();
    state.tick(0);
    state.start_run(common::run_id(1), RunKind::Chat, "what does a seat cost?");
    state.apply(AppEvent::TurnStep {
        run: common::run_id(1),
        step: 0,
        thought: "the price is on the pricing page".into(),
        action: common::browse("https://example.com/pricing", "read the per-seat price"),
    });
    state.apply(AppEvent::TurnStepDone {
        run: common::run_id(1),
        step: 0,
        observation: "that failed: the page never loaded".into(),
        duration_ms: 900,
    });
    for (seq, text) in [(0, "A seat "), (1, "is $29.")] {
        state.apply(AppEvent::TurnDelta {
            run: common::run_id(1),
            seq,
            text: text.to_owned(),
        });
    }
    state.tick(5_000);

    // One pane, so the assertions are about the words and not the width.
    let frame = common::render(&state, 60, 20);
    assert!(
        frame.contains("example.com/pricing"),
        "the card does not name what the model is doing:\n{frame}"
    );
    assert!(
        frame.contains("failed"),
        "a failed step is not visibly failed:\n{frame}"
    );
    assert!(
        frame.contains("900 ms"),
        "the card does not say how long it took:\n{frame}"
    );
    assert!(
        frame.contains("A seat is $29."),
        "the streamed answer is not on screen:\n{frame}"
    );
}

/// While a turn runs the status line is about the turn: what is answering,
/// how long it has been, how much it has done and what it has spent.
#[test]
fn the_status_line_reports_the_running_turn() {
    let mut state = common::state();
    state.tick(0);
    state.start_run(common::run_id(1), RunKind::Chat, "what does a seat cost?");
    state.apply(AppEvent::TurnStep {
        run: common::run_id(1),
        step: 0,
        thought: String::new(),
        action: common::browse("https://example.com/pricing", "read the per-seat price"),
    });
    state.apply(AppEvent::TurnStepDone {
        run: common::run_id(1),
        step: 0,
        observation: "Done · $29 per seat".into(),
        duration_ms: 900,
    });
    state.apply(AppEvent::TurnCost {
        run: common::run_id(1),
        usage: TurnUsage {
            input_tokens: 1_800,
            output_tokens: 95,
            requests: 2,
            ..TurnUsage::default()
        },
    });
    state.tick(65_000);

    let frame = common::render(&state, 120, 40);
    assert!(
        frame.contains("sol-latest · 1:05 · 1 step(s) · 1800 in / 95 out"),
        "the status line does not report the running turn:\n{frame}"
    );
}

/// The conversation switcher. It never shows anything but a title and a
/// timestamp: a thread's contents are not a picker's business.
#[test]
fn the_session_picker_lists_conversations() {
    let mut state = common::state();
    state.load_thread(common::conversation_id(1), Some("launch week".into()), &[]);
    state.show_sessions(vec![
        SessionRow {
            id: common::conversation_id(1),
            title: "launch week".into(),
            when: "2026-09-19 08:14 UTC".into(),
            active: true,
        },
        SessionRow {
            id: common::conversation_id(2),
            title: "ad spend review".into(),
            when: "2026-09-18 17:02 UTC".into(),
            active: false,
        },
        SessionRow {
            id: common::conversation_id(3),
            title: "untitled · 00000000-0000-7000-9000-000000000003".into(),
            when: "2026-09-18 09:41 UTC".into(),
            active: false,
        },
    ]);
    let frame = common::render(&state, 120, 40);
    assert!(frame.contains("launch week"));
    assert!(frame.contains("open"), "the open thread is not marked");
    insta::assert_snapshot!("session_picker", frame);
}

#[test]
fn the_settings_view_renders_the_four_k6_paths_and_the_doctor_facts() {
    let mut state = common::state();
    open_settings(&mut state);
    assert_eq!(state.view, View::Settings);
    insta::assert_snapshot!("settings_configured", common::render(&state, 120, 40));
}

#[test]
fn a_fresh_install_says_so_on_every_row_it_cannot_fill() {
    let mut state = State::new(common::empty_bootstrap());
    open_settings(&mut state);
    insta::assert_snapshot!("settings_fresh_install", common::render(&state, 120, 40));
}

#[test]
fn the_masked_key_prompt_never_renders_the_key() {
    let mut state = common::state();
    open_settings(&mut state);
    select_row(&mut state, "(a) OpenAI API key");
    press(&mut state, KeyCode::Char('s'));
    for character in SECRET.chars() {
        press(&mut state, KeyCode::Char(character));
    }
    assert_eq!(state.prompt.as_ref().map(neo_tui::Prompt::len), Some(26));

    let frame = common::render(&state, 120, 40);
    assert!(!frame.contains(SECRET), "the key reached the frame");
    assert!(!frame.contains("sk-live"), "a key prefix reached the frame");
    assert!(
        !frame.contains("do-not-render"),
        "key material reached the frame"
    );
    insta::assert_snapshot!("keys_masked_prompt", frame);
}

#[test]
fn the_help_overlay_lists_the_live_keymap() {
    let mut state = common::state();
    press(&mut state, KeyCode::Char('?'));
    insta::assert_snapshot!("help_overlay", common::render(&state, 120, 40));
}

/// The subscription login overlay (K7): the whole sign-in happens here, so
/// this frame is the contract. It must show the plan, the authorize URL and
/// the loopback the core is listening on — and the code that comes back must
/// never appear, because the front end never receives one.
#[test]
fn the_login_overlay_shows_the_vendor_url_and_never_a_code() {
    let mut state = common::state();
    open_settings(&mut state);
    select_row(&mut state, "(b) Claude Pro/Max");
    // `c` on an OAuth row asks the core to begin a login; the overlay is put
    // up by the loop once the core hands back the URL, which is what this
    // test stands in for.
    let command = {
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE);
        let action = KeyMap.resolve(key, &state);
        state.apply_action(action)
    };
    assert!(
        matches!(command, Some(neo_tui::Command::BeginLogin { provider }) if provider == "anthropic-oauth"),
        "c on the Claude Pro/Max row must start Starkbot's own login, got {command:?}"
    );

    state.login = Some(neo_tui::Login {
        provider: "anthropic-oauth",
        title: "Claude Pro/Max",
        url: "https://claude.ai/oauth/authorize?client_id=9d1c250a&state=abc123".into(),
        redirect_uri: "http://localhost:54545/callback".into(),
        phase: neo_tui::LoginPhase::Waiting,
        paste: String::new(),
        remaining: 296,
    });
    let frame = common::render(&state, 120, 40);
    assert!(frame.contains("claude.ai/oauth/authorize"), "no vendor URL");
    assert!(frame.contains("localhost:54545"), "no loopback shown");
    insta::assert_snapshot!("login_waiting", frame);
}

/// The paste fallback, for a browser that cannot reach the loopback port. A
/// redirect URL is not a stored credential, so unlike a key prompt it is shown
/// — a user who cannot see their paste cannot tell it arrived whole.
#[test]
fn the_login_overlay_shows_what_was_pasted() {
    let mut state = common::state();
    state.login = Some(neo_tui::Login {
        provider: "openai-codex",
        title: "ChatGPT Plus/Pro",
        url: "https://auth.openai.com/oauth/authorize?client_id=app_EMoamEEZ".into(),
        redirect_uri: "http://localhost:1455/auth/callback".into(),
        phase: neo_tui::LoginPhase::Pasting,
        paste: String::new(),
        remaining: 240,
    });
    for character in "http://localhost:1455/auth/callback?code=xyz".chars() {
        press(&mut state, KeyCode::Char(character));
    }
    let pasted = state
        .login
        .as_ref()
        .map(|login| login.paste.clone())
        .unwrap_or_default();
    assert_eq!(pasted, "http://localhost:1455/auth/callback?code=xyz");

    // Enter hands it to the core and clears the buffer: the front end does not
    // keep a copy of a single-use code.
    let command = {
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let action = KeyMap.resolve(key, &state);
        state.apply_action(action)
    };
    assert!(matches!(
        command,
        Some(neo_tui::Command::FinishLoginPasted { .. })
    ));
    assert_eq!(
        state.login.as_ref().map(|login| login.paste.clone()),
        Some(String::new()),
        "the pasted code must not be kept after it is handed over"
    );
    // And a `{:?}` of that command must not print the code.
    assert!(!format!("{command:?}").contains("code=xyz"));
}

/// A fresh install opens on Connections, not the panes: every credential can
/// be added here, so setup never needs a shell.
#[test]
fn a_fresh_install_opens_on_connections() {
    let state = State::new(common::empty_bootstrap());
    assert_eq!(state.view, View::Settings);
    assert!(state.setup_needed());
    let row = state.rows().get(state.row).cloned();
    assert!(
        row.is_some_and(|row| !row.heading),
        "the cursor must start on something actionable"
    );
}

/// The four widths every page is checked at (14 §6).
const WIDTHS: [(u16, u16); 4] = [(120, 40), (100, 30), (80, 24), (60, 20)];

/// A card on screen at `width`×`height`, armed the way the loop arms it: the
/// frame carries the sentence, then the debounce elapses. What comes back is
/// the frame a user would actually be answering.
fn armed_card(seq: usize, width: u16, height: u16) -> String {
    let mut state = common::state();
    state.apply(common::envelope(seq));
    common::paint(&mut state, width, height);
    state.tick(neo_tui::CARD_ARM_MS);
    common::render(&state, width, height)
}

/// The confirm card, at every width the TUI supports (16 §5.5).
///
/// The sentence is the whole point, so it is asserted present in full rather
/// than only snapshotted: a card that truncated "`Pay $42.00 now` says
/// “pay”." into "`Pay $42.00 now`…" would still match a blessed golden while
/// telling the user something else entirely.
#[test]
fn the_confirm_card_is_legible_at_every_width() {
    for (width, height) in WIDTHS {
        let frame = armed_card(1, width, height);
        for fragment in [
            "Pay $42.00 now",
            "says “pay”.",
            "shop.test/cart",
            "safety:spends",
            "[y] yes",
            "[n] no",
        ] {
            assert!(
                frame.contains(fragment),
                "{width}x{height} lost {fragment:?}:\n{frame}"
            );
        }
        // Q2 cannot remember an allow, so the card must not offer to.
        assert!(
            !frame.contains("[r]"),
            "{width}x{height} offered to remember an allow:\n{frame}"
        );
        insta::assert_snapshot!(format!("confirm_card_{width}x{height}"), frame);
    }
}

/// The free-text question, at every width. It says how to answer it — `i`
/// opens the one overlay that can take the keyboard back off a card — and it
/// offers no `y`, because there is nothing to say yes to.
#[test]
fn the_free_text_ask_card_is_legible_at_every_width() {
    for (width, height) in WIDTHS {
        let frame = armed_card(5, width, height);
        for fragment in ["Invoice number", "I'll carry on.", "[i] type your answer"] {
            assert!(
                frame.contains(fragment),
                "{width}x{height} lost {fragment:?}:\n{frame}"
            );
        }
        assert!(
            !frame.contains("[y]"),
            "{width}x{height} offered yes to a question:\n{frame}"
        );
        insta::assert_snapshot!(format!("ask_card_{width}x{height}"), frame);
    }
}

/// Before the debounce elapses the card says so rather than advertising keys
/// that do nothing: an inert binding that is on screen reads as a broken one.
#[test]
fn a_card_inside_the_debounce_offers_no_keys() {
    let mut state = common::state();
    state.apply(common::envelope(1));
    let frame = common::paint(&mut state, 100, 30);
    assert!(frame.contains("reading…"), "no reading state:\n{frame}");
    assert!(
        !frame.contains("[y] yes"),
        "keys offered too early:\n{frame}"
    );
}

/// An options ask numbers its answers and shows which one `y` would send, so
/// the keystroke is never a guess. `j` moves that mark: a highlight the
/// keyboard cannot move is worse than no highlight at all.
#[test]
fn an_options_ask_shows_its_answers_and_moves_the_highlight() {
    let mut state = common::state();
    state.apply(common::envelope(8));
    common::paint(&mut state, 100, 30);
    state.tick(neo_tui::CARD_ARM_MS);

    let frame = common::render(&state, 100, 30);
    assert!(
        frame.contains(" 1 ▸ andrew@stark.test"),
        "the first answer is not marked:\n{frame}"
    );
    assert!(
        frame.contains(" 2   ops@stark.test"),
        "the second answer is not numbered:\n{frame}"
    );

    press(&mut state, KeyCode::Char('j'));
    let moved = common::render(&state, 100, 30);
    assert!(
        moved.contains(" 2 ▸ ops@stark.test") && moved.contains(" 1   andrew@stark.test"),
        "j did not move the highlight:\n{moved}"
    );
}

/// A gate waiting its turn is counted on the card in front of it: a user who
/// approves one thing needs to know another question is coming, not discover
/// it when the screen changes under them.
#[test]
fn a_queued_gate_is_counted_on_the_card_in_front_of_it() {
    let mut state = common::state();
    state.apply(common::envelope(1));
    let alone = common::paint(&mut state, 100, 30);
    assert!(
        !alone.contains("waiting behind"),
        "nothing is queued yet:\n{alone}"
    );

    state.apply(common::envelope(7));
    let queued = common::render(&state, 100, 30);
    assert!(
        queued.contains("1 more waiting behind this one"),
        "the queued gate is invisible:\n{queued}"
    );
}
