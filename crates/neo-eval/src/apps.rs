//! Which target applications this machine actually has.
//!
//! An eval suite that fails because an app is not installed tells you nothing
//! about the agent. Every case declares the [`App`] it needs, and the runner
//! skips — loudly, with the reason — rather than reporting a failure that is
//! really an absent dependency.

use std::path::{Path, PathBuf};

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
}

impl App {
    /// The name to hand `neo app` / the `app` action: a bundle id where one is
    /// stable, otherwise the display name.
    #[must_use]
    pub const fn selector(self) -> &'static str {
        match self {
            Self::Chrome => "com.google.Chrome",
            Self::TextEdit => "com.apple.TextEdit",
            Self::LibreOffice => "org.libreoffice.script",
            Self::Numbers => "Numbers",
            Self::DiffusionStudio => "Diffusion Studio",
            Self::Powermove => "Powermove",
            Self::DegenMediaStudio => "Degen Media Studio",
        }
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
        }
    }

    /// Where the bundle would be. Several of these ship under a name that is
    /// not the product name — on this machine Numbers is installed as
    /// *Numbers Creator Studio*, so the search covers name variants rather
    /// than one hard-coded path.
    #[must_use]
    pub fn candidates(self) -> Vec<PathBuf> {
        let names: &[&str] = match self {
            Self::Chrome => &["Google Chrome.app"],
            Self::TextEdit => &["TextEdit.app"],
            Self::LibreOffice => &["LibreOffice.app"],
            Self::Numbers => &["Numbers.app", "Numbers Creator Studio.app"],
            Self::DiffusionStudio => &["Diffusion Studio.app", "diffusion-studio.app"],
            Self::Powermove => &["Powermove.app"],
            Self::DegenMediaStudio => &["Degen Media Studio.app", "DegenMediaStudio.app"],
        };
        let roots = [
            PathBuf::from("/Applications"),
            PathBuf::from("/System/Applications"),
            PathBuf::from("/Applications/Utilities"),
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join("Applications"),
        ];
        roots
            .iter()
            .flat_map(|root| names.iter().map(move |name| root.join(name)))
            .collect()
    }

    /// The installed bundle, if there is one.
    #[must_use]
    pub fn installed(self) -> Option<PathBuf> {
        self.candidates().into_iter().find(|path| is_bundle(path))
    }
}

fn is_bundle(path: &Path) -> bool {
    path.join("Contents/MacOS").is_dir()
}

/// Every target and whether it is present, for a report header and for skip
/// decisions.
#[must_use]
pub fn availability() -> Vec<(App, Option<PathBuf>)> {
    [
        App::Chrome,
        App::TextEdit,
        App::LibreOffice,
        App::Numbers,
        App::DiffusionStudio,
        App::Powermove,
        App::DegenMediaStudio,
    ]
    .into_iter()
    .map(|app| (app, app.installed()))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TextEdit is the one target A23 guarantees, because it ships with the
    /// OS. If this fails on a Mac, no app case can run.
    ///
    /// macOS-only by its own terms: the Linux targets are desktop entries
    /// and arrive with L3.
    #[cfg(target_os = "macos")]
    #[test]
    fn textedit_is_always_installed() {
        assert!(
            App::TextEdit.installed().is_some(),
            "TextEdit ships with macOS and is the canonical A23 target"
        );
    }

    /// A product installed under a different bundle name still has to be
    /// found — this is not hypothetical, it is how Numbers is installed here.
    #[test]
    fn an_app_is_found_under_any_of_its_bundle_names() {
        let names: Vec<String> = App::Numbers
            .candidates()
            .iter()
            .filter_map(|path| path.file_name().map(|name| name.to_string_lossy().into()))
            .collect();
        assert!(names.iter().any(|name| name == "Numbers.app"));
        assert!(
            names
                .iter()
                .any(|name| name == "Numbers Creator Studio.app")
        );
    }

    #[test]
    fn availability_covers_every_target() {
        let rows = availability();
        assert_eq!(rows.len(), 7);
        assert!(rows.iter().any(|(app, _)| *app == App::DiffusionStudio));
    }
}
