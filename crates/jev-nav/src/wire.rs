//! Raw TypeSafe "System One" client: structured state in, typed answers out.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Map, Value, json};

pub const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
pub const DEFAULT_MODEL: &str = "jev-latest";

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

    /// A yes/no answer as a probability, if the head was asked and is well formed.
    pub fn yes(&self, name: &str) -> Option<f64> {
        let answer = self.answers.get(name)?;
        answer
            .get("noul")
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite() && (0.0..=1.0).contains(n))
    }
}

#[derive(Clone)]
pub struct TypeSafe {
    http: reqwest::Client,
    key: String,
    pub endpoint: String,
    pub model: String,
}

impl TypeSafe {
    pub fn new(key: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(25))
            .build()
            .expect("http client");
        Self {
            http,
            key: key.into(),
            endpoint: ENDPOINT.into(),
            model: DEFAULT_MODEL.into(),
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
}
