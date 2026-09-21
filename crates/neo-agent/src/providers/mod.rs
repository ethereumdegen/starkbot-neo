//! Provider implementations: the only place in the workspace that knows a
//! vendor's hostname or wire shape (05 §1 rule 3, 08 "Rules that keep the seam
//! honest" 1).
//!
//! Every client is built from an injected `base_url`, so a test points it at a
//! `wiremock` server and CI can grep for vendor hosts outside this module.

pub(crate) mod anthropic;
pub mod anthropic_oauth;
pub mod anthropic_oauth_model;
mod catalog;
pub mod codex_oauth;
mod key_check;
pub(crate) mod openai;
mod turn;

pub use anthropic::AnthropicKeyValidator;
pub use anthropic_oauth::AnthropicOauthInference;
pub use anthropic_oauth_model::ClaudeSubscription;
pub use catalog::{Catalog, classify_all};
pub use codex_oauth::CodexOauthInference;
pub(crate) use key_check::required_model;
pub use key_check::{KeyBases, model_list_state};
pub use openai::OpenAiKeyValidator;
pub use turn::Turn;
