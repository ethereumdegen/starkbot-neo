#![allow(clippy::expect_used)]
// A live test's whole output is the answer the vendor gave; there is nothing
// to assert about a sentence a model wrote, so it is printed and read.
#![allow(clippy::print_stdout)]

//! The Claude Pro/Max subscription driving a real chat turn.
//!
//! Both tests are `#[ignore]`d: they spend the user's plan allowance against
//! `api.anthropic.com` with the credential in the *installed* data directory,
//! which is the only place a connected subscription lives. Run them with
//!
//! ```text
//! cargo test -p neo-agent --test live_claude_oauth -- --ignored --nocapture
//! ```
//!
//! after `neo account --provider anthropic-oauth login` reports `connected`.
//! A debug build reads the credential from `<data_dir>/keys.json`, so no
//! Keychain prompt appears.

use std::path::PathBuf;
use std::sync::Arc;

use neo_agent::Runtime;
use neo_agent::agent::{ChatMessage, ChatRequest};

/// The installed data directory — the same one `neo` itself opens. A live
/// test cannot use a temporary one: the credential it needs is in this one.
fn installed_data_dir() -> PathBuf {
    #[allow(clippy::disallowed_methods)]
    let home = std::env::var("HOME").expect("a home directory");
    PathBuf::from(home).join("Library/Application Support/com.starkbot.neo")
}

fn runtime() -> Arc<Runtime> {
    let directory = installed_data_dir();
    assert!(
        directory.is_dir(),
        "no installed data directory at {}; run `neo` once first",
        directory.display(),
    );
    Arc::new(Runtime::open(&directory).expect("the installed store opens"))
}

async fn answer(prompt: &str, max_steps: usize) -> String {
    let runtime = runtime();
    let settings = runtime.settings().expect("settings");
    assert_eq!(
        settings.models.inference.provider.as_str(),
        neo_core::PROVIDER_ANTHROPIC_OAUTH,
        "select the Claude subscription before running this",
    );

    let conversation = runtime
        .new_conversation(Some("live oauth turn".to_owned()))
        .expect("a conversation")
        .id;
    let outcome = runtime
        .chat(
            ChatRequest::new(conversation, vec![ChatMessage::user(prompt)])
                .with_max_steps(max_steps),
        )
        .await
        .expect("the turn runs on the subscription");

    println!("--- prompt ---\n{prompt}");
    println!("--- answer ---\n{}", outcome.text);
    println!("--- steps ---");
    for step in &outcome.steps {
        println!("  {} -> {}", step.action, step.observation);
    }
    println!("--- usage ---\n{:?}", outcome.usage);
    outcome.text
}

/// The regression this whole path exists for: a chat turn on a Claude
/// subscription used to be refused before it reached the vendor.
#[tokio::test]
#[ignore = "spends the connected Claude plan against api.anthropic.com"]
async fn a_plain_turn_answers_on_the_subscription() {
    let text = answer("In one short sentence, what is the capital of France?", 2).await;
    assert!(!text.trim().is_empty(), "the plan answered with nothing");
}

/// The half a chat box cannot fake. Without `tool_use` coming back as a rig
/// tool call the graph never acts, so a turn that reports the page heading
/// proves the translation in both directions on a real vendor response.
#[tokio::test]
#[ignore = "spends the connected Claude plan and drives a real browser"]
async fn a_turn_uses_a_tool_on_the_subscription() {
    let text = answer(
        "Open https://example.com in the browser and tell me the text of the \
         page's main heading.",
        8,
    )
    .await;
    assert!(
        text.to_lowercase().contains("example domain"),
        "the model never read the page: {text}",
    );
}
