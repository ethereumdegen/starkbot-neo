//! Applications: identity, the list, and launching one.
//!
//! Identity is the Wayland `app_id` (17 §3.2). For GTK and Tauri apps it is
//! the desktop-entry id and the macOS bundle id — `dev.degenpaint.studio`,
//! `org.gnome.Nautilus` — which is why [`crate::AppSel::BundleId`], the deny
//! list and every pack hint carry over from macOS unchanged. Verified on
//! this machine: Hyprland reports `class="org.gnome.Nautilus"` for Nautilus
//! and `class="dev.degenpaint.studio"` for degen-paint's Studio.
//!
//! The list is the compositor's window list, joined to the desktop entry for
//! a human name. It is *not* the process table: an app with no window is not
//! something a navigator can drive, exactly as `NSWorkspace`'s regular
//! applications exclude agents.
//!
//! Launching reads the `.desktop` entry, strips the field codes and spawns
//! the program directly — not a shell, not `gtk-launch`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::wm::{Client, WindowManager};
use crate::types::AppInfo;

/// A parsed `[Desktop Entry]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DesktopEntry {
    /// The entry id: the file name without `.desktop`, which is the value
    /// GTK and Tauri also use as the Wayland `app_id`.
    pub id: String,
    /// `Name`.
    pub name: String,
    /// `Exec`, verbatim, field codes included.
    pub exec: String,
    /// `Terminal=true`: the entry wants a terminal emulator, which this
    /// crate will not open (P3).
    pub terminal: bool,
    /// The `.desktop` file this was read from, for an inventory row that
    /// says where the answer came from.
    pub path: PathBuf,
}

/// Every directory XDG says desktop entries live in, most specific first.
fn entry_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("XDG_DATA_HOME") {
        dirs.push(PathBuf::from(home).join("applications"));
    } else if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/applications"));
    }
    let system =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".to_owned());
    for dir in system.split(':').filter(|d| !d.is_empty()) {
        dirs.push(PathBuf::from(dir).join("applications"));
    }
    dirs
}

/// Parse one `.desktop` file's `[Desktop Entry]` group.
fn parse_entry(path: &Path) -> Option<DesktopEntry> {
    let id = path.file_stem()?.to_str()?.to_owned();
    let body = std::fs::read_to_string(path).ok()?;
    let mut in_group = false;
    let mut name = None;
    let mut exec = None;
    let mut terminal = false;
    let mut kind = None;
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_group {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        // Localised keys (`Name[de]`) are skipped: the navigator's selectors
        // and the packs are written against the unlocalised name.
        match key.trim() {
            "Name" => name = Some(value.trim().to_owned()),
            "Exec" => exec = Some(value.trim().to_owned()),
            "Terminal" => terminal = value.trim() == "true",
            "Type" => kind = Some(value.trim().to_owned()),
            _ => {}
        }
    }
    if kind.as_deref() != Some("Application") {
        return None;
    }
    Some(DesktopEntry {
        name: name.unwrap_or_else(|| id.clone()),
        exec: exec?,
        terminal,
        path: path.to_path_buf(),
        id,
    })
}

/// Every application entry on this system, keyed by id, nearest first.
pub(crate) fn entries() -> BTreeMap<String, DesktopEntry> {
    let mut out = BTreeMap::new();
    for dir in entry_dirs() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for file in read.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let Some(entry) = parse_entry(&path) else {
                continue;
            };
            // Earlier directories win: `~/.local/share` overrides `/usr`.
            out.entry(entry.id.clone()).or_insert(entry);
        }
    }
    out
}

/// The entry an `app_id` or a name names.
pub(crate) fn find_entry(selector: &str) -> Option<DesktopEntry> {
    let all = entries();
    if let Some(exact) = all.get(selector) {
        return Some(exact.clone());
    }
    let lower = selector.to_lowercase();
    all.values()
        .find(|e| e.id.eq_ignore_ascii_case(selector))
        .or_else(|| all.values().find(|e| e.name.eq_ignore_ascii_case(selector)))
        .or_else(|| {
            all.values()
                .find(|e| e.name.to_lowercase().contains(&lower))
        })
        .cloned()
}

/// Split an `Exec` into argv, dropping the field codes.
///
/// `%f %F %u %U %i %c %k %d %D %n %N %v %m` are the desktop-entry
/// specification's placeholders for files, URLs, the icon and the entry's
/// own name. Neo launches an app with no document, so every one of them
/// resolves to nothing. `%%` is a literal percent.
pub(crate) fn exec_argv(exec: &str) -> Vec<String> {
    let mut argv = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = exec.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' if quote.is_some() => {
                if let Some(next) = chars.next() {
                    current.push(next);
                    started = true;
                }
            }
            '"' | '\'' if quote == Some(ch) => quote = None,
            '"' | '\'' if quote.is_none() => {
                quote = Some(ch);
                started = true;
            }
            '%' if quote.is_none() => match chars.next() {
                Some('%') => {
                    current.push('%');
                    started = true;
                }
                // A field code on its own is dropped whole; one glued to
                // other text (`--file=%f`) leaves the text behind.
                Some(_) => {}
                None => {}
            },
            c if c.is_whitespace() && quote.is_none() => {
                if started {
                    argv.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        argv.push(current);
    }
    argv.retain(|word| !word.is_empty());
    argv
}

/// Start the program a desktop entry names.
///
/// Directly: no shell, no `gtk-launch`, no `systemd-run`. Standard streams
/// are detached so the child does not write into the agent's terminal and
/// does not die with it.
pub(crate) fn launch(entry: &DesktopEntry) -> bool {
    if entry.terminal {
        // `Terminal=true` means "run this in a terminal emulator", which is
        // the one thing this crate refuses to open (P3, the deny list).
        return false;
    }
    let argv = exec_argv(&entry.exec);
    let Some((program, args)) = argv.split_first() else {
        return false;
    };
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

/// The running applications: one row per process that owns a window.
///
/// `frontmost` is the compositor's active window, which is Wayland's whole
/// answer to macOS's menu-bar owner (17 §9).
pub(crate) fn running(wm: &dyn WindowManager) -> Vec<AppInfo> {
    let clients = wm.clients().unwrap_or_default();
    let active = wm.frontmost().ok().flatten();
    let entries = entries();
    let mut by_pid: BTreeMap<i32, AppInfo> = BTreeMap::new();
    for client in &clients {
        let frontmost = active.as_ref().is_some_and(|a| a.address == client.address);
        let name = entries
            .get(&client.app_id)
            .map(|e| e.name.clone())
            .unwrap_or_else(|| human_name(client));
        let info = AppInfo {
            name,
            bundle_id: (!client.app_id.is_empty()).then(|| client.app_id.clone()),
            pid: client.pid,
            frontmost,
        };
        match by_pid.get_mut(&client.pid) {
            // One process, several windows: the active one decides what the
            // app is called and whether it is frontmost. LibreOffice shows
            // up as `soffice` and `libreoffice-calc` at once.
            Some(existing) if frontmost || existing.bundle_id.is_none() => *existing = info,
            Some(_) => {}
            None => {
                by_pid.insert(client.pid, info);
            }
        }
    }
    by_pid.into_values().collect()
}

/// Something readable for an app with no desktop entry.
fn human_name(client: &Client) -> String {
    if !client.app_id.is_empty() {
        return client.app_id.clone();
    }
    client.title.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The field codes of the desktop-entry specification are placeholders
    /// for a document, and Neo launches an app with none. Leaving one in
    /// argv makes the app open a file literally called `%U`.
    #[test]
    fn exec_field_codes_are_stripped() {
        assert_eq!(
            exec_argv("nautilus --new-window %U"),
            ["nautilus", "--new-window"]
        );
        assert_eq!(exec_argv("soffice --calc %U"), ["soffice", "--calc"]);
        assert_eq!(exec_argv("app %f %F %u %U %i %c %k"), ["app"]);
    }

    /// An `Exec` is not a shell command line, but it does carry the
    /// specification's quoting, and a path with a space must survive it.
    #[test]
    fn exec_quoting_keeps_one_argument() {
        assert_eq!(
            exec_argv("\"/opt/My App/run\" --flag"),
            ["/opt/My App/run", "--flag"]
        );
        assert_eq!(exec_argv("app --title=100%%"), ["app", "--title=100%"]);
    }
}
