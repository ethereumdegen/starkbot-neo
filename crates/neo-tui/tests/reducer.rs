//! Event reducer behaviour (14 §6, "Event handling").

mod common;

use neo_core::{
    AppEvent, InferenceConnection, KeyState, Message, MessageKind, MessageRole, MessageSource,
    NavStepKind, NoticeLevel, PROVIDER_ANTHROPIC, PROVIDER_OPENAI, ReasoningEffort, Settings,
    TurnUsage,
};
use neo_tui::{RunKind, RunState};

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
    let last = state.activity.back().map(|entry| (entry.kind, entry.detail.clone()));
    assert_eq!(
        last,
        Some(("latency", "step 41ms · jev 168ms".to_owned()))
    );
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
    assert_eq!(state.activity.back().map(|entry| entry.kind), Some("notice"));
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
