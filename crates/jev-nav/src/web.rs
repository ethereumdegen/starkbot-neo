//! `CdpObserver`: observes and acts on one owned tab. Port of `browser.py`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use neo_cdp::{CdpError, Page};
use serde_json::{Value, json};

pub use crate::observer::ObserveError;
use crate::observer::Observer;
use crate::policy::Action;
use crate::rules::MAX_ELEMENT_ACTIONS;

/// Runs in the page; the only non-Rust code in this crate.
pub const SNAPSHOT_JS: &str = include_str!("../js/snapshot.js");

fn context_lost(error: &CdpError) -> bool {
    matches!(
        error,
        CdpError::Protocol { message, .. }
            if message.contains("Cannot find context") || message.contains("Execution context was destroyed")
    )
}

fn runtime_action(action: &Action) -> Action {
    let mut runtime = action.clone();
    if let Some(local_node) = action.get("local_node").cloned() {
        runtime.insert("node".into(), local_node);
    }
    runtime
}

/// Scroll and wait are the surface's own controls, not elements: the JS
/// budget counts what `snapshot.js` calls `elementActions`, and so does this.
fn is_control(action: &Value) -> bool {
    matches!(action["kind"].as_str(), Some("scroll" | "wait"))
}

/// Re-apply [`MAX_ELEMENT_ACTIONS`] to the merged action list, returning how
/// many element actions were dropped.
///
/// `snapshot.js` caps itself, but it runs once per execution context, so a
/// page with six iframes used to hand Jev up to seven capped lists at once
/// (R3.4). The controls survive the cut — a page whose element list was
/// truncated is exactly the page where scrolling and waiting still help.
fn cap_element_actions(actions: &mut Vec<Value>) -> u64 {
    let before = actions.len();
    let mut kept = 0usize;
    actions.retain(|action| {
        if is_control(action) {
            return true;
        }
        kept += 1;
        kept <= MAX_ELEMENT_ACTIONS
    });
    u64::try_from(before - actions.len()).unwrap_or(u64::MAX)
}

/// What the child frames contributed to one observation, before it is folded
/// into the top document's snapshot.
#[derive(Default)]
struct FrameContributions {
    /// Frame actions, already re-namespaced and offset into page coordinates.
    actions: Vec<Value>,
    /// `[frame id, frame marker]` pairs, for the freshness comparison.
    markers: Vec<Value>,
    /// Visible frame text in frame order.
    text: Vec<String>,
    /// Guards under their namespaced node ids.
    guards: Vec<(String, Value)>,
    /// Element actions `snapshot.js` dropped, summed over every frame.
    omitted: u64,
    /// Deterministic page signals summed across readable child frames.
    signals: HashMap<String, u64>,
    /// Frames CDP listed that no snapshot came back from — cross-origin, or
    /// gone between the listing and the evaluation.
    unreadable: u64,
}

/// Fold the child frames into the top document's snapshot.
///
/// Split out of `CdpObserver::snapshot` because the page-wide element budget
/// only exists here — `snapshot.js` caps per execution context — and a budget
/// that cannot be tested without a browser is a budget that stops holding
/// (R3.4).
fn merge_frames(root: &mut Value, frames: FrameContributions) {
    let mut omitted = root["omitted_actions"].as_u64().unwrap_or(0) + frames.omitted;
    if let Some(actions) = root["actions"].as_array_mut() {
        actions.extend(frames.actions);
        omitted += cap_element_actions(actions);
    }
    if let Some(guards) = root["guards"].as_object_mut() {
        guards.extend(frames.guards);
    }
    let mut text = root["text"].as_str().unwrap_or_default().to_owned();
    for child_text in frames.text {
        text.push('\n');
        text.push_str(&child_text);
    }
    root["text"] = json!(text.chars().take(6000).collect::<String>());
    root["marker"] = json!([root["marker"].clone(), frames.markers]);
    root["omitted_actions"] = json!(omitted);
    for (name, count) in frames.signals {
        let total = root["signals"][&name].as_u64().unwrap_or(0) + count;
        root["signals"][name] = json!(total);
    }
    root["signals"]["cross_origin_frames"] = json!(frames.unreadable);
}

pub struct CdpObserver {
    page: Page,
    after_input: Option<Action>,
    attachments: Vec<PathBuf>,
    context_id: Option<u64>,
    frame_contexts: HashMap<String, u64>,
}

impl CdpObserver {
    pub fn new(page: Page) -> Self {
        Self {
            page,
            after_input: None,
            attachments: Vec::new(),
            context_id: None,
            frame_contexts: HashMap::new(),
        }
    }

    pub fn with_attachments(mut self, attachments: Vec<PathBuf>) -> Self {
        self.attachments = attachments;
        self
    }

    pub fn page(&self) -> &Page {
        &self.page
    }

    async fn context_id(&mut self) -> Result<u64, ObserveError> {
        if let Some(context_id) = self.context_id {
            return Ok(context_id);
        }
        let context_id = self.page.create_isolated_world("starkbot-neo").await?;
        self.context_id = Some(context_id);
        Ok(context_id)
    }

    async fn evaluate(&mut self, expression: &str) -> Result<Value, ObserveError> {
        let context_id = self.context_id().await?;
        match self
            .page
            .evaluate_in_context(context_id, expression, false)
            .await
        {
            Err(error) if context_lost(&error) => {
                self.context_id = None;
                Err(ObserveError::Stale("document changed during evaluation"))
            }
            Err(CdpError::Exception(_)) => {
                self.context_id = None;
                Err(ObserveError::Stale("document changed during evaluation"))
            }
            other => Ok(other?),
        }
    }

    async fn evaluate_action(
        &mut self,
        action: &Action,
        expression: &str,
        await_promise: bool,
    ) -> Result<Value, ObserveError> {
        if let Some(context_id) = action.get("context_id").and_then(Value::as_u64) {
            return match self
                .page
                .evaluate_in_context(context_id, expression, await_promise)
                .await
            {
                Err(error) if context_lost(&error) => {
                    if let Some(frame_id) = action.get("frame_id").and_then(Value::as_str) {
                        self.frame_contexts.remove(frame_id);
                    }
                    Err(ObserveError::Stale("frame changed during evaluation"))
                }
                Err(CdpError::Exception(_)) => {
                    Err(ObserveError::Stale("frame changed during evaluation"))
                }
                other => Ok(other?),
            };
        }
        let context_id = self.context_id().await?;
        match self
            .page
            .evaluate_in_context(context_id, expression, await_promise)
            .await
        {
            Err(error) if context_lost(&error) => {
                self.context_id = None;
                Err(ObserveError::Stale("document changed during evaluation"))
            }
            Err(CdpError::Exception(_)) => {
                self.context_id = None;
                Err(ObserveError::Stale("document changed during evaluation"))
            }
            other => Ok(other?),
        }
    }

    /// One semantic snapshot merged from the top document and every CDP child frame.
    async fn snapshot(&mut self) -> Result<Value, ObserveError> {
        let mut root = self.evaluate(SNAPSHOT_JS).await?;
        let frames = self.page.child_frames().await?;
        let mut frames_seen = FrameContributions::default();
        let mut observed_frames = 0u64;

        for (index, frame) in frames.iter().enumerate() {
            let context_id = match self.frame_contexts.get(&frame.id).copied() {
                Some(context_id) => context_id,
                None => {
                    let context_id = self
                        .page
                        .create_isolated_world_for_frame(
                            &frame.id,
                            &format!("starkbot-neo-frame-{}", frame.id),
                        )
                        .await?;
                    self.frame_contexts.insert(frame.id.clone(), context_id);
                    context_id
                }
            };
            let mut child = match self
                .page
                .evaluate_in_context(context_id, SNAPSHOT_JS, false)
                .await
            {
                Ok(snapshot) => snapshot,
                Err(CdpError::Exception(_)) => {
                    self.frame_contexts.remove(&frame.id);
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            observed_frames += 1;
            frames_seen.omitted += child["omitted_actions"].as_u64().unwrap_or(0);
            if let Some(signals) = child["signals"].as_object() {
                for (name, count) in signals {
                    if let Some(count) = count.as_u64() {
                        *frames_seen.signals.entry(name.clone()).or_insert(0) += count;
                    }
                }
            }
            frames_seen
                .markers
                .push(json!([&frame.id, child["marker"].clone()]));
            if let Some(text) = child["text"].as_str() {
                frames_seen.text.push(text.to_owned());
            }
            let namespace = ((index as u64) + 1) << 32;
            if let Some(guards) = child["guards"].as_object() {
                for (local, guard) in guards {
                    if let Ok(local) = local.parse::<u64>() {
                        frames_seen
                            .guards
                            .push(((namespace | local).to_string(), guard.clone()));
                    }
                }
            }
            let frame_page_key = child["page_key"].clone();
            if let Some(actions) = child["actions"].as_array_mut() {
                for candidate in actions {
                    let local_node = candidate.get("node").and_then(Value::as_u64);
                    let id = candidate
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    if local_node.is_none()
                        && matches!(id.as_str(), "wait" | "scroll_down" | "scroll_up")
                    {
                        continue;
                    }
                    let Some(action) = candidate.as_object_mut() else {
                        continue;
                    };
                    action.insert("context_id".into(), json!(context_id));
                    action.insert("frame_id".into(), json!(&frame.id));
                    action.insert("frame_page_key".into(), frame_page_key.clone());
                    action.insert("frame_offset_x".into(), json!(frame.offset_x));
                    action.insert("frame_offset_y".into(), json!(frame.offset_y));
                    action.insert("id".into(), json!(format!("f{}_{}", index + 1, id)));
                    if let Some(local_node) = local_node {
                        action.insert("local_node".into(), json!(local_node));
                        action.insert("node".into(), json!(namespace | local_node));
                    }
                    if let Some(rect) = action.get_mut("rect").and_then(Value::as_object_mut) {
                        if let Some(x) = rect.get("x").and_then(Value::as_f64) {
                            rect.insert("x".into(), json!(x + frame.offset_x));
                        }
                        if let Some(y) = rect.get("y").and_then(Value::as_f64) {
                            rect.insert("y".into(), json!(y + frame.offset_y));
                        }
                    }
                    for (field, offset) in [("x", frame.offset_x), ("y", frame.offset_y)] {
                        if let Some(value) = action.get(field).and_then(Value::as_f64) {
                            action.insert(field.into(), json!(value + offset));
                        }
                    }
                    frames_seen.actions.push(Value::Object(action.clone()));
                }
            }
        }

        frames_seen.unreadable = frames.len() as u64 - observed_frames;
        merge_frames(&mut root, frames_seen);
        Ok(root)
    }

    /// One atomic snapshot of the page's visible controls, values and text.
    pub async fn observe(&mut self) -> Result<Value, ObserveError> {
        Observer::observe(self).await
    }

    /// Is the page still what the decision was made on?
    pub async fn fresh(
        &mut self,
        observation: &Value,
        action: Option<&Action>,
    ) -> Result<bool, ObserveError> {
        Observer::fresh(self, observation, action).await
    }

    /// Execute one observed action, typing `text` for a fill.
    pub async fn act(
        &mut self,
        action: &Action,
        observation: &Value,
        text: Option<&str>,
    ) -> Result<(), ObserveError> {
        Observer::act(self, action, observation, text).await
    }
}

#[async_trait::async_trait]
impl Observer for CdpObserver {
    async fn observe(&mut self) -> Result<Value, ObserveError> {
        if let Some(action) = self.after_input.take() {
            let runtime = runtime_action(&action);
            let _ = self
                .evaluate_action(
                    &action,
                    &format!("({AFTER_INPUT_JS})({})", Value::Object(runtime.clone())),
                    true,
                )
                .await;
            if action.get("kind").and_then(Value::as_str) == Some("fill")
                && action.get("contenteditable").and_then(Value::as_bool) == Some(true)
            {
                let node = runtime
                    .get("node")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                let expected = action
                    .get("expected")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let verified = self
                    .evaluate_action(
                        &action,
                        &format!("({VERIFY_TEXT_JS})({node},{})", json!(expected)),
                        false,
                    )
                    .await
                    .unwrap_or(json!(false));
                if verified != json!(true) {
                    return Err(ObserveError::Uncertain(
                        "contenteditable text was not confirmed".into(),
                    ));
                }
            }
            if action.get("kind").and_then(Value::as_str) == Some("click")
                && let Some(popup) = self.page.popup(Duration::ZERO).await?
            {
                popup.set_viewport(1120, 780, 1.0).await?;
                self.page = popup;
                self.context_id = None;
                self.frame_contexts.clear();
            }
        }
        for attempt in 0..10 {
            match self.snapshot().await {
                Ok(Value::Null) | Err(ObserveError::Stale(_)) if attempt < 9 => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Ok(Value::Null) => return Err(ObserveError::Stale("document is navigating")),
                Ok(mut observation) => {
                    if self.attachments.is_empty()
                        && let Some(actions) = observation["actions"].as_array_mut()
                    {
                        actions.retain(|action| action["kind"] != "upload");
                    }
                    return Ok(observation);
                }
                Err(error) => return Err(error),
            }
        }
        Err(ObserveError::Stale("page did not settle"))
    }

    /// Click/select compare the target and its nearby context; everything else
    /// compares the whole semantic marker.
    async fn fresh(
        &mut self,
        observation: &Value,
        action: Option<&Action>,
    ) -> Result<bool, ObserveError> {
        let kind = action
            .and_then(|action| action.get("kind"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if let (true, Some(node), Some(action)) = (
            matches!(kind, "click" | "select"),
            action
                .and_then(|action| action.get("node"))
                .and_then(Value::as_u64),
            action,
        ) {
            let local_node = action
                .get("local_node")
                .and_then(Value::as_u64)
                .unwrap_or(node);
            let current = self
                .evaluate_action(
                    action,
                    &format!(
                        "(() => {{ const c=window.__jevFast; return c ? [c.pageKey(),c.guard(c.nodes.get({local_node}))] : null; }})()"
                    ),
                    false,
                )
                .await?;
            let expected_page_key = action
                .get("frame_page_key")
                .unwrap_or(&observation["page_key"]);
            let expected = json!([expected_page_key, observation["guards"][node.to_string()]]);
            return Ok(current == expected);
        }
        let current = self.snapshot().await?;
        Ok(current["marker"] == observation["marker"])
    }

    async fn act(
        &mut self,
        action: &Action,
        observation: &Value,
        text: Option<&str>,
    ) -> Result<(), ObserveError> {
        if !Observer::fresh(self, observation, Some(action)).await? {
            return Err(ObserveError::Stale("page changed since this decision"));
        }
        let kind = action
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match kind {
            "wait" => tokio::time::sleep(Duration::from_millis(100)).await,
            "scroll" => {
                let delta = action.get("delta").and_then(Value::as_f64).unwrap_or(560.0);
                let x = action.get("x").and_then(Value::as_f64).unwrap_or(550.0);
                let y = action.get("y").and_then(Value::as_f64).unwrap_or(650.0);
                self.page.wheel(x, y, delta).await?;
            }
            "click" | "fill" | "select" => {
                let runtime = runtime_action(action);
                let target = self
                    .evaluate_action(
                        action,
                        &format!("({RESOLVE_TARGET_JS})({})", Value::Object(runtime)),
                        false,
                    )
                    .await;
                let target = match (kind, target) {
                    ("select", Err(_)) => {
                        return Err(ObserveError::Uncertain(
                            "dropdown execution was interrupted".into(),
                        ));
                    }
                    ("select", Ok(Value::Null)) => {
                        return Err(ObserveError::Uncertain(
                            "dropdown execution was not confirmed".into(),
                        ));
                    }
                    (_, Ok(Value::Null)) => {
                        return Err(ObserveError::Stale("target changed or is covered"));
                    }
                    (_, other) => other?,
                };
                if kind != "select" {
                    let offset_x = action
                        .get("frame_offset_x")
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0);
                    let offset_y = action
                        .get("frame_offset_y")
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0);
                    let x = target["x"].as_f64().unwrap_or(0.0) + offset_x;
                    let y = target["y"].as_f64().unwrap_or(0.0) + offset_y;
                    self.page.click(x, y).await?;
                    if kind == "fill" {
                        let value = text.unwrap_or_default();
                        if action.get("contenteditable").and_then(Value::as_bool) == Some(true) {
                            self.page.replace_text_multiline(value).await?;
                        } else {
                            self.page.replace_text(value).await?;
                        }
                    }
                }
            }
            "upload" => {
                let node = action
                    .get("local_node")
                    .or_else(|| action.get("node"))
                    .and_then(Value::as_u64)
                    .ok_or(ObserveError::Stale("file input has no observed node"))?;
                if self.attachments.is_empty() {
                    return Err(ObserveError::Stale("no attachment was supplied"));
                }
                let context_id = match action.get("context_id").and_then(Value::as_u64) {
                    Some(context_id) => context_id,
                    None => self.context_id().await?,
                };
                self.page
                    .set_file_input_files(context_id, node, &self.attachments)
                    .await?;
            }
            "press" => {
                let key = action
                    .get("key")
                    .and_then(Value::as_str)
                    .ok_or(ObserveError::Stale("key target is missing"))?;
                self.page.press_key(key).await?;
            }
            _ => return Err(ObserveError::Stale("unknown action kind")),
        }
        let mut recorded = action.clone();
        if kind == "fill" {
            recorded.insert("expected".into(), json!(text.unwrap_or_default()));
        }
        self.after_input = (kind != "wait").then_some(recorded);
        Ok(())
    }
}

const RESOLVE_TARGET_JS: &str = r#"action => window.__jevFast?.resolve(action) ?? null"#;

const AFTER_INPUT_JS: &str = r#"action => new Promise(resolve => {
  const field=window.__jevFast?.nodes.get(action.node);
  const autocomplete=action.kind==='fill' && field?.getAttribute('role')==='combobox';
  let frames=0,stopped=false;
  const finish=()=>{stopped=true;resolve(true)};
  setTimeout(finish,autocomplete ? 200 : 50);
  const ready=()=>{
    if (stopped) return;
    const ids=(field?.getAttribute('aria-controls')||field?.getAttribute('aria-owns')||'').split(/\s+/).filter(Boolean);
    const owner=field?.ownerDocument||document,root=field?.getRootNode?.()||owner;
    const roots=ids.length ? ids.map(id=>root.getElementById?.(id)||owner.getElementById(id)).filter(Boolean) : [root];
    const options=roots.flatMap(root=>[...root.querySelectorAll('[role="option"]')]);
    if (++frames>=2 && (!autocomplete || options.some(e=>{
      const r=e.getBoundingClientRect();
      return r.width && r.height && r.bottom>0 && r.top<(e.ownerDocument.defaultView?.innerHeight||innerHeight) &&
        e.checkVisibility({checkOpacity:true,checkVisibilityCSS:true});
    }))) finish();
    else requestAnimationFrame(ready);
  };
  requestAnimationFrame(ready);
})"#;

const VERIFY_TEXT_JS: &str = r#"(node,expected) => {
  const e=window.__jevFast?.nodes.get(node);
  const normalise=value=>String(value||'').replace(/\s+/g,' ').trim();
  return !!e?.isConnected && normalise(e.innerText).includes(normalise(expected));
}"#;

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{FrameContributions, MAX_ELEMENT_ACTIONS, merge_frames};

    fn buttons(prefix: &str, count: usize) -> Vec<Value> {
        (0..count)
            .map(|n| json!({ "kind": "click", "node": n, "label": format!("{prefix} {n}") }))
            .collect()
    }

    /// `snapshot.js` caps itself per execution context, so six iframes handed
    /// Jev seven capped lists — about 1,750 actions — and every one of them
    /// claimed to have omitted nothing. The merge is the only place the
    /// page-wide budget can exist (R3.4).
    #[test]
    fn merging_frames_re_applies_the_element_budget_and_keeps_the_controls() {
        let mut root = json!({
            "actions": [],
            "guards": {},
            "text": "top document",
            "marker": ["root"],
            "omitted_actions": 4,
        });
        let mut actions = buttons("top", MAX_ELEMENT_ACTIONS);
        actions.push(json!({ "id": "wait", "kind": "wait", "label": "Wait for the page" }));
        actions.push(json!({ "id": "scroll_down", "kind": "scroll", "label": "Scroll down" }));
        root["actions"] = json!(actions);
        let frames = FrameContributions {
            actions: buttons("frame", MAX_ELEMENT_ACTIONS * 2),
            markers: vec![json!(["frame-1", ["m"]])],
            text: vec!["inside the frame".into()],
            guards: vec![("4294967297".into(), json!("guard"))],
            omitted: 7,
            signals: std::collections::HashMap::new(),
            unreadable: 2,
        };

        merge_frames(&mut root, frames);

        let merged = root["actions"].as_array().map_or(&[][..], Vec::as_slice);
        assert_eq!(
            merged
                .iter()
                .filter(|action| action["kind"] == "click")
                .count(),
            MAX_ELEMENT_ACTIONS,
            "the merged list must still fit the budget"
        );
        // The top document's actions are the ones that survive, in order.
        assert_eq!(merged[0]["label"], json!("top 0"));
        // A truncated page is exactly the page where scrolling and waiting
        // still help, so the controls are never what gets cut.
        assert!(merged.iter().any(|action| action["kind"] == "wait"));
        assert!(merged.iter().any(|action| action["kind"] == "scroll"));
        // What was dropped is what the model is told about: 4 by the top
        // document, 7 across the frames, and the 500 this cut just made.
        assert_eq!(
            root["omitted_actions"],
            json!(4 + 7 + MAX_ELEMENT_ACTIONS * 2)
        );
        assert_eq!(root["signals"]["cross_origin_frames"], json!(2));
        assert_eq!(root["text"], json!("top document\ninside the frame"));
        assert_eq!(root["guards"]["4294967297"], json!("guard"));
    }
}
