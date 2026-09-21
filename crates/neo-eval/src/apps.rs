//! Which target applications this machine actually has.
//!
//! An eval suite that fails because an app is not installed tells you nothing
//! about the agent. Every case declares the [`App`] it needs, and the runner
//! skips — loudly, with the reason — rather than reporting a failure that is
//! really an absent dependency.
//!
//! Detection is [`neo_ax::lookup`], which is the same question the navigator
//! asks when it is told to open something: a desktop entry on Linux, an
//! application bundle on macOS. This module used to test for
//! `Contents/MacOS`, so on Linux *every* app case skipped and the suite
//! silently measured nothing — the gap 17 §L3 records as *"`neo-eval` app
//! detection by desktop entry … still to run"*.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A target application an eval case can drive.
///
/// Serialisable because a front end lists the suite before running it, and a
/// window that shows "LibreOffice: not installed" needs the app itself, not a
/// pre-rendered string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum App {
    /// The managed Chrome the navigator launches (10).
    Chrome,
    /// Ships on every Mac, which is why A23 makes it the canonical smoke test.
    TextEdit,
    /// LibreOffice Calc — the spreadsheet path of A23.
    LibreOffice,
    /// Numbers, the Apple spreadsheet.
    Numbers,
    /// Diffusion Studio, the video editor of A12′.
    DiffusionStudio,
    /// Powermove, the Electron editor of A12′/A21.
    Powermove,
    /// Degen Media Studio, the generator of A13′.
    DegenMediaStudio,
    /// degen-paint's Studio: the fourth media app, and the one that works on
    /// Linux (A37). The target of the S8d smoke test.
    DegenPaint,
}

impl App {
    /// Every id this application is known by, most precise first.
    ///
    /// Two platforms name the same product differently and neither name is
    /// wrong: macOS resolves a bundle by its name (`Numbers`), Linux a
    /// desktop entry by its id (`libreoffice-calc`), and the reverse-DNS form
    /// (`dev.degenpaint.studio`) is what both the Wayland `app_id` and the
    /// macOS bundle id use for a GTK or Tauri app. Listing them together lets
    /// one case description run on either platform, which is what 17 §L3
    /// asks of the suite.
    #[must_use]
    pub const fn selectors(self) -> &'static [&'static str] {
        match self {
            Self::Chrome => &["com.google.Chrome", "Google Chrome", "chromium"],
            Self::TextEdit => &["com.apple.TextEdit", "TextEdit"],
            Self::LibreOffice => &["org.libreoffice.script", "LibreOffice", "libreoffice-calc"],
            Self::Numbers => &["Numbers", "Numbers Creator Studio"],
            Self::DiffusionStudio => &["Diffusion Studio", "diffusion-studio"],
            Self::Powermove => &["Powermove"],
            Self::DegenMediaStudio => &["Degen Media Studio", "DegenMediaStudio"],
            Self::DegenPaint => &["dev.degenpaint.studio", "degen-paint"],
        }
    }

    /// The name to hand `neo app` / the `app` action.
    ///
    /// Resolved against what is installed rather than hard-coded, because the
    /// selector has to be one this machine can act on: handing a macOS bundle
    /// id to the AT-SPI backend names nothing. Falls back to the canonical id
    /// when the app is absent, so a skip message still says what was looked
    /// for.
    #[must_use]
    pub fn selector(self) -> String {
        self.resolved()
            .map(|app| app.id)
            .unwrap_or_else(|| self.selectors()[0].to_owned())
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Chrome => "Google Chrome",
            Self::TextEdit => "TextEdit",
            Self::LibreOffice => "LibreOffice",
            Self::Numbers => "Numbers",
            Self::DiffusionStudio => "Diffusion Studio",
            Self::Powermove => "Powermove",
            Self::DegenMediaStudio => "Degen Media Studio",
            Self::DegenPaint => "degen-paint Studio",
        }
    }

    /// The installed application, by the first of its names this machine
    /// answers to.
    #[must_use]
    pub fn resolved(self) -> Option<neo_ax::InstalledApp> {
        self.selectors().iter().find_map(|id| neo_ax::lookup(id))
    }

    /// Where the application was found, for a report header and skip
    /// decisions.
    #[must_use]
    pub fn installed(self) -> Option<PathBuf> {
        self.resolved().map(|app| app.path)
    }
}

/// Every target this suite knows, in the order a report lists them.
pub const ALL: [App; 8] = [
    App::Chrome,
    App::TextEdit,
    App::LibreOffice,
    App::Numbers,
    App::DiffusionStudio,
    App::Powermove,
    App::DegenMediaStudio,
    App::DegenPaint,
];

/// Every target and whether it is present, for a report header and for skip
/// decisions.
#[must_use]
pub fn availability() -> Vec<(App, Option<PathBuf>)> {
    ALL.into_iter().map(|app| (app, app.installed())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TextEdit is the one target A23 guarantees, because it ships with the
    /// OS. If this fails on a Mac, no app case can run.
    #[cfg(target_os = "macos")]
    #[test]
    fn textedit_is_always_installed() {
        assert!(
            App::TextEdit.installed().is_some(),
            "TextEdit ships with macOS and is the canonical A23 target"
        );
    }

    /// A product installed under a different bundle name still has to be
    /// found — this is not hypothetical, it is how Numbers is installed on
    /// the author's machine.
    #[test]
    fn an_app_is_known_by_every_name_it_ships_under() {
        assert!(App::Numbers.selectors().contains(&"Numbers"));
        assert!(App::Numbers.selectors().contains(&"Numbers Creator Studio"));
    }

    /// The selector has to be something the platform under the test can
    /// actually act on, so an absent app still reports the canonical id
    /// rather than an empty string.
    #[test]
    fn an_absent_app_still_names_what_was_looked_for() {
        assert_eq!(App::Powermove.selectors()[0], "Powermove");
        assert!(!App::Powermove.selector().is_empty());
    }

    #[test]
    fn availability_covers_every_target() {
        let rows = availability();
        assert_eq!(rows.len(), ALL.len());
        assert!(rows.iter().any(|(app, _)| *app == App::DiffusionStudio));
        assert!(rows.iter().any(|(app, _)| *app == App::DegenPaint));
    }
}
