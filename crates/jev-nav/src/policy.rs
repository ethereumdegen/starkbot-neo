//! Turns one observation into the questions of a single TypeSafe request, and
//! the answers back into one decision. Port of `model.py::action_space/choose`.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::rules::{NEXT_ACTION, ON_TASK, SAFETY, TARGET};
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
            elements.push(element);
            elements.len() - 1
        });
        let element = &mut elements[position];
        let index = (position + 1).to_string();
        if operation == Operation::Select && !element.contains_key("options") {
            // The options bucket is opened here rather than when the row is
            // created, because an element can offer SELECT alongside CLICK —
            // a native `AXPopUpButton` does, and `ax::actions_of` emits its
            // CLICK first — and a row created by the CLICK had no bucket, so
            // every option collapsed onto the same bare `index` target and
            // whichever option came last silently won (R3.2).
            element.insert(
                "value".into(),
                action.get("current_value").cloned().unwrap_or(json!("")),
            );
            element.insert("options".into(), json!([]));
        }
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
    /// The yes/no heads this request actually asked for.
    ///
    /// `resolve` needs to tell "not asked" from "asked and came back
    /// unreadable"; both used to leave the head absent from `Decision::safety`
    /// and the loop read absent as `0.0` (R1.1). Asked-and-unreadable is now
    /// a failed step, and absent means exactly "not asked".
    safety: Vec<&'static str>,
}

pub fn build_request(
    page: &Value,
    space: &ActionSpace,
    goal: &str,
    history: &[Value],
    safety_heads: bool,
    on_task_floor: f32,
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
    // Heads cost per-step classifier latency, so only the ones this run can
    // act on are asked: the risk ceilings when `safety_heads` is on, and the
    // drift floor when there is a floor to enforce.
    let mut asked: Vec<(&'static str, &'static str)> = Vec::new();
    if safety_heads {
        asked.extend_from_slice(SAFETY);
    }
    if on_task_floor > 0.0 {
        asked.push(ON_TASK);
    }
    for (name, instructions) in &asked {
        questions.insert(
            (*name).into(),
            json!({ "type": "noul", "instructions": { "goal": goal, "rules": instructions } }),
        );
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
    // What the observer could not show has to reach the model: a target that
    // was dropped to fit a budget is not a target that does not exist, and a
    // model that cannot see its target should answer BLOCKED rather than
    // claim DONE (R3.4). The web path counts what it dropped; the native one
    // only knows that it dropped something, so the flag carries what the
    // count cannot.
    let omitted = page["omitted_actions"].as_u64().unwrap_or(0);
    let state = json!({
        "page": { "url": page["url"], "title": page["title"], "text": page["text"] },
        "elements": space.elements,
        "recent_actions": recent,
        "omitted_actions": omitted,
        "actions_truncated": omitted > 0 || page["omitted_actions"].as_bool().unwrap_or(false),
        "unreadable_frames": page["signals"]["cross_origin_frames"].as_u64().unwrap_or(0),
    });
    Request {
        state,
        questions: Value::Object(questions),
        operations: operations.keys().cloned().collect(),
        safety: asked.iter().map(|(name, _)| *name).collect(),
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
    // Asked for, therefore required (A-Q7, R1.1). A head that did not come
    // back is a broken response, not a safe one: the alternative is executing
    // a mutation whose risk nothing scored, which is exactly how a truncated
    // answer used to score `outward` at zero and send the message.
    for name in &request.safety {
        decision
            .safety
            .insert((*name).into(), evaluation.noul(name)?);
    }
    Ok(decision)
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value, json};

    use super::{Operation, action_space};

    fn action(value: Value) -> super::Action {
        value.as_object().cloned().unwrap_or_else(Map::new)
    }

    /// A native pop-up button offers CLICK *and* SELECT on one element
    /// (`neo_ax::mapping`), and `ax::actions_of` emits the CLICK first. Each
    /// option must still become its own `index:option` target: they used to
    /// collapse onto the bare element index, so whichever option the observer
    /// listed last silently won whatever Jev chose (R3.2).
    #[test]
    fn a_clickable_element_that_also_selects_still_offers_one_target_per_option() {
        let space = action_space(&[
            action(json!({ "kind": "click", "node": 7, "label": "Format", "role": "popupbutton" })),
            action(json!({
                "kind": "select", "node": 7, "label": "Format → Plain Text",
                "role": "popupbutton", "value": "Plain Text", "current_value": "Rich Text",
            })),
            action(json!({
                "kind": "select", "node": 7, "label": "Format → Rich Text",
                "role": "popupbutton", "value": "Rich Text", "current_value": "Rich Text",
            })),
        ]);

        let selects = space
            .targets
            .get(&Operation::Select)
            .map_or_else(Vec::new, |candidates| {
                candidates.keys().cloned().collect::<Vec<_>>()
            });
        assert_eq!(selects, ["1:1", "1:2"]);
        assert_eq!(
            space.targets[&Operation::Select]["1:1"]["value"],
            json!("Plain Text")
        );
        assert_eq!(space.elements.len(), 1);
        let element = &space.elements[0];
        assert_eq!(element["operations"], json!(["CLICK", "SELECT"]));
        assert_eq!(element["label"], json!("Format"));
        // The row reports what is selected now, not one of the choices.
        assert_eq!(element["value"], json!("Rich Text"));
        assert_eq!(
            element["options"],
            json!([
                { "index": "1:1", "label": "Format → Plain Text", "value": "Plain Text" },
                { "index": "1:2", "label": "Format → Rich Text", "value": "Rich Text" },
            ])
        );
    }
}
