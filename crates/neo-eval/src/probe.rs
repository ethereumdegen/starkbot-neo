//! Reading the application's own state back, after the agent has finished.
//!
//! This is the difference between an eval and a transcript review. The model's
//! answer is an opinion; a probe is a fact. Each probe re-reads the surface
//! through the same accessibility path the agent used and reports what is
//! *there*, so an assertion can be a statement about the document.
//!
//! Probes are deliberately read-only: they call `AxHandle::table`, never an
//! action. A probe that could change the app would be able to make its own
//! assertion pass.

use neo_agent::Runtime;
use neo_ax::{AppSel, AxHandle};
use serde_json::{Value, json};
use spice_framework::agent::AgentConfig;

use crate::apps::App;

/// What to read after the turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Probe {
    /// Every text value in the app's focused window, plus the window title.
    /// The broadest probe: assertions then look for their expected string.
    AppText { app: String },
    /// One spreadsheet cell, by its address label (`A1`).
    ///
    /// Calc and Numbers both expose grid cells as accessibility elements
    /// labelled with their address (a finding recorded in 01 §spreadsheets),
    /// which is what makes this possible without a macro language (P9).
    Cell { app: String, cell: String },
    /// The element table's shape: how many rows the model was offered, and the
    /// window title. Useful for the pruning work A23 calls for — a case can
    /// assert that a spreadsheet's menus survived the 250-row budget.
    Surface { app: String },
    /// What a web page recorded about what was done to it (16 §6.2).
    ///
    /// The navigation review set needs the *page's* state, and an
    /// accessibility read of Chrome's window cannot give it: a shadow-root
    /// field, a cross-origin frame and a closed popup tab are all invisible
    /// to it, and a page that scrolled looks identical to one that did not.
    /// So each review-set fixture mirrors what its own handlers did into
    /// `localStorage`, and this reads that back from a tab of the harness's
    /// own on the same origin — see [`crate::pages`] for why a probe cannot
    /// simply read the tab the navigator drove.
    ///
    /// `url` is the read-only state page on the origin being scored, which is
    /// what selects the origin: the cross-origin case records on one and the
    /// frame lives on the other.
    Page { app: String, url: String },
    /// What degen-paint reports about the document it is holding (A37).
    ///
    /// The media app is the one target whose result cannot be read off an
    /// accessibility tree. A canvas is pixels; "is there a logo on it" is not
    /// a label, and the window says the same thing whether the document is
    /// empty or finished. degen-paint answers that about itself over the
    /// read-only grounding API its skill contract declares, and reading it is
    /// allowed exactly where driving it would not be: 12 §2 makes the UI the
    /// only mutation path and leaves side channels read-only. The agent still
    /// has to have *operated the Studio* to change what this returns.
    Grounding { app: String, base: String },
    /// Where an executable resolves from the same `PATH` the agent inherits.
    CommandPath { app: String, command: String },
    /// The current columns and cards of one Octaweave project.
    OctaweaveProject { app: String, project: String },
}

impl Probe {
    /// Read a probe out of a case's config: `{"probe": {...}}`.
    #[must_use]
    pub fn from_config(config: &AgentConfig) -> Option<Self> {
        let probe = config.data.get("probe")?;
        let app = probe.get("app").and_then(Value::as_str)?.to_owned();
        match probe.get("kind").and_then(Value::as_str)? {
            "app_text" => Some(Self::AppText { app }),
            "cell" => Some(Self::Cell {
                app,
                cell: probe.get("cell").and_then(Value::as_str)?.to_owned(),
            }),
            "surface" => Some(Self::Surface { app }),
            "page" => Some(Self::Page {
                app,
                url: probe.get("url").and_then(Value::as_str)?.to_owned(),
            }),
            "grounding" => Some(Self::Grounding {
                app,
                base: probe.get("base").and_then(Value::as_str)?.to_owned(),
            }),
            "command_path" => Some(Self::CommandPath {
                app,
                command: probe.get("command").and_then(Value::as_str)?.to_owned(),
            }),
            "octaweave_project" => Some(Self::OctaweaveProject {
                app,
                project: probe.get("project").and_then(Value::as_str)?.to_owned(),
            }),
            _ => None,
        }
    }

    /// The config fragment for a case builder.
    #[must_use]
    pub fn config(&self) -> Value {
        match self {
            Self::AppText { app } => json!({ "probe": { "kind": "app_text", "app": app } }),
            Self::Cell { app, cell } => {
                json!({ "probe": { "kind": "cell", "app": app, "cell": cell } })
            }
            Self::Surface { app } => json!({ "probe": { "kind": "surface", "app": app } }),
            Self::Page { app, url } => {
                json!({ "probe": { "kind": "page", "app": app, "url": url } })
            }
            Self::Grounding { app, base } => {
                json!({ "probe": { "kind": "grounding", "app": app, "base": base } })
            }
            Self::CommandPath { app, command } => {
                json!({ "probe": { "kind": "command_path", "app": app, "command": command } })
            }
            Self::OctaweaveProject { app, project } => {
                json!({
                    "probe": {
                        "kind": "octaweave_project",
                        "app": app,
                        "project": project
                    }
                })
            }
        }
    }

    #[must_use]
    pub fn app(&self) -> &str {
        match self {
            Self::AppText { app }
            | Self::Cell { app, .. }
            | Self::Surface { app }
            | Self::Page { app, .. }
            | Self::Grounding { app, .. }
            | Self::CommandPath { app, .. }
            | Self::OctaweaveProject { app, .. } => app,
        }
    }
}

/// Convenience: the probe for one known target.
#[must_use]
pub fn cell_of(app: App, cell: &str) -> Probe {
    Probe::Cell {
        app: app.selector().to_owned(),
        cell: cell.to_owned(),
    }
}

/// Where degen-paint's grounding API answers, overridable for a fixture.
///
/// The default is the one its own skill contract declares, so a case does not
/// restate the vendor's address.
#[must_use]
pub fn grounding_base() -> String {
    std::env::var("DPAINT_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:4317".to_owned())
}

/// A client for degen-paint's loopback server.
///
/// One connection per request, on purpose. The server is a hand-rolled
/// `std::net` loop that answers and closes; `reqwest`'s default pool then
/// tries to reuse a socket the other end has already shut, and the second
/// call on it — a POST after a GET — fails with "error sending request"
/// before anything is sent. Measured: every consensus run's fixture failed
/// exactly there, on the undo that follows the status read.
pub(crate) fn grounding_client() -> reqwest::Client {
    reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Read one grounding endpoint, tolerating an absent one.
///
/// A probe reports what it found; it does not fail the case because a
/// document has no digest yet. The distinction matters: "no project is open"
/// is a *result* the assertions are entitled to see, not an error.
pub(crate) async fn grounding_get(http: &reqwest::Client, url: &str) -> Option<Value> {
    let response = http
        .get(url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json().await.ok()
}

/// What degen-paint says about the document it is holding.
///
/// Every key is flattened to the top level rather than left nested, because
/// `Assertion::ExpectToolArg` matches one argument by name: a case asserts
/// `drawn == true`, not a path into a vendor document. The raw `digest` rides
/// along for the judge, which needs the colours and the object tree to tell a
/// logo from a blank canvas.
async fn read_grounding(app: &str, base: &str) -> Result<Value, ProbeError> {
    let http = grounding_client();
    let base = base.trim_end_matches('/');

    let status = grounding_get(&http, &format!("{base}/api/v1/status"))
        .await
        .ok_or_else(|| {
            ProbeError::Ax(format!(
                "degen-paint's grounding API did not answer at {base}; \
                 the Studio has to be running for this case to be scored"
            ))
        })?;

    let project = status.get("project").cloned().unwrap_or(Value::Null);
    let revision = status.get("revision").and_then(Value::as_u64).unwrap_or(0);
    let active = status.get("activeDoc").and_then(Value::as_str);

    let digest = match active {
        Some(doc) => grounding_get(&http, &format!("{base}/api/v1/doc/{doc}/digest")).await,
        None => None,
    };
    let lint = match active {
        Some(doc) => grounding_get(&http, &format!("{base}/api/v1/doc/{doc}/lint")).await,
        None => None,
    };
    let history = grounding_get(&http, &format!("{base}/api/v1/history?limit=40"))
        .await
        .unwrap_or(Value::Null);

    let objects = digest
        .as_ref()
        .and_then(|d| d.get("tree"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    // What "drawn" has to mean, measured rather than assumed: a freshly
    // created vector document already reports one tree entry and renders to
    // nothing, so counting objects would pass a blank canvas. Alpha coverage
    // is the fraction of the canvas with any opacity at all — it is zero
    // until something is actually on it.
    let coverage = digest
        .as_ref()
        .and_then(|d| d.get("alpha_coverage"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let lint_errors = lint
        .as_ref()
        .and_then(|l| l.get("errors"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    // The journal is the app's own record of what happened to it, which is
    // what makes "it exported" a fact rather than a claim in the transcript.
    let entries = history.as_array().map(Vec::as_slice).unwrap_or_default();
    let exported = entries.iter().any(|entry| {
        entry
            .get("op")
            .and_then(Value::as_str)
            .is_some_and(|op| op.contains("export") || op.contains("render"))
    });

    Ok(json!({
        "app": app,
        "open": !project.is_null(),
        "project": project,
        "revision": revision,
        "document": active,
        "objects": objects,
        "alpha_coverage": coverage,
        "drawn": coverage > 0.01,
        "mean_color": digest.as_ref().and_then(|d| d.get("mean_color")).cloned(),
        "dominant_colors": digest
            .as_ref()
            .and_then(|d| d.get("dominant_colors"))
            .cloned(),
        "exported": exported,
        "lint_errors": lint_errors,
        "digest": digest,
        "history": history,
    }))
}

/// Observe the application. The returned object becomes the `probe` tool
/// call's arguments, so every key here is assertable with `ExpectToolArg`.
pub async fn run_probe(runtime: &Runtime, probe: &Probe) -> Result<Value, ProbeError> {
    // A page probe reads the browser profile, not the accessibility tree, so
    // it neither needs nor waits on the Accessibility grant — and it must not
    // refuse on a machine that has not been granted one, because the
    // navigation review set is the half of the suite that does not drive a
    // native app at all.
    if let Probe::Page { url, .. } = probe {
        return crate::pages::read_state(runtime.data_dir(), url).await;
    }
    // Likewise a grounding read: it is an HTTP GET against the application's
    // own read-only contract, so it has nothing to do with the accessibility
    // grant and must not be gated behind one.
    if let Probe::Grounding { app, base } = probe {
        return read_grounding(app, base).await;
    }
    if let Probe::CommandPath { command, .. } = probe {
        let path = crate::apps::command_path(command).ok_or_else(|| {
            ProbeError::Command(format!("`{command}` is not installed or is not on PATH"))
        })?;
        return Ok(json!({
            "command": command,
            "path": path,
        }));
    }
    if let Probe::OctaweaveProject { project, .. } = probe {
        let executable = crate::apps::command_path("octaweave").ok_or_else(|| {
            ProbeError::Command("`octaweave` is not installed or is not on PATH".to_owned())
        })?;
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            tokio::process::Command::new(executable)
                .args(["project", "get", project, "--compact"])
                .output(),
        )
        .await
        .map_err(|_| ProbeError::Command("`octaweave project get` timed out".to_owned()))?
        .map_err(|error| ProbeError::Command(error.to_string()))?;
        if !output.status.success() {
            return Err(ProbeError::Command(format!(
                "`octaweave project get` exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        return serde_json::from_slice(&output.stdout)
            .map_err(|error| ProbeError::Command(format!("invalid Octaweave JSON: {error}")));
    }
    if !AxHandle::trusted() {
        return Err(ProbeError::NotTrusted);
    }
    let ax = AxHandle::spawn().map_err(|error| ProbeError::Ax(error.to_string()))?;
    let selector = selector_of(probe.app());
    // The app is already frontmost after the agent drove it; resolving it
    // again by selector is how the probe survives the agent having opened a
    // new window.
    let running = ax
        .activate(&selector)
        .await
        .map_err(|error| ProbeError::Ax(error.to_string()))?;
    let table = ax
        .table(&AppSel::Pid(running.pid))
        .await
        .map_err(|error| ProbeError::Ax(error.to_string()))?;

    let title = table.window.title.clone();
    match probe {
        Probe::AppText { .. } => {
            // Both the window's static text and every element value: a
            // document's contents can live in either, depending on the app.
            let mut text = table.text.clone();
            for value in table.elements.iter().filter_map(|row| row.value.as_deref()) {
                if !value.trim().is_empty() && !text.contains(value) {
                    text.push('\n');
                    text.push_str(value);
                }
            }
            Ok(json!({
                "app": running.name,
                "window": title,
                "text": text,
                "rows": table.elements.len(),
            }))
        }
        Probe::Cell { cell, .. } => {
            // The cell is the row whose label is the address. A missing cell
            // is reported as `null`, never as an empty string: "not there" and
            // "there and empty" are different failures.
            // Three different facts, because they fail differently: the cell
            // is not in the table at all (the grid was pruned away, or the
            // sheet is not open), the cell is there and empty, or the cell
            // holds something. Collapsing them into one boolean is how an
            // assertion ends up unable to say what went wrong.
            let row = table.elements.iter().find(|row| row.label == *cell);
            let value = row.and_then(|row| row.value.clone());
            // `neo-ax` reports a blank text field as the literal "empty".
            let has_value = value
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty() && value != "empty");
            // Two conventions, because two spreadsheets disagree. Calc labels
            // a grid cell with its **address** (`A1`) and carries the contents
            // as the value. Numbers labels a cell with its **contents** and
            // exposes no address at all — measured, once
            // `AXEnhancedUserInterface` made its grid visible. So the probe
            // reports the address lookup *and* everything the grid says, and a
            // case asserts against whichever its app actually offers.
            let grid: Vec<String> = table
                .elements
                .iter()
                .filter(|row| matches!(row.role.as_str(), "cell" | "textfield" | "row"))
                .map(|row| match row.value.as_deref() {
                    Some(value) if value != "empty" => format!("{}={value}", row.label),
                    _ => row.label.clone(),
                })
                .collect();
            Ok(json!({
                "app": running.name,
                "window": title,
                "cell": cell,
                "value": value,
                "present": row.is_some(),
                "found": has_value,
                "grid": grid.join(" · "),
            }))
        }
        Probe::Surface { .. } => {
            let menus = table
                .elements
                .iter()
                .filter(|row| row.role.contains("menu"))
                .count();
            let roles: Vec<String> = {
                let mut roles: Vec<String> =
                    table.elements.iter().map(|row| row.role.clone()).collect();
                roles.sort_unstable();
                roles.dedup();
                roles
            };
            // Which toggles are on. This is what lets a case assert the
            // *outcome* of "turn on bold" instead of a proxy like how many
            // actions it took — a budget assertion fails a run that did the
            // job in four steps and passes one that did nothing in one.
            let engaged: Vec<String> = table
                .elements
                .iter()
                .filter(|row| row.state.selected || row.state.checked == Some(neo_ax::Checked::On))
                .map(|row| row.label.clone())
                .filter(|label| !label.trim().is_empty())
                .collect();
            Ok(json!({
                "app": running.name,
                "window": title,
                "rows": table.elements.len(),
                "engaged": engaged,
                // A23's open question: on a spreadsheet the 250-row budget is
                // eaten by grid cells, so the app's own menus never get
                // offered. This is the number that has to move.
                "menus": menus,
                "truncated": table.truncated,
                "roles": roles,
            }))
        }
        // Both answered above, before the accessibility handle was taken.
        Probe::Page { url, .. } => crate::pages::read_state(runtime.data_dir(), url).await,
        Probe::Grounding { app, base } => read_grounding(app, base).await,
        Probe::CommandPath { .. } => unreachable!("command probes return before accessibility"),
        Probe::OctaweaveProject { .. } => {
            unreachable!("Octaweave probes return before accessibility")
        }
    }
}

/// A bundle id (a dot and no space), a pid (all digits), or a name — the same
/// rule the `app` action uses, so a case names its target once.
pub(crate) fn selector_of(app: &str) -> AppSel {
    if let Ok(pid) = app.parse::<i32>() {
        return AppSel::Pid(pid);
    }
    if app.contains('.') && !app.contains(' ') {
        return AppSel::BundleId(app.to_owned());
    }
    AppSel::Name(app.to_owned())
}

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error(
        "not trusted for Accessibility — grant it in System Settings › Privacy & Security › Accessibility"
    )]
    NotTrusted,
    #[error("could not read the application: {0}")]
    Ax(String),
    /// The review set's own pages are missing or unreachable. A harness
    /// fault, reported as the run's error so a case is not scored as a model
    /// failure for it.
    #[error("the review set's fixtures are not available: {0}")]
    Fixtures(String),
    #[error("could not read the page back: {0}")]
    Browser(String),
    #[error("could not resolve the command: {0}")]
    Command(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A case declares its probe in config and the adapter reads it back; the
    /// round trip is the contract between `cases.rs` and the runner.
    #[test]
    fn a_probe_round_trips_through_the_config() {
        for probe in [
            Probe::AppText {
                app: "com.apple.TextEdit".into(),
            },
            Probe::Cell {
                app: "org.libreoffice.script".into(),
                cell: "A1".into(),
            },
            Probe::Surface {
                app: "Numbers".into(),
            },
            Probe::Page {
                app: "com.google.Chrome".into(),
                url: "http://127.0.0.1:8787/nav-state.html".into(),
            },
            Probe::CommandPath {
                app: "Octaweave CLI".into(),
                command: "octaweave".into(),
            },
            Probe::OctaweaveProject {
                app: "Octaweave CLI".into(),
                project: "starkbot-neo".into(),
            },
        ] {
            let config = AgentConfig {
                data: probe.config(),
            };
            assert_eq!(Probe::from_config(&config), Some(probe));
        }
    }

    #[test]
    fn a_config_without_a_probe_has_none() {
        let config = AgentConfig {
            data: json!({ "something": "else" }),
        };
        assert_eq!(Probe::from_config(&config), None);
    }

    #[test]
    fn an_app_is_selected_the_same_way_the_agent_selects_it() {
        assert_eq!(selector_of("482"), AppSel::Pid(482));
        assert_eq!(
            selector_of("com.apple.TextEdit"),
            AppSel::BundleId("com.apple.TextEdit".into())
        );
        assert_eq!(
            selector_of("Diffusion Studio"),
            AppSel::Name("Diffusion Studio".into())
        );
    }
}
