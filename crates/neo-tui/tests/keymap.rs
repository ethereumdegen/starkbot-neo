//! Key handling (14 §6, "Key handling"). The two rules that must never break:
//! quitting is explicit, and nothing resolves a confirm that is not on screen.

mod common;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use neo_agent::ax::AxRequest;
use neo_core::AppEvent;
use neo_eval::Selection;
use neo_tui::state::PromptKind;
use neo_tui::{
    Action, CARD_ARM_MS, Command, KeyMap, Mode, NavSpec, Pane, RunKind, RunState, Section,
    SessionRow, State, View,
};

fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn control(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

fn feed(state: &mut State, key: KeyEvent) -> Option<Command> {
    let action = KeyMap.resolve(key, state);
    state.apply_action(action)
}

/// Type `line` on the `:` line and run it, the way a user does — through the
/// keymap, so a binding that stopped reaching COMMAND mode fails here too.
fn run_line(state: &mut State, line: &str) -> Option<Command> {
    feed(state, press(KeyCode::Char(':')));
    assert_eq!(state.mode, Mode::Command);
    for character in line.chars() {
        feed(state, press(KeyCode::Char(character)));
    }
    feed(state, press(KeyCode::Enter))
}

#[test]
fn tab_cycles_the_panes_and_digits_jump_to_them() {
    let mut state = common::state();
    assert_eq!(state.focus, Pane::Conversation);

    feed(&mut state, press(KeyCode::Tab));
    assert_eq!(state.focus, Pane::Runs);
    feed(&mut state, press(KeyCode::Tab));
    assert_eq!(state.focus, Pane::Mind);
    feed(&mut state, press(KeyCode::Tab));
    assert_eq!(state.focus, Pane::Conversation);

    feed(&mut state, press(KeyCode::BackTab));
    assert_eq!(state.focus, Pane::Mind);
    feed(&mut state, press(KeyCode::Char('2')));
    assert_eq!(state.focus, Pane::Runs);
}

#[test]
fn quitting_is_explicit() {
    let mut state = common::state();

    // `q` at top level only asks.
    feed(&mut state, press(KeyCode::Char('q')));
    assert!(!state.quit);
    assert!(state.quit_prompt);

    // ...and the question can be answered no.
    feed(&mut state, press(KeyCode::Char('n')));
    assert!(!state.quit);
    assert!(!state.quit_prompt);

    // A bare `q` inside a view leaves the view instead of quitting.
    feed(&mut state, press(KeyCode::Char(',')));
    assert_eq!(state.view, View::Settings);
    feed(&mut state, press(KeyCode::Char('q')));
    assert_eq!(state.view, View::Panes);
    assert!(!state.quit);
    assert!(!state.quit_prompt);

    // `q` while typing is a character, not a quit.
    feed(&mut state, press(KeyCode::Char('i')));
    assert_eq!(state.mode, Mode::Insert);
    feed(&mut state, press(KeyCode::Char('q')));
    assert_eq!(state.composer, "q");
    assert!(!state.quit);
    assert!(!state.quit_prompt);

    // Ctrl-Q is the one binding that quits outright, from any mode.
    feed(&mut state, control(KeyCode::Char('q')));
    assert!(state.quit);
}

/// The rule, now that a card can really arrive: no keystroke resolves a
/// confirm whose sentence is not on the screen and has not been there long
/// enough to read (04 §13, 14 §3). The card here comes from the shared
/// corpus and is armed by drawing a real frame, so this is the runtime path
/// rather than a hand-set pair of flags.
#[test]
fn no_key_resolves_a_confirm_that_is_not_on_screen() {
    let mut state = common::state();
    // No card at all: `y` and `n` are ordinary normal-mode keys and neither
    // produces a confirm resolution.
    for code in [KeyCode::Char('y'), KeyCode::Char('n'), KeyCode::Enter] {
        assert_eq!(KeyMap.resolve(press(code), &state), Action::None);
    }

    // Raised but never drawn: still nothing.
    state.apply(common::envelope(1));
    assert_eq!(state.mode, Mode::Card);
    assert_eq!(
        KeyMap.resolve(press(KeyCode::Char('y')), &state),
        Action::None
    );
    assert_eq!(
        KeyMap.resolve(press(KeyCode::Char('n')), &state),
        Action::None
    );

    // Drawn, but inside the arming debounce: still nothing.
    common::paint(&mut state, 100, 30);
    state.tick(CARD_ARM_MS - 1);
    assert_eq!(
        KeyMap.resolve(press(KeyCode::Char('y')), &state),
        Action::None
    );

    // Drawn and armed: now, and only now, `y` resolves — and `Enter` never
    // does, so a stray composer return cannot approve anything.
    state.tick(CARD_ARM_MS);
    assert_eq!(
        KeyMap.resolve(press(KeyCode::Char('y')), &state),
        Action::ResolveConfirm { approve: true }
    );
    assert_eq!(KeyMap.resolve(press(KeyCode::Enter), &state), Action::None);
    assert_eq!(
        KeyMap.resolve(press(KeyCode::Esc), &state),
        Action::UnfocusCard
    );
}

/// A card the frame never carried cannot be answered by a keystroke, however
/// long it has been pending: below the minimum size the TUI draws one line
/// and the card is not on it, so the gate stays open rather than being
/// approved by a user who is looking at "terminal too small".
#[test]
fn a_card_that_did_not_reach_the_frame_cannot_be_resolved() {
    let mut state = common::state();
    state.apply(common::envelope(1));

    let frame = common::paint(&mut state, 59, 19);
    assert!(
        frame.contains("too small"),
        "not the too-small frame:\n{frame}"
    );
    assert!(
        !frame.contains("says"),
        "the sentence reached a frame it should not have"
    );
    state.tick(10_000);
    assert_eq!(
        KeyMap.resolve(press(KeyCode::Char('y')), &state),
        Action::None
    );

    // Arm it on a terminal that fits, then shrink: the sentence has left the
    // screen, so the keys go dead again.
    common::paint(&mut state, 100, 30);
    state.tick(11_000);
    assert_eq!(
        KeyMap.resolve(press(KeyCode::Char('y')), &state),
        Action::ResolveConfirm { approve: true }
    );
    common::paint(&mut state, 59, 19);
    state.tick(20_000);
    assert_eq!(
        KeyMap.resolve(press(KeyCode::Char('y')), &state),
        Action::None
    );
}

/// The help overlay covers the card, so while it is up the sentence is not
/// on screen in the sense the rule means — and the overlay owns the keyboard
/// anyway, which is the same answer from the other direction.
#[test]
fn an_overlay_over_the_card_disarms_it() {
    let mut state = common::state();
    state.apply(common::envelope(1));
    common::paint(&mut state, 100, 30);
    state.tick(CARD_ARM_MS);

    state.help = true;
    common::paint(&mut state, 100, 30);
    state.tick(CARD_ARM_MS * 4);
    assert_eq!(
        KeyMap.resolve(press(KeyCode::Char('y')), &state),
        Action::None,
        "a covered card was resolvable"
    );

    // Uncovered, the sentence is on screen again — and the debounce starts
    // over, because this is a new sighting of it.
    state.help = false;
    common::paint(&mut state, 100, 30);
    assert_eq!(
        KeyMap.resolve(press(KeyCode::Char('y')), &state),
        Action::None,
        "a card the user has only just seen again was already live"
    );
    state.tick(CARD_ARM_MS * 5);
    assert_eq!(
        KeyMap.resolve(press(KeyCode::Char('y')), &state),
        Action::ResolveConfirm { approve: true }
    );
}

#[test]
fn the_kill_switch_is_reachable_from_every_mode_and_drops_a_half_typed_key() {
    let mut state = common::state();
    for mode in [Mode::Normal, Mode::Insert, Mode::Command, Mode::Search] {
        state.mode = mode;
        assert_eq!(
            KeyMap.resolve(control(KeyCode::Char('c')), &state),
            Action::KillSwitch,
            "kill switch unreachable in {mode:?}"
        );
    }

    state.mode = Mode::Normal;
    state.view = View::Settings;
    state.row = openai_row(&state);
    feed(&mut state, press(KeyCode::Char('s')));
    feed(&mut state, press(KeyCode::Char('s')));
    feed(&mut state, press(KeyCode::Char('k')));
    assert_eq!(state.prompt.as_ref().map(neo_tui::Prompt::len), Some(2));

    feed(&mut state, control(KeyCode::Char('c')));
    assert!(state.prompt.is_none());
}

#[test]
fn setting_a_key_is_masked_and_produces_one_set_key_command() {
    let mut state = common::state();
    state.view = View::Settings;
    state.row = openai_row(&state);

    feed(&mut state, press(KeyCode::Char('s')));
    let prompt = state.prompt.as_ref().map(|prompt| {
        (
            prompt.masked,
            prompt.visible().to_owned(),
            prompt.kind.clone(),
        )
    });
    assert_eq!(
        prompt,
        Some((
            true,
            String::new(),
            PromptKind::SetKey {
                account: "openai".into()
            }
        ))
    );

    for character in "sk-live-abcdef".chars() {
        feed(&mut state, press(KeyCode::Char(character)));
    }
    // The masked buffer is never exposed, even to a caller holding the state.
    assert_eq!(
        state.prompt.as_ref().map(neo_tui::Prompt::visible),
        Some("")
    );

    let command = feed(&mut state, press(KeyCode::Enter));
    assert_eq!(
        command,
        Some(Command::SetKey {
            account: "openai".into(),
            raw: "sk-live-abcdef".into()
        })
    );
    assert!(state.prompt.is_none());
    // ...and a debug print of that command still cannot leak it.
    let printed = format!("{command:?}");
    assert!(!printed.contains("sk-live"), "{printed}");
}

#[test]
fn x_removes_the_selected_account_and_enter_selects_its_k6_path() {
    let mut state = common::state();
    state.view = View::Settings;
    state.row = anthropic_row(&state);

    assert_eq!(
        feed(&mut state, press(KeyCode::Char('x'))),
        Some(Command::RemoveKey {
            account: "anthropic".into()
        })
    );
    assert_eq!(
        feed(&mut state, press(KeyCode::Enter)),
        Some(Command::PatchSettings {
            section: "models",
            patch: serde_json::json!({ "inference": { "provider": "anthropic" } })
        })
    );
}

/// The `:` line is still closed: unknown input is rejected, never forwarded.
/// What changed is that the commands on the list now do their job instead of
/// naming a milestone.
#[test]
fn the_command_line_is_closed_and_rejects_anything_not_on_the_list() {
    let mut state = common::state();
    // `:doctor` re-runs the checks as well as opening the section: a cached
    // row is not a readiness report.
    assert_eq!(run_line(&mut state, "doctor"), Some(Command::Doctor));
    assert_eq!(state.view, View::Settings);
    assert_eq!(state.mode, Mode::Normal);

    feed(&mut state, press(KeyCode::Char('q')));
    assert_eq!(run_line(&mut state, "!sh"), None);
    assert_eq!(state.status.as_deref(), Some("unknown command: !sh"));

    // A command that names a capability the core does not have says so, and
    // says which capability — not which milestone.
    assert_eq!(run_line(&mut state, "packs"), None);
    assert!(
        state
            .status
            .as_deref()
            .is_some_and(|status| status.contains("pack registry")),
        "{:?}",
        state.status
    );
}

/// `:nav` takes the same flags `neo nav` does, and the goal is everything
/// left over — a goal is a sentence, not a token.
#[test]
fn nav_parses_the_url_the_goal_and_every_flag() {
    let mut state = common::state();
    assert_eq!(
        run_line(
            &mut state,
            "nav https://x.com --headless post the launch note --profile /tmp/p --no-safety"
        ),
        Some(Command::Nav {
            options: NavSpec {
                url: "https://x.com".into(),
                goal: "post the launch note".into(),
                headless: true,
                profile: Some("/tmp/p".into()),
                safety_heads: false,
            }
        })
    );

    // Safety heads are on unless turned off, matching `neo nav`'s default —
    // and so is the window: headed is what a user watching it needs (Q1.1).
    let Some(Command::Nav { options }) = run_line(&mut state, "nav https://x.com read it") else {
        panic!("a bare :nav did not produce a run");
    };
    assert!(options.safety_heads);
    assert!(!options.headless);

    // A goal-less nav is refused with the usage, not sent as an empty goal.
    assert_eq!(run_line(&mut state, "nav https://x.com"), None);
    assert!(
        state
            .status
            .as_deref()
            .is_some_and(|line| line.contains("--headless")),
        "{:?}",
        state.status
    );
}

#[test]
fn app_takes_a_target_and_the_rest_as_the_goal() {
    let mut state = common::state();
    assert_eq!(
        run_line(&mut state, "app Numbers put 42 in B2"),
        Some(Command::AppGoal {
            app: "Numbers".into(),
            goal: "put 42 in B2".into()
        })
    );
    assert_eq!(run_line(&mut state, "app Numbers"), None);
}

/// Every `neo ax` request shape is reachable, and a row index that is not a
/// number is refused here rather than by the actor.
#[test]
fn ax_reaches_every_request_shape() {
    let mut state = common::state();
    for (line, expected) in [
        ("ax trusted", AxRequest::Trusted),
        ("ax apps", AxRequest::Apps),
        ("ax table Mail", AxRequest::Table { app: "Mail".into() }),
        (
            "ax press Mail 12",
            AxRequest::Press {
                app: "Mail".into(),
                index: 12,
            },
        ),
        (
            "ax set Mail 3 hello there",
            AxRequest::Set {
                app: "Mail".into(),
                index: 3,
                text: "hello there".into(),
            },
        ),
        (
            "ax menu Mail File › New Message",
            AxRequest::Menu {
                app: "Mail".into(),
                path: "File › New Message".into(),
            },
        ),
        (
            "ax type Mail some words",
            AxRequest::Type {
                app: "Mail".into(),
                text: "some words".into(),
            },
        ),
        (
            "ax key Mail Return",
            AxRequest::Key {
                app: "Mail".into(),
                key: "Return".into(),
            },
        ),
    ] {
        assert_eq!(
            run_line(&mut state, line),
            Some(Command::Ax { request: expected }),
            "{line}"
        );
    }

    assert_eq!(run_line(&mut state, "ax press Mail twelve"), None);
    assert_eq!(run_line(&mut state, "ax fly Mail"), None);
    assert_eq!(state.status.as_deref(), Some("unknown ax request: fly"));

    // And typed text is counted, never printed, by a `{:?}` of the command.
    let command = run_line(&mut state, "ax type Mail hunter2");
    let printed = format!("{command:?}");
    assert!(!printed.contains("hunter2"), "{printed}");
}

#[test]
fn eval_parses_its_selection_and_refuses_a_second_concurrent_suite() {
    let mut state = common::state();
    assert_eq!(
        run_line(
            &mut state,
            "eval --filter numbers --tag browser --tag known-gap --once"
        ),
        Some(Command::Eval {
            selection: Selection {
                filter: Some("numbers".into()),
                tags: vec!["browser".into(), "known-gap".into()],
                once: true,
            }
        })
    );
    assert_eq!(run_line(&mut state, "eval --list"), Some(Command::EvalList));

    // The cases share the keyboard and the frontmost application, so a
    // second suite is refused while one is live rather than interleaved.
    state.start_run(common::run_id(1), RunKind::Eval, "suite");
    assert_eq!(run_line(&mut state, "eval"), None);
    assert!(
        state
            .status
            .as_deref()
            .is_some_and(|line| line.contains("already running")),
        "{:?}",
        state.status
    );
}

/// `x` cancels the run the Mind pane is tracing, and says what a stop cannot
/// reclaim rather than claiming the browser is already gone.
#[test]
fn x_stops_the_selected_run_and_is_honest_about_what_it_cannot_reclaim() {
    let mut state = common::state();
    assert_eq!(
        KeyMap.resolve(press(KeyCode::Char('x')), &state),
        Action::StopRun
    );
    // Nothing running: `x` says so and sends nothing.
    assert_eq!(feed(&mut state, press(KeyCode::Char('x'))), None);
    assert_eq!(state.status.as_deref(), Some("nothing is running"));

    state.start_run(common::run_id(1), RunKind::Nav, "https://x.com — post");
    assert_eq!(
        feed(&mut state, press(KeyCode::Char('x'))),
        Some(Command::StopRun {
            run: common::run_id(1)
        })
    );
    // Stopping, not cancelled: the token still has to reach the navigator.
    assert_eq!(
        state.runs.first().map(|run| run.state.clone()),
        Some(RunState::Stopping)
    );
    assert!(
        state
            .status
            .as_deref()
            .is_some_and(|line| line.contains("stay changed")),
        "a stop must not claim it undid what already happened: {:?}",
        state.status
    );

    // `Ctrl-C` keeps its kill-switch job: every live run, not just the one
    // being traced.
    state.start_run(common::run_id(2), RunKind::Chat, "post the launch note");
    assert_eq!(
        feed(&mut state, control(KeyCode::Char('c'))),
        Some(Command::StopAll)
    );
    assert!(state.runs.iter().all(|run| run.state == RunState::Stopping));
}

/// `:stop` is the same intent typed.
#[test]
fn stop_on_the_command_line_cancels_the_selected_run() {
    let mut state = common::state();
    state.start_run(common::run_id(1), RunKind::Nav, "https://x.com — post");
    assert_eq!(
        run_line(&mut state, "stop"),
        Some(Command::StopRun {
            run: common::run_id(1)
        })
    );
}

#[test]
fn conversations_are_reachable_from_the_keyboard_and_the_command_line() {
    let mut state = common::state();
    // `Ctrl-N` used to say conversations were a later milestone.
    assert_eq!(
        feed(&mut state, control(KeyCode::Char('n'))),
        Some(Command::NewConversation { title: None })
    );
    assert_eq!(
        run_line(&mut state, "new launch week"),
        Some(Command::NewConversation {
            title: Some("launch week".into())
        })
    );
    assert_eq!(
        feed(&mut state, press(KeyCode::Char('t'))),
        Some(Command::ListConversations)
    );
    assert_eq!(
        run_line(&mut state, "sessions"),
        Some(Command::ListConversations)
    );
    assert_eq!(
        run_line(&mut state, "rename launch week"),
        Some(Command::RenameConversation {
            title: "launch week".into()
        })
    );
    // A bare `:rename` opens a prompt pre-filled with the current title.
    state.conversation_title = Some("old name".into());
    assert_eq!(run_line(&mut state, "rename"), None);
    assert_eq!(
        state.prompt.as_ref().map(neo_tui::Prompt::visible),
        Some("old name")
    );
}

/// The picker owns motion while it is up, so `j` cannot scroll the pane
/// behind it, and `Enter` switches to whatever the cursor is on.
#[test]
fn the_session_picker_moves_and_opens() {
    let mut state = common::state();
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
    ]);
    assert_eq!(state.sessions.as_ref().map(|picker| picker.row), Some(0));

    feed(&mut state, press(KeyCode::Char('j')));
    assert_eq!(state.sessions.as_ref().map(|picker| picker.row), Some(1));
    // It clamps rather than wrapping: a picker that wraps loses the user.
    feed(&mut state, press(KeyCode::Char('j')));
    assert_eq!(state.sessions.as_ref().map(|picker| picker.row), Some(1));

    assert_eq!(
        feed(&mut state, press(KeyCode::Enter)),
        Some(Command::SwitchConversation {
            conversation: common::conversation_id(2)
        })
    );
    assert!(state.sessions.is_none());
}

/// `m` used to say listening arrived with a later milestone. `listen.enabled`
/// is a real stored field, so it flips it.
#[test]
fn m_toggles_the_listen_setting() {
    let mut state = common::state();
    let before = state.settings.listen.enabled;
    assert_eq!(
        feed(&mut state, press(KeyCode::Char('m'))),
        Some(Command::PatchSettings {
            section: "listen",
            patch: serde_json::json!({ "enabled": !before })
        })
    );
}

/// Settings rows for the sections that had none. A bool flips in place; a
/// number opens the one-line prompt and patches only its own leaf.
#[test]
fn every_settings_section_has_editable_rows() {
    let mut state = common::state();
    state.view = View::Settings;
    for section in Section::ALL {
        assert!(
            state
                .rows()
                .iter()
                .any(|row| row.section == section && !row.heading),
            "{section:?} has no rows"
        );
    }

    state.row = row_index(&state, "notifications");
    assert_eq!(
        feed(&mut state, press(KeyCode::Enter)),
        Some(Command::PatchSettings {
            section: "general",
            patch: serde_json::json!({ "notifications": false })
        })
    );

    // A nested pointer patches only its own leaf, so the two sibling
    // thresholds are untouched — that is the whole point of a merge patch.
    state.row = row_index(&state, "confirm · outward");
    assert_eq!(feed(&mut state, press(KeyCode::Enter)), None);
    // The prompt opens pre-filled with the stored value, so an edit that
    // replaces it clears first — Ctrl-U, the same key the composer uses.
    assert_eq!(
        state.prompt.as_ref().map(neo_tui::Prompt::visible),
        Some("0.4")
    );
    feed(&mut state, control(KeyCode::Char('u')));
    for character in "0.25".chars() {
        feed(&mut state, press(KeyCode::Char(character)));
    }
    assert_eq!(
        feed(&mut state, press(KeyCode::Enter)),
        Some(Command::PatchSettings {
            section: "safety",
            patch: serde_json::json!({ "confirm_at": { "outward": 0.25 } })
        })
    );

    // A closed set cycles rather than prompting.
    state.row = row_index(&state, "duplex");
    assert_eq!(
        feed(&mut state, press(KeyCode::Enter)),
        Some(Command::PatchSettings {
            section: "voice",
            patch: serde_json::json!({ "duplex": "half" })
        })
    );

    // A value the field cannot hold is refused with the shape it wants,
    // rather than sent for the core to reject.
    state.row = row_index(&state, "keep traces (days)");
    feed(&mut state, press(KeyCode::Enter));
    feed(&mut state, control(KeyCode::Char('u')));
    for character in "soon".chars() {
        feed(&mut state, press(KeyCode::Char(character)));
    }
    assert_eq!(feed(&mut state, press(KeyCode::Enter)), None);
    assert_eq!(
        state.status.as_deref(),
        Some("privacy.trace_days: a whole number, e.g. 30")
    );
}

/// The audit's third inconsistency: list motion claimed `k`, so check-key
/// could never fire and the cursor could never move up.
#[test]
fn k_moves_the_cursor_and_shifted_k_checks_the_key() {
    let mut state = common::state();
    state.view = View::Settings;
    state.row = openai_row(&state);

    assert_eq!(
        KeyMap.resolve(press(KeyCode::Char('k')), &state),
        Action::SelectPrevious
    );
    feed(&mut state, press(KeyCode::Char('k')));
    assert_eq!(state.row, openai_row(&state) - 1);

    state.row = openai_row(&state);
    assert_eq!(
        feed(&mut state, press(KeyCode::Char('K'))),
        Some(Command::CheckKey {
            account: "openai".into()
        })
    );
}

/// Tab completion over the closed list, and over `:ax`'s closed set of
/// requests — the only argument that is drawn from one.
#[test]
fn tab_completes_a_command_and_an_ax_request() {
    let mut state = common::state();
    feed(&mut state, press(KeyCode::Char(':')));
    for character in "sess".chars() {
        feed(&mut state, press(KeyCode::Char(character)));
    }
    feed(&mut state, press(KeyCode::Tab));
    assert_eq!(state.line, "sessions");

    feed(&mut state, press(KeyCode::Esc));
    feed(&mut state, press(KeyCode::Char(':')));
    for character in "na".chars() {
        feed(&mut state, press(KeyCode::Char(character)));
    }
    feed(&mut state, press(KeyCode::Tab));
    // A command that takes arguments completes with the space already typed.
    assert_eq!(state.line, "nav ");

    feed(&mut state, press(KeyCode::Esc));
    feed(&mut state, press(KeyCode::Char(':')));
    for character in "ax tab".chars() {
        feed(&mut state, press(KeyCode::Char(character)));
    }
    feed(&mut state, press(KeyCode::Tab));
    assert_eq!(state.line, "ax table");

    // Ambiguity completes nothing rather than guessing: `:s` is settings,
    // sessions and stop.
    feed(&mut state, press(KeyCode::Esc));
    feed(&mut state, press(KeyCode::Char(':')));
    feed(&mut state, press(KeyCode::Char('s')));
    feed(&mut state, press(KeyCode::Tab));
    assert_eq!(state.line, "s");
}

#[test]
fn gg_and_g_jump_to_the_first_and_last_settings_row() {
    let mut state = common::state();
    state.view = View::Settings;
    let last = state.rows().len() - 1;

    feed(&mut state, press(KeyCode::Char('G')));
    assert_eq!(state.row, last);
    // A single `g` is pending, not a jump.
    feed(&mut state, press(KeyCode::Char('g')));
    assert_eq!(state.row, last);
    feed(&mut state, press(KeyCode::Char('g')));
    assert_eq!(state.row, 0);
}

/// Typing while a turn is running steers that turn. The message is never
/// refused and never dropped: if the run settled first, the loop sends it
/// as a turn of its own, which is the same keystroke doing the right thing
/// either way.
#[test]
fn a_message_typed_into_a_running_turn_steers_it() {
    let mut state = common::state();
    state.start_run(common::run_id(1), RunKind::Chat, "post the launch note");

    feed(&mut state, press(KeyCode::Char('i')));
    for character in "use the other account".chars() {
        feed(&mut state, press(KeyCode::Char(character)));
    }
    assert_eq!(
        feed(&mut state, press(KeyCode::Enter)),
        Some(Command::Steer {
            run: common::run_id(1),
            text: "use the other account".into()
        })
    );
    // On screen immediately, marked as having reached a turn in flight.
    let row = state
        .thread
        .last()
        .unwrap_or_else(|| panic!("no steered row"));
    assert_eq!(row.text, "use the other account");
    assert!(row.steered);
    // The composer is cleared and still editable — nothing is locked.
    assert!(state.composer.is_empty());
    assert_eq!(state.mode, Mode::Insert);

    // Once that turn is over the very same words start a new one.
    state.apply(AppEvent::TurnFinished {
        run: common::run_id(1),
        text: "posted".into(),
        steps: 1,
        exhausted: false,
        usage: None,
    });
    for character in "use the other account".chars() {
        feed(&mut state, press(KeyCode::Char(character)));
    }
    assert_eq!(
        feed(&mut state, press(KeyCode::Enter)),
        Some(Command::Send {
            text: "use the other account".into()
        })
    );
}

/// A steer the run could not take becomes a new turn, and the optimistic
/// row goes away so the ordinary send path can put the stored one back.
#[test]
fn a_steer_the_run_missed_leaves_no_orphan_row() {
    let mut state = common::state();
    state.start_run(common::run_id(1), RunKind::Chat, "post the launch note");
    feed(&mut state, press(KeyCode::Char('i')));
    state.composer = "use the other account".into();
    feed(&mut state, press(KeyCode::Enter));
    assert_eq!(state.thread.len(), 1);

    state.steer_missed("use the other account");
    assert!(state.thread.is_empty(), "{:?}", state.thread);
}

/// `Esc` is the interrupt. With something live it stops that run; with
/// nothing live it is the quit question it has always been.
#[test]
fn esc_stops_a_live_run_and_otherwise_asks_to_quit() {
    let mut state = common::state();
    assert_eq!(
        KeyMap.resolve(press(KeyCode::Esc), &state),
        Action::CloseOverlay
    );
    feed(&mut state, press(KeyCode::Esc));
    assert!(state.quit_prompt, "Esc with nothing running must still ask");
    feed(&mut state, press(KeyCode::Char('n')));

    state.start_run(common::run_id(1), RunKind::Chat, "post the launch note");
    assert_eq!(KeyMap.resolve(press(KeyCode::Esc), &state), Action::StopRun);
    assert_eq!(
        feed(&mut state, press(KeyCode::Esc)),
        Some(Command::StopRun {
            run: common::run_id(1)
        })
    );
    assert!(
        !state.quit_prompt,
        "the interrupt must not raise the quit prompt"
    );
    // Stopping, not stopped: the token still has to reach the run.
    assert_eq!(
        state.runs.first().map(|run| run.state.clone()),
        Some(RunState::Stopping)
    );

    // What the turn already produced is still on screen after the stop.
    state.apply(AppEvent::TurnFailed {
        run: common::run_id(1),
        error: "cancelled".into(),
    });
    assert_eq!(
        state.runs.first().map(|run| run.state.clone()),
        Some(RunState::Cancelled)
    );
}

fn openai_row(state: &State) -> usize {
    row_index(state, "(a) OpenAI API key")
}

fn anthropic_row(state: &State) -> usize {
    row_index(state, "(c) Anthropic API key")
}

fn row_index(state: &State, label: &str) -> usize {
    state
        .rows()
        .iter()
        .position(|row| row.label == label)
        .unwrap_or_else(|| panic!("no {label} row"))
}
