//! `AxObserver` — the same navigator over a native macOS app (01, A23).
//!
//! `neo-ax` reads the focused window as an element table and executes one
//! action at a time; this module is the thin translation between that table and
//! the observation shape `policy` already consumes, so a run against TextEdit,
//! Calc or Excel uses the identical policy, budgets and gates as a web page.
//!
//! The mapping is deliberately small: an `Element`'s `operations` become
//! actions — `click`, `fill`, one `select` per enumerable option, and one
//! `press` per offered key on whichever element holds focus — the table's
//! `controls` become `SCROLL_UP` / `SCROLL_DOWN` / `WAIT`, and a `neo-ax`
//! guard answers the freshness check. The model only ever answers an index
//! into the table.

use async_trait::async_trait;
use neo_ax::{
    AppSel, AxAction, AxError, AxHandle, Control, Element, ElementTable, Key, Modifier, Operation,
    ScrollDir,
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

fn stale(reason: impl Into<std::borrow::Cow<'static, str>>) -> ObserveError {
    ObserveError::Stale(reason.into())
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

/// The keys `PRESS_KEY` may offer, with the name the target head uses
/// (01 §AX → operation: Enter, Escape, Tab, Shift-Tab, Space, arrows).
///
/// The alternative — dropping `press` from the AX action space — was
/// rejected: `neo_ax::input` posts these very keys to the app, and the table
/// says which element holds focus, so a key action can name a real element
/// instead of guessing at one. Nothing wider is offered; modifier chords go
/// through `MENU`, whose labels the rules layer can read.
const PRESS_KEYS: [(&str, Key, &[Modifier]); 9] = [
    ("enter", Key::Return, &[]),
    ("escape", Key::Escape, &[]),
    ("tab", Key::Tab, &[]),
    ("shift_tab", Key::Tab, &[Modifier::Shift]),
    ("space", Key::Space, &[]),
    ("up", Key::Up, &[]),
    ("down", Key::Down, &[]),
    ("left", Key::Left, &[]),
    ("right", Key::Right, &[]),
];

/// The fields every element action carries: what `action_space` builds the
/// element row from, and what the guard needs back.
fn base_action(element: &Element, kind: &str) -> Action {
    let mut action = Map::new();
    action.insert("kind".into(), json!(kind));
    action.insert("node".into(), json!(u64::from(element.index)));
    action.insert("label".into(), json!(element.label));
    action.insert("role".into(), json!(element.role));
    action.insert("enabled".into(), json!(element.state.enabled));
    if let Some(container) = &element.container {
        action.insert("container".into(), json!(container));
    }
    if let Some(value) = &element.value {
        action.insert("value".into(), json!(value));
        action.insert("current_value".into(), json!(value));
    }
    action
}

/// The actions one observation offers, in the shape `action_space` reads:
/// `kind`, `node`, `label`, `role`, `current_value` and `enabled`, plus
/// `option_index` on a select and `key` on a press.
fn actions_of(table: &ElementTable) -> Vec<Action> {
    let mut actions = Vec::new();
    for element in &table.elements {
        for operation in &element.operations {
            match operation {
                Operation::Click => actions.push(base_action(element, "click")),
                Operation::TypeText => actions.push(base_action(element, "fill")),
                // A menu leaf is pressed like any other control; its label is
                // the full path, which is what `select_menu` needs back.
                Operation::Menu => {
                    let mut action = base_action(element, "click");
                    action.insert("menu".into(), json!(true));
                    actions.push(action);
                }
                // A select is one candidate *per option*: `action_space`
                // builds the target head as `"{index}:{n}"` over the options
                // it sees, and only a fill is ever given text, so the option
                // has to travel in the candidate itself. An element with no
                // enumerable options therefore offers no select at all —
                // a pop-up that hides its menu until it opens stays a click.
                Operation::Select => {
                    for (position, option) in element.options.iter().enumerate() {
                        let mut action = base_action(element, "select");
                        // `" → "` is the separator `action_space` strips to
                        // recover the element's own label.
                        action.insert(
                            "label".into(),
                            json!(format!("{} → {option}", element.label)),
                        );
                        // The option is what this candidate chooses;
                        // `current_value` stays what the control reads now.
                        action.insert("value".into(), json!(option));
                        action.insert("option_index".into(), json!(position as u64));
                        actions.push(action);
                    }
                }
            }
        }
        // PRESS_KEY reaches the app, not an element: `input::press_key` posts
        // to the process and the keystroke lands wherever focus is, so naming
        // any element but the focused one would be a lie. An element with no
        // operations is inoperable by construction — a secure field, or a
        // disabled control — and stays that way even when it has focus.
        if element.state.focused && !element.operations.is_empty() {
            for (name, _, _) in PRESS_KEYS {
                let mut action = base_action(element, "press");
                action.insert("label".into(), json!(format!("{} → {name}", element.label)));
                action.insert("key".into(), json!(name));
                actions.push(action);
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

/// Everything about an observation that survives a re-read: which window it
/// is, how much is in it, and the value of the element that holds focus.
///
/// The focused value is folded in because a `neo-ax` window guard is
/// value-blind — it answers "same app, same window, nothing on top" — so
/// without it an app that rewrote the field a run just typed into would still
/// look fresh. The web path compares its whole semantic marker for the same
/// reason (`web.rs`).
///
/// It is *only* the focused value, not every value: a native window redraws
/// constantly (clocks, progress, selection counts), and a marker that moved
/// on each of those would turn every full-freshness check into a stale loop.
/// The focused element is the one a run acts on, so it is where a change that
/// matters shows up.
fn content_of(table: &ElementTable) -> String {
    let focused = table.elements.iter().find(|element| element.state.focused);
    format!(
        "{}|{}|{}|{}={}",
        table.window.title,
        table.elements.len(),
        table.window.modal,
        focused.map_or(0, |element| element.index),
        focused
            .and_then(|element| element.value.as_deref())
            .unwrap_or_default(),
    )
}

/// The identity of one observation: the generation its indices belong to,
/// then its content. An index means nothing outside its generation, so the
/// generation leads and [`content_part`] drops it when two *reads* are
/// compared.
fn marker_of(table: &ElementTable) -> String {
    format!("{}|{}", table.generation, content_of(table))
}

/// The generation-free half of a marker. Reading the table mints a new
/// generation, so comparing two reads means comparing this.
fn content_part(marker: &str) -> &str {
    marker.split_once('|').map_or("", |(_, content)| content)
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
    ) -> Result<Option<std::borrow::Cow<'static, str>>, ObserveError> {
        let marker = observation["marker"].as_str().unwrap_or_default();
        // An observation this observer did not just take cannot be judged:
        // the indices, refs and frames a guard needs are gone.
        let Some(table) = self.table_for(observation) else {
            return Ok(Some("that observation is no longer the current one".into()));
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
                None => return Ok(Some("that element is not in the current observation".into())),
            },
            None => table.guard_window(),
        };
        // The guard's own sentence is the diagnosis — which check failed, and
        // against what — so it travels instead of being flattened into one
        // fixed line.
        if let Some(reason) = self.ax.guard(&guard).await.map_err(observe_error)?.reason() {
            return Ok(Some(reason.into()));
        }
        if node.is_some() {
            return Ok(None);
        }
        // No target means the whole surface has to still be the one Jev
        // judged — the check that decides whether a `DONE` is believed. The
        // window guard cannot answer that on its own: it never reads a value.
        // So the table is read once more and compared by content, exactly as
        // the web path re-snapshots and compares its marker.
        //
        // The read costs this observation its refs (the actor keeps only the
        // newest generation actable), which is why it is confined to the
        // no-target check: nothing executes after one.
        let current = self.ax.table(&self.app).await.map_err(observe_error)?;
        Ok((content_of(&current) != content_part(marker))
            .then(|| "the surface no longer holds what Jev judged".into()))
    }

    async fn act(
        &mut self,
        action: &Action,
        observation: &Value,
        text: Option<&str>,
    ) -> Result<(), ObserveError> {
        if let Some(reason) = Observer::fresh(self, observation, Some(action)).await? {
            return Err(stale(reason));
        }
        // A wait is the navigator's own pause: nothing to ask the app.
        if action.get("kind").and_then(Value::as_str) == Some("wait") {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            return Ok(());
        }
        let ax_action = {
            let table = self
                .table_for(observation)
                .ok_or_else(|| stale("the app changed since this decision"))?;
            ax_action_of(table, action, text)?
        };
        self.ax.act(&ax_action).await.map_err(observe_error)?;
        Ok(())
    }
}

/// The one `neo-ax` action a decision means, resolved against the table its
/// candidate came from.
///
/// Pure on purpose: every (candidate → action) mapping is then testable
/// against a fixture observation, which is the only way the select and press
/// paths can be exercised without a live app in front of the actor.
fn ax_action_of(
    table: &ElementTable,
    action: &Action,
    text: Option<&str>,
) -> Result<AxAction, ObserveError> {
    let kind = action
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if kind == "scroll" {
        let direction = match action.get("id").and_then(Value::as_str).unwrap_or_default() {
            "SCROLL_UP" => ScrollDir::Up,
            "SCROLL_DOWN" => ScrollDir::Down,
            _ => return Err(ObserveError::Uncertain("unknown control".into())),
        };
        return Ok(AxAction::Scroll {
            direction,
            target: None,
        });
    }

    let node = action
        .get("node")
        .and_then(Value::as_u64)
        .ok_or_else(|| stale("the decision named no element"))?;
    let element = u16::try_from(node)
        .ok()
        .and_then(|index| table.element(index))
        .ok_or_else(|| stale("that element is not in the current observation"))?;
    let target = element.reference();
    match kind {
        "fill" => Ok(AxAction::SetValue {
            target,
            text: text.unwrap_or_default().to_owned(),
        }),
        // By position in the element's own list, never by typed text: only a
        // fill reaches the text helper, so a select that waited for text
        // could never execute (it never did).
        "select" => {
            let option = action
                .get("option_index")
                .and_then(Value::as_u64)
                .and_then(|position| usize::try_from(position).ok())
                .and_then(|position| element.options.get(position))
                .ok_or_else(|| stale("the decision named no option"))?;
            Ok(AxAction::SelectOption {
                target,
                option: option.clone(),
            })
        }
        // The key travels in the candidate; `PRESS_KEYS` is the only source
        // of those names, so an unknown one is a malformed decision rather
        // than something to guess a default for.
        "press" => {
            let name = action
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let (_, key, modifiers) = PRESS_KEYS
                .iter()
                .find(|(offered, _, _)| *offered == name)
                .ok_or_else(|| stale("the decision named no key"))?;
            Ok(AxAction::Key {
                key: *key,
                modifiers: modifiers.to_vec(),
            })
        }
        // A menu leaf carries its full path in the label; anything else is
        // an ordinary press.
        "click" if action.get("menu").and_then(Value::as_bool) == Some(true) => {
            Ok(AxAction::SelectMenu {
                path: action
                    .get("label")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .split(" › ")
                    .map(str::to_owned)
                    .collect(),
            })
        }
        "click" => Ok(AxAction::Press { target }),
        _ => Err(ObserveError::Uncertain("unknown action kind".into())),
    }
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::policy::{Operation as Op, action_space};

    /// A fixture observation. It is deserialised rather than built because an
    /// `Element`'s ref and frame are private to `neo-ax`; serde fills them
    /// with the defaults a caller that never reaches the actor would see.
    fn table(generation: u32, elements: Value) -> ElementTable {
        serde_json::from_value(json!({
            "generation": generation,
            "app": { "name": "Fixture", "bundle_id": "test.fixture", "pid": 42, "frontmost": true },
            "window": { "title": "Order", "modal": false },
            "text": "",
            "elements": elements,
            "controls": ["SCROLL_DOWN"],
            "truncated": false,
        }))
        .expect("the fixture table")
    }

    fn element(index: u64, role: &str, label: &str) -> Value {
        json!({
            "index": index,
            "role": role,
            "label": label,
            "state": { "enabled": true, "focused": false, "selected": false, "expanded": false },
            "operations": [],
        })
    }

    fn size_select() -> Value {
        let mut select = element(1, "combobox", "Size");
        select["operations"] = json!(["SELECT"]);
        select["options"] = json!(["Small", "Medium", "Large"]);
        select["value"] = json!("Small");
        select
    }

    /// The AX select offered one action carrying the element's *current*
    /// value, so the target head could name the control but never an option,
    /// and `act` looked for the option in text a select is never given — the
    /// operation could not execute at all. One candidate per option now makes
    /// the head well-formed and the chosen option recoverable.
    #[test]
    fn a_select_offers_one_candidate_per_option_and_acts_on_the_chosen_one() {
        let table = table(7, json!([size_select()]));
        let actions = actions_of(&table);
        let space = action_space(&actions);

        let candidates = space.targets.get(&Op::Select).expect("a select head");
        let offered: Vec<&str> = candidates.keys().map(String::as_str).collect();
        assert_eq!(offered, ["1:1", "1:2", "1:3"]);
        // What the model reads: the element still shows what it holds now,
        // and each option is a distinct choice under it.
        assert_eq!(space.elements[0]["label"], json!("Size"));
        assert_eq!(space.elements[0]["value"], json!("Small"));
        assert_eq!(space.elements[0]["options"][1]["value"], json!("Medium"));

        let chosen = candidates.get("1:2").expect("the second option");
        let acted = ax_action_of(&table, chosen, None).expect("the option resolves");
        let AxAction::SelectOption { option, .. } = acted else {
            panic!("a select candidate must act as a select: {acted:?}");
        };
        assert_eq!(option, "Medium");
    }

    /// An element that claims SELECT with nothing enumerable behind it (a
    /// pop-up whose menu only exists once opened) must offer no select at
    /// all: a candidate no decision could resolve is worse than the click
    /// that does work.
    #[test]
    fn a_select_with_no_enumerable_options_offers_nothing_to_select() {
        let mut popup = element(1, "popupbutton", "Size");
        popup["operations"] = json!(["CLICK", "SELECT"]);
        popup["value"] = json!("Small");
        let table = table(7, json!([popup]));

        let actions = actions_of(&table);
        assert!(!actions.iter().any(|action| action["kind"] == "select"));
        assert!(!action_space(&actions).targets.contains_key(&Op::Select));
        assert!(actions.iter().any(|action| action["kind"] == "click"));
    }

    /// PRESS_KEY was unreachable in both directions: nothing emitted a press
    /// candidate and `act` hardcoded Return. Keys are offered for the element
    /// that holds focus — the only one `input::press_key` can reach — and the
    /// candidate carries the key it names, modifier included.
    #[test]
    fn keys_are_offered_for_the_focused_control_only() {
        let mut field = element(1, "textfield", "Search");
        field["operations"] = json!(["TYPE_TEXT"]);
        field["state"]["focused"] = json!(true);
        let mut button = element(2, "button", "Go");
        button["operations"] = json!(["CLICK"]);
        let table = table(7, json!([field, button]));

        let space = action_space(&actions_of(&table));
        let candidates = space.targets.get(&Op::PressKey).expect("a press head");
        let offered: Vec<&str> = candidates.keys().map(String::as_str).collect();
        assert_eq!(
            offered,
            [
                "1:down",
                "1:enter",
                "1:escape",
                "1:left",
                "1:right",
                "1:shift_tab",
                "1:space",
                "1:tab",
                "1:up"
            ],
            "only the focused element's keys, and only the supported set"
        );

        let chosen = candidates.get("1:shift_tab").expect("shift-tab");
        let acted = ax_action_of(&table, chosen, None).expect("the key resolves");
        assert_eq!(
            acted,
            AxAction::Key {
                key: Key::Tab,
                modifiers: vec![Modifier::Shift],
            }
        );
    }

    /// A secure field is shown with no operations precisely so that nothing
    /// can be done to it. A key is something done to it, so focus does not
    /// buy one — the same rule that keeps a disabled control inert.
    #[test]
    fn a_focused_secure_field_offers_no_keys() {
        let mut secure = element(1, "securefield", "Password");
        secure["state"]["focused"] = json!(true);
        let table = table(7, json!([secure]));

        assert!(
            !actions_of(&table)
                .iter()
                .any(|action| action["kind"] == "press")
        );
    }

    /// The marker is what a no-target freshness check compares a fresh read
    /// against, and a `neo-ax` window guard never reads a value: before this,
    /// a run could claim DONE after the field it had typed into was rewritten
    /// underneath it.
    #[test]
    fn a_changed_focused_value_moves_the_marker() {
        let mut field = element(1, "textfield", "To");
        field["operations"] = json!(["TYPE_TEXT"]);
        field["state"]["focused"] = json!(true);
        field["value"] = json!("draft");
        let mut counter = element(2, "statictext", "Items");
        counter["value"] = json!("0 items");

        let before = table(7, json!([field.clone(), counter.clone()]));
        field["value"] = json!("sent");
        let typed = table(7, json!([field.clone(), counter.clone()]));
        assert_ne!(
            content_part(&marker_of(&before)),
            content_part(&marker_of(&typed))
        );

        // …while a value nobody is acting on churns on every redraw, and a
        // marker that moved with it would make every full check stale.
        field["value"] = json!("draft");
        counter["value"] = json!("1 item");
        let redrawn = table(7, json!([field, counter]));
        assert_eq!(
            content_part(&marker_of(&before)),
            content_part(&marker_of(&redrawn))
        );
    }

    /// Reading the table mints a generation, so the two halves of the marker
    /// answer different questions: content compares two reads, the whole
    /// marker says whether an observation is the one still in hand.
    #[test]
    fn a_re_read_keeps_the_content_and_changes_the_identity() {
        let rows = json!([element(1, "button", "Send")]);
        let first = table(7, rows.clone());
        let second = table(8, rows);

        assert_eq!(
            content_part(&marker_of(&first)),
            content_part(&marker_of(&second))
        );
        assert_ne!(marker_of(&first), marker_of(&second));
    }
}
