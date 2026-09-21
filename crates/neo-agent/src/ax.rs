//! `neo ax …` as a library: the direct accessibility surface.
//!
//! What Jev sees, and one action at a time by hand. Every mutating call goes
//! through the same freshness guard the navigator uses, so a stale index is
//! refused here exactly as it is inside a run.
//!
//! # Why this is not in the CLI
//!
//! It was, and that meant the only way to list applications, read an element
//! table or press one row was to type `neo ax …` in a terminal. A desktop
//! window cannot shell out to itself, so the whole capability was invisible
//! to every front end but one. [`ax`] returns [`AxResponse`] instead of
//! printing it; the CLI's job shrinks to mapping its `clap` enum onto
//! [`AxRequest`] and printing what comes back.

use std::sync::Arc;

use neo_ax::{AppSel, AxHandle, ElementTable, Freshness, Guard, Key};
use neo_core::{AppEvent, CoreError, NavStepKind, RunId};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::runtime::Runtime;

/// What `neo ax …` asks for (01 §CLI surface).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AxRequest {
    /// Is this binary a trusted accessibility client?
    Trusted,
    /// Every running application with a pid.
    Apps,
    /// The element table Jev would be shown.
    Table { app: String },
    /// Press one row of the current table.
    Press { app: String, index: u16 },
    /// Put text into one row of the current table.
    Set {
        app: String,
        index: u16,
        text: String,
    },
    /// Invoke a menu path, levels separated by `›` or `>`.
    Menu { app: String, path: String },
    /// Type into whatever is focused.
    Type { app: String, text: String },
    /// Press one named key in whatever is focused.
    Key { app: String, key: String },
}

impl AxRequest {
    /// What this request will do to the screen, or `None` if it only looks.
    ///
    /// The phrasing is what a refused run is shown, so it reads as the
    /// sentence in the refusal: "neo-tui is running `press row 3 of Finder`".
    #[must_use]
    pub fn acts(&self) -> Option<String> {
        match self {
            AxRequest::Trusted | AxRequest::Apps | AxRequest::Table { .. } => None,
            AxRequest::Press { app, index } => Some(format!("press row {index} of {app}")),
            AxRequest::Set { app, index, .. } => Some(format!("type into row {index} of {app}")),
            AxRequest::Menu { app, path } => Some(format!("invoke {path} in {app}")),
            AxRequest::Type { app, .. } => Some(format!("type into {app}")),
            AxRequest::Key { app, key } => Some(format!("press {key} in {app}")),
        }
    }
}

/// The three shapes an [`AxRequest`] answers with.
///
/// Untagged on purpose: the CLI printed these values bare, and a refactor
/// that wrapped them in a discriminant would change what every script reading
/// `neo ax` sees. The variants stay distinguishable because the two object
/// shapes this crate owns refuse unknown fields, and an element table carries
/// a `generation` neither of them has.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AxResponse {
    /// `neo ax trusted`.
    Trust(TrustReport),
    /// `neo ax apps`.
    Apps(Vec<neo_ax::AppInfo>),
    /// Every mutating request: press, set, type, key, menu.
    Acted(ActReport),
    /// `neo ax table`. Boxed: a table is two orders of magnitude bigger than
    /// any other variant, and an unboxed one would set the size of all four.
    Table(Box<ElementTable>),
}

/// Whether this binary may read and drive other applications, and where the
/// user goes to change that.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct TrustReport {
    pub trusted: bool,
    /// Posting a synthetic key event is a separate grant from reading the
    /// tree, and a run that can read but not type fails in a confusing way.
    pub can_post_events: bool,
    /// Deep link to the pane the user flips the switch in.
    pub settings: String,
}

/// What one hand-driven action did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ActReport {
    pub performed: bool,
    /// `Debug` of [`neo_ax::Method`] — `Ax`, `Cg` — because that is what the
    /// CLI has always printed and scripts parse it.
    pub method: String,
    /// Whether the target had to be found again by fingerprint first.
    pub relocated: bool,
    pub summary: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AxError {
    #[error(
        "this binary is not trusted for Accessibility; grant it in System Settings › Privacy & Security › Accessibility (under `cargo run` that is your terminal)"
    )]
    NotTrusted,
    /// Another run already has the keyboard; acting would type into its
    /// window. Reads and table dumps never reach this — they observe.
    #[error(transparent)]
    ScreenBusy(#[from] crate::screen::ScreenBusy),
    #[error("the accessibility actor could not start")]
    Spawn(#[source] neo_ax::AxError),
    #[error("no row {0} in that table")]
    NoRow(u16),
    /// The observation the index belongs to no longer describes the screen.
    /// Acting anyway would press whatever moved into that position.
    #[error("the surface is stale: {0}")]
    Stale(String),
    #[error("`{0}` is not one of the offered keys")]
    UnknownKey(String),
    #[error(transparent)]
    Ax(#[from] neo_ax::AxError),
    /// Stopped on purpose, not broken.
    #[error(transparent)]
    Cancelled(#[from] CoreError),
}

/// Answer one accessibility request.
///
/// `run` names this call in the [`AppEvent::NavStep`] events it publishes, so
/// a front end can show `activated Mail · pid 431` while the table is being
/// walked; `cancel` stops it between stages. A stopped call releases every
/// modifier the actor is holding, because the alternative is leaving a
/// Command key down in somebody else's application.
///
/// # Errors
///
/// Fails when Accessibility is not granted, when the actor will not start,
/// when the row does not exist, when the surface moved under the index, when
/// the key is not one of the offered names, or when `cancel` fires.
pub async fn ax(
    runtime: &Arc<Runtime>,
    request: AxRequest,
    run: RunId,
    cancel: &CancellationToken,
) -> Result<AxResponse, AxError> {
    // Asking whether we are trusted must work while we are not: it is the
    // question whose answer tells the user to go and grant it.
    if matches!(request, AxRequest::Trusted) {
        return Ok(AxResponse::Trust(TrustReport {
            trusted: AxHandle::trusted(),
            can_post_events: neo_ax::can_post_events(),
            settings: neo_ax::ACCESSIBILITY_SETTINGS_URL.to_owned(),
        }));
    }
    if !AxHandle::trusted() {
        return Err(AxError::NotTrusted);
    }
    stop_check(cancel)?;

    // Acting takes the keyboard; observing does not. A `Table` dump that
    // races another run's action was already stale when it rendered, which
    // `neo-ax`'s freshness guard reports — locking it would serialise the
    // one call a user makes *to find out* what is going on.
    let _screen = match request.acts() {
        Some(what) => Some(runtime.acquire_screen(run, what)?),
        None => None,
    };

    let handle = AxHandle::spawn().map_err(AxError::Spawn)?;
    let result = dispatch(runtime, &handle, request, run, cancel).await;
    if result.is_err() {
        // Includes the cancelled path: whatever the actor was holding down
        // goes up before this returns.
        handle.stop();
    }
    result
}

async fn dispatch(
    runtime: &Arc<Runtime>,
    ax: &AxHandle,
    request: AxRequest,
    run: RunId,
    cancel: &CancellationToken,
) -> Result<AxResponse, AxError> {
    match request {
        AxRequest::Trusted => unreachable!("answered before the actor starts"),
        AxRequest::Apps => Ok(AxResponse::Apps(ax.apps().await?)),
        AxRequest::Table { app } => {
            let table = observe_for(runtime, ax, &app, run, cancel).await?;
            Ok(AxResponse::Table(Box::new(table)))
        }
        AxRequest::Press { app, index } => {
            let table = observe_for(runtime, ax, &app, run, cancel).await?;
            let (guard, target) = row(&table, index)?;
            act_guarded(ax, guard, neo_ax::AxAction::Press { target }, cancel).await
        }
        AxRequest::Set { app, index, text } => {
            let table = observe_for(runtime, ax, &app, run, cancel).await?;
            let (guard, target) = row(&table, index)?;
            act_guarded(
                ax,
                guard,
                neo_ax::AxAction::SetValue { target, text },
                cancel,
            )
            .await
        }
        AxRequest::Type { app, text } => {
            let table = observe_for(runtime, ax, &app, run, cancel).await?;
            act_guarded(
                ax,
                table.guard_window(),
                neo_ax::AxAction::TypeText { text },
                cancel,
            )
            .await
        }
        AxRequest::Key { app, key } => {
            let key = key_named(&key).ok_or(AxError::UnknownKey(key))?;
            let table = observe_for(runtime, ax, &app, run, cancel).await?;
            act_guarded(
                ax,
                table.guard_window(),
                neo_ax::AxAction::Key {
                    key,
                    modifiers: Vec::new(),
                },
                cancel,
            )
            .await
        }
        AxRequest::Menu { app, path } => {
            let table = observe_for(runtime, ax, &app, run, cancel).await?;
            act_guarded(
                ax,
                table.guard_window(),
                neo_ax::AxAction::SelectMenu {
                    path: menu_path(&path),
                },
                cancel,
            )
            .await
        }
    }
}

/// Activate the app and take the table the indices belong to.
///
/// Activation is not politeness: a backgrounded application exposes little
/// more than its menu bar, so an index read from a table taken while it was
/// behind another window names nothing.
async fn observe_for(
    runtime: &Arc<Runtime>,
    ax: &AxHandle,
    app: &str,
    run: RunId,
    cancel: &CancellationToken,
) -> Result<ElementTable, AxError> {
    let info = ax.activate(&app_selector(app)).await?;
    note(
        runtime,
        run,
        format!("activated                 {} · pid {}", info.name, info.pid),
    );
    stop_check(cancel)?;
    Ok(ax.table(&AppSel::Pid(info.pid)).await?)
}

/// The guard and the element reference for one row, or the failure that says
/// the row is not there.
fn row(table: &ElementTable, index: u16) -> Result<(Guard, neo_ax::Ref), AxError> {
    let guard = table.guard_for(index).ok_or(AxError::NoRow(index))?;
    let target = table
        .element(index)
        .map(neo_ax::Element::reference)
        .ok_or(AxError::NoRow(index))?;
    Ok((guard, target))
}

/// Check the surface still is the one the index was read from, then act.
async fn act_guarded(
    ax: &AxHandle,
    guard: Guard,
    action: neo_ax::AxAction,
    cancel: &CancellationToken,
) -> Result<AxResponse, AxError> {
    if let Freshness::Stale(reason) = ax.guard(&guard).await? {
        return Err(AxError::Stale(format!("{reason:?}")));
    }
    // The last moment a stop costs nothing: after this the action is out in
    // the world.
    stop_check(cancel)?;
    let outcome = ax.act(&action).await?;
    Ok(AxResponse::Acted(ActReport {
        performed: outcome.performed,
        method: format!("{:?}", outcome.method),
        relocated: outcome.relocated,
        summary: outcome.summary,
    }))
}

/// A bundle id (contains a dot and no space), a pid (all digits), or a name.
#[must_use]
pub fn app_selector(app: &str) -> AppSel {
    if let Ok(pid) = app.parse::<i32>() {
        return AppSel::Pid(pid);
    }
    if app.contains('.') && !app.contains(' ') {
        return AppSel::BundleId(app.to_owned());
    }
    AppSel::Name(app.to_owned())
}

/// The keys a caller may name.
///
/// Deliberately short: this is a hand-driving surface, not a keyboard
/// emulator, and every name here maps to a key the actor can post without a
/// layout lookup. Both spellings of the three keys people disagree about are
/// accepted, because `esc` and `enter` are what a hand types.
#[must_use]
pub fn key_named(name: &str) -> Option<Key> {
    match name.to_ascii_lowercase().as_str() {
        "return" | "enter" => Some(Key::Return),
        "escape" | "esc" => Some(Key::Escape),
        "tab" => Some(Key::Tab),
        "space" => Some(Key::Space),
        "delete" | "backspace" => Some(Key::Delete),
        "up" => Some(Key::Up),
        "down" => Some(Key::Down),
        "left" => Some(Key::Left),
        "right" => Some(Key::Right),
        _ => None,
    }
}

/// `Format › Make Plain Text` as the levels the actor walks.
fn menu_path(path: &str) -> Vec<String> {
    path.split(['›', '>'])
        .map(|level| level.trim().to_owned())
        .filter(|level| !level.is_empty())
        .collect()
}

/// One line of progress, for a front end that is watching this call.
fn note(runtime: &Arc<Runtime>, run: RunId, line: String) {
    runtime.publish(AppEvent::NavStep {
        run,
        step: 0,
        line,
        kind: NavStepKind::Launch,
    });
}

fn stop_check(cancel: &CancellationToken) -> Result<(), AxError> {
    if cancel.is_cancelled() {
        return Err(AxError::Cancelled(CoreError::Cancelled));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_is_selected_by_pid_bundle_id_or_name() {
        assert_eq!(app_selector("482"), AppSel::Pid(482));
        assert_eq!(
            app_selector("com.apple.TextEdit"),
            AppSel::BundleId("com.apple.TextEdit".into())
        );
        // A name with a space is a name even when it contains a dot.
        assert_eq!(
            app_selector("Diffusion Studio"),
            AppSel::Name("Diffusion Studio".into())
        );
    }

    /// Both spellings of every key people disagree about, and nothing else:
    /// an unknown name has to fail rather than silently press something.
    #[test]
    fn the_key_table_accepts_both_spellings_and_refuses_the_rest() {
        for (name, key) in [
            ("return", Key::Return),
            ("ENTER", Key::Return),
            ("escape", Key::Escape),
            ("Esc", Key::Escape),
            ("tab", Key::Tab),
            ("space", Key::Space),
            ("delete", Key::Delete),
            ("backspace", Key::Delete),
            ("up", Key::Up),
            ("down", Key::Down),
            ("left", Key::Left),
            ("right", Key::Right),
        ] {
            assert_eq!(key_named(name), Some(key), "{name}");
        }
        assert_eq!(key_named("f13"), None);
        assert_eq!(key_named(""), None);
        assert_eq!(key_named("command"), None);
    }

    #[test]
    fn a_menu_path_splits_on_either_separator() {
        assert_eq!(
            menu_path("Format › Make Plain Text"),
            vec!["Format".to_owned(), "Make Plain Text".to_owned()]
        );
        assert_eq!(
            menu_path("File > Save >"),
            vec!["File".to_owned(), "Save".to_owned()]
        );
    }

    /// The CLI prints these values bare, so the wire shape is the contract:
    /// each variant must serialise to exactly the object it did before and
    /// come back as itself.
    #[test]
    fn every_response_survives_a_round_trip_untagged() {
        let trust = AxResponse::Trust(TrustReport {
            trusted: true,
            can_post_events: false,
            settings: neo_ax::ACCESSIBILITY_SETTINGS_URL.to_owned(),
        });
        let json = serde_json::to_value(&trust).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(json["trusted"], true);
        assert_eq!(json["can_post_events"], false);
        assert_eq!(json["settings"], neo_ax::ACCESSIBILITY_SETTINGS_URL);
        assert_eq!(
            serde_json::from_value::<AxResponse>(json).unwrap_or_else(|error| panic!("{error}")),
            trust
        );

        let acted = AxResponse::Acted(ActReport {
            performed: true,
            method: "Ax".to_owned(),
            relocated: false,
            summary: "pressed Send".to_owned(),
        });
        let json = serde_json::to_value(&acted).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(json["performed"], true);
        assert_eq!(json["method"], "Ax");
        assert_eq!(
            serde_json::from_value::<AxResponse>(json).unwrap_or_else(|error| panic!("{error}")),
            acted
        );

        let apps = AxResponse::Apps(vec![neo_ax::AppInfo {
            name: "Mail".to_owned(),
            bundle_id: Some("com.apple.mail".to_owned()),
            pid: 431,
            frontmost: true,
        }]);
        let json = serde_json::to_value(&apps).unwrap_or_else(|error| panic!("{error}"));
        assert!(json.is_array());
        assert_eq!(
            serde_json::from_value::<AxResponse>(json).unwrap_or_else(|error| panic!("{error}")),
            apps
        );

        // `WindowInfo`'s fingerprint is crate-private to `neo-ax`, so the
        // table is built the way a consumer sees it: off the wire.
        let wire = serde_json::json!({
            "generation": 3,
            "app": { "name": "Mail", "bundle_id": null, "pid": 431, "frontmost": true },
            "window": { "title": "Inbox", "modal": false },
            "text": "Inbox",
            "elements": [],
            "controls": ["WAIT"],
            "truncated": false,
        });
        // A table must not be mistaken for an act report, nor the reverse —
        // that is the whole risk an untagged enum takes on.
        let table = serde_json::from_value::<AxResponse>(wire.clone())
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(matches!(table, AxResponse::Table(_)));
        assert_eq!(
            serde_json::to_value(&table).unwrap_or_else(|error| panic!("{error}")),
            wire
        );
    }
}
