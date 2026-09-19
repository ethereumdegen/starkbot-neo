#![forbid(unsafe_code)]

use std::fmt;
use zeroize::Zeroize;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SecretError {
    #[error("a secret cannot be empty")]
    Empty,
}

/// Process-local secret material.
///
/// The inner value is intentionally private, is never serializable, and is
/// zeroized on drop. Share a secret with `Arc<Secret>` rather than cloning it.
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Result<Self, SecretError> {
        let value = value.into();
        if value.is_empty() {
            return Err(SecretError::Empty);
        }
        Ok(Self(value))
    }

    /// Expose secret bytes only at an audited provider credential boundary.
    pub fn expose(&self) -> &str {
        &self.0
    }

    fn last_four(&self) -> String {
        let mut chars = self.0.chars().rev().take(4).collect::<Vec<_>>();
        chars.reverse();
        chars.into_iter().collect()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Secret(•••• {})", self.last_four())
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_secrets() {
        assert!(matches!(Secret::new(""), Err(SecretError::Empty)));
    }

    #[test]
    fn formatting_is_redacted() {
        let secret = Secret::new("sk-example-123456").unwrap_or_else(|error| panic!("{error}"));
        let debug = format!("{secret:?}");
        let display = secret.to_string();

        assert_eq!(debug, "Secret(•••• 3456)");
        assert_eq!(display, debug);
        assert!(!debug.contains("sk-example"));
    }
}
