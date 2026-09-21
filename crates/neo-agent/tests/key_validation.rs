#![allow(clippy::expect_used)]
//! What the OpenAI and Anthropic key validators do to a real HTTP exchange,
//! and what `Runtime::check_key` does when there is nothing to check.
//!
//! Every call goes to a `wiremock` server on loopback: no vendor is contacted
//! and no key exists on this machine.

use neo_agent::Runtime;
use neo_agent::providers::{AnthropicKeyValidator, KeyBases, OpenAiKeyValidator};
use neo_core::AppEvent;
use neo_keys::{ACCOUNT_OPENAI, KeyState, KeyValidator, Secret};
use url::Url;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY: &str = "sk-test-not-a-real-key";

fn secret() -> Secret {
    Secret::new(KEY).expect("a non-empty test value")
}

fn base(server: &MockServer) -> Url {
    Url::parse(&server.uri()).expect("wiremock hands out a valid URL")
}

fn catalog(ids: &[&str]) -> serde_json::Value {
    serde_json::json!({ "data": ids.iter().map(|id| serde_json::json!({ "id": id })).collect::<Vec<_>>() })
}

#[tokio::test]
async fn openai_sends_a_bearer_header_and_reports_present() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(catalog(&["sol-latest"])))
        .expect(1)
        .mount(&server)
        .await;

    let validator = OpenAiKeyValidator::new(base(&server))
        .expect("a client builds")
        .requiring("sol-latest");
    let state = validator.validate(&secret()).await.expect("a 200 is readable");

    assert_eq!(state, KeyState::Present);
    let requests = server.received_requests().await.expect("recording is on");
    assert_eq!(requests.len(), 1);
    // The credential rides in a header, never in the URL (08 rule 1).
    assert!(requests[0].url.query().is_none());
    assert!(!requests[0].url.as_str().contains(KEY));
}

#[tokio::test]
async fn a_catalog_without_the_configured_model_is_limited() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(catalog(&["gpt-4.1"])))
        .mount(&server)
        .await;

    let validator = OpenAiKeyValidator::new(base(&server))
        .expect("a client builds")
        .requiring("sol-latest");

    assert_eq!(
        validator.validate(&secret()).await.expect("a 200 is readable"),
        KeyState::Limited
    );
}

#[tokio::test]
async fn a_rejected_key_is_invalid() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let validator = OpenAiKeyValidator::new(base(&server)).expect("a client builds");

    assert_eq!(
        validator.validate(&secret()).await.expect("a 401 is a verdict"),
        KeyState::Invalid
    );
}

#[tokio::test]
async fn a_server_fault_leaves_the_key_unjudged() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let validator = OpenAiKeyValidator::new(base(&server)).expect("a client builds");

    assert_eq!(
        validator.validate(&secret()).await.expect("no verdict is not an error"),
        KeyState::Unchecked
    );
}

#[tokio::test]
async fn an_unreachable_vendor_leaves_the_key_unjudged() {
    // Port 9 (discard) refuses connections on loopback; an offline user must
    // never be told their key is bad.
    let base = Url::parse("http://127.0.0.1:9").expect("a valid loopback URL");
    let validator = OpenAiKeyValidator::new(base).expect("a client builds");

    assert_eq!(
        validator.validate(&secret()).await.expect("offline is not an error"),
        KeyState::Unchecked
    );
}

#[tokio::test]
async fn anthropic_sends_its_own_header_pair() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("x-api-key", KEY))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(catalog(&["claude-sonnet-5"])))
        .expect(1)
        .mount(&server)
        .await;

    let validator = AnthropicKeyValidator::new(base(&server))
        .expect("a client builds")
        .requiring("claude-sonnet-5");

    assert_eq!(
        validator.validate(&secret()).await.expect("a 200 is readable"),
        KeyState::Present
    );
    let requests = server.received_requests().await.expect("recording is on");
    assert!(!requests[0].url.as_str().contains(KEY));
}

#[tokio::test]
async fn checking_an_account_with_no_key_makes_no_request() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().expect("a temporary data directory");
    let runtime = Runtime::with_key_bases(directory.path(), KeyBases::all(base(&server)))
        .expect("the store opens");
    let mut events = runtime.subscribe();

    // This machine has no OpenAI key in the Keychain and none in the
    // environment, so the check must answer from local state alone.
    let status = runtime
        .check_key(ACCOUNT_OPENAI)
        .await
        .expect("a missing key is not an error");

    assert_eq!(status.state, KeyState::Missing);
    assert!(
        server
            .received_requests()
            .await
            .expect("recording is on")
            .is_empty()
    );
    match events.try_recv().expect("the check announces itself").event {
        AppEvent::KeyStatus { account, status } => {
            assert_eq!(account, ACCOUNT_OPENAI);
            assert_eq!(status, KeyState::Missing);
        }
        other => panic!("expected a key-status event, got {other:?}"),
    }
    assert!(events.try_recv().is_err(), "exactly one event per check");
}

#[tokio::test]
async fn refreshing_a_catalog_without_a_key_keeps_the_cache_empty() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().expect("a temporary data directory");
    let runtime = Runtime::with_key_bases(directory.path(), KeyBases::all(base(&server)))
        .expect("the store opens");

    let models = runtime
        .refresh_models(ACCOUNT_OPENAI)
        .await
        .expect("a missing key is not an error");

    assert!(models.is_empty());
    assert!(
        server
            .received_requests()
            .await
            .expect("recording is on")
            .is_empty(),
        "a refresh with no key must not call the vendor"
    );
}

/// The whole refresh path with a key present. `Runtime` reads the key from the
/// Keychain first and falls back to `OPENAI_API_KEY` (05 §6), so this test
/// needs that variable set — which a test cannot do to its own process without
/// `unsafe` in edition 2024. Run it deliberately:
///
/// ```sh
/// OPENAI_API_KEY=dummy cargo test -p neo-agent -- --ignored
/// ```
///
/// No vendor is contacted: the catalogue comes from `wiremock`.
#[tokio::test]
#[ignore = "needs OPENAI_API_KEY in the environment; see the doc comment"]
async fn a_refresh_caches_the_classified_catalog() {
    assert!(
        std::env::var("OPENAI_API_KEY").is_ok(),
        "run this test with OPENAI_API_KEY set"
    );
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(catalog(&[
            "gpt-5.6-sol",
            "gpt-5.6-luna",
            "gpt-transcribe",
            "whisper-1",
            "gpt-4.1",
        ])))
        .mount(&server)
        .await;

    let directory = tempfile::tempdir().expect("a temporary data directory");
    let runtime = Runtime::with_key_bases(directory.path(), KeyBases::all(base(&server)))
        .expect("the store opens");
    let mut events = runtime.subscribe();

    let cached = runtime
        .refresh_models(ACCOUNT_OPENAI)
        .await
        .expect("the catalogue reads");

    let ids: Vec<&str> = cached
        .iter()
        .map(|model| model.info.reference.id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["gpt-5.6-luna", "gpt-5.6-sol", "gpt-transcribe"],
        "hidden and unclassified ids never reach the cache"
    );
    match events.try_recv().expect("a refresh announces itself").event {
        AppEvent::ModelsChanged { models } => {
            assert_eq!(models.models.len(), 3);
            assert!(models.refreshed_at.is_some());
        }
        other => panic!("expected models_changed, got {other:?}"),
    }

    // `sol-latest` now resolves against a real cached catalogue.
    let catalog_ids: Vec<String> = cached
        .iter()
        .map(|model| model.info.reference.id.clone())
        .collect();
    assert_eq!(
        neo_core::resolve(neo_core::PROVIDER_OPENAI, neo_core::SOL_LATEST, &catalog_ids).as_deref(),
        Some("gpt-5.6-sol")
    );
}
