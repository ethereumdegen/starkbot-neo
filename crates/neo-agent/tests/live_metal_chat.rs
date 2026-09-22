//! Live smoke coverage for Metalcraft's real inference and Bash tool path.
//!
//! Ignored in the ordinary suite because it uses the operator's configured
//! inference connection. Run it explicitly when changing the agent registry.

use std::sync::Arc;

use neo_agent::Runtime;
use neo_agent::agent::{ChatMessage, ChatRequest};

#[tokio::test]
#[ignore = "requires a configured inference connection"]
async fn metalcraft_bash_discovers_octaweave() {
    let directory = tempfile::tempdir().expect("a temporary project directory");
    let data_dir = neo_core::paths::data_dir().expect("the Starkbot data directory");
    let runtime = Arc::new(Runtime::open(&data_dir).expect("the configured runtime opens"));
    let conversation = runtime
        .new_conversation(Some("Metalcraft Bash smoke".to_owned()))
        .expect("the conversation opens");
    let request = ChatRequest::new(
        conversation.id,
        vec![ChatMessage::user(
            "Use bash to run `which octaweave`, then reply with only its absolute path.",
        )],
    )
    .with_cwd(directory.path());

    let outcome = runtime
        .chat(request)
        .await
        .expect("Metalcraft completes the turn");

    assert!(
        outcome.text.trim_end().ends_with("/octaweave"),
        "Metalcraft did not discover Octaweave through bash: {:?}",
        outcome.text
    );
    assert!(
        runtime
            .history(conversation.id, 20)
            .expect("the recorded thread reads")
            .iter()
            .any(|message| message.text.trim_end().ends_with("/octaweave")),
        "the streamed Metalcraft answer was not persisted in the Starkbot thread"
    );
    assert_eq!(
        runtime
            .turns(conversation.id, 20)
            .expect("the turn records read")
            .len(),
        1,
        "Metalcraft usage should produce one Starkbot turn record"
    );
    runtime
        .delete_conversation(conversation.id)
        .expect("the live smoke conversation is removed");
}

#[tokio::test]
#[ignore = "requires a configured inference connection"]
async fn steering_reaches_the_live_metalcraft_graph() {
    let directory = tempfile::tempdir().expect("a temporary project directory");
    let data_dir = neo_core::paths::data_dir().expect("the Starkbot data directory");
    let runtime = Arc::new(Runtime::open(&data_dir).expect("the configured runtime opens"));
    let conversation = runtime
        .new_conversation(Some("Metalcraft steering smoke".to_owned()))
        .expect("the conversation opens");
    let request = ChatRequest::new(
        conversation.id,
        vec![ChatMessage::user(
            "Use bash to run `sleep 10`, then reply with exactly: ORIGINAL",
        )],
    )
    .with_cwd(directory.path());
    let run = request.run;
    let mut events = runtime.subscribe();
    let worker = {
        let runtime = Arc::clone(&runtime);
        tokio::spawn(async move { runtime.chat(request).await })
    };

    loop {
        let envelope = tokio::time::timeout(std::time::Duration::from_secs(30), events.recv())
            .await
            .expect("Metalcraft starts bash")
            .expect("the event stream stays open");
        if matches!(
            envelope.event,
            neo_core::AppEvent::TurnStep {
                run: event_run,
                action: neo_core::ActionSummary {
                    kind: neo_core::ActionKind::Bash,
                    ..
                },
                ..
            } if event_run == run
        ) {
            break;
        }
    }
    assert!(
        runtime
            .steer(run, "Stop waiting and reply with exactly: STEERED")
            .expect("the steer is accepted"),
        "the Metalcraft turn must remain reachable while it runs"
    );
    let outcome = worker
        .await
        .expect("the Metalcraft worker joins")
        .expect("the steered Metalcraft turn completes");

    assert_eq!(outcome.text.trim(), "STEERED");
    runtime
        .delete_conversation(conversation.id)
        .expect("the live smoke conversation is removed");
}

#[tokio::test]
#[ignore = "requires a configured inference connection"]
async fn cancellation_stops_live_metalcraft_bash() {
    let directory = tempfile::tempdir().expect("a temporary project directory");
    let data_dir = neo_core::paths::data_dir().expect("the Starkbot data directory");
    let runtime = Arc::new(Runtime::open(&data_dir).expect("the configured runtime opens"));
    let conversation = runtime
        .new_conversation(Some("Metalcraft cancellation smoke".to_owned()))
        .expect("the conversation opens");
    let cancel = tokio_util::sync::CancellationToken::new();
    let request = ChatRequest::new(
        conversation.id,
        vec![ChatMessage::user(
            "Use bash to run `sleep 30`, then reply with exactly: TOO_LATE",
        )],
    )
    .with_cwd(directory.path())
    .with_cancel(cancel.clone());
    let run = request.run;
    let mut events = runtime.subscribe();
    let worker = {
        let runtime = Arc::clone(&runtime);
        tokio::spawn(async move { runtime.chat(request).await })
    };

    loop {
        let envelope = tokio::time::timeout(std::time::Duration::from_secs(30), events.recv())
            .await
            .expect("Metalcraft starts bash")
            .expect("the event stream stays open");
        if matches!(
            envelope.event,
            neo_core::AppEvent::TurnStep {
                run: event_run,
                action: neo_core::ActionSummary {
                    kind: neo_core::ActionKind::Bash,
                    ..
                },
                ..
            } if event_run == run
        ) {
            break;
        }
    }
    let stopped_at = std::time::Instant::now();
    cancel.cancel();
    let outcome = worker
        .await
        .expect("the Metalcraft worker joins")
        .expect("cancellation is an ordinary turn outcome");

    assert!(outcome.cancelled);
    assert!(
        stopped_at.elapsed() < std::time::Duration::from_secs(5),
        "cancelling left the Bash command running"
    );
    assert!(!outcome.text.contains("TOO_LATE"));
    runtime
        .delete_conversation(conversation.id)
        .expect("the live smoke conversation is removed");
}
