//! Raw TypeSafe "System One" client: structured state in, typed answers out.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Map, Value, json};

#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("model connection failed; no action executed")]
    Connection,
    #[error("model provider returned HTTP {0}; no action executed")]
    Status(u16),
    #[error("invalid TypeSafe response for `{0}`; no action executed")]
    Invalid(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChoiceAnswer {
    pub choice: String,
    pub probabilities: BTreeMap<String, f64>,
    pub confidence: f64,
}

#[derive(Debug, Clone)]
pub struct Evaluation {
    pub model: String,
    pub answers: Map<String, Value>,
    pub usage: Value,
    pub latency: Duration,
}

impl Evaluation {
    /// A `choice` answer, checked against the ids that were actually offered.
    pub fn choice(&self, name: &str, offered: &[String]) -> Result<ChoiceAnswer, WireError> {
        let invalid = || WireError::Invalid(name.to_owned());
        let answer: ChoiceAnswer = self
            .answers
            .get(name)
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .ok_or_else(invalid)?;
        let finite = |n: f64| n.is_finite() && (0.0..=1.0).contains(&n);
        let same_keys = answer.probabilities.len() == offered.len()
            && offered
                .iter()
                .all(|id| answer.probabilities.contains_key(id));
        let sum: f64 = answer.probabilities.values().sum();
        let top = answer.probabilities.values().copied().fold(0.0, f64::max);
        let chosen = answer
            .probabilities
            .get(&answer.choice)
            .copied()
            .unwrap_or(-1.0);
        let valid = same_keys
            && finite(answer.confidence)
            && answer.probabilities.values().all(|n| finite(*n))
            && (sum - 1.0).abs() < 0.02
            && chosen >= top - 1e-6;
        if valid { Ok(answer) } else { Err(invalid()) }
    }

    /// A yes/no head's probability, from the `noul` shape TypeSafe answers
    /// with: `{"type":"noul","noul":0.93}` — confirmed against the live API
    /// on 2026-09-21 and recorded in `plans/spikes.md`, which retires the
    /// *(verify)* this carried.
    ///
    /// Fails closed (A-Q7). A head that was asked and did not come back, or
    /// came back in a shape this does not recognise, is an error rather than
    /// an absent probability: the callers are the safety heads, and an
    /// unanswered safety question used to read as "not risky", which is the
    /// one interpretation a mis-shaped response must never get.
    pub fn noul(&self, name: &str) -> Result<f64, WireError> {
        let invalid = || WireError::Invalid(name.to_owned());
        self.answers
            .get(name)
            .ok_or_else(invalid)?
            .get("noul")
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite() && (0.0..=1.0).contains(n))
            .ok_or_else(invalid)
    }
}

/// How long one Jev call may take before the step fails.
///
/// Short on purpose (10 §3): a step's whole budget is a few hundred
/// milliseconds, so a classifier that has not answered in five seconds has
/// already failed the loop whether or not it eventually replies. The retry
/// ladder in `evaluate` covers the transient statuses; this covers silence.
const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct TypeSafe {
    http: reqwest::Client,
    key: String,
    pub endpoint: String,
    pub model: String,
}

impl TypeSafe {
    pub fn new(
        key: impl Into<String>,
        endpoint: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        let http = reqwest::Client::new();
        Self {
            http,
            key: key.into(),
            endpoint: endpoint.into(),
            model: model.into(),
        }
    }

    pub async fn evaluate(
        &self,
        state: &Value,
        questions: &Value,
    ) -> Result<Evaluation, WireError> {
        let body = json!({ "model": self.model, "state": state, "questions": questions });
        let started = Instant::now();
        let mut attempt = 0;
        let response = loop {
            let response = self
                .http
                .post(&self.endpoint)
                .bearer_auth(&self.key)
                .timeout(TIMEOUT)
                .json(&body)
                .send()
                .await
                .map_err(|_| WireError::Connection)?;
            let status = response.status().as_u16();
            if matches!(status, 429 | 503 | 529) && attempt < 2 {
                tokio::time::sleep(Duration::from_millis(500 << attempt)).await;
                attempt += 1;
                continue;
            }
            if !response.status().is_success() {
                return Err(WireError::Status(status));
            }
            break response;
        };
        let value: Value = response
            .json()
            .await
            .map_err(|_| WireError::Invalid("body".into()))?;
        Ok(Evaluation {
            model: value["model"].as_str().unwrap_or_default().to_owned(),
            answers: value["answers"].as_object().cloned().unwrap_or_default(),
            usage: value.get("usage").cloned().unwrap_or(Value::Null),
            latency: started.elapsed(),
        })
    }

    /// The smallest possible authenticated request: one yes/no head over an
    /// empty state.
    ///
    /// This is what a key check is made of (05 §6). It is a real request, so
    /// it costs one Jev call — there is no unauthenticated health endpoint to
    /// ask instead. `Ok(())` means the credential works; `Status(401)` /
    /// `Status(403)` mean it does not; anything else says nothing about the
    /// key.
    pub async fn ping(&self) -> Result<(), WireError> {
        let state = json!({ "page": { "text": "" } });
        let questions = json!({
            "reachable": {
                "type": "noul",
                "instructions": { "rules": "Answer yes. This request only checks the credential." }
            }
        });
        self.evaluate(&state, &questions).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]

    use super::*;

    fn evaluation(answers: Value) -> Evaluation {
        Evaluation {
            model: "jev-test".into(),
            answers: answers.as_object().cloned().expect("an answers object"),
            usage: Value::Null,
            latency: Duration::ZERO,
        }
    }

    /// The live shape, verbatim from `api.typesafe.ai` (plans/spikes.md).
    #[test]
    fn a_yes_no_head_reads_its_probability() {
        let evaluation = evaluation(json!({ "spends": { "type": "noul", "noul": 0.93 } }));

        assert_eq!(evaluation.noul("spends").expect("a well-formed head"), 0.93);
    }

    /// The regression this change exists for: a head that is absent, or that
    /// answers in some other shape, must not be readable as a probability at
    /// all — the navigator treats an unreadable safety head as maximally
    /// risky, and it can only do that if this refuses to invent one.
    #[test]
    fn an_unanswered_or_mis_shaped_head_is_an_error_not_a_zero() {
        let missing = evaluation(json!({ "outward": { "type": "noul", "noul": 0.1 } }));
        assert!(matches!(
            missing.noul("spends"),
            Err(WireError::Invalid(head)) if head == "spends"
        ));

        for mis_shaped in [
            json!({ "spends": { "type": "choice", "choice": "yes" } }),
            json!({ "spends": { "type": "noul", "noul": "0.9" } }),
            json!({ "spends": { "type": "noul", "noul": 1.4 } }),
            json!({ "spends": { "type": "noul" } }),
            json!({ "spends": true }),
        ] {
            assert!(
                evaluation(mis_shaped).noul("spends").is_err(),
                "a probability was read out of a shape that does not carry one"
            );
        }
    }
}
