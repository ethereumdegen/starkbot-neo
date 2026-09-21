//! Where Starkbot Neo keeps its files.
//!
//! One module, because the answer differs by platform and two crates that
//! disagree open two different stores: the CLI, the TUI and the desktop shell
//! must all land on the same `neo.db`.
//!
//! macOS keeps the Apple layout under the bundle id — `~/Library/Application
//! Support/com.starkbot.neo`, `~/Library/Logs/…`, `~/Library/Caches/…`.
//! Everywhere else the XDG base directories apply, under `starkbot-neo`:
//! `$XDG_DATA_HOME` (default `~/.local/share`), `$XDG_STATE_HOME`
//! (`~/.local/state`) for logs, `$XDG_CACHE_HOME` (`~/.cache`) and
//! `$XDG_CONFIG_HOME` (`~/.config`).
//!
//! Every function answers `None` when there is no absolute `$HOME` to build
//! on, rather than inventing a relative path that would put a database
//! wherever the process happened to be started.

#[cfg(not(target_os = "macos"))]
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The macOS bundle id, which is also the directory name under `~/Library`.
pub const BUNDLE_ID: &str = "com.starkbot.neo";

/// The directory name under each XDG base directory.
#[cfg(not(target_os = "macos"))]
const XDG_DIR: &str = "starkbot-neo";

/// The store, the backups and anything else that must survive a reinstall.
#[must_use]
pub fn data_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Some(home()?.join("Library/Application Support").join(BUNDLE_ID))
    }
    #[cfg(not(target_os = "macos"))]
    {
        xdg(std::env::var_os("XDG_DATA_HOME"), ".local/share")
    }
}

/// The desktop shell's control socket, beside the store it belongs to.
///
/// Here rather than in either front end because the socket is a rendezvous
/// between two processes: the window binds it and `neo say` connects to it,
/// and a crate that spelled the name differently would report that no window
/// is open while one sits on screen. Taken as an argument rather than read
/// from [`data_dir`] so a `--data-dir` run reaches its own window rather than
/// the default one's.
#[must_use]
pub fn control_socket(data_dir: &Path) -> PathBuf {
    data_dir.join("control.sock")
}

/// Logs and other state worth keeping but not worth backing up.
#[must_use]
pub fn state_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Some(home()?.join("Library/Logs").join(BUNDLE_ID))
    }
    #[cfg(not(target_os = "macos"))]
    {
        xdg(std::env::var_os("XDG_STATE_HOME"), ".local/state")
    }
}

/// Anything that can be thrown away and regenerated.
#[must_use]
pub fn cache_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Some(home()?.join("Library/Caches").join(BUNDLE_ID))
    }
    #[cfg(not(target_os = "macos"))]
    {
        xdg(std::env::var_os("XDG_CACHE_HOME"), ".cache")
    }
}

/// User-editable configuration. macOS has no separate place for it —
/// `~/Library/Preferences` holds plists a program did not hand-write — so it
/// shares the data directory there.
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        data_dir()
    }
    #[cfg(not(target_os = "macos"))]
    {
        xdg(std::env::var_os("XDG_CONFIG_HOME"), ".config")
    }
}

fn home() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    home.is_absolute().then_some(home)
}

/// An XDG base directory, honouring the variable only when it is absolute:
/// the specification says a relative value is to be ignored as if unset.
#[cfg(not(target_os = "macos"))]
fn xdg(variable: Option<OsString>, fallback: &str) -> Option<PathBuf> {
    if let Some(base) = variable.map(PathBuf::from)
        && base.is_absolute()
    {
        return Some(base.join(XDG_DIR));
    }
    Some(home()?.join(fallback).join(XDG_DIR))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three must be distinct on every platform: logs that land in the
    /// data directory get backed up and restored with the store, and a cache
    /// that lands there survives the reinstall meant to clear it.
    #[test]
    fn data_state_and_cache_are_three_different_absolute_places() {
        let resolved: Vec<PathBuf> = [data_dir(), state_dir(), cache_dir()]
            .into_iter()
            .flatten()
            .collect();
        assert_eq!(resolved.len(), 3, "HOME is set when tests run");
        for (index, one) in resolved.iter().enumerate() {
            assert!(one.is_absolute(), "{} is relative", one.display());
            for other in &resolved[index + 1..] {
                assert_ne!(one, other);
            }
        }
    }

    /// Honouring a relative `$XDG_*` would put the store under the process's
    /// working directory, which for a desktop-launched app is `/`.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_relative_xdg_variable_falls_back_to_home() {
        assert_eq!(
            xdg(Some(OsString::from("relative/path")), ".local/share"),
            home().map(|home| home.join(".local/share").join(XDG_DIR)),
        );
        assert_eq!(
            xdg(Some(OsString::from("/srv/xdg")), ".local/share"),
            Some(PathBuf::from("/srv/xdg").join(XDG_DIR)),
        );
    }
}
