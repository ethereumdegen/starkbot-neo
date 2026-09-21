//! Driving the Claude Code CLI headlessly: one turn is one process, its answer
//! is read off the NDJSON stream, and nothing but assistant text, thinking and
//! the final result is accepted.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use neo_core::ProviderAccount;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use super::account::{AuthStatus, account_from_status};

#[derive(Debug, thiserror::Error)]
pub enum ClaudeError {
    #[error("the Claude Code CLI is not available at {0}")]
    Missing(PathBuf),
    #[error("could not run the Claude Code CLI: {0}")]
    Io(#[from] std::io::Error),
    /// The CLI answered in a shape this runtime does not accept — including an
    /// event that would mean it ran a command or touched a file (P3, A22).
    #[error("the Claude Code CLI answered unexpectedly: {0}")]
    Protocol(String),
    #[error("the Claude Code CLI failed: {0}")]
    Failed(String),
    #[error("no Claude subscription is connected; run `neo account --provider claude-subscription login`")]
    SignedOut,
    #[error("the Claude Code CLI did not answer within {0:?}")]
    Timeout(Duration),
    #[error("system clock error: {0}")]
    Clock(#[from] std::time::SystemTimeError),
    #[error("system clock cannot fit in a SQLite integer")]
    ClockOverflow,
}

/// Where the helper lives and how it is confined.
#[derive(Clone, Debug)]
pub struct ClaudeCodeConfig {
    /// The CLI to run. `configured_executable` finds it; a pinned nested helper
    /// replaces it once §9.1's fetch lands.
    pub executable: PathBuf,
    /// The CLI's own configuration home (`CLAUDE_CONFIG_DIR`). A dedicated
    /// directory keeps Starkbot's session out of the user's everyday Claude
    /// Code — at the cost of a separate login. Point it at `~/.claude` to reuse
    /// an existing login.
    pub config_dir: PathBuf,
    /// An empty scratch directory: the CLI's working directory, so nothing in
    /// the user's projects is even visible to it.
    pub workspace: PathBuf,
    /// `--model`; `None` leaves the CLI's own default.
    pub model: Option<String>,
    pub timeout: Duration,
}

impl ClaudeCodeConfig {
    pub fn new(executable: impl Into<PathBuf>, home: impl AsRef<Path>) -> Self {
        let home = home.as_ref();
        Self {
            executable: executable.into(),
            config_dir: home.join("config"),
            workspace: home.join("workspace"),
            model: None,
            timeout: Duration::from_secs(300),
        }
    }

    /// Reuse an existing Claude Code login instead of a dedicated home.
    #[must_use]
    pub fn with_config_dir(mut self, config_dir: impl Into<PathBuf>) -> Self {
        self.config_dir = config_dir.into();
        self
    }

    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }
}

/// One finished turn.
#[derive(Clone, Debug, PartialEq)]
pub struct ClaudeTurn {
    pub text: String,
    pub model: String,
    pub session_id: String,
    pub duration_ms: u64,
    /// The CLI's own usage block, kept verbatim. Plan work is
    /// `Usd::Unpriced` (05 §7): the dollar figure the CLI prints is never
    /// shown as Starkbot spend.
    pub usage: Value,
}

pub struct ClaudeCode {
    config: ClaudeCodeConfig,
}

impl ClaudeCode {
    pub fn new(config: ClaudeCodeConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &ClaudeCodeConfig {
        &self.config
    }

    /// What `claude auth status` says, as a redacted account row.
    pub async fn account(&self) -> Result<ProviderAccount, ClaudeError> {
        let status = self.auth_status().await?;
        Ok(account_from_status(&status, now_ms()?))
    }

    pub async fn auth_status(&self) -> Result<AuthStatus, ClaudeError> {
        self.prepare().await?;
        let output = self.command(["auth", "status"])?.output().await?;
        let text = String::from_utf8_lossy(&output.stdout);
        // The CLI exits non-zero when signed out but still prints the object,
        // so the JSON is what decides, not the exit status.
        serde_json::from_str::<AuthStatus>(text.trim())
            .map_err(|error| ClaudeError::Protocol(format!("`auth status`: {error}")))
    }

    /// Hand the terminal to the vendor's own login flow.
    ///
    /// Neo never sees the browser round-trip, the code exchange or the token:
    /// the CLI owns all three and stores the credential in its own home.
    pub async fn login(&self) -> Result<ProviderAccount, ClaudeError> {
        self.prepare().await?;
        let status = self
            .command(["auth", "login"])?
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .await?;
        if !status.success() {
            return Err(ClaudeError::Failed(
                "the Claude Code login did not complete".into(),
            ));
        }
        self.account().await
    }

    /// Sign this app's dedicated session out, leaving the user's everyday
    /// Claude Code login alone.
    pub async fn logout(&self) -> Result<ProviderAccount, ClaudeError> {
        self.prepare().await?;
        let _ = self.command(["auth", "logout"])?.output().await?;
        self.account().await
    }

    /// One text turn on the connected subscription.
    pub async fn complete_text(&self, prompt: &str) -> Result<ClaudeTurn, ClaudeError> {
        self.turn(prompt, None).await
    }

    /// One turn that must answer with JSON matching `schema` — the text
    /// helper's and the extractor's shape (04 §14, 10).
    pub async fn complete_json(
        &self,
        prompt: &str,
        schema: &Value,
    ) -> Result<(Value, ClaudeTurn), ClaudeError> {
        let encoded = serde_json::to_string(schema)
            .map_err(|error| ClaudeError::Protocol(format!("unusable schema: {error}")))?;
        let turn = self.turn(prompt, Some(&encoded)).await?;
        let value = serde_json::from_str(turn.text.trim()).map_err(|error| {
            ClaudeError::Protocol(format!("the answer was not the JSON asked for: {error}"))
        })?;
        Ok((value, turn))
    }

    async fn turn(&self, prompt: &str, schema: Option<&str>) -> Result<ClaudeTurn, ClaudeError> {
        if !self.auth_status().await?.subscription() {
            return Err(ClaudeError::SignedOut);
        }
        let mut command = self.command(["--print"])?;
        command.args(self.confinement());
        command.args(["--output-format", "stream-json", "--verbose"]);
        if let Some(schema) = schema {
            command.args(["--json-schema", schema]);
        }
        if let Some(model) = &self.config.model {
            command.args(["--model", model]);
        }
        command
            .arg(prompt)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ClaudeError::Protocol("stdout was not piped".into()))?;
        let read = read_turn(stdout);
        let turn = match tokio::time::timeout(self.config.timeout, read).await {
            Ok(result) => result,
            Err(_) => {
                let _ = child.kill().await;
                return Err(ClaudeError::Timeout(self.config.timeout));
            }
        };
        let _ = child.wait().await;
        turn
    }

    /// The flags that make a coding agent safe for a marketing harness (P3):
    /// no built-in tools at all, no discovered configuration, no prompt that
    /// could ask a human who is not there, and an empty working directory.
    fn confinement(&self) -> Vec<OsString> {
        [
            "--restricted",
            "--tools",
            "",
            "--disallowedTools",
            "*",
            "--permission-mode",
            "dontAsk",
            "--permission-prompts",
            "none",
            "--disable-slash-commands",
            "--strict-mcp-config",
            "--no-session-persistence",
        ]
        .into_iter()
        .map(OsString::from)
        .collect()
    }

    fn command<'a>(
        &self,
        args: impl IntoIterator<Item = &'a str>,
    ) -> Result<Command, ClaudeError> {
        if !self.config.executable.is_file() && self.config.executable.components().count() > 1 {
            return Err(ClaudeError::Missing(self.config.executable.clone()));
        }
        let mut command = Command::new(&self.config.executable);
        command
            .args(args)
            .current_dir(&self.config.workspace)
            .env_clear()
            .env("CLAUDE_CONFIG_DIR", &self.config.config_dir)
            .kill_on_drop(true);
        copy_safe_environment(&mut command);
        Ok(command)
    }

    async fn prepare(&self) -> Result<(), ClaudeError> {
        tokio::fs::create_dir_all(&self.config.config_dir).await?;
        tokio::fs::create_dir_all(&self.config.workspace).await?;
        Ok(())
    }
}

/// Events this runtime accepts, and the ones that end a turn.
///
/// `system`/`assistant`/`result` are the normal stream. A `user` event carrying
/// a tool result, or an assistant message containing a `tool_use` block, would
/// mean the CLI ran something despite the confinement: that is a protocol
/// failure, not a turn (A22).
#[derive(Deserialize)]
#[serde(tag = "type")]
enum Event {
    #[serde(rename = "system")]
    System { session_id: Option<String> },
    #[serde(rename = "assistant")]
    Assistant { message: AssistantMessage },
    #[serde(rename = "result")]
    Result(ResultEvent),
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct AssistantMessage {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking {},
    #[serde(rename = "tool_use")]
    ToolUse { name: Option<String> },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct ResultEvent {
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    duration_ms: Option<u64>,
    #[serde(default)]
    usage: Option<Value>,
    #[serde(default)]
    permission_denials: Vec<Value>,
}

async fn read_turn(stdout: tokio::process::ChildStdout) -> Result<ClaudeTurn, ClaudeError> {
    let mut lines = BufReader::new(stdout).lines();
    let mut session_id = String::new();
    let mut model = String::new();
    let mut text = String::new();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event: Event = serde_json::from_str(line)
            .map_err(|error| ClaudeError::Protocol(format!("unreadable event: {error}")))?;
        match event {
            Event::System { session_id: id } => {
                if let Some(id) = id {
                    session_id = id;
                }
            }
            Event::Assistant { message } => {
                if let Some(reported) = message.model {
                    model = reported;
                }
                for block in message.content {
                    match block {
                        ContentBlock::Text { text: chunk } => text.push_str(&chunk),
                        ContentBlock::ToolUse { name } => {
                            return Err(ClaudeError::Protocol(format!(
                                "the CLI tried to use the `{}` tool, which this runtime forbids",
                                name.unwrap_or_else(|| "unknown".into())
                            )));
                        }
                        ContentBlock::Thinking {} | ContentBlock::Other => {}
                    }
                }
            }
            Event::Result(result) => {
                if !result.permission_denials.is_empty() {
                    return Err(ClaudeError::Protocol(
                        "the CLI asked for a permission this runtime denies".into(),
                    ));
                }
                if result.is_error {
                    return Err(ClaudeError::Failed(
                        result
                            .result
                            .or(result.subtype)
                            .unwrap_or_else(|| "the turn failed".into()),
                    ));
                }
                let answer = result.result.unwrap_or(text);
                return Ok(ClaudeTurn {
                    text: answer,
                    model,
                    session_id: result.session_id.unwrap_or(session_id),
                    duration_ms: result.duration_ms.unwrap_or_default(),
                    usage: result.usage.unwrap_or(Value::Null),
                });
            }
            Event::Other => {}
        }
    }
    Err(ClaudeError::Protocol(
        "the stream ended without a result".into(),
    ))
}

fn copy_safe_environment(command: &mut Command) {
    // `PATH` is needed because the CLI shells out to its own node runtime.
    for name in ["HOME", "PATH", "TMPDIR", "LANG", "LC_ALL", "USER", "LOGNAME"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

/// Starkbot's dedicated helper home under Application Support.
#[must_use]
pub fn default_claude_home(application_support: &Path) -> PathBuf {
    application_support.join("claude-code")
}

/// `NEO_CLAUDE_BIN` overrides the executable; otherwise `claude` is resolved on
/// `PATH`, which is how a developer machine already has it.
#[must_use]
pub fn configured_executable() -> OsString {
    std::env::var_os("NEO_CLAUDE_BIN").unwrap_or_else(|| OsString::from("claude"))
}

fn now_ms() -> Result<i64, ClaudeError> {
    let milliseconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis();
    i64::try_from(milliseconds).map_err(|_| ClaudeError::ClockOverflow)
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn the_dedicated_home_is_not_the_users_own_claude() {
        let home = default_claude_home(Path::new("/tmp/starkbot-neo"));
        assert_eq!(home, Path::new("/tmp/starkbot-neo/claude-code"));
        let config = ClaudeCodeConfig::new("claude", &home);
        assert!(config.config_dir.starts_with(&home));
        assert!(config.workspace.starts_with(&home));
        assert_ne!(config.config_dir, Path::new("/tmp/starkbot-neo/.claude"));
    }

    #[test]
    fn every_built_in_tool_is_switched_off() {
        let config = ClaudeCodeConfig::new("claude", Path::new("/tmp/x"));
        let flags: Vec<String> = ClaudeCode::new(config)
            .confinement()
            .into_iter()
            .map(|flag| flag.to_string_lossy().into_owned())
            .collect();
        for expected in [
            "--restricted",
            "--tools",
            "--disallowedTools",
            "--permission-mode",
            "dontAsk",
            "--permission-prompts",
            "none",
            "--disable-slash-commands",
        ] {
            assert!(flags.iter().any(|flag| flag == expected), "{expected}");
        }
        assert!(
            !flags.iter().any(|flag| flag.contains("dangerously")),
            "no bypass flag may ever appear"
        );
    }
}
