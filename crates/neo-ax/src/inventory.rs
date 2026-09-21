//! Which applications this machine can actually open.
//!
//! Two callers need the same answer and used to have neither. The agent is
//! told what it may operate (A23), and until it is told, a name in a task —
//! "use degen-paint" — resolves to nothing and the model guesses a website.
//! `neo-eval` decides whether a case can run at all, and its only test was a
//! macOS bundle layout, so on Linux every app case skipped
//! (17 §L3: *"`neo-eval` app detection by desktop entry … still to run"*).
//!
//! The list is **installed applications**, not running ones. That is the
//! question both callers ask: `backend::atspi::apps::list` answers "what has a
//! window right now", which is the wrong set for "can this machine open
//! degen-paint" — an app that is not running is exactly the case that needs
//! launching.
//!
//! Identity is the platform's launch key and nothing else: the desktop-entry
//! id on Linux (which is also the Wayland `app_id` and, for GTK and Tauri
//! apps, the macOS bundle id), the bundle name on macOS (what
//! `NSWorkspace::fullPathForApplication` takes). No plist is parsed and no
//! process is spawned to build it.

use std::path::PathBuf;

/// One application this machine can open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledApp {
    /// What [`crate::AppSel::BundleId`] and `neo app` name it by: the
    /// desktop-entry id on Linux, the bundle name on macOS.
    pub id: String,
    /// The human name, for a sentence the model or a report reads.
    pub name: String,
    /// The `.desktop` file or the `.app` bundle it was found at.
    pub path: PathBuf,
}

/// Strip everything a human varies: case, spaces, dots, hyphens.
///
/// This is what lets the three spellings of one product meet — `degen-paint`
/// as typed, `degen-paint Studio` as named, `dev.degenpaint.studio` as
/// launched all normalise to a string containing `degenpaint`. Matching on
/// the raw text matches none of them to each other.
fn normalize(text: &str) -> String {
    text.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Every application this machine can open, by name.
#[must_use]
pub fn inventory() -> Vec<InstalledApp> {
    let mut apps = platform::inventory();
    apps.sort_by(|left, right| left.name.cmp(&right.name));
    apps.dedup_by(|left, right| left.id == right.id);
    apps
}

/// The application `selector` names, if this machine has it.
///
/// Tried in falling order of confidence, so a precise selector is never
/// beaten by a loose one: the exact id, then the exact name, then a
/// normalised containment either way round. The last rule is the one a task
/// needs — a user writes "degen-paint", the entry is `dev.degenpaint.studio`.
#[must_use]
pub fn lookup(selector: &str) -> Option<InstalledApp> {
    let apps = inventory();
    let wanted = normalize(selector);
    if wanted.is_empty() {
        return None;
    }
    apps.iter()
        .find(|app| app.id == selector)
        .or_else(|| apps.iter().find(|app| app.name == selector))
        .or_else(|| apps.iter().find(|app| normalize(&app.id) == wanted))
        .or_else(|| apps.iter().find(|app| normalize(&app.name) == wanted))
        .or_else(|| {
            apps.iter().find(|app| {
                normalize(&app.id).contains(&wanted) || normalize(&app.name).contains(&wanted)
            })
        })
        .cloned()
}

#[cfg(target_os = "linux")]
mod platform {
    use super::InstalledApp;

    /// The desktop entries, which is the same set `launch` can spawn: an
    /// entry this filters out is one the navigator would refuse anyway.
    pub(super) fn inventory() -> Vec<InstalledApp> {
        crate::backend::atspi::apps::entries()
            .into_values()
            .filter(|entry| !entry.terminal)
            .map(|entry| InstalledApp {
                path: entry.path.clone(),
                id: entry.id,
                name: entry.name,
            })
            .collect()
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::path::PathBuf;

    use super::InstalledApp;

    /// Where macOS keeps applications, in the order `NSWorkspace` resolves
    /// them.
    fn roots() -> Vec<PathBuf> {
        let mut roots = vec![
            PathBuf::from("/Applications"),
            PathBuf::from("/Applications/Utilities"),
            PathBuf::from("/System/Applications"),
            PathBuf::from("/System/Applications/Utilities"),
        ];
        if let Some(home) = std::env::var_os("HOME") {
            roots.push(PathBuf::from(home).join("Applications"));
        }
        roots
    }

    pub(super) fn inventory() -> Vec<InstalledApp> {
        let mut apps = Vec::new();
        for root in roots() {
            let Ok(read) = std::fs::read_dir(&root) else {
                continue;
            };
            for file in read.flatten() {
                let path = file.path();
                if path.extension().and_then(|e| e.to_str()) != Some("app") {
                    continue;
                }
                // The same test the bundle checks elsewhere use: a directory
                // named `.app` with no executable inside is not an app.
                if !path.join("Contents/MacOS").is_dir() {
                    continue;
                }
                let Some(name) = path.file_stem().and_then(|n| n.to_str()) else {
                    continue;
                };
                apps.push(InstalledApp {
                    id: name.to_owned(),
                    name: name.to_owned(),
                    path,
                });
            }
        }
        apps
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    use super::InstalledApp;

    /// A platform with no backend can open nothing, and says so by being
    /// empty rather than by failing: callers already render "not installed".
    pub(super) fn inventory() -> Vec<InstalledApp> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, name: &str) -> InstalledApp {
        InstalledApp {
            id: id.to_owned(),
            name: name.to_owned(),
            path: PathBuf::from("/nowhere"),
        }
    }

    /// The spelling a person actually types. This is the case the baseline
    /// run failed on: "degen-paint" reached no application, so the model
    /// treated it as a domain name and browsed for it.
    #[test]
    fn a_hyphenated_product_name_finds_its_reverse_dns_entry() {
        let apps = [app("dev.degenpaint.studio", "degen-paint Studio")];
        let wanted = normalize("degen-paint");

        assert!(
            apps.iter()
                .any(|found| normalize(&found.id).contains(&wanted)),
            "`degen-paint` has to reach `dev.degenpaint.studio`"
        );
    }

    /// Normalisation is only allowed to erase punctuation and case. If it
    /// erased more, two unrelated apps would collide and `lookup` would
    /// return whichever sorted first.
    #[test]
    fn normalizing_keeps_apps_apart() {
        assert_eq!(normalize("degen-paint Studio"), "degenpaintstudio");
        assert_eq!(normalize("dev.degenpaint.studio"), "devdegenpaintstudio");
        assert_ne!(normalize("Numbers"), normalize("LibreOffice"));
    }

    /// An empty or punctuation-only selector must not match the first app on
    /// the machine: every normalised string contains the empty string.
    #[test]
    fn an_empty_selector_matches_nothing() {
        assert_eq!(lookup(""), None);
        assert_eq!(lookup("  -.- "), None);
    }
}
