//! `AxObserver` — the same navigator over a native macOS app (01, A23).
//!
//! `neo-ax` reads the focused window as an element table and executes one
//! action at a time; this module is the thin translation between that table and
//! the observation shape `policy` already consumes, so a run against TextEdit,
//! Calc or Excel uses the identical policy, budgets and gates as a web page.
//!
//! The mapping is deliberately small: an `Element`'s `operations` become one
//! action each (`click`, `fill`) or one per enumerated option (`select`), the
//! table's `controls` become `SCROLL_UP` / `SCROLL_DOWN` / `WAIT` under the
//! `scroll` and `wait` kinds the web observer uses, and `guards` carries the
//! per-element fingerprint the freshness check compares. The model only ever
//! answers an index into the table.

use async_trait::async_trait;
use neo_ax::{
    AppSel, AxAction, AxError, AxHandle, Control, ElementTable, Freshness, Key, Operation,
};
use serde_json::{Map, Value, json};

use crate::observer::{ObserveError, Observer};
use crate::policy::Action;

/// A navigator observer over one native app.
pub struct AxObserver {
    ax: AxHandle,
    app: AppSel,
    /// The last table. An element index means nothing without it — the refs,
    /// the frames and the window fingerprint a guard needs all live here — so
    /// acting on an index from an older observation is refused rather than
    /// guessed at.
    last: Option<ElementTable>,
}

impl AxObserver {
    pub fn new(ax: AxHandle, app: AppSel) -> Self {
        Self {
            ax,
            app,
            last: None,
        }
    }

    pub fn app(&self) -> &AppSel {
        &self.app
    }

    /// The window as it stands now, read fresh.
    ///
    /// A caller that has finished a run needs what the app ends up saying —
    /// the native counterpart of reading the page after the browser stops —
    /// and the last observation is by then one action out of date.
    ///
    /// # Errors
    ///
    /// As [`neo_ax::AxHandle::table`].
    pub async fn table(&self) -> Result<ElementTable, AxError> {
        self.ax.table(&self.app).await
    }

    /// The table the given observation came from, or `None` when the
    /// observation is not the current one.
    fn table_for(&self, observation: &Value) -> Option<&ElementTable> {
        let table = self.last.as_ref()?;
        (marker_of(table) == observation["marker"].as_str().unwrap_or_default()).then_some(table)
    }
}

fn stale(reason: &'static str) -> ObserveError {
    ObserveError::Stale(reason)
}

fn observe_error(error: AxError) -> ObserveError {
    match error {
        AxError::ScreenLocked => stale("the screen is locked"),
        // The actor's own sentence is the diagnosis — which app, which call,
        // whether the value came back — so it is carried through instead of
        // being replaced by a fixed one.
        other => ObserveError::Uncertain(other.to_string().into()),
    }
}

/// One JSON action per (element, operation) pair — one per *option* for a
/// SELECT — in the shape `action_space` reads: `kind`, `node`, `label`,
/// `role`, plus `value`/`current_value` for a select and `enabled` for the
/// guard.
///
/// The `kind` vocabulary is the one `js/snapshot.js` emits, because the loop
/// in `lib.rs` matches on it directly: this observer used to spell every
/// window control `{"kind":"control"}` while the web one spells them
/// `scroll`/`wait`, so three WAITs in a row — what the prompt tells the model
/// to do while a sheet loads — ended a native run with "three actions in a
/// row changed nothing", and a safety head firing on a WAIT blocked instead
/// of waiting (R3.1). The vocabulary belongs in a shared type both observers
/// construct rather than in two string tables; until it is one, they must be
/// kept identical by hand.
fn actions_of(table: &ElementTable) -> Vec<Action> {
    let mut actions = Vec::new();
    for element in &table.elements {
        let mut base = Map::new();
        base.insert("node".into(), json!(u64::from(element.index)));
        base.insert("label".into(), json!(element.label));
        base.insert("role".into(), json!(element.role));
        base.insert("enabled".into(), json!(element.state.enabled));
        if let Some(container) = &element.container {
            base.insert("container".into(), json!(container));
        }
        if let Some(value) = &element.value {
            base.insert("value".into(), json!(value));
            base.insert("current_value".into(), json!(value));
        }
        for operation in &element.operations {
            match operation {
                // One action per enumerated option, exactly as `snapshot.js`
                // does for a `<select>`, so `action_space` builds `index:N`
                // targets and the chosen option arrives in `value` as a real
                // target. This used to be one action carrying the whole
                // `options` list and no `option`, which `act` could never
                // resolve: every SELECT returned stale, and five in a row
                // blocked the run (R3.2). `neo_ax::mapping` offers SELECT only
                // when the options are already enumerated, so an empty list
                // offers nothing here.
                Operation::Select => {
                    for option in &element.options {
                        let mut action = base.clone();
                        action.insert("kind".into(), json!("select"));
                        action.insert(
                            "label".into(),
                            json!(format!("{} → {option}", element.label)),
                        );
                        action.insert("value".into(), json!(option));
                        action.insert(
                            "current_value".into(),
                            json!(element.value.clone().unwrap_or_default()),
                        );
                        actions.push(action);
                    }
                }
                Operation::TypeText => {
                    let mut action = base.clone();
                    action.insert("kind".into(), json!("fill"));
                    actions.push(action);
                }
                // A menu leaf is pressed like any other control; its label is
                // the full path, which is what `select_menu` needs back.
                Operation::Click | Operation::Menu => {
                    let mut action = base.clone();
                    action.insert("kind".into(), json!("click"));
                    if matches!(operation, Operation::Menu) {
                        action.insert("menu".into(), json!(true));
                    }
                    actions.push(action);
                }
            }
        }
    }
    for control in &table.controls {
        let (id, kind) = match control {
            Control::ScrollUp => ("SCROLL_UP", "scroll"),
            Control::ScrollDown => ("SCROLL_DOWN", "scroll"),
            Control::Wait => ("WAIT", "wait"),
        };
        let mut action = Map::new();
        action.insert("kind".into(), json!(kind));
        action.insert("id".into(), json!(id));
        action.insert("label".into(), json!(id));
        actions.push(action);
    }
    actions
}

/// A window fingerprint plus the shape of the table: the marker `fresh` uses
/// when an action has no single target.
fn marker_of(table: &ElementTable) -> String {
    format!(
        "{}|{}|{}|{}",
        table.generation,
        table.window.title,
        table.elements.len(),
        table.window.modal
    )
}

fn observation_of(table: &ElementTable) -> Value {
    json!({
        "generation": table.generation,
        "pid": table.app.pid,
        "title": table.window.title,
        "app": table.app.name,
        "bundle_id": table.app.bundle_id,
        "modal": table.window.modal,
        // A native window has no URL unless it is a web area; `AXURL` gives one
        // when there is one, and `null` is honest when there is not.
        "url": table.window.url,
        "text": table.text,
        "marker": marker_of(table),
        "actions": actions_of(table),
        "omitted_actions": table.truncated,
    })
}

#[async_trait]
impl Observer for AxObserver {
    async fn observe(&mut self) -> Result<Value, ObserveError> {
        let table = self.ax.table(&self.app).await.map_err(observe_error)?;
        let observation = observation_of(&table);
        self.last = Some(table);
        Ok(observation)
    }

    async fn fresh(
        &mut self,
        observation: &Value,
        action: Option<&Action>,
    ) -> Result<bool, ObserveError> {
        // An observation this observer did not just take cannot be judged:
        // the indices, refs and frames a guard needs are gone.
        let Some(table) = self.table_for(observation) else {
            return Ok(false);
        };
        let node = action
            .and_then(|action| action.get("node"))
            .and_then(Value::as_u64);
        let guard = match node {
            Some(node) => match u16::try_from(node)
                .ok()
                .and_then(|index| table.guard_for(index))
            {
                Some(guard) => guard,
                None => return Ok(false),
            },
            None => table.guard_window(),
        };
        match self.ax.guard(&guard).await.map_err(observe_error)? {
            Freshness::Fresh => Ok(true),
            Freshness::Stale(_) => Ok(false),
        }
    }

    async fn act(
        &mut self,
        action: &Action,
        observation: &Value,
        text: Option<&str>,
    ) -> Result<(), ObserveError> {
        if !Observer::fresh(self, observation, Some(action)).await? {
            return Err(stale("the app changed since this decision"));
        }
        let kind = action
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match kind {
            // A wait is the navigator's own pause: nothing to ask the app.
            "wait" => {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                return Ok(());
            }
            "scroll" => {
                let id = action.get("id").and_then(Value::as_str).unwrap_or_default();
                let direction = match id {
                    "SCROLL_UP" => neo_ax::ScrollDir::Up,
                    "SCROLL_DOWN" => neo_ax::ScrollDir::Down,
                    _ => return Err(ObserveError::Uncertain("unknown control".into())),
                };
                let ax_action = AxAction::Scroll {
                    direction,
                    target: None,
                };
                self.ax.act(&ax_action).await.map_err(observe_error)?;
                return Ok(());
            }
            _ => {}
        }

        let node = action
            .get("node")
            .and_then(Value::as_u64)
            .ok_or_else(|| stale("the decision named no element"))?;
        let target = u16::try_from(node)
            .ok()
            .and_then(|index| self.last.as_ref()?.element(index))
            .map(neo_ax::Element::reference)
            .ok_or_else(|| stale("that element is not in the current observation"))?;
        let ax_action = match kind {
            "fill" => AxAction::SetValue {
                target,
                text: text.unwrap_or_default().to_owned(),
            },
            // The option is a target, not free text: `actions_of` emits one
            // action per enumerated option and `policy::action_space` turns
            // each into an `index:N` target, so `value` carries whichever one
            // Jev chose. `text` is `None` here — the loop only asks the text
            // helper for a `fill`.
            "select" => AxAction::SelectOption {
                target,
                option: action
                    .get("value")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| stale("the decision named no option"))?,
            },
            // The only key a web `press` carries today is Enter; the wider key
            // set arrives with the PRESS_KEY operation (01 §AX → operation).
            "press" => AxAction::Key {
                key: Key::Return,
                modifiers: Vec::new(),
            },
            // A menu leaf carries its full path in the label; anything else is
            // an ordinary press.
            "click" if action.get("menu").and_then(Value::as_bool) == Some(true) => {
                AxAction::SelectMenu {
                    path: action
                        .get("label")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .split(" › ")
                        .map(str::to_owned)
                        .collect(),
                }
            }
            "click" => AxAction::Press { target },
            _ => return Err(ObserveError::Uncertain("unknown action kind".into())),
        };
        self.ax.act(&ax_action).await.map_err(observe_error)?;
        Ok(())
    }
}
