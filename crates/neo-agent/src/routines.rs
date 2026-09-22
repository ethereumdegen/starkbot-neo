//! Routines: a parameterised recipe of neo's own gated tools (06 §4.2).
//!
//! A navigator run is one coherent goal against one screen. That is the right
//! unit for "click the thing", and the wrong unit for a sequence whose state
//! does not survive between runs. Measured on degen-paint Studio: the command
//! palette closes on blur, so *choose the op* and *fill the form it opened*
//! cannot be two `app` calls — the second one arrives to find the palette shut
//! and the form gone. Every run of the logo task died there, whatever the
//! model worded its goals as.
//!
//! A routine is the unit that spans it: the steps run back to back inside one
//! screen hold, so what step two opened is still open for step three.
//!
//! What this is not: a scripting language. There are no loops, no conditionals
//! and no references between steps (06 §4.2), and a routine gets **no gate
//! exemptions** — every step runs the same deterministic rules, safety heads
//! and confirm path as anything else, because a recipe the user installed is
//! still not a permission the user granted.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use neo_ax::{AppSel, AxAction, AxHandle, Key, Modifier};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::agent::{AppOptions, run_app};
use crate::runtime::Runtime;
use neo_core::RunId;

/// How long a routine may run when it does not say.
const DEFAULT_MAX_SECS: u64 = 120;
/// The ceiling 06 §4.2 puts on `max_secs`.
const MAX_SECS_CEILING: u64 = 300;
/// The confidence the verify head must reach for a routine to be `done`.
const VERIFY_FLOOR: f64 = 0.6;

/// One step: a tool from the closed list, and its arguments.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Step {
    pub tool: String,
    #[serde(default)]
    pub args: Value,
}

/// A recipe, as the pack wrote it.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Routine {
    pub name: String,
    /// The pack it came from; `<pack>/<name>` is how it is addressed.
    #[serde(default)]
    pub pack: String,
    /// Where that pack lives, so a step can reach the rest of it — the app
    /// hints, and the control channel they declare.
    #[serde(skip)]
    pub pack_dir: PathBuf,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub examples: Vec<String>,
    #[serde(default)]
    pub params: Value,
    pub steps: Vec<Step>,
    #[serde(default)]
    pub verify: String,
    #[serde(default)]
    pub max_secs: Option<u64>,
}

impl Routine {
    /// `<pack>/<name>`, which is what a caller names.
    #[must_use]
    pub fn id(&self) -> String {
        if self.pack.is_empty() {
            self.name.clone()
        } else {
            format!("{}/{}", self.pack, self.name)
        }
    }

    fn budget(&self) -> Duration {
        Duration::from_secs(
            self.max_secs
                .unwrap_or(DEFAULT_MAX_SECS)
                .min(MAX_SECS_CEILING),
        )
    }
}

/// How a routine ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// Every step ran and the verify head agreed.
    Done,
    /// Every step ran; the verify head did not agree, or could not be asked.
    ///
    /// Deliberately distinct from `Failed`: the work may well have happened,
    /// and reporting an unreachable judge as a failure would be a lie about
    /// the application's state.
    Unverified,
    /// A step errored, was blocked, or the budget ran out.
    Failed,
}

/// What a routine did, in the shape a tool result is read from.
#[derive(Clone, Debug, Serialize)]
pub struct RoutineRun {
    pub routine: String,
    pub disposition: Disposition,
    /// One line per step: what it was and what it produced.
    pub steps: Vec<String>,
    /// The last thing the screen said, which is what a handoff reads.
    pub observation: String,
    /// Why it stopped, when it did not finish.
    pub detail: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum RoutineError {
    #[error("no routine is named `{0}`")]
    Unknown(String),
    /// Named with the fix rather than only the gap: a model that is told
    /// only "needs `op`" re-sends the same call without it.
    #[error(
        "the routine needs `{0}`, which was not given — pass it inside `params`, as \
         {{\"name\": \"…\", \"params\": {{\"{0}\": \"…\"}}}}"
    )]
    MissingParam(String),
    #[error("`{0}` is not a step a routine may take")]
    UnknownStep(String),
    #[error("the accessibility surface refused: {0}")]
    Ax(String),
}

/// Every routine of every installed pack.
#[must_use]
pub fn installed() -> Vec<Routine> {
    let Some(root) = crate::skills::packs_dir() else {
        return Vec::new();
    };
    let Ok(packs) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out: Vec<Routine> = packs
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .flat_map(|entry| read_pack(&entry.path()))
        .collect();
    out.sort_by_key(Routine::id);
    out
}

fn read_pack(pack: &Path) -> Vec<Routine> {
    let name = pack
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_owned();
    let Ok(entries) = std::fs::read_dir(pack.join("desktop/routines")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let text = std::fs::read_to_string(entry.path()).ok()?;
            let mut routine: Routine = serde_json::from_str(&text).ok()?;
            routine.pack.clone_from(&name);
            routine.pack_dir = pack.to_path_buf();
            (!routine.steps.is_empty()).then_some(routine)
        })
        .collect()
}

/// Where a pack's app declares its own control channel, if it declares one.
///
/// The channel is the app's own GUI dispatch — the endpoint its own front end
/// posts to — not a CLI and not an MCP server. Using it is what lets a step
/// change the document without taking the seat: no pointer warped out from
/// under the person at the machine, no window pulled to the front, no
/// keystrokes taken from whatever they were typing into. Accessibility is
/// still how the app is *read*; this is how it is told to do a thing that
/// accessibility cannot express without the keyboard.
fn control_base(pack: &Path) -> Option<(String, String)> {
    let hints = std::fs::read_dir(pack.join("desktop/apps")).ok()?;
    for hint in hints.flatten() {
        let text = std::fs::read_to_string(hint.path()).ok()?;
        let value: Value = serde_json::from_str(&text).ok()?;
        let Some(control) = value.get("control") else {
            continue;
        };
        if control.get("kind").and_then(Value::as_str) != Some("http") {
            continue;
        }
        let base = control.get("base")?;
        // The environment wins, so a fixture can point this at its own
        // server without editing an installed pack.
        let resolved = base
            .get("env")
            .and_then(Value::as_str)
            .and_then(|name| std::env::var(name).ok())
            .or_else(|| {
                base.get("default")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })?;
        let path = control
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("/api")
            .to_owned();
        return Some((resolved.trim_end_matches('/').to_owned(), path));
    }
    None
}

/// Call the app's own control channel: one `{method, params}` post.
async fn call_pack_tool(pack: &Path, args: &Value) -> Result<String, String> {
    let (base, path) =
        control_base(pack).ok_or_else(|| "this pack declares no control channel".to_owned())?;
    let method = args
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| "a call_pack_tool step needs a `method`".to_owned())?;
    let params = args.get("params").cloned().unwrap_or(Value::Null);
    let body = serde_json::json!({ "method": method, "params": params });
    let response = reqwest::Client::new()
        .post(format!("{base}{path}"))
        .timeout(Duration::from_secs(120))
        .json(&body)
        .send()
        .await
        .map_err(|error| format!("{base}{path} could not be reached: {error}"))?;
    let value: Value = response
        .json()
        .await
        .map_err(|error| format!("{base}{path} answered in an unreadable shape: {error}"))?;
    // The channel answers `{ok:false, error}` with HTTP 200, so the status is
    // not the verdict — a step that read only the status would call every
    // refused op a success.
    if value.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(value
            .get("result")
            .map(ToString::to_string)
            .unwrap_or_else(|| "ok".to_owned()))
    } else {
        // The refusal is the useful half of the answer — it names the field
        // that was wrong and, for a schema violation, the shape the op
        // actually takes. It arrives as an object, so reading it as a string
        // threw all of that away and handed back a sentence that said
        // nothing: a run then re-guessed the same call four times.
        let refusal = value.get("error").map_or_else(
            || "the control channel refused the call".to_owned(),
            |error| {
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .map_or_else(|| error.to_string(), str::to_owned)
            },
        );
        Err(refusal)
    }
}

/// The prompt section naming what the `routine` tool can be asked for.
///
/// A tool the model is never told the arguments of is a tool it will not use,
/// so each routine is rendered with its id, its sentence and its parameter
/// names — enough to call it, and not the whole schema, which the packs write
/// for a validator rather than for a reader.
#[must_use]
pub fn render(routines: &[Routine]) -> String {
    if routines.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n\nRoutines available to the `routine` tool. Each runs its steps \
         back to back against one screen, which is the only way to do work \
         that spans a dialog or a form. Prefer one over a sequence of `app` \
         goals whenever it fits.",
    );
    for routine in routines {
        let params = routine
            .params
            .get("properties")
            .and_then(Value::as_object)
            .map(|properties| properties.keys().cloned().collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        out.push_str(&format!("\n- `{}` — {}", routine.id(), routine.description));
        if !params.is_empty() {
            out.push_str(&format!(" · params: {params}"));
        }
        // One example, the pack's own wording. 06 §4.2 says the description
        // *and* the examples are what a routine is chosen by, and it is
        // right: `dp-import-file`'s description said "from a URL" and was
        // never called; its example says "import https://…/favicon.svg as a
        // layer so I can work from the real mark", which is the sentence
        // that makes the possibility real.
        let examples: Vec<String> = routine
            .examples
            .iter()
            .take(3)
            .map(|example| format!("\"{example}\""))
            .collect();
        if !examples.is_empty() {
            out.push_str(&format!(" · e.g. {}", examples.join(", ")));
        }
    }
    out
}

/// The routine `id` names, by `<pack>/<name>` or by bare name.
#[must_use]
pub fn find(id: &str) -> Option<Routine> {
    let all = installed();
    all.iter()
        .find(|routine| routine.id() == id)
        .or_else(|| all.iter().find(|routine| routine.name == id))
        .cloned()
}

/// Substitute `{param}` into one string.
///
/// A parameter the caller left out becomes the empty string rather than the
/// literal `{name}`: the goals these land in are sentences for a navigator,
/// and a brace in one reads as something to look for on the screen.
fn fill(text: &str, params: &Map<String, Value>) -> String {
    let mut out = text.to_owned();
    for (key, value) in params {
        let rendered = match value {
            Value::Null => String::new(),
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        out = out.replace(&format!("{{{key}}}"), &rendered);
    }
    out
}

/// The parameter name when `text` is exactly one placeholder and nothing else.
fn whole_placeholder(text: &str) -> Option<&str> {
    let inner = text.strip_prefix('{')?.strip_suffix('}')?;
    (!inner.is_empty() && !inner.contains(['{', '}'])).then_some(inner)
}

fn filled_args(args: &Value, params: &Map<String, Value>) -> Value {
    match args {
        // A string that is *only* a placeholder carries the parameter's own
        // type through, because a step argument is not always prose: an op's
        // arguments are an object, and rendering them into `"{args}"` would
        // hand the app a JSON string where it wants a JSON object. Anywhere
        // else in a sentence, a value still renders as text.
        Value::String(text) => match whole_placeholder(text) {
            // Absent or null, it is `null` on the wire — the app's own
            // default applies. Rendered as text it would be the literal
            // `{mode}`, which no schema accepts and no default covers.
            Some(key) => params.get(key).cloned().unwrap_or(Value::Null),
            None => Value::String(fill(text, params)),
        },
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| filled_args(item, params)).collect())
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), filled_args(value, params)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Every required parameter the schema names is present and not null.
fn require_params(routine: &Routine, params: &Map<String, Value>) -> Result<(), RoutineError> {
    let required = routine
        .params
        .get("required")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    for name in required.iter().filter_map(Value::as_str) {
        let given = params.get(name).is_some_and(|value| !value.is_null());
        if !given {
            return Err(RoutineError::MissingParam(name.to_owned()));
        }
    }
    Ok(())
}

fn arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

/// One modifier-plus-key combo, as a routine writes it (`cmd+shift+e`).
///
/// `cmd` is accepted and means the platform's own command modifier, because
/// the packs are written once for both platforms (A36).
fn parse_combo(combo: &str) -> Option<(Key, Vec<Modifier>)> {
    let mut modifiers = Vec::new();
    let mut key = None;
    for part in combo.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        match part.to_ascii_lowercase().as_str() {
            "cmd" | "command" | "super" | "meta" => modifiers.push(Modifier::Command),
            "ctrl" | "control" => modifiers.push(Modifier::Control),
            "alt" | "option" => modifiers.push(Modifier::Option),
            "shift" => modifiers.push(Modifier::Shift),
            "enter" | "return" => key = Some(Key::Return),
            "escape" | "esc" => key = Some(Key::Escape),
            "tab" => key = Some(Key::Tab),
            "space" => key = Some(Key::Space),
            "delete" | "backspace" => key = Some(Key::Delete),
            "up" => key = Some(Key::Up),
            "down" => key = Some(Key::Down),
            "left" => key = Some(Key::Left),
            "right" => key = Some(Key::Right),
            _ => return None,
        }
    }
    key.map(|key| (key, modifiers))
}

/// Run one routine, step by step, inside one screen hold.
///
/// `screen` is the caller's hold. Every step drives the same surface, so they
/// share it rather than each taking and dropping one — which is the whole
/// reason a routine can do what a sequence of `app` calls cannot.
pub async fn run(
    runtime: &Arc<Runtime>,
    routine: &Routine,
    params: &Map<String, Value>,
    run_id: RunId,
    cancel: &CancellationToken,
    screen: Option<crate::screen::ScreenScope>,
) -> Result<RoutineRun, RoutineError> {
    require_params(routine, params)?;
    let settings = runtime.settings().unwrap_or_default();
    let deadline = Instant::now() + routine.budget();

    let ax = AxHandle::spawn().map_err(|error| RoutineError::Ax(error.to_string()))?;
    let mut steps: Vec<String> = Vec::new();
    let mut observation = String::new();
    let mut app_hint: Option<String> = None;

    for step in &routine.steps {
        if cancel.is_cancelled() {
            return Ok(stopped(
                routine,
                steps,
                observation,
                "the routine was cancelled",
            ));
        }
        if Instant::now() >= deadline {
            return Ok(stopped(
                routine,
                steps,
                observation,
                "the routine ran out of time",
            ));
        }
        let args = filled_args(&step.args, params);
        let outcome: Result<String, String> = match step.tool.as_str() {
            "launch_app" | "focus_app" => {
                let name = arg(&args, "name").unwrap_or_default().to_owned();
                app_hint = Some(name.clone());
                ax.activate(&selector_of(&name))
                    .await
                    .map(|app| format!("focused {}", app.name))
                    .map_err(|error| error.to_string())
            }
            "select_menu" => {
                let path: Vec<String> = args
                    .get("path")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                ax.act(&AxAction::SelectMenu { path: path.clone() })
                    .await
                    .map(|done| done.summary)
                    .map_err(|error| error.to_string())
            }
            "type_text" => {
                let text = arg(&args, "text").unwrap_or_default().to_owned();
                ax.act(&AxAction::TypeText { text: text.clone() })
                    .await
                    .map(|done| done.summary)
                    .map_err(|error| error.to_string())
            }
            "key" => {
                let combo = arg(&args, "combo").unwrap_or_default();
                match parse_combo(combo) {
                    Some((key, modifiers)) => ax
                        .act(&AxAction::Key { key, modifiers })
                        .await
                        .map(|done| done.summary)
                        .map_err(|error| error.to_string()),
                    None => Err(format!("`{combo}` is not a key this routine may press")),
                }
            }
            "wait_for" => {
                let wanted = arg(&args, "text").unwrap_or_default().to_owned();
                let secs = args
                    .get("max_secs")
                    .and_then(Value::as_u64)
                    .unwrap_or(10)
                    .min(60);
                wait_for(&ax, app_hint.as_deref(), &wanted, secs).await
            }
            "call_pack_tool" => {
                let answered = call_pack_tool(&routine.pack_dir, &args).await;
                // What the channel said *is* the observation for a routine
                // that never touches the screen. Without this the verify head
                // is handed an empty string and every such routine comes back
                // `unverified` however plainly it succeeded.
                if let Ok(said) = &answered {
                    observation.clone_from(said);
                }
                answered
            }
            "navigate" => {
                let goal = arg(&args, "goal").unwrap_or_default().to_owned();
                let app = arg(&args, "app")
                    .map(str::to_owned)
                    .or_else(|| app_hint.clone())
                    .unwrap_or_default();
                if app.is_empty() {
                    Err("a navigate step needs an application to drive".to_owned())
                } else {
                    let mut options = AppOptions::unattended(&settings, &app, &goal);
                    options.screen = screen;
                    match run_app(runtime, &options, run_id, cancel).await {
                        Ok(run) => {
                            observation = run.observation.clone();
                            Ok(run.observation)
                        }
                        Err(error) => Err(error.to_string()),
                    }
                }
            }

            other => return Err(RoutineError::UnknownStep(other.to_owned())),
        };

        match outcome {
            Ok(said) => steps.push(format!("{}: {}", step.tool, first_line(&said))),
            Err(why) => {
                steps.push(format!("{}: failed — {why}", step.tool));
                return Ok(stopped(routine, steps, observation, &why));
            }
        }
    }

    // The screen's last word, for the verify head and for a handoff.
    if let Some(app) = app_hint.as_deref()
        && let Ok(table) = ax.table(&selector_of(app)).await
    {
        observation = table.text;
    }

    let disposition = match verified(runtime, routine, &observation).await {
        Some(true) => Disposition::Done,
        Some(false) => Disposition::Unverified,
        None => Disposition::Unverified,
    };
    Ok(RoutineRun {
        routine: routine.id(),
        disposition,
        steps,
        observation,
        detail: None,
    })
}

/// One line per step, and a short one: the full result of the last step is
/// the routine's observation and is rendered in full beneath the list, so
/// a step line that repeated it would put a two-kilobyte digest in the
/// model's context twice.
fn first_line(text: &str) -> String {
    const STEP_LINE: usize = 160;
    let line = text.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= STEP_LINE {
        return line.to_owned();
    }
    let kept: String = line.chars().take(STEP_LINE).collect();
    format!("{kept}…")
}

fn stopped(routine: &Routine, steps: Vec<String>, observation: String, why: &str) -> RoutineRun {
    RoutineRun {
        routine: routine.id(),
        disposition: Disposition::Failed,
        steps,
        observation,
        detail: Some(why.to_owned()),
    }
}

fn selector_of(app: &str) -> AppSel {
    if let Ok(pid) = app.parse::<i32>() {
        return AppSel::Pid(pid);
    }
    if app.contains('.') && !app.contains(' ') {
        return AppSel::BundleId(app.to_owned());
    }
    AppSel::Name(app.to_owned())
}

/// Poll the surface until it says `wanted`.
async fn wait_for(
    ax: &AxHandle,
    app: Option<&str>,
    wanted: &str,
    secs: u64,
) -> Result<String, String> {
    let Some(app) = app else {
        return Err("a wait_for step needs an application to watch".to_owned());
    };
    let selector = selector_of(app);
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Ok(table) = ax.table(&selector).await
            && table.text.to_lowercase().contains(&wanted.to_lowercase())
        {
            return Ok(format!("saw \"{wanted}\""));
        }
        if Instant::now() >= deadline {
            return Err(format!("\"{wanted}\" never appeared"));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// One Jev yes/no on the routine's own `verify` sentence (06 §4.2).
///
/// `None` means the question could not be asked. It is reported as unverified,
/// never as success and never as failure: a vendor being unreachable says
/// nothing about what the application did.
async fn verified(runtime: &Arc<Runtime>, routine: &Routine, observation: &str) -> Option<bool> {
    if routine.verify.trim().is_empty() {
        return None;
    }
    let jev = runtime.jev().ok()?;
    let state = serde_json::json!({ "screen": { "text": observation } });
    let questions = serde_json::json!({
        "verified": {
            "type": "noul",
            "instructions": {
                "rules": format!(
                    "Answer yes only if what the screen says shows this is true: {}",
                    routine.verify
                )
            }
        }
    });
    let evaluation = jev.evaluate(&state, &questions).await.ok()?;
    evaluation.noul("verified").ok().map(|p| p >= VERIFY_FLOOR)
}
#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect()
    }

    #[test]
    fn a_parameter_lands_in_every_string_it_is_written_into() {
        let args = serde_json::json!({
            "goal": "open the project {project}",
            "path": ["File", "Open {project}"]
        });
        let filled = filled_args(&args, &params(&[("project", Value::String("acme".into()))]));

        assert_eq!(filled["goal"], "open the project acme");
        assert_eq!(filled["path"][1], "Open acme");
    }

    /// A parameter the caller omitted must not leave `{name}` in a sentence a
    /// navigator is going to read as something to find on the screen.
    #[test]
    fn an_omitted_parameter_leaves_no_braces_behind() {
        let filled = fill(
            "export {document} at {scale}",
            &params(&[("document", Value::Null), ("scale", Value::Null)]),
        );

        assert_eq!(filled, "export  at ");
        assert!(!filled.contains('{'));
    }

    #[test]
    fn a_missing_required_parameter_is_refused_by_name() {
        let routine = Routine {
            name: "dp-open-project".into(),
            pack: "media-apps".into(),
            pack_dir: PathBuf::new(),
            description: String::new(),
            examples: Vec::new(),
            params: serde_json::json!({ "required": ["project"] }),
            steps: vec![Step {
                tool: "focus_app".into(),
                args: Value::Null,
            }],
            verify: String::new(),
            max_secs: None,
        };

        let missing = require_params(&routine, &params(&[]));
        assert!(matches!(missing, Err(RoutineError::MissingParam(name)) if name == "project"));
        assert!(
            require_params(
                &routine,
                &params(&[("project", Value::String("acme".into()))])
            )
            .is_ok()
        );
    }

    /// The combo vocabulary is closed, and an unknown one must be refused
    /// rather than silently pressing nothing.
    #[test]
    fn a_combo_parses_to_its_key_and_modifiers() {
        let Some((key, modifiers)) = parse_combo("cmd+shift+escape") else {
            panic!("cmd+shift+escape is a known combo")
        };
        assert_eq!(key, Key::Escape);
        assert!(modifiers.contains(&Modifier::Command));
        assert!(modifiers.contains(&Modifier::Shift));

        let Some((plain, none)) = parse_combo("enter") else {
            panic!("enter is a known combo")
        };
        assert_eq!(plain, Key::Return);
        assert!(none.is_empty());

        // A key outside the closed set is refused, never silently dropped.
        assert!(parse_combo("cmd+frobnicate").is_none());
    }

    /// `max_secs` is capped, so a pack cannot hold the screen indefinitely.
    #[test]
    fn the_time_budget_is_bounded_by_the_contract() {
        let routine = |secs| Routine {
            name: "r".into(),
            pack: "p".into(),
            pack_dir: PathBuf::new(),
            description: String::new(),
            examples: Vec::new(),
            params: Value::Null,
            steps: vec![Step {
                tool: "focus_app".into(),
                args: Value::Null,
            }],
            verify: String::new(),
            max_secs: secs,
        };

        assert_eq!(routine(None).budget().as_secs(), DEFAULT_MAX_SECS);
        assert_eq!(routine(Some(9_000)).budget().as_secs(), MAX_SECS_CEILING);
        assert_eq!(routine(Some(30)).budget().as_secs(), 30);
    }
    /// An op's arguments are an object, and the control channel wants one.
    /// Rendering `"{args}"` as text would hand it a JSON string instead and
    /// every op would be refused for a malformed argument.
    #[test]
    fn a_whole_placeholder_keeps_its_parameter_s_type() {
        let step = serde_json::json!({ "params": { "op": "{op}", "args": "{args}" } });
        let filled = filled_args(
            &step,
            &params(&[
                ("op", Value::String("vector.object.add-ellipse".into())),
                ("args", serde_json::json!({ "cx": 512, "rx": 360 })),
            ]),
        );

        assert!(filled["params"]["args"].is_object(), "{filled}");
        assert_eq!(filled["params"]["args"]["cx"], 512);
        assert_eq!(filled["params"]["op"], "vector.object.add-ellipse");
    }

    /// A whole placeholder nobody supplied is `null`, never the literal
    /// `{name}`: the app then applies its own default instead of refusing a
    /// value that was never meant to be one.
    #[test]
    fn an_unsupplied_whole_placeholder_is_null() {
        let filled = filled_args(
            &serde_json::json!({ "params": { "path": "{path}", "mode": "{mode}" } }),
            &params(&[("path", Value::String("x.svg".into()))]),
        );

        assert_eq!(filled["params"]["path"], "x.svg");
        assert!(filled["params"]["mode"].is_null(), "{filled}");
    }

    /// A placeholder inside a sentence still renders as text, or a navigate
    /// goal would stop being a sentence.
    #[test]
    fn a_placeholder_among_words_still_renders_as_text() {
        let filled = filled_args(
            &serde_json::json!({ "goal": "run {op} now" }),
            &params(&[("op", Value::String("lint.run".into()))]),
        );

        assert_eq!(filled["goal"], "run lint.run now");
    }
}
