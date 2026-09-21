//! Turns one observation into the questions of a single TypeSafe request, and
//! the answers back into one decision. Port of `model.py::action_space/choose`.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::rules::{NEXT_ACTION, SAFETY, TARGET};
use crate::wire::{Evaluation, WireError};

/// One executable thing the observer offered (a row of `snapshot.js` `actions`).
pub type Action = Map<String, Value>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Operation {
    Click,
    TypeText,
    Select,
    Upload,
    PressKey,
}

impl Operation {
    pub fn name(self) -> &'static str {
        match self {
            Self::Click => "CLICK",
            Self::TypeText => "TYPE_TEXT",
            Self::Select => "SELECT",
            Self::Upload => "UPLOAD",
            Self::PressKey => "PRESS_KEY",
        }
    }

    fn from_kind(kind: &str) -> Option<Self> {
        match kind {
            "click" => Some(Self::Click),
            "fill" => Some(Self::TypeText),
            "select" => Some(Self::Select),
            "upload" => Some(Self::Upload),
            "press" => Some(Self::PressKey),
            _ => None,
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Self::Click => {
                "Click an element, button, menu option, autocomplete suggestion, or calendar day."
            }
            Self::TypeText => {
                "Enter or replace text in an editable field. A small LLM will supply the value from the goal."
            }
            Self::Select => "Select an observed dropdown value.",
            Self::Upload => "Upload the file supplied with the goal to a file input.",
            Self::PressKey => "Press one offered key in the currently focused control.",
        }
    }
}

/// Elements for the state, per-operation targets, and the control actions (scroll, wait).
pub struct ActionSpace {
    pub elements: Vec<Value>,
    pub targets: BTreeMap<Operation, BTreeMap<String, Action>>,
    pub controls: BTreeMap<String, Action>,
}

pub fn action_space(actions: &[Action]) -> ActionSpace {
    let mut elements: Vec<Map<String, Value>> = Vec::new();
    let mut index_of: BTreeMap<u64, usize> = BTreeMap::new();
    let mut targets: BTreeMap<Operation, BTreeMap<String, Action>> = BTreeMap::new();
    let mut controls = BTreeMap::new();

    for action in actions {
        let kind = action
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(operation) = Operation::from_kind(kind) else {
            let id = action
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_uppercase();
            controls.insert(id, action.clone());
            continue;
        };
        let Some(node) = action.get("node").and_then(Value::as_u64) else {
            continue;
        };
        let position = *index_of.entry(node).or_insert_with(|| {
            let mut element = Map::new();
            for key in ["role", "value", "checked", "selected", "expanded"] {
                if let Some(value) = action.get(key) {
                    element.insert(key.into(), value.clone());
                }
            }
            let label = action
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or_default();
            element.insert("index".into(), json!((elements.len() + 1).to_string()));
            element.insert(
                "label".into(),
                json!(label.split(" → ").next().unwrap_or(label)),
            );
            element.insert("operations".into(), json!([]));
            if operation == Operation::Select {
                element.insert(
                    "value".into(),
                    action.get("current_value").cloned().unwrap_or(json!("")),
                );
                element.insert("options".into(), json!([]));
            }
            elements.push(element);
            elements.len() - 1
        });
        let element = &mut elements[position];
        let index = (position + 1).to_string();
        if let Some(Value::Array(operations)) = element.get_mut("operations")
            && !operations.iter().any(|o| o == operation.name())
        {
            operations.push(json!(operation.name()));
        }
        let mut target = index.clone();
        if operation == Operation::Select {
            if let Some(Value::Array(options)) = element.get_mut("options") {
                target = format!("{index}:{}", options.len() + 1);
                options.push(json!({ "index": target, "label": action.get("label"), "value": action.get("value") }));
            }
        } else if operation == Operation::PressKey {
            let key = action
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or_default();
            target = format!("{index}:{key}");
        }
        targets
            .entry(operation)
            .or_default()
            .insert(target, action.clone());
    }

    ActionSpace {
        elements: elements.into_iter().map(Value::Object).collect(),
        targets,
        controls,
    }
}

pub struct Request {
    pub state: Value,
    pub questions: Value,
    operations: Vec<String>,
    /// Whether the safety heads were asked, and therefore are required back.
    safety_heads: bool,
}

pub fn build_request(
    page: &Value,
    space: &ActionSpace,
    goal: &str,
    history: &[Value],
    safety_heads: bool,
) -> Request {
    let mut operations = Map::new();
    for operation in space.targets.keys() {
        operations.insert(operation.name().into(), json!(operation.describe()));
    }
    for (id, control) in &space.controls {
        operations.insert(
            id.clone(),
            control.get("label").cloned().unwrap_or(json!(id)),
        );
    }
    operations.insert(
        "DONE".into(),
        json!("Every requirement is visibly satisfied."),
    );
    operations.insert(
        "BLOCKED".into(),
        json!("No supported operation can progress."),
    );

    let mut questions = Map::new();
    questions.insert(
        "operation".into(),
        json!({ "type": "choice", "criteria": operations, "instructions": { "goal": goal, "rules": NEXT_ACTION } }),
    );
    for (operation, candidates) in &space.targets {
        let mut criteria = Map::new();
        for (index, action) in candidates {
            let mut entry = Map::new();
            let label = action
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or_default();
            entry.insert("element".into(), json!(format!("[{index}] {label}")));
            let current = action
                .get("current_value")
                .or_else(|| action.get("value"))
                .cloned();
            entry.insert("current_value".into(), current.unwrap_or(json!("")));
            for key in ["role", "checked", "selected", "expanded"] {
                if let Some(value) = action.get(key) {
                    entry.insert(key.into(), value.clone());
                }
            }
            criteria.insert(index.clone(), Value::Object(entry));
        }
        questions.insert(
            format!("{}_target", operation.name().to_lowercase()),
            json!({
                "type": "choice",
                "criteria": criteria,
                "instructions": { "goal": goal, "operation": operation.name(), "rules": [NEXT_ACTION, TARGET] },
            }),
        );
    }
    if safety_heads {
        for (name, instructions) in SAFETY {
            questions.insert(
                (*name).into(),
                json!({ "type": "noul", "instructions": { "goal": goal, "rules": instructions } }),
            );
        }
    }

    let recent: Vec<Value> = history
        .iter()
        .rev()
        .take(10)
        .rev()
        .map(|entry| {
            let mut kept = Map::new();
            for key in ["action", "kind", "text", "page_changed"] {
                kept.insert(key.into(), entry.get(key).cloned().unwrap_or(Value::Null));
            }
            Value::Object(kept)
        })
        .collect();
    let state = json!({
        "page": { "url": page["url"], "title": page["title"], "text": page["text"] },
        "elements": space.elements,
        "recent_actions": recent,
    });
    Request {
        state,
        questions: Value::Object(questions),
        operations: operations.keys().cloned().collect(),
        safety_heads,
    }
}

#[derive(Debug, Clone)]
pub struct Decision {
    /// `CLICK`, `TYPE_TEXT`, `SELECT`, a control id (`SCROLL_DOWN`, `WAIT`…), `DONE` or `BLOCKED`.
    pub operation: String,
    pub operation_confidence: f64,
    pub operation_probabilities: BTreeMap<String, f64>,
    /// The observed action to execute, when the operation has a target or is a control.
    pub action: Option<Action>,
    pub target: Option<String>,
    pub target_confidence: Option<f64>,
    pub safety: BTreeMap<String, f64>,
}

pub fn resolve(
    request: &Request,
    space: &ActionSpace,
    evaluation: &Evaluation,
) -> Result<Decision, WireError> {
    let operation_answer = evaluation.choice("operation", &request.operations)?;
    let operation = operation_answer.choice.clone();
    let mut decision = Decision {
        operation: operation.clone(),
        operation_confidence: operation_answer.confidence,
        operation_probabilities: operation_answer.probabilities,
        action: space.controls.get(&operation).cloned(),
        target: None,
        target_confidence: None,
        safety: BTreeMap::new(),
    };
    // Unused target heads cannot cause an action: only the chosen operation's head is read.
    if let Some((_, candidates)) = space.targets.iter().find(|(op, _)| op.name() == operation) {
        let offered: Vec<String> = candidates.keys().cloned().collect();
        let head = format!("{}_target", operation.to_lowercase());
        let target = evaluation.choice(&head, &offered)?;
        decision.action = candidates.get(&target.choice).cloned();
        decision.target_confidence = Some(target.confidence);
        decision.target = Some(target.choice);
    }
    // Asked for, therefore required (A-Q7). A safety head that did not come
    // back is a broken response, not a safe one: the alternative is
    // executing a mutation whose risk nothing scored.
    if request.safety_heads {
        for (name, _) in SAFETY {
            decision
                .safety
                .insert((*name).into(), evaluation.noul(name)?);
        }
    }
    Ok(decision)
}
