#![allow(clippy::expect_used)]
//! What a TypeSafe key check makes of each answer. No vendor is contacted: the
//! endpoint is a `wiremock` server.

use neo_judge::TypeSafeKeyValidator;
use neo_keys::{KeyState, KeyValidator, Secret};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY: &str = "ts-test-not-a-real-key";

fn validator(server: &MockServer) -> TypeSafeKeyValidator {
    TypeSafeKeyValidator::new(format!("{}/v1/systemone", server.uri()), "jev-latest")
}

fn secret() -> Secret {
    Secret::new(KEY).expect("a non-empty test value")
}

#[tokio::test]
async fn a_working_key_is_present_and_travels_in_a_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "model": "jev-latest",
            "answers": { "reachable": { "noul": 0.99 } }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let state = validator(&server)
        .validate(&secret())
        .await
        .expect("a 200 is a verdict");

    assert_eq!(state, KeyState::Present);
    let requests = server.received_requests().await.expect("recording is on");
    assert!(!requests[0].url.as_str().contains(KEY));
}

#[tokio::test]
async fn a_rejected_key_is_invalid() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    assert_eq!(
        validator(&server)
            .validate(&secret())
            .await
            .expect("a 401 is a verdict"),
        KeyState::Invalid
    );
}

#[tokio::test]
async fn a_rate_limit_says_nothing_about_the_key() {
    let server = MockServer::start().await;
    // `wire` retries 429 twice before giving up, so the mock answers every time.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;

    assert_eq!(
        validator(&server)
            .validate(&secret())
            .await
            .expect("a rate limit is not an error"),
        KeyState::Unchecked
    );
}

#[tokio::test]
async fn an_unreachable_endpoint_says_nothing_about_the_key() {
    let validator = TypeSafeKeyValidator::new("http://127.0.0.1:9/v1/systemone", "jev-latest");

    assert_eq!(
        validator
            .validate(&secret())
            .await
            .expect("offline is not an error"),
        KeyState::Unchecked
    );
}
