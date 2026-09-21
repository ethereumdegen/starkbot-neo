//! The Starkbot-owned key accounts and the development-only environment
//! fallback that can stand in for the Keychain (05 §6).
//!
//! The Keychain is authoritative. The environment is a fallback for developers
//! running `neo` and `neo tui` from a shell; a release app reads a key from the
//! environment only under `cfg!(debug_assertions)`, and never otherwise. The
//! lookup lives here rather than in the callers so that exactly one table maps
//! an account to an environment variable name.
//!
//! Neither subscription path (ChatGPT via the Codex helper, the Claude
//! subscription via the Claude Code CLI) is an account here, so neither has an
//! environment fallback: those credentials live in the vendor helpers' own
//! homes and Starkbot never stores or reads them (K6, 05 §6).

use crate::Secret;

/// The OpenAI API-key account (K6 path a).
pub const ACCOUNT_OPENAI: &str = "openai";
/// The Anthropic API-key account (K6 path c).
pub const ACCOUNT_ANTHROPIC: &str = "anthropic";
/// The TypeSafe account used by the navigator (A6).
pub const ACCOUNT_TYPESAFE: &str = "typesafe";

/// The core direct-key accounts, in the order the UI reports them.
///
/// Direct-key accounts are an open set (05 §6): user-installed packs add their
/// own `requires_env` names at runtime. These three are the ones Starkbot
/// itself owns.
pub const CORE_ACCOUNTS: [&str; 3] = [ACCOUNT_OPENAI, ACCOUNT_ANTHROPIC, ACCOUNT_TYPESAFE];

/// Environment variable a core account may fall back to, if it has one.
///
/// Unknown accounts — including every pack account, which reads only the
/// environment names its own manifest declares — get `None`.
#[must_use]
pub fn env_var(account: &str) -> Option<&'static str> {
    match account {
        ACCOUNT_OPENAI => Some("OPENAI_API_KEY"),
        ACCOUNT_ANTHROPIC => Some("ANTHROPIC_API_KEY"),
        ACCOUNT_TYPESAFE => Some("TYPESAFE_API_KEY"),
        _ => None,
    }
}

/// Read an account's secret from the environment.
///
/// Only a caller that has already missed in the Keychain should use this: the
/// Keychain wins whenever it holds the account. A variable that is unset, empty
/// or only whitespace is treated as absent rather than as an empty secret, so a
/// stray `export OPENAI_API_KEY=` never masks a stored key. Surrounding
/// whitespace — a trailing newline from a shell heredoc, most often — is
/// trimmed off the value.
#[must_use]
pub fn from_env(account: &str) -> Option<Secret> {
    from_lookup(account, |name| std::env::var(name).ok())
}

/// The body of [`from_env`] with the environment read injected, so the mapping
/// and the empty-value rule can be tested without mutating the process
/// environment (which is `unsafe` in edition 2024, and this crate forbids
/// `unsafe`).
fn from_lookup(account: &str, lookup: impl FnOnce(&str) -> Option<String>) -> Option<Secret> {
    let raw = lookup(env_var(account)?)?;
    Secret::new(raw.trim()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_core_account_to_a_variable() {
        assert_eq!(env_var(ACCOUNT_OPENAI), Some("OPENAI_API_KEY"));
        assert_eq!(env_var(ACCOUNT_ANTHROPIC), Some("ANTHROPIC_API_KEY"));
        assert_eq!(env_var(ACCOUNT_TYPESAFE), Some("TYPESAFE_API_KEY"));
        assert!(
            CORE_ACCOUNTS
                .iter()
                .all(|account| env_var(account).is_some())
        );
    }

    #[test]
    fn unknown_accounts_have_no_fallback() {
        assert_eq!(env_var("chatgpt-codex"), None);
        assert_eq!(env_var("claude-subscription"), None);
        assert_eq!(env_var("some-pack-key"), None);
        assert_eq!(env_var(""), None);
        assert!(from_lookup("some-pack-key", |_| Some("value".to_owned())).is_none());
    }

    #[test]
    fn reads_the_accounts_own_variable() {
        let secret = from_lookup(ACCOUNT_ANTHROPIC, |name| {
            assert_eq!(name, "ANTHROPIC_API_KEY");
            Some("  sk-ant-example-7788\n".to_owned())
        });

        let secret = secret.unwrap_or_else(|| panic!("expected a secret from the environment"));
        #[allow(clippy::disallowed_methods)]
        let exposed = secret.expose();
        assert_eq!(exposed, "sk-ant-example-7788");
    }

    #[test]
    fn blank_values_are_absent_not_empty_secrets() {
        assert!(from_lookup(ACCOUNT_OPENAI, |_| Some(String::new())).is_none());
        assert!(from_lookup(ACCOUNT_OPENAI, |_| Some("   \n".to_owned())).is_none());
        assert!(from_lookup(ACCOUNT_OPENAI, |_| None).is_none());
    }
}
