//! `claude auth status` as a redacted [`ProviderAccount`].
//!
//! The CLI prints a JSON object; the fields Neo needs are the ones that say
//! *whether* a subscription is connected and *which plan*, never a token. Extra
//! fields are ignored on purpose so a CLI upgrade cannot break the parse.

use neo_core::{ProviderAccount, ProviderAccountStatus, ProviderId, PROVIDER_CLAUDE_SUBSCRIPTION};
use serde::Deserialize;

/// What `claude auth status` reports. Observed on Claude Code 2.1.236:
/// `{"loggedIn": false, "authMethod": "none", "apiProvider": "firstParty"}`.
/// The optional fields appear once a login exists *(verify their exact names
/// against a logged-in CLI)*.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    #[serde(default)]
    pub logged_in: bool,
    #[serde(default)]
    pub auth_method: Option<String>,
    #[serde(default)]
    pub api_provider: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default, alias = "subscriptionType", alias = "plan")]
    pub plan_type: Option<String>,
    #[serde(default, alias = "organization", alias = "workspace")]
    pub organization: Option<String>,
}

impl AuthStatus {
    /// Is this a *subscription* login rather than a Console API key?
    ///
    /// `claude auth login --console` signs in to an Anthropic Console account,
    /// which bills API usage instead of using the plan. That is the
    /// `anthropic` API-key path (K6 path c) wearing the CLI's coat, so it is
    /// not what the subscription runtime reports as connected.
    #[must_use]
    pub fn subscription(&self) -> bool {
        let console = self
            .auth_method
            .as_deref()
            .is_some_and(|method| method.eq_ignore_ascii_case("console"))
            || self
                .api_provider
                .as_deref()
                .is_some_and(|provider| provider.eq_ignore_ascii_case("console"));
        self.logged_in && !console
    }
}

/// The redacted row Neo stores. `updated_at` is the caller's clock reading, so
/// the store and the event agree on one timestamp.
#[must_use]
pub fn account_from_status(status: &AuthStatus, updated_at: i64) -> ProviderAccount {
    ProviderAccount {
        provider: ProviderId::new(PROVIDER_CLAUDE_SUBSCRIPTION),
        status: if status.subscription() {
            ProviderAccountStatus::Connected
        } else {
            ProviderAccountStatus::SignedOut
        },
        email: status.email.clone(),
        plan_type: status.plan_type.clone().or_else(|| {
            status
                .subscription()
                .then(|| status.auth_method.clone())
                .flatten()
        }),
        workspace: status.organization.clone(),
        // Rate-limit windows arrive with the turn loop in M5; an unknown
        // allowance is `None`, never an invented number.
        allowance: None,
        updated_at,
    }
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]

    use super::*;

    fn parse(json: &str) -> AuthStatus {
        serde_json::from_str(json).expect("the CLI prints an object")
    }

    #[test]
    fn a_signed_out_cli_is_signed_out() {
        let status = parse(r#"{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty"}"#);
        assert!(!status.subscription());
        let account = account_from_status(&status, 7);
        assert_eq!(account.status, ProviderAccountStatus::SignedOut);
        assert_eq!(account.updated_at, 7);
        assert_eq!(account.allowance, None);
    }

    #[test]
    fn a_plan_login_is_connected_and_keeps_only_redacted_fields() {
        let status = parse(
            r#"{"loggedIn":true,"authMethod":"claudeai","apiProvider":"firstParty",
                 "email":"someone@example.com","subscriptionType":"max","organization":"Acme"}"#,
        );
        let account = account_from_status(&status, 1);
        assert_eq!(account.status, ProviderAccountStatus::Connected);
        assert_eq!(account.email.as_deref(), Some("someone@example.com"));
        assert_eq!(account.plan_type.as_deref(), Some("max"));
        assert_eq!(account.workspace.as_deref(), Some("Acme"));
    }

    #[test]
    fn a_console_login_is_not_the_subscription_path() {
        // `claude auth login --console` bills an API account: that is K6 path
        // c, not the subscription runtime.
        let status = parse(r#"{"loggedIn":true,"authMethod":"console"}"#);
        assert!(!status.subscription());
        assert_eq!(
            account_from_status(&status, 0).status,
            ProviderAccountStatus::SignedOut
        );
    }

    #[test]
    fn unknown_fields_do_not_break_the_parse() {
        let status = parse(r#"{"loggedIn":true,"somethingNew":{"a":1}}"#);
        assert!(status.subscription());
    }
}
