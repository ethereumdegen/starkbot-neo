#![allow(clippy::expect_used)]
//! The Claude subscription runtime against a stand-in CLI.
//!
//! The real binary is not driven here: a small script plays the documented
//! NDJSON stream back, so the parse, the confinement and the refusal to accept
//! a tool call are all testable without a login or a network.

use std::path::{Path, PathBuf};

use neo_agent::claude::{ClaudeCode, ClaudeCodeConfig, ClaudeError};
use neo_core::ProviderAccountStatus;
use serde_json::json;

/// Write a `claude` stand-in that answers `auth status` with `status_json` and
/// `--print` with the given NDJSON lines.
fn fake_cli(home: &Path, status_json: &str, stream: &[&str]) -> PathBuf {
    let path = home.join("claude");
    let stream = stream.join("\n");
    let script = format!(
        "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = \"status\" ]; then\n    cat <<'STATUS'\n{status_json}\nSTATUS\n    exit 0\n  fi\ndone\ncat <<'STREAM'\n{stream}\nSTREAM\n"
    );
    std::fs::write(&path, script).expect("the stand-in writes");
    let mut permissions = std::fs::metadata(&path)
        .expect("the stand-in exists")
        .permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&path, permissions).expect("the stand-in is executable");
    path
}

const CONNECTED: &str = r#"{"loggedIn":true,"authMethod":"claudeai","subscriptionType":"max"}"#;
const SIGNED_OUT: &str = r#"{"loggedIn":false,"authMethod":"none"}"#;

fn client(home: &Path, status_json: &str, stream: &[&str]) -> ClaudeCode {
    let executable = fake_cli(home, status_json, stream);
    ClaudeCode::new(ClaudeCodeConfig::new(executable, home))
}

#[tokio::test]
async fn a_connected_subscription_reports_its_plan() {
    let home = tempfile::tempdir().expect("a temporary home");
    let claude = client(home.path(), CONNECTED, &[]);

    let account = claude.account().await.expect("the status parses");

    assert_eq!(account.status, ProviderAccountStatus::Connected);
    assert_eq!(account.plan_type.as_deref(), Some("max"));
    assert_eq!(
        account.provider.as_str(),
        neo_core::PROVIDER_CLAUDE_SUBSCRIPTION
    );
    assert!(account.allowance.is_none(), "no invented allowance");
}

#[tokio::test]
async fn a_text_turn_reads_the_result_event() {
    let home = tempfile::tempdir().expect("a temporary home");
    let claude = client(
        home.path(),
        CONNECTED,
        &[
            r#"{"type":"system","subtype":"init","session_id":"s-1"}"#,
            r#"{"type":"assistant","message":{"model":"claude-sonnet-5","content":[{"type":"thinking"},{"type":"text","text":"Two."}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"Two.","session_id":"s-1","duration_ms":42,"usage":{"input_tokens":9}}"#,
        ],
    );

    let turn = claude.complete_text("1+1?").await.expect("a clean turn");

    assert_eq!(turn.text, "Two.");
    assert_eq!(turn.model, "claude-sonnet-5");
    assert_eq!(turn.session_id, "s-1");
    assert_eq!(turn.duration_ms, 42);
    assert_eq!(turn.usage["input_tokens"], json!(9));
}

#[tokio::test]
async fn strict_json_answers_are_parsed() {
    let home = tempfile::tempdir().expect("a temporary home");
    let claude = client(
        home.path(),
        CONNECTED,
        &[
            r#"{"type":"result","subtype":"success","is_error":false,"result":"{\"text\":\"Bergen\"}","session_id":"s-2"}"#,
        ],
    );

    let (value, _turn) = claude
        .complete_json("the city", &json!({"type":"object"}))
        .await
        .expect("a JSON turn");

    assert_eq!(value["text"], json!("Bergen"));
}

#[tokio::test]
async fn a_tool_call_ends_the_turn() {
    let home = tempfile::tempdir().expect("a temporary home");
    let claude = client(
        home.path(),
        CONNECTED,
        &[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"ran it"}"#,
        ],
    );

    let error = claude
        .complete_text("do something")
        .await
        .expect_err("a tool call is a protocol failure");

    match error {
        ClaudeError::Protocol(message) => {
            assert!(message.contains("Bash"), "{message}");
            assert!(message.contains("forbids"), "{message}");
        }
        other => panic!("expected a protocol failure, got {other}"),
    }
}

#[tokio::test]
async fn a_denied_permission_ends_the_turn() {
    let home = tempfile::tempdir().expect("a temporary home");
    let claude = client(
        home.path(),
        CONNECTED,
        &[
            r#"{"type":"result","subtype":"success","is_error":false,"result":"hmm","permission_denials":[{"tool_name":"Write"}]}"#,
        ],
    );

    let error = claude
        .complete_text("write a file")
        .await
        .expect_err("a denial is a protocol failure");
    assert!(matches!(error, ClaudeError::Protocol(_)), "{error}");
}

#[tokio::test]
async fn a_failed_turn_reports_the_cli_message() {
    let home = tempfile::tempdir().expect("a temporary home");
    let claude = client(
        home.path(),
        CONNECTED,
        &[r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"model overloaded"}"#],
    );

    match claude.complete_text("hello").await {
        Err(ClaudeError::Failed(message)) => assert_eq!(message, "model overloaded"),
        other => panic!("expected a failed turn, got {other:?}"),
    }
}

#[tokio::test]
async fn nothing_runs_without_a_subscription() {
    let home = tempfile::tempdir().expect("a temporary home");
    let claude = client(
        home.path(),
        SIGNED_OUT,
        &[r#"{"type":"result","subtype":"success","is_error":false,"result":"should never be read"}"#],
    );

    assert!(matches!(
        claude.complete_text("hello").await,
        Err(ClaudeError::SignedOut)
    ));
    assert_eq!(
        claude
            .account()
            .await
            .expect("the status still parses")
            .status,
        ProviderAccountStatus::SignedOut
    );
}

#[tokio::test]
async fn a_stream_without_a_result_is_a_protocol_failure() {
    let home = tempfile::tempdir().expect("a temporary home");
    let claude = client(
        home.path(),
        CONNECTED,
        &[r#"{"type":"assistant","message":{"content":[{"type":"text","text":"half"}]}}"#],
    );

    assert!(matches!(
        claude.complete_text("hello").await,
        Err(ClaudeError::Protocol(_))
    ));
}

/// The real CLI, with whatever login the config home holds. Run it when you
/// want to see the subscription answer:
///
/// ```sh
/// cargo test -p neo-agent --test claude_cli -- --ignored live
/// ```
#[tokio::test]
#[ignore = "drives the real Claude Code CLI and needs a connected subscription"]
async fn live_subscription_answers_a_turn() {
    let home = tempfile::tempdir().expect("a temporary home");
    let executable = PathBuf::from(neo_agent::claude::configured_executable());
    let config = ClaudeCodeConfig::new(executable, home.path()).with_config_dir(
        std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").expect("HOME is set")).join(".claude")
            }),
    );
    let claude = ClaudeCode::new(config);

    let account = claude.account().await.expect("the CLI reports its status");
    assert_eq!(
        account.status,
        ProviderAccountStatus::Connected,
        "log in first: neo account login --provider claude-subscription"
    );

    let turn = claude
        .complete_text("Reply with exactly: ready")
        .await
        .expect("the subscription answers");
    assert!(
        turn.text.to_lowercase().contains("ready"),
        "unexpected answer: {}",
        turn.text
    );
}
