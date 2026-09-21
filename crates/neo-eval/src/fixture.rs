//! Putting an application into a known state before a case runs.
//!
//! The first run of the TextEdit case failed for a reason that had nothing to
//! do with the agent: the probe read a document left behind by an earlier
//! session (`"neo-ax 1789848524920673000 via…"` in an `Untitled 2` window).
//! Both the agent and the probe were looking at whatever happened to be on
//! screen. An eval without a fixture measures the machine's history.
//!
//! Fixtures use the same accessibility path as everything else — here
//! `AxAction::SelectMenu(["File", "New"])`, which walks the app's real menu
//! bar. No AppleScript, no Apple Events, no per-app macro language (P9), and
//! no synthetic Cmd-N guess that would silently do nothing in an app that
//! binds the key differently.

use neo_ax::{AppSel, AxAction, AxHandle};
use serde_json::{Value, json};
use spice_framework::agent::AgentConfig;

use crate::probe::ProbeError;

/// What to do to the application before the agent gets it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fixture {
    /// Bring the app to the front and open a new, empty document through
    /// `File ▸ New`.
    FreshDocument { app: String },
    /// Open a spreadsheet with known contents, so a case can assert on data
    /// it put there.
    ///
    /// This exists because `File ▸ New` cannot be used on LibreOffice yet: a
    /// running LibreOffice with no open document has **no focused window**, so
    /// there is no observation to resolve a menu against — `neo ax table
    /// LibreOffice` answers `has no focused window`. Opening a document is
    /// also better eval design: a case that asserts `A1 == "Starkbot 42"`
    /// needs A1 to have been put there by the fixture, not by whatever ran
    /// last.
    Spreadsheet { app: String, a1: String },
    /// Open an empty text document.
    ///
    /// Why not `File ▸ New`: an app whose last document was closed has **no
    /// focused window**, so there is no observation for `SelectMenu` to
    /// resolve against and the action answers `stale reference`. Opening a
    /// file needs no window to already exist, which is the point of a fixture.
    TextDocument { app: String },
    /// Only bring the app to the front. For apps with no document model.
    Activate { app: String },
}

impl Fixture {
    #[must_use]
    pub fn from_config(config: &AgentConfig) -> Option<Self> {
        let fixture = config.data.get("fixture")?;
        let app = fixture.get("app").and_then(Value::as_str)?.to_owned();
        match fixture.get("kind").and_then(Value::as_str)? {
            "fresh_document" => Some(Self::FreshDocument { app }),
            "spreadsheet" => Some(Self::Spreadsheet {
                app,
                a1: fixture
                    .get("a1")
                    .and_then(Value::as_str)
                    .unwrap_or("Starkbot 42")
                    .to_owned(),
            }),
            "text_document" => Some(Self::TextDocument { app }),
            "activate" => Some(Self::Activate { app }),
            _ => None,
        }
    }

    #[must_use]
    pub fn config(&self) -> Value {
        match self {
            Self::FreshDocument { app } => {
                json!({ "fixture": { "kind": "fresh_document", "app": app } })
            }
            Self::Spreadsheet { app, a1 } => {
                json!({ "fixture": { "kind": "spreadsheet", "app": app, "a1": a1 } })
            }
            Self::TextDocument { app } => {
                json!({ "fixture": { "kind": "text_document", "app": app } })
            }
            Self::Activate { app } => json!({ "fixture": { "kind": "activate", "app": app } }),
        }
    }

    #[must_use]
    pub fn app(&self) -> &str {
        match self {
            Self::FreshDocument { app }
            | Self::Spreadsheet { app, .. }
            | Self::TextDocument { app }
            | Self::Activate { app } => app,
        }
    }
}

/// Apply the fixture. Errors are the harness's, not the agent's: a case whose
/// fixture fails must not be scored as a model failure.
pub async fn apply(fixture: &Fixture) -> Result<Value, ProbeError> {
    if !AxHandle::trusted() {
        return Err(ProbeError::NotTrusted);
    }
    // A document has to exist before the app can be observed at all, so this
    // runs before `activate`.
    let opened = match fixture {
        Fixture::Spreadsheet { app, a1 } => Some(open_spreadsheet(app, a1)?),
        Fixture::TextDocument { .. } => Some(open_text_document()?),
        _ => None,
    };
    let ax = AxHandle::spawn().map_err(|error| ProbeError::Ax(error.to_string()))?;
    let selector = crate::probe::selector_of(fixture.app());
    let running = ax
        .activate(&selector)
        .await
        .map_err(|error| ProbeError::Ax(error.to_string()))?;

    match fixture {
        Fixture::Activate { .. } => Ok(json!({ "app": running.name, "fixture": "activate" })),
        Fixture::TextDocument { .. } | Fixture::Spreadsheet { .. } => {
            let a1 = match fixture {
                Fixture::Spreadsheet { a1, .. } => a1.as_str(),
                _ => "",
            };
            // The document opens asynchronously; the window has to be there
            // before the agent observes it.
            let mut table = None;
            for _ in 0..20 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if let Ok(observed) = ax.table(&AppSel::Pid(running.pid)).await {
                    table = Some(observed);
                    break;
                }
            }
            let table = table.ok_or_else(|| {
                ProbeError::Ax("the spreadsheet never showed a window".to_owned())
            })?;
            // A modal means something is in the agent's way — Numbers shows a
            // file chooser, and one left over from an earlier session counts
            // too. Wait for the document's own window to come forward before
            // deciding it is stuck, because the chooser closing is exactly
            // what opening a document does.
            let wanted = opened
                .as_ref()
                .and_then(|path| path.file_stem())
                .map(|stem| stem.to_string_lossy().to_string())
                .unwrap_or_default();
            let mut table = table;
            for _ in 0..20 {
                if !table.window.modal && table.window.title.contains(&wanted) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if let Ok(observed) = ax.table(&AppSel::Pid(running.pid)).await {
                    table = observed;
                }
            }
            if table.window.modal {
                return Err(ProbeError::Ax(format!(
                    "a modal dialog is in the way: {:?}",
                    table.window.title
                )));
            }
            // The fixture's own document has to be the one in front, or the
            // case measures whatever was already open. In a full-suite run
            // this is the difference between reading the fixture's value and
            // reading the previous case's leftovers.
            if !wanted.is_empty() && !table.window.title.contains(&wanted) {
                return Err(ProbeError::Ax(format!(
                    "the fixture's document never came forward: the window is {:?}, wanted {wanted:?}",
                    table.window.title
                )));
            }
            Ok(json!({
                "app": running.name,
                "fixture": "spreadsheet",
                "file": opened.map(|path| path.display().to_string()),
                "a1": a1,
                "window": table.window.title,
                "rows": table.elements.len(),
                "cells": table
                    .elements
                    .iter()
                    .filter(|row| row.role == "textfield")
                    .count(),
            }))
        }
        Fixture::FreshDocument { .. } => {
            let path = new_document(&ax, AppSel::Pid(running.pid)).await?;
            // The new window has to exist before the agent starts, or it will
            // observe the old one.
            tokio::time::sleep(std::time::Duration::from_millis(900)).await;
            let table = ax
                .table(&AppSel::Pid(running.pid))
                .await
                .map_err(|error| ProbeError::Ax(error.to_string()))?;
            Ok(json!({
                "app": running.name,
                "fixture": "fresh_document",
                "menu": path,
                "window": table.window.title,
                "text_len": table.text.chars().count(),
            }))
        }
    }
}

/// An empty **rich text** document, opened.
///
/// `.rtf`, not `.txt`: a plain-text document opens TextEdit in plain-text
/// mode, where bold does not exist — no toolbar toggle, no `Format ▸ Font ▸
/// Bold`. A formatting case against a `.txt` fixture cannot pass, and the
/// first version of this fixture wrote one, so the case "passed" only while it
/// was asserting how many actions the agent took instead of whether bold went
/// on. The document needs one character so there is something to format.
fn open_text_document() -> Result<std::path::PathBuf, ProbeError> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let path =
        std::env::temp_dir().join(format!("starkbot-eval-{}-{stamp}.rtf", std::process::id()));
    let document = "{\\rtf1\\ansi\\deff0{\\fonttbl{\\f0 Helvetica;}}\\fs28 Draft\n}";
    std::fs::write(&path, document).map_err(|error| ProbeError::Ax(error.to_string()))?;
    open_path(&path)?;
    Ok(path)
}

/// Hand one fixed path to `/usr/bin/open`. Harness code: the model cannot
/// reach it, so P3's "no command execution" is about the agent, not this.
fn open_path(path: &std::path::Path) -> Result<(), ProbeError> {
    let status = std::process::Command::new("/usr/bin/open")
        .arg(path)
        .status()
        .map_err(|error| ProbeError::Ax(error.to_string()))?;
    if !status.success() {
        return Err(ProbeError::Ax(format!(
            "`open {}` exited with {status}",
            path.display()
        )));
    }
    Ok(())
}

/// Write a one-cell spreadsheet and open it.
///
/// The format is **flat ODF** (`.fods`): a single XML file that Calc opens
/// directly. A `.csv` would raise the Text Import dialog, and a modal dialog
/// is exactly what a fixture must not leave in front of the agent.
///
/// `/usr/bin/open` with one fixed argument is harness code, not an agent
/// action — P3 constrains what the *model* may do, and the model cannot reach
/// this.
fn open_spreadsheet(app: &str, a1: &str) -> Result<std::path::PathBuf, ProbeError> {
    // A unique name per application of the fixture. Reusing one path meant a
    // second case re-opened a document LibreOffice already had open, which
    // raises a reload prompt — and a modal in front of the agent is exactly
    // what a fixture must not leave behind.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    // Format per app, because a fixture's job is to leave a document open and
    // no dialog in front of it. LibreOffice opens flat ODF silently and raises
    // the Text Import dialog for CSV; Numbers has no flat-ODF importer and
    // opens CSV straight into a table. Choosing here is harness scaffolding,
    // not agent policy — the agent never sees a file format.
    let numbers = app.to_lowercase().contains("numbers");
    let extension = if numbers { "csv" } else { "fods" };
    let path = std::env::temp_dir().join(format!(
        "starkbot-eval-{}-{stamp}.{extension}",
        std::process::id()
    ));
    let escaped = a1
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" office:version="1.3" office:mimetype="application/vnd.oasis.opendocument.spreadsheet">
 <office:body>
  <office:spreadsheet>
   <table:table table:name="Eval">
    <table:table-row>
     <table:table-cell office:value-type="string"><text:p>{escaped}</text:p></table:table-cell>
    </table:table-row>
   </table:table>
  </office:spreadsheet>
 </office:body>
</office:document>
"#
    );
    let document = if numbers {
        // Numbers imports the first CSV row as the table's **header**, so a
        // one-line file leaves the data row empty and `A1` reads as the
        // table's name — which is the file name, and is what the first run of
        // this fixture actually returned. A header row plus a data row puts
        // the value where a spreadsheet user would look for it.
        format!("Value\n{}\n", a1.replace('"', "\"\""))
    } else {
        document
    };
    std::fs::write(&path, document).map_err(|error| ProbeError::Ax(error.to_string()))?;
    let status = std::process::Command::new("/usr/bin/open")
        .arg(&path)
        .status()
        .map_err(|error| ProbeError::Ax(error.to_string()))?;
    if !status.success() {
        return Err(ProbeError::Ax(format!(
            "`open {}` exited with {status}",
            path.display()
        )));
    }
    Ok(path)
}

/// Open a new document, whatever this app calls it.
///
/// Two things make this more than one call. An app may put `New` behind a
/// submenu — LibreOffice's is `File ▸ New ▸ Spreadsheet`, and a bare
/// `File ▸ New` resolves to the submenu rather than an item. And an
/// observation goes **stale** the moment the app re-renders, which a menu
/// opening reliably causes; `neo-ax` answers `stale reference: take a new
/// observation`, so each attempt re-observes first. That is the documented
/// contract, not a workaround.
async fn new_document(ax: &AxHandle, app: AppSel) -> Result<Vec<String>, ProbeError> {
    const PATHS: [&[&str]; 4] = [
        &["File", "New", "Spreadsheet"],
        &["File", "New", "Text Document"],
        &["File", "New"],
        &["File", "New Window"],
    ];
    let mut last = String::new();
    for attempt in 0..2 {
        for path in PATHS {
            // A fresh observation helps `SelectMenu` resolve against the
            // current generation, but it must not gate the attempt: an app
            // whose last document was closed has **no focused window**, so
            // `table` fails — and opening a new document is exactly the fix
            // for that. Gating on it made every TextEdit case fail with an
            // empty reason once an earlier case had closed the window.
            let _ = ax.table(&app).await;
            let owned: Vec<String> = path.iter().map(|part| (*part).to_owned()).collect();
            match ax
                .act(&AxAction::SelectMenu {
                    path: owned.clone(),
                })
                .await
            {
                Ok(_) => return Ok(owned),
                Err(error) => last = error.to_string(),
            }
        }
        if attempt == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        }
    }
    Err(ProbeError::Ax(format!(
        "no New-document menu item could be used: {last}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fixture_round_trips_through_the_config() {
        for fixture in [
            Fixture::FreshDocument {
                app: "com.apple.TextEdit".into(),
            },
            Fixture::Activate {
                app: "Numbers".into(),
            },
        ] {
            let config = AgentConfig {
                data: fixture.config(),
            };
            assert_eq!(Fixture::from_config(&config), Some(fixture));
        }
    }

    #[test]
    fn a_config_without_a_fixture_has_none() {
        let config = AgentConfig {
            data: json!({ "probe": { "kind": "surface", "app": "TextEdit" } }),
        };
        assert_eq!(Fixture::from_config(&config), None);
    }
}
