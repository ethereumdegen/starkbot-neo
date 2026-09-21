//! The deny list, enforced at the lowest level (P3, A9).
//!
//! `neo-ax` refuses denied apps from every command, so no caller can bypass
//! it by reaching for a different entry point. Pure data and matching: the
//! actor consults it before it touches an `AXUIElement`.

use crate::types::AppInfo;

/// Bundle ids that are never read or driven.
///
/// Terminal-class apps and editors with integrated terminals would turn the
/// navigator into a shell; the arbitrary-power tools would turn it into a
/// scripting host; the secret stores would hand out credentials.
const DENIED_BUNDLE_IDS: [&str; 26] = [
    // Terminal-class.
    "com.apple.Terminal",
    "com.googlecode.iterm2",
    "dev.warp.Warp-Stable",
    "com.mitchellh.ghostty",
    "net.kovidgoyal.kitty",
    "org.alacritty",
    "com.github.wez.wezterm",
    "co.zeit.hyper",
    "org.tabby",
    // Editors with integrated terminals.
    "com.microsoft.VSCode",
    "com.microsoft.VSCodeInsiders",
    "com.vscodium",
    "com.todesktop.230313mzl4w4u92", // Cursor
    "com.exafunction.windsurf",
    "dev.zed.Zed",
    "com.panic.Nova",
    // Arbitrary-power tools.
    "com.apple.ScriptEditor2",
    "com.apple.Automator",
    "com.apple.shortcuts",
    // Secrets.
    "com.apple.keychainaccess",
    "com.apple.Passwords",
    "com.agilebits.onepassword7",
    "com.1password.1password",
    "com.bitwarden.desktop",
    "com.apple.systempreferences",
    "com.apple.SystemProfiler",
];

/// Bundle id prefixes that cover a family of builds (JetBrains ships one
/// bundle id per IDE and per edition).
const DENIED_PREFIXES: [&str; 2] = ["com.jetbrains.", "com.google.android.studio"];

/// Localized names of the same apps, lower-cased.
///
/// A bundle id is the reliable key, but an app can be launched by name before
/// its bundle id is known, and a process may expose no bundle id at all. The
/// name list closes both holes.
const DENIED_NAMES: [&str; 23] = [
    "terminal",
    "iterm",
    "iterm2",
    "warp",
    "ghostty",
    "kitty",
    "alacritty",
    "wezterm",
    "hyper",
    "tabby",
    "visual studio code",
    "code",
    "cursor",
    "windsurf",
    "vscodium",
    "zed",
    "nova",
    "script editor",
    "automator",
    "shortcuts",
    "keychain access",
    "passwords",
    "system settings",
];

/// The policy the actor enforces.
#[derive(Clone, Debug)]
pub struct AxPolicy {
    /// Extra bundle ids or name substrings the user added. Only tightening is
    /// possible from configuration (P8).
    pub extra: Vec<String>,
    /// This process's own pid, never listed, walked or targeted.
    pub own_pid: i32,
}

impl AxPolicy {
    /// The default policy for this process.
    #[must_use]
    pub fn new(own_pid: i32) -> Self {
        Self { extra: Vec::new(), own_pid }
    }

    /// Whether the app must never be read or driven.
    #[must_use]
    pub fn is_denied(&self, app: &AppInfo) -> bool {
        if app.pid == self.own_pid {
            return true;
        }
        if let Some(bundle) = app.bundle_id.as_deref() {
            if DENIED_BUNDLE_IDS.iter().any(|d| d.eq_ignore_ascii_case(bundle)) {
                return true;
            }
            let lower = bundle.to_lowercase();
            if DENIED_PREFIXES.iter().any(|p| lower.starts_with(p)) {
                return true;
            }
            if self.extra.iter().any(|e| e.eq_ignore_ascii_case(bundle)) {
                return true;
            }
        }
        let name = app.name.to_lowercase();
        if DENIED_NAMES.contains(&name.as_str()) {
            return true;
        }
        self.extra.iter().any(|e| {
            let e = e.to_lowercase();
            !e.is_empty() && name.contains(&e)
        })
    }

    /// Whether the crate refuses this app to the web path instead (Chrome
    /// family is `CdpObserver`'s, not ours).
    #[must_use]
    pub fn is_chrome_family(app: &AppInfo) -> bool {
        const CHROME: [&str; 9] = [
            "com.google.Chrome",
            "com.google.Chrome.beta",
            "com.google.Chrome.canary",
            "org.chromium.Chromium",
            "com.brave.Browser",
            "com.microsoft.edgemac",
            "company.thebrowser.Browser",
            "com.vivaldi.Vivaldi",
            "com.operasoftware.Opera",
        ];
        app.bundle_id
            .as_deref()
            .is_some_and(|b| CHROME.iter().any(|c| c.eq_ignore_ascii_case(b)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str, bundle: Option<&str>, pid: i32) -> AppInfo {
        AppInfo {
            name: name.to_owned(),
            bundle_id: bundle.map(ToOwned::to_owned),
            pid,
            frontmost: false,
        }
    }

    #[test]
    fn terminal_class_editors_and_secret_stores_are_denied() {
        let policy = AxPolicy::new(1);
        for bundle in [
            "com.apple.Terminal",
            "com.googlecode.iterm2",
            "com.microsoft.VSCode",
            "dev.zed.Zed",
            "com.jetbrains.intellij",
            "com.apple.keychainaccess",
            "com.apple.ScriptEditor2",
        ] {
            assert!(policy.is_denied(&app("x", Some(bundle), 2)), "{bundle} must be denied");
        }
        assert!(!policy.is_denied(&app("Notes", Some("com.apple.Notes"), 2)));
    }

    #[test]
    fn a_denied_app_is_refused_by_name_even_with_no_bundle_id() {
        let policy = AxPolicy::new(1);
        for name in ["Terminal", "iTerm2", "Cursor", "Keychain Access", "System Settings"] {
            assert!(policy.is_denied(&app(name, None, 2)), "{name} must be denied by name");
        }
        assert!(!policy.is_denied(&app("TextEdit", None, 2)));
    }

    #[test]
    fn our_own_pid_is_never_readable() {
        let policy = AxPolicy::new(7);
        assert!(policy.is_denied(&app("Neo", Some("com.starkbot.neo"), 7)));
    }

    #[test]
    fn user_entries_match_a_bundle_id_or_a_name_substring() {
        let mut policy = AxPolicy::new(1);
        policy.extra.push("Banking".into());
        assert!(policy.is_denied(&app("My Banking App", Some("com.bank"), 2)));
        policy.extra.push("com.example.thing".into());
        assert!(policy.is_denied(&app("Thing", Some("COM.EXAMPLE.THING"), 3)));
        assert!(!policy.is_denied(&app("Notes", Some("com.apple.Notes"), 4)));
    }

    #[test]
    fn chrome_family_is_routed_to_the_web_path() {
        assert!(AxPolicy::is_chrome_family(&app("Chrome", Some("com.google.Chrome"), 2)));
        assert!(AxPolicy::is_chrome_family(&app("Arc", Some("company.thebrowser.Browser"), 2)));
        assert!(!AxPolicy::is_chrome_family(&app("Safari", Some("com.apple.Safari"), 2)));
    }
}
