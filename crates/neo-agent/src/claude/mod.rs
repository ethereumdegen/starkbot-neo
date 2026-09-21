//! The Claude subscription path (K6 path d, A22): Starkbot drives the user's
//! own pinned Claude Code CLI in headless streaming-JSON mode and never sees a
//! token. The CLI owns the login, the refresh and the credential store; Neo
//! keeps only a redacted account row (05 §6).
//!
//! Two rules shape everything here:
//!
//! * **P3** — Starkbot is not a coding agent. Every built-in tool of the CLI is
//!   switched off (`--restricted`, `--tools ""`, `--disallowedTools "*"`), it
//!   runs in an empty scratch directory, and any command-execution or
//!   file-change event on the stream ends the turn with a protocol error.
//! * **A22** — one tool surface. This runtime answers text and strict JSON
//!   today; Starkbot's own tools arrive as a local MCP server when the Sol loop
//!   lands in M5, and they will pass the same `Gated<T>` as every other path.

mod account;
mod client;

pub use account::{AuthStatus, account_from_status};
pub use client::{
    ClaudeCode, ClaudeCodeConfig, ClaudeError, ClaudeTurn, configured_executable,
    default_claude_home,
};
