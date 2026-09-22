//! Event reducer behaviour (14 §6, "Event handling").

mod common;

use neo_core::{
    AppEvent, ConfirmId, GateOutcome, InferenceConnection, KeyState, Message, MessageKind,
    MessageRole, MessageSource, NavStepKind, NoticeLevel, PROVIDER_ANTHROPIC, PROVIDER_OPENAI,
    ReasoningEffort, ResolutionVia, Settings, TurnUsage,
};
use neo_tui::{Action, CARD_ARM_MS, Card, CardKind, Command, Mode, RunKind, RunState, State};

#[test]
fn settings_changed_replaces_what_the_settings_view_renders() {
    let mut state = common::state();
    let before = state
        .rows()
        .into_iter()
        .find(|row| row.label == "sol_effort")
        .map(|row| row.value);
    assert_eq!(before.as_deref(), Some("low"));

    let mut settings = Settings::default();
    settings.models.sol_effort = ReasoningEffort::High;
    settings.identity.name = "Nova".into();
    state.apply(AppEvent::SettingsChanged {
        settings: Box::new(settings),
    });

    let rows = state.rows();
    let after = rows
        .iter()
        .find(|row| row.label == "sol_effort")
        .map(|row| row.value.clone());
    assert_eq!(after.as_deref(), Some("high"));
    assert_eq!(state.settings.identity.name, "Nova");
}

#[test]
fn a_key_status_event_updates_exactly_that_account() {
    let mut state = common::state();
    assert_eq!(state.key_state(PROVIDER_OPENAI), KeyState::Present);
    assert_eq!(state.key_state(PROVIDER_ANTHROPIC), KeyState::Missing);
    assert_eq!(state.key_state("typesafe"), KeyState::Present);

    state.apply(AppEvent::KeyStatus {
        account: PROVIDER_ANTHROPIC.into(),
        status: KeyState::Present,
    });

    assert_eq!(state.key_state(PROVIDER_ANTHROPIC), KeyState::Present);
    assert_eq!(state.key_state(PROVIDER_OPENAI), KeyState::Present);
    assert_eq!(state.key_state("typesafe"), KeyState::Present);
}

#[test]
fn losing_the_openai_key_drops_the_inference_connection_and_raises_typed_only() {
    let mut state = common::state();
    assert_eq!(state.inference, InferenceConnection::OpenAiKey);
    assert!(!state.typed_only());

    state.apply(AppEvent::KeyStatus {
        account: PROVIDER_OPENAI.into(),
        status: KeyState::Invalid,
    });

    assert_eq!(state.inference, InferenceConnection::None);
    assert!(state.typed_only());
    assert!(
        state
            .rows()
            .iter()
            .any(|row| row.value.contains("typed-only"))
    );
}

#[test]
fn an_event_the_reducer_does_not_model_only_lands_in_the_activity_ring() {
    let mut state = common::state();
    let settings = state.settings.clone();
    let keys = state.keys.clone();

    state.apply(AppEvent::Latency {
        step_ms_p50: 41,
        jev_ms_p50: 168,
    });

    assert_eq!(state.settings, settings);
    assert_eq!(state.keys, keys);
    let last = state
        .activity
        .back()
        .map(|entry| (entry.kind, entry.detail.clone()));
    assert_eq!(last, Some(("latency", "step 41ms · jev 168ms".to_owned())));
}

#[test]
fn the_activity_ring_is_bounded_and_keeps_the_newest() {
    let mut state = common::state();
    for index in 0..(neo_tui::state::ACTIVITY_CAP + 25) {
        state.apply(AppEvent::Latency {
            step_ms_p50: u32::try_from(index).unwrap_or(0),
            jev_ms_p50: 0,
        });
    }
    assert_eq!(state.activity.len(), neo_tui::state::ACTIVITY_CAP);
    assert_eq!(
        state.activity.back().map(|entry| entry.detail.clone()),
        Some(format!(
            "step {}ms · jev 0ms",
            neo_tui::state::ACTIVITY_CAP + 24
        ))
    );
}

#[test]
fn a_notice_reaches_the_status_line_as_well_as_the_ring() {
    let mut state = common::state();
    state.apply(AppEvent::Notice {
        level: NoticeLevel::Warning,
        code: "jev.unreachable".into(),
        text: "confirming everything until Jev is back".into(),
    });
    assert_eq!(
        state.status.as_deref(),
        Some("confirming everything until Jev is back")
    );
    assert_eq!(
        state.activity.back().map(|entry| entry.kind),
        Some("notice")
    );
}

/// The answer appears as the model writes it, and it appears in the order
/// the model wrote it — not the order the transport happened to deliver.
#[test]
fn turn_deltas_assemble_one_answer_in_seq_order() {
    let mut state = common::state();
    state.start_run(common::run_id(1), RunKind::Chat, "what does a seat cost?");

    // The middle slice arrives last; the sentence still reads correctly.
    for (seq, text) in [(0, "A seat "), (2, "a month."), (1, "is $29 ")] {
        state.apply(AppEvent::TurnDelta {
            run: common::run_id(1),
            seq,
            text: text.to_owned(),
        });
    }
    assert_eq!(
        state.turn.as_ref().map(|turn| turn.answer.as_str()),
        Some("A seat is $29 a month.")
    );

    // A slice delivered twice is not written twice.
    state.apply(AppEvent::TurnDelta {
        run: common::run_id(1),
        seq: 1,
        text: "is $29 ".to_owned(),
    });
    assert_eq!(
        state.turn.as_ref().map(|turn| turn.answer.as_str()),
        Some("A seat is $29 a month.")
    );

    // A delta for a run this front end is not tracking changes nothing.
    state.apply(AppEvent::TurnDelta {
        run: common::run_id(9),
        seq: 0,
        text: "not this turn".to_owned(),
    });
    assert_eq!(
        state.turn.as_ref().map(|turn| turn.answer.as_str()),
        Some("A seat is $29 a month.")
    );
}

/// One action produces one card, and that card advances in place: opened by
/// the step, kept current by the lines inside it, closed by the outcome.
#[test]
fn a_step_opens_one_card_whose_state_advances_to_its_outcome() {
    let mut state = common::state();
    state.tick(0);
    state.start_run(common::run_id(1), RunKind::Chat, "post the launch note");
    state.apply(AppEvent::TurnStep {
        run: common::run_id(1),
        step: 0,
        thought: "the composer is the only place a post can be written".into(),
        action: common::browse("https://x.com/compose", "post the launch note"),
    });

    let card = |state: &neo_tui::State| {
        state
            .turn
            .as_ref()
            .and_then(|turn| turn.cards.first().cloned())
            .unwrap_or_else(|| panic!("no card for the step"))
    };
    assert_eq!(state.turn.as_ref().map(|turn| turn.cards.len()), Some(1));
    assert_eq!(card(&state).state, RunState::Running);
    assert!(card(&state).intent.contains("https://x.com/compose"));
    assert_eq!(card(&state).detail, None);

    state.apply(AppEvent::NavStep {
        run: common::run_id(1),
        step: 1,
        line: "opened https://x.com/compose".into(),
        kind: NavStepKind::Launch,
    });
    assert_eq!(state.turn.as_ref().map(|turn| turn.cards.len()), Some(1));
    assert_eq!(
        card(&state).detail.as_deref(),
        Some("opened https://x.com/compose")
    );

    state.apply(AppEvent::TurnStepDone {
        run: common::run_id(1),
        step: 0,
        observation: "Done · the post is up".into(),
        duration_ms: 1_200,
    });
    assert_eq!(state.turn.as_ref().map(|turn| turn.cards.len()), Some(1));
    assert_eq!(card(&state).state, RunState::Done);
    assert_eq!(card(&state).duration_ms, Some(1_200));
}

/// A failed action does not end the turn — the model is told and tries
/// something else — so the only way a user learns it failed is the card.
#[test]
fn a_failed_action_closes_its_card_as_failed() {
    let mut state = common::state();
    state.start_run(common::run_id(1), RunKind::Chat, "post the launch note");
    state.apply(AppEvent::TurnStep {
        run: common::run_id(1),
        step: 0,
        thought: String::new(),
        action: common::browse("https://x.com/compose", "post the launch note"),
    });
    state.apply(AppEvent::TurnStepDone {
        run: common::run_id(1),
        step: 0,
        observation: "that failed: the composer never opened".into(),
        duration_ms: 900,
    });
    assert_eq!(
        state
            .turn
            .as_ref()
            .and_then(|turn| turn.cards.first())
            .map(|card| card.state.clone()),
        Some(RunState::Failed(
            "that failed: the composer never opened".into()
        ))
    );
}

/// The status line's token counts come from the running total, which is
/// republished after every round trip and must not be summed.
#[test]
fn the_running_cost_replaces_rather_than_accumulates() {
    let mut state = common::state();
    state.start_run(common::run_id(1), RunKind::Chat, "what does a seat cost?");
    for (input, output, requests) in [(900, 40, 1), (1_800, 95, 2)] {
        state.apply(AppEvent::TurnCost {
            run: common::run_id(1),
            usage: TurnUsage {
                input_tokens: input,
                output_tokens: output,
                requests,
                ..TurnUsage::default()
            },
        });
    }
    assert_eq!(
        state
            .turn
            .as_ref()
            .and_then(|turn| turn.usage)
            .map(|usage| (usage.input_tokens, usage.output_tokens)),
        Some((1_800, 95))
    );
}

/// A cancelled turn comes back as `TurnFinished` carrying the partial
/// answer, not as `TurnFailed`. The run still has to read `cancelled`, or
/// a stop is indistinguishable from an answer.
#[test]
fn a_stopped_turn_finishes_as_cancelled() {
    let mut state = common::state();
    state.start_run(common::run_id(1), RunKind::Chat, "what does a seat cost?");
    state.apply(AppEvent::TurnDelta {
        run: common::run_id(1),
        seq: 0,
        text: "A seat ".to_owned(),
    });
    state.apply_action(neo_tui::Action::StopRun);
    assert_eq!(
        state.runs.first().map(|run| run.state.clone()),
        Some(RunState::Stopping)
    );

    state.apply(AppEvent::TurnNote {
        run: common::run_id(1),
        step: 0,
        line: neo_agent::agent::STOPPED.to_owned(),
    });
    state.apply(AppEvent::TurnFinished {
        run: common::run_id(1),
        text: "A seat ".into(),
        steps: 1,
        exhausted: false,
        usage: None,
    });
    assert_eq!(
        state.runs.first().map(|run| run.state.clone()),
        Some(RunState::Cancelled)
    );
    assert!(
        state
            .runs
            .first()
            .is_some_and(|run| run.last.starts_with("stopped")),
        "{:?}",
        state.runs.first().map(|run| run.last.clone())
    );
}

/// A steering message is on screen before it is stored, so the store's own
/// record has to land on that row rather than beside it.
#[test]
fn the_stored_record_of_a_steer_is_adopted_rather_than_doubled() {
    let mut state = common::state();
    state.load_thread(common::conversation_id(1), None, &[]);
    state.start_run(common::run_id(1), RunKind::Chat, "post the launch note");
    state.composer = "use the other account".into();
    state.apply_action(neo_tui::Action::ComposerSubmit);
    assert_eq!(state.thread.len(), 1);

    let stored = Message {
        id: neo_core::MessageId::new(),
        conversation_id: common::conversation_id(1),
        role: MessageRole::User,
        source: MessageSource::Typed,
        kind: MessageKind::Text,
        text: "use the other account".into(),
        at: 1_700_000_000_000,
        task_id: None,
        spoken: false,
        meta: None,
    };
    state.apply(AppEvent::Message {
        message: stored.clone(),
    });
    assert_eq!(state.thread.len(), 1, "the steered message was doubled");
    let row = state.thread.first().unwrap_or_else(|| panic!("no row"));
    assert!(row.steered, "the row lost its steering marker");
    assert_eq!(row.id, stored.id, "the stored id was not adopted");

    // A second, unrelated message still appends.
    state.apply(AppEvent::Message {
        message: Message {
            id: neo_core::MessageId::new(),
            text: "and use the short link".into(),
            ..stored
        },
    });
    assert_eq!(state.thread.len(), 2);
}

// ---------------------------------------------------------------- the cards
//
// Driven from `fixtures/cards/envelopes.jsonl` (16 §5.5) rather than from
// envelopes written here: the webview's reducer tests read the same file, so
// a field either front end stops honouring fails on one side or the other.

/// Arm the card the way the loop does: draw a frame, fold the renderer's
/// report back in, then let the debounce elapse.
fn arm(state: &mut State) {
    common::paint(state, 100, 30);
    state.tick(CARD_ARM_MS);
}

fn confirm_id(card: &Card) -> ConfirmId {
    match card.kind {
        CardKind::Confirm { id, .. } => id,
        CardKind::Ask { .. } => panic!("an ask card where a confirm was expected"),
    }
}

/// A tripped gate is a card, and the card carries what the user needs to
/// decide: the sentence, the page, and why it stopped.
#[test]
fn a_confirm_request_raises_the_card_the_corpus_describes() {
    let mut state = common::state();
    state.apply(common::envelope(1));

    let card = state.card.as_ref().unwrap_or_else(|| panic!("no card"));
    assert_eq!(card.sentence, "`Pay $42.00 now` says “pay”.");
    assert_eq!(
        card.context.as_deref(),
        Some("Checkout — https://shop.test/cart")
    );
    assert_eq!(card.cause.as_deref(), Some("safety:spends"));
    // Q2 has nowhere to keep a remembered allow, so no card may claim it can.
    assert!(!card.can_remember);
    // It arrives unarmed and unrendered: the keys are dead until a frame has
    // carried the sentence for the debounce window.
    assert!(!card.live(), "a card was live before it was ever drawn");
    assert_eq!(state.mode, Mode::Card);
}

/// The resolution takes the card down, whoever made it — this front end, the
/// webview, a voice answer, or the broker timing it out. A card left over a
/// run that has moved on is the dead end this phase removes.
#[test]
fn a_resolution_takes_the_card_down_and_an_unrelated_one_leaves_it_alone() {
    let mut state = common::state();
    state.apply(common::envelope(1));
    let live = confirm_id(state.card.as_ref().unwrap_or_else(|| panic!("no card")));

    // Another gate's resolution is not this card's.
    state.apply(AppEvent::ConfirmResolved {
        confirm_id: ConfirmId::new(),
        outcome: GateOutcome::TimedOut,
        via: ResolutionVia::Timeout,
    });
    assert!(
        state.card.is_some(),
        "an unrelated resolution cleared the card"
    );

    state.apply(AppEvent::ConfirmResolved {
        confirm_id: live,
        outcome: GateOutcome::TimedOut,
        via: ResolutionVia::Timeout,
    });
    assert!(state.card.is_none(), "the card outlived its gate");
    assert_eq!(state.mode, Mode::Normal);
}

/// `y` and `n` become one command each, addressed to the gate and stamped
/// with the surface that answered it. The reducer returns the command; it
/// never reaches the runtime itself (14 §4).
#[test]
fn y_and_n_emit_the_resolution_for_this_card_via_the_card() {
    let mut state = common::state();
    state.apply(common::envelope(1));
    let live = confirm_id(state.card.as_ref().unwrap_or_else(|| panic!("no card")));
    arm(&mut state);

    assert_eq!(
        state.apply_action(Action::ResolveConfirm { approve: true }),
        Some(Command::ResolveConfirm {
            confirm: live,
            outcome: GateOutcome::Confirmed,
            via: ResolutionVia::Card,
        })
    );
    // The card stays up until the core says the gate is settled: the answer
    // has to reach the run before the sentence may leave the screen.
    assert!(state.card.is_some());

    assert_eq!(
        state.apply_action(Action::ResolveConfirm { approve: false }),
        Some(Command::ResolveConfirm {
            confirm: live,
            outcome: GateOutcome::Denied,
            via: ResolutionVia::Card,
        })
    );
}

/// The arming rule is the reducer's too, not only the keymap's: an action
/// that arrives from anywhere else — a future binding table, a replayed
/// macro — must not resolve a card nobody has seen (04 §13).
#[test]
fn an_unarmed_card_refuses_the_resolution_and_says_why() {
    let mut state = common::state();
    state.apply(common::envelope(1));

    // Never drawn: `rendered` is false, so nothing resolves.
    assert_eq!(
        state.apply_action(Action::ResolveConfirm { approve: true }),
        None
    );
    assert!(
        state
            .status
            .as_deref()
            .is_some_and(|line| line.contains("y and n")),
        "a refused keystroke said nothing: {:?}",
        state.status
    );

    // Drawn, but inside the debounce window.
    common::paint(&mut state, 100, 30);
    state.tick(CARD_ARM_MS - 1);
    assert_eq!(
        state.apply_action(Action::ResolveConfirm { approve: true }),
        None
    );

    state.tick(CARD_ARM_MS);
    assert!(
        state
            .apply_action(Action::ResolveConfirm { approve: true })
            .is_some(),
        "the card never armed"
    );
}

/// An options ask is answered with the option's own text, so the broker
/// never has to map an index back onto a list it may have reordered.
#[test]
fn an_options_ask_answers_with_the_option_the_user_picked() {
    let mut state = common::state();
    state.apply(common::envelope(8));
    arm(&mut state);
    let card = state.card.as_ref().unwrap_or_else(|| panic!("no card"));
    let ask = match card.kind {
        CardKind::Ask { id, .. } => id,
        CardKind::Confirm { .. } => panic!("a confirm card where an ask was expected"),
    };
    assert_eq!(card.highlighted(), Some("andrew@stark.test"));

    // `j` moves the highlight; `y` sends whatever it is on.
    state.apply_action(Action::SelectNext);
    assert_eq!(
        state.apply_action(Action::ResolveConfirm { approve: true }),
        Some(Command::AnswerAsk {
            ask,
            answer: "ops@stark.test".to_owned(),
            via: ResolutionVia::Card,
        })
    );

    // A digit picks and sends in one keystroke.
    assert_eq!(
        state.apply_action(Action::AnswerAsk(1)),
        Some(Command::AnswerAsk {
            ask,
            answer: "andrew@stark.test".to_owned(),
            via: ResolutionVia::Card,
        })
    );
    // A number that is not on the card answers nothing.
    assert_eq!(state.apply_action(Action::AnswerAsk(7)), None);
    // Neither does the arming rule stop applying to digits.
    state.apply(AppEvent::AskResolved {
        ask_id: ask,
        answer: "ops@stark.test".to_owned(),
        via: ResolutionVia::Card,
    });
    state.apply(common::envelope(8));
    assert_eq!(state.apply_action(Action::AnswerAsk(1)), None);
}

/// A free-text question is typed into the prompt overlay — the one overlay
/// that takes the keyboard back off a card — and sent unmasked.
#[test]
fn a_free_text_ask_is_answered_through_the_prompt() {
    let mut state = common::state();
    state.apply(common::envelope(5));
    arm(&mut state);
    let ask = match state
        .card
        .as_ref()
        .unwrap_or_else(|| panic!("no card"))
        .kind
    {
        CardKind::Ask { id, .. } => id,
        CardKind::Confirm { .. } => panic!("a confirm card where an ask was expected"),
    };
    // There is nothing to pick, so `y` opens the prompt instead of answering.
    assert_eq!(
        state.apply_action(Action::ResolveConfirm { approve: true }),
        None
    );
    let prompt = state.prompt.as_ref().unwrap_or_else(|| panic!("no prompt"));
    assert_eq!(
        prompt.label,
        "Tell me what goes in “Invoice number” and I'll carry on."
    );
    assert!(
        !prompt.masked,
        "an invoice number was collected as a secret"
    );

    for character in "INV-2291".chars() {
        state.apply_action(Action::PromptChar(character));
    }
    assert_eq!(
        state.apply_action(Action::PromptSubmit),
        Some(Command::AnswerAsk {
            ask,
            answer: "INV-2291".to_owned(),
            via: ResolutionVia::Card,
        })
    );
    assert!(state.prompt.is_none());
    assert!(
        state.card.is_some(),
        "the question left before it was settled"
    );

    // An empty answer is not an answer: the question is still waiting.
    state.apply_action(Action::EnterInsert);
    assert!(state.prompt.is_some(), "i did not reopen the answer prompt");
    assert_eq!(state.apply_action(Action::PromptSubmit), None);

    state.apply(common::envelope(6));
    assert!(
        state.card.is_none(),
        "the answered question stayed on screen"
    );
}

/// One card on screen at a time — two sentences competing for one keystroke
/// is how the wrong thing gets approved — but the second gate is kept, not
/// dropped, and it comes up unarmed in its turn.
#[test]
fn a_second_gate_waits_behind_the_card_on_screen() {
    let mut state = common::state();
    state.apply(common::envelope(1));
    let first = confirm_id(state.card.as_ref().unwrap_or_else(|| panic!("no card")));
    arm(&mut state);
    state.apply(common::envelope(7));
    assert_eq!(state.queued_cards.len(), 1);
    assert_eq!(
        confirm_id(state.card.as_ref().unwrap_or_else(|| panic!("no card"))),
        first,
        "the newer gate pushed the one being read off the screen"
    );

    // The same gate republished must not double it.
    state.apply(common::envelope(7));
    assert_eq!(state.queued_cards.len(), 1);

    state.apply(AppEvent::ConfirmResolved {
        confirm_id: first,
        outcome: GateOutcome::Confirmed,
        via: ResolutionVia::Card,
    });
    let card = state
        .card
        .as_ref()
        .unwrap_or_else(|| panic!("the queued gate was dropped"));
    assert_eq!(card.cause.as_deref(), Some("upload:forms.test"));
    assert!(!card.live(), "a queued card came up already armed");
    assert!(state.queued_cards.is_empty());
}

/// A gate can settle while it is still waiting its turn — the broker times it
/// out, or another surface answers it — and must not then be shown.
#[test]
fn a_queued_gate_that_settles_first_is_never_shown() {
    let mut state = common::state();
    state.apply(common::envelope(1));
    state.apply(common::envelope(7));
    let queued = confirm_id(
        state
            .queued_cards
            .front()
            .unwrap_or_else(|| panic!("nothing queued")),
    );
    state.apply(AppEvent::ConfirmResolved {
        confirm_id: queued,
        outcome: GateOutcome::TimedOut,
        via: ResolutionVia::Timeout,
    });
    assert!(state.queued_cards.is_empty());

    let live = confirm_id(state.card.as_ref().unwrap_or_else(|| panic!("no card")));
    state.apply(AppEvent::ConfirmResolved {
        confirm_id: live,
        outcome: GateOutcome::Confirmed,
        via: ResolutionVia::Card,
    });
    assert!(state.card.is_none(), "a settled gate was put on screen");
}

/// `r` is honest while the core cannot remember an allow: it says so and
/// changes nothing. A card that silently toggled a flag nothing honours
/// would be promising the user something the product does not do.
#[test]
fn remembering_an_allow_says_it_is_not_built_yet() {
    let mut state = common::state();
    state.apply(common::envelope(1));
    arm(&mut state);
    let before = state.card.clone();

    assert_eq!(state.apply_action(Action::ToggleRemember), None);
    assert!(
        state
            .status
            .as_deref()
            .is_some_and(|line| line.contains("not built yet")),
        "r claimed something: {:?}",
        state.status
    );
    assert_eq!(state.card, before, "r changed the card");
}

/// The whole corpus, in order, through the reducer: every request raises its
/// card and every resolution takes it down, leaving nothing behind. This is
/// the drift guard — the webview replays the same eight envelopes.
#[test]
fn the_shared_corpus_leaves_no_card_behind() {
    let mut state = common::state();
    let events = common::corpus();
    assert_eq!(events.len(), 8, "the corpus changed shape");
    for event in events {
        let request = matches!(
            event,
            AppEvent::ConfirmRequest { .. } | AppEvent::AskRequest { .. }
        );
        state.apply(event);
        if request {
            assert!(state.card.is_some(), "a request raised no card");
        }
    }
    // Envelopes 7 and 8 are the two extra gates, and the corpus never
    // resolves them: they are the queue's fixtures.
    assert_eq!(state.queued_cards.len(), 1);
    assert!(state.card.is_some());
}

/// The frame loop only draws when the state says it changed, so an action
/// that changes the screen and forgets to say so is a terminal that has
/// stopped painting — which is what a hang looks like from the chair. The
/// arms that return a `Command` are the ones that used to forget.
#[test]
fn an_action_that_carries_a_command_still_asks_for_a_frame() {
    let mut state = common::state();

    // `/settings` changes the view and carries no command.
    state.dirty = false;
    state.apply_action(Action::EnterCommand);
    for character in "settings".chars() {
        state.apply_action(Action::LineChar(character));
    }
    state.dirty = false;
    assert_eq!(state.apply_action(Action::LineSubmit), None);
    assert_eq!(state.view, neo_tui::View::Settings);
    assert!(state.dirty, "the settings view never reached a frame");

    // `/doctor` changes the view and carries one.
    state.dirty = false;
    state.apply_action(Action::EnterCommand);
    for character in "doctor".chars() {
        state.apply_action(Action::LineChar(character));
    }
    state.dirty = false;
    assert_eq!(
        state.apply_action(Action::LineSubmit),
        Some(Command::Doctor)
    );
    assert!(state.dirty, "the doctor view never reached a frame");
}
