//! The text helper: a small fast LLM that writes one field value, only on `TYPE_TEXT`.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::policy::Action;
use crate::rules::TEXT_VALUE;

#[derive(Debug, thiserror::Error)]
pub enum TextError {
    #[error("no text helper is configured; nothing typed")]
    Unconfigured,
    #[error("text helper request failed: {0}")]
    Request(String),
    #[error("text helper returned no valid field value; nothing typed")]
    Invalid,
}

pub struct TextValue {
    pub text: String,
    pub latency: Duration,
    pub usage: Value,
}

pub fn field_context(goal: &str, action: &Action, observation: &Value, history: &[Value]) -> Value {
    let recent: Vec<Value> = history
        .iter()
        .rev()
        .take(6)
        .rev()
        .map(|h| json!({ "action": h.get("action"), "text": h.get("text") }))
        .collect();
    let page_text: String = observation["text"]
        .as_str()
        .unwrap_or_default()
        .chars()
        .take(6000)
        .collect();
    json!({
        "goal": goal,
        "field": { "label": action.get("label"), "role": action.get("role"), "value": action.get("value") },
        "page": { "title": observation["title"], "text": page_text },
        "recent_actions": recent,
    })
}

/// Any OpenAI-compatible chat-completions endpoint.
#[derive(Clone)]
pub struct OpenAiTextHelper {
    http: reqwest::Client,
    key: String,
    pub base_url: String,
    pub model: String,
    /// Extra request fields, e.g. `{"reasoning_effort": "none"}`.
    pub extra: Value,
}

impl OpenAiTextHelper {
    pub fn new(key: impl Into<String>, model: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(25))
            .build()
            .expect("http client");
        Self {
            http,
            key: key.into(),
            base_url: "https://api.openai.com/v1".into(),
            model: model.into(),
            extra: json!({}),
        }
    }

    pub async fn value(&self, context: &Value) -> Result<TextValue, TextError> {
        let mut body = json!({
            "model": self.model,
            "response_format": { "type": "json_object" },
            "messages": [
                { "role": "system", "content": TEXT_VALUE },
                { "role": "user", "content": context.to_string() },
            ],
        });
        if let (Some(body), Some(extra)) = (body.as_object_mut(), self.extra.as_object()) {
            body.extend(extra.clone());
        }
        let started = Instant::now();
        let response = self
            .http
            .post(format!(
                "{}/chat/completions",
                self.base_url.trim_end_matches('/')
            ))
            .bearer_auth(&self.key)
            .json(&body)
            .send()
            .await
            .map_err(|e| TextError::Request(e.to_string()))?;
        if !response.status().is_success() {
            let status = response.status();
            let detail: String = response
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(300)
                .collect();
            return Err(TextError::Request(format!("HTTP {status}: {detail}")));
        }
        let result: Value = response
            .json()
            .await
            .map_err(|e| TextError::Request(e.to_string()))?;
        let content = result["choices"][0]["message"]["content"]
            .as_str()
            .ok_or(TextError::Invalid)?;
        let output: Value = serde_json::from_str(content).map_err(|_| TextError::Invalid)?;
        let object = output.as_object().ok_or(TextError::Invalid)?;
        let text = object
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if object.len() != 1 || text.trim().is_empty() || text.chars().count() > 2000 {
            return Err(TextError::Invalid);
        }
        Ok(TextValue {
            text: text.to_owned(),
            latency: started.elapsed(),
            usage: result.get("usage").cloned().unwrap_or(Value::Null),
        })
    }
}
