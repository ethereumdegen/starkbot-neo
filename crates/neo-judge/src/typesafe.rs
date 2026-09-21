//! Is the stored TypeSafe key good? One `ping` answers it (05 §6).

use async_trait::async_trait;
use jev_nav::wire::{TypeSafe, WireError};
use neo_keys::{ACCOUNT_TYPESAFE, KeyState, KeyValidator, Secret, ValidationError};

/// The endpoint and model a check asks on. Injected, like every other vendor
/// seam (08 rule 1), so a test can point it at a fixture server.
pub struct TypeSafeKeyValidator {
    endpoint: String,
    model: String,
}

impl TypeSafeKeyValidator {
    pub fn new(endpoint: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            model: model.into(),
        }
    }
}

#[async_trait]
impl KeyValidator for TypeSafeKeyValidator {
    fn account(&self) -> &str {
        ACCOUNT_TYPESAFE
    }

    async fn validate(&self, secret: &Secret) -> Result<KeyState, ValidationError> {
        // The audited credential boundary clippy.toml points at: the key goes
        // into the one Jev client, which sends it as a bearer header.
        #[allow(clippy::disallowed_methods)]
        let client = TypeSafe::new(secret.expose(), &self.endpoint, &self.model);
        match client.ping().await {
            Ok(()) => Ok(KeyState::Present),
            // A rejected credential is the only verdict about the key itself.
            Err(WireError::Status(401 | 403)) => Ok(KeyState::Invalid),
            // Reachability, rate limits and a provider fault say nothing about
            // the key, so they never call it bad (05 §6).
            Err(WireError::Status(_) | WireError::Connection) => Ok(KeyState::Unchecked),
            Err(WireError::Invalid(what)) => Err(ValidationError::protocol(
                ACCOUNT_TYPESAFE,
                format!("the ping answered in an unreadable shape: {what}"),
            )),
        }
    }
}
