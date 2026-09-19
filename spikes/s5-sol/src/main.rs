use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use metalcraft::rig::client::CompletionClient;
use metalcraft::rig::providers::openai;
use metalcraft::{
    AgentMessage, AgentOptions, AgentState, Executor, GraphError, LlmCallSnapshot,
    LlmResponseSnapshot, RunOutcome, Tool, ToolChoice, ToolRegistry,
    create_react_agent_with_options,
};
use serde::Serialize;
use serde_json::{Value, json};

const REQUIRED_ADVANCES: usize = 10;

#[derive(Clone)]
struct Progress {
    completed: Arc<AtomicUsize>,
}

struct AdvanceTool {
    progress: Progress,
}

#[async_trait]
impl Tool for AdvanceTool {
    fn name(&self) -> &str {
        "advance"
    }

    fn description(&self) -> &str {
        "Complete exactly the next numbered step in the replay probe. Call once per turn."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "step": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": REQUIRED_ADVANCES,
                    "description": "The next sequential step number"
                }
            },
            "required": ["step"],
            "additionalProperties": false
        })
    }

    async fn call(&self, args: Value) -> metalcraft::Result<Value> {
        let step = args
            .get("step")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| GraphError::Node {
                node: "advance".into(),
                message: "step must be an integer".into(),
            })?;
        let expected = self.progress.completed.load(Ordering::SeqCst) + 1;
        if step != expected {
            return Err(GraphError::Node {
                node: "advance".into(),
                message: format!("expected step {expected}, received {step}"),
            });
        }
        self.progress.completed.store(step, Ordering::SeqCst);
        Ok(json!({
            "accepted": true,
            "completed": step,
            "remaining": REQUIRED_ADVANCES - step
        }))
    }
}

struct FinishTool {
    progress: Progress,
}

#[async_trait]
impl Tool for FinishTool {
    fn name(&self) -> &str {
        "finish"
    }

    fn description(&self) -> &str {
        "Finish the probe only after all ten sequential advance calls succeeded."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    async fn call(&self, _args: Value) -> metalcraft::Result<Value> {
        let completed = self.progress.completed.load(Ordering::SeqCst);
        if completed != REQUIRED_ADVANCES {
            return Err(GraphError::Node {
                node: "finish".into(),
                message: format!(
                    "probe is incomplete: {completed}/{REQUIRED_ADVANCES}; call advance next"
                ),
            });
        }
        Ok(json!({"completed": true, "advance_calls": completed}))
    }
}

#[derive(Default)]
struct Observations {
    replayed_reasoning_items_per_request: Vec<usize>,
    reasoning_summaries: Vec<String>,
    input_tokens: u64,
    output_tokens: u64,
    reasoning_tokens: u64,
    cached_input_tokens: u64,
}

#[derive(Serialize)]
struct Report {
    model: String,
    completed: bool,
    advance_calls: usize,
    model_calls: usize,
    retained_reasoning_items: usize,
    replayed_reasoning_items_per_request: Vec<usize>,
    reasoning_summaries: Vec<String>,
    input_tokens: u64,
    output_tokens: u64,
    reasoning_tokens: u64,
    cached_input_tokens: u64,
    elapsed_ms: u128,
    request: Value,
}

fn count_reasoning_items(value: &Value) -> usize {
    match value {
        Value::Array(values) => values.iter().map(count_reasoning_items).sum(),
        Value::Object(map) => {
            usize::from(map.get("type").and_then(Value::as_str) == Some("reasoning"))
                + map.values().map(count_reasoning_items).sum::<usize>()
        }
        _ => 0,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let started = Instant::now();
    let api_key = std::env::var("OPENAI_API_KEY").context("OPENAI_API_KEY is required")?;
    let model_name =
        std::env::var("STARKBOT_SOL_MODEL").unwrap_or_else(|_| "gpt-5.6-sol".to_string());
    let client = openai::Client::new(&api_key)?;
    let model = client.completion_model(&model_name);

    let progress = Progress {
        completed: Arc::new(AtomicUsize::new(0)),
    };
    let observations = Arc::new(Mutex::new(Observations::default()));

    let call_observations = observations.clone();
    let call_hook = Arc::new(move |snapshot: &LlmCallSnapshot| {
        let replayed = snapshot
            .history
            .iter()
            .map(count_reasoning_items)
            .sum::<usize>();
        call_observations
            .lock()
            .expect("observation mutex poisoned")
            .replayed_reasoning_items_per_request
            .push(replayed);
    });

    let response_observations = observations.clone();
    let response_hook = Arc::new(move |snapshot: &LlmResponseSnapshot| {
        let mut observations = response_observations
            .lock()
            .expect("observation mutex poisoned");
        observations
            .reasoning_summaries
            .extend(snapshot.reasoning_summaries.iter().cloned());
        observations.input_tokens += snapshot.usage.input_tokens;
        observations.output_tokens += snapshot.usage.output_tokens;
        observations.reasoning_tokens += snapshot.usage.reasoning_tokens;
        observations.cached_input_tokens += snapshot.usage.cached_input_tokens;
    });

    let tools = ToolRegistry::new()
        .register(AdvanceTool {
            progress: progress.clone(),
        })
        .register(FinishTool {
            progress: progress.clone(),
        });
    let request = json!({
        "parallel_tool_calls": false,
        "store": false,
        "include": ["reasoning.encrypted_content"],
        "prompt_cache_key": "neo:s5-sol-replay"
    });
    let graph = create_react_agent_with_options(
        model,
        tools,
        "You are a deterministic replay probe. Call advance exactly once per model turn, with steps 1 through 10 in order. After advance(10) succeeds, call finish. Never call finish early and never skip or combine steps.",
        AgentOptions {
            llm_call_hook: Some(call_hook),
            llm_response_hook: Some(response_hook),
            tool_choice: ToolChoice::Required,
            terminal_tools: vec!["finish".into()],
            reasoning_effort: Some("low".into()),
            additional_params: Some(request.clone()),
            ..Default::default()
        },
    )?;

    let outcome = Executor::new(graph)
        .max_steps(40)
        .run(
            AgentState::new("Run the ten-step reasoning replay probe now."),
            "s5-sol-replay",
        )
        .await?;
    let RunOutcome::Completed(state) = outcome else {
        bail!("probe did not complete: {outcome:?}");
    };

    let advance_calls = progress.completed.load(Ordering::SeqCst);
    let retained_reasoning_items = state
        .messages
        .iter()
        .filter(|message| matches!(message, AgentMessage::Reasoning { .. }))
        .count();
    let observations = Arc::try_unwrap(observations)
        .map_err(|_| anyhow::anyhow!("observation hooks still retained"))?
        .into_inner()
        .map_err(|_| anyhow::anyhow!("observation mutex poisoned"))?;
    let report = Report {
        model: model_name,
        completed: true,
        advance_calls,
        model_calls: observations.replayed_reasoning_items_per_request.len(),
        retained_reasoning_items,
        replayed_reasoning_items_per_request: observations.replayed_reasoning_items_per_request,
        reasoning_summaries: observations.reasoning_summaries,
        input_tokens: observations.input_tokens,
        output_tokens: observations.output_tokens,
        reasoning_tokens: observations.reasoning_tokens,
        cached_input_tokens: observations.cached_input_tokens,
        elapsed_ms: started.elapsed().as_millis(),
        request,
    };

    let output_dir = std::path::Path::new("spikes/out/s5-sol");
    std::fs::create_dir_all(output_dir)?;
    let encoded = serde_json::to_vec_pretty(&report)?;
    std::fs::write(output_dir.join("report.json"), &encoded)?;
    println!("{}", String::from_utf8(encoded)?);

    if report.advance_calls != REQUIRED_ADVANCES {
        bail!("expected {REQUIRED_ADVANCES} advance calls, got {advance_calls}");
    }
    if report.model_calls < REQUIRED_ADVANCES + 1 {
        bail!(
            "expected at least 11 model calls, got {}",
            report.model_calls
        );
    }
    if report.retained_reasoning_items == 0 {
        bail!("model returned no replayable encrypted reasoning item");
    }
    if report
        .replayed_reasoning_items_per_request
        .windows(2)
        .any(|counts| counts[1] < counts[0])
    {
        bail!("reasoning replay count regressed between requests");
    }
    if !report
        .replayed_reasoning_items_per_request
        .iter()
        .skip(1)
        .any(|count| *count > 0)
    {
        bail!("no later request replayed an encrypted reasoning item");
    }
    Ok(())
}
