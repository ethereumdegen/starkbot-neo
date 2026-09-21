//! Storage for Starkbot-owned secrets, in whichever store the platform has.
//!
//! Three backends sit behind one [`Keychain`]: the login Keychain through
//! `SecItem*` on macOS ([`secitem`]), the session's Secret Service provider
//! over D-Bus on Linux ([`secret_service`]), and a JSON file for development
//! on both. The platform backend is chosen at compile time, the file backend
//! at run time by [`KEYCHAIN_BACKEND_ENV`] / [`KEYCHAIN_FILE_ENV`].
//!
//! What the backend never changes: [`Secret`]'s zeroization, the redaction of
//! its `Debug`/`Display`, and the `expose()` audit points. Only the
//! destination of the bytes is platform-specific, and each platform module
//! speaks bytes and [`KeychainError`] — no `security_framework` or
//! `secret_service` type reaches a signature in this file, so the logic here
//! compiles and is tested identically on both platforms.
//!
//! The tests in this module use the file backend, whatever the environment
//! says. They confine themselves to a unique per-run service name in a
//! temporary directory and delete every item they create, including when an
//! assertion fails.

use std::collections::BTreeMap;
use std::path::PathBuf;

use zeroize::Zeroize;

use crate::{KeyState, Secret, SecretError};

#[cfg(target_os = "macos")]
mod secitem;
#[cfg(target_os = "linux")]
mod secret_service;

/// The platform's store, under one name. Both modules expose the same three
/// functions over bytes, so [`Keychain`] never names a platform type.
#[cfg(target_os = "macos")]
use secitem as os;
#[cfg(target_os = "linux")]
use secret_service as os;

// A third platform is not a stub waiting to be written: Starkbot builds and
// is CI-tested on macOS and Linux (16 §8), and silently falling back to the
// clear-text file backend on an unknown OS would be a lie about where a user's
// keys live.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!(
    "neo-keys has an OS secret store for macOS (Keychain) and Linux (Secret Service) only"
);

/// Keychain service every Starkbot-owned credential lives under.
pub const DEFAULT_SERVICE: &str = "com.starkbot.neo";

#[derive(Debug, thiserror::Error)]
pub enum KeychainError {
    #[error("keychain account name `{0}` is not usable")]
    InvalidAccount(String),
    #[error("the keychain item for account `{account}` is not a usable secret")]
    InvalidItem { account: String },
    /// The store refused the operation. Carries the platform's own code and
    /// message, neither of which can contain the value.
    #[error("secret store {operation} for account `{account}` failed: {detail}")]
    Store {
        operation: &'static str,
        account: String,
        detail: String,
    },
    /// Linux only, and a real machine state rather than a failure: a session
    /// with no Secret Service provider — a server install, a bare ssh session,
    /// a container. `doctor` shows this row, so the message must name the one
    /// command that fixes it and the escape hatch for a machine that will
    /// never have a keyring.
    #[error(
        "no secret store is running: this session has no Secret Service provider on D-Bus. \
         Install and start one — `sudo apt install gnome-keyring` (or `kwalletmanager5` on KDE) \
         and log in again — or set NEO_KEYCHAIN_BACKEND=file to keep keys in a file beside your data"
    )]
    NoSecretService,
    /// Linux only: the provider is there, but the collection is locked and
    /// the unlock prompt was dismissed or never arrived (a headless session).
    #[error(
        "the secret store is locked, so {operation} for account `{account}` was refused: \
         unlock your login keyring (its password is your login password) and try again"
    )]
    SecretStoreLocked {
        operation: &'static str,
        account: String,
    },
}

/// Read/write access to the Starkbot-owned secrets in the platform's store.
///
/// This is the only type in the workspace that moves a secret value in or out
/// of storage; nothing here logs, and no variant of [`KeychainError`] can
/// carry key material.
pub struct Keychain {
    service: String,
    backend: Backend,
}

/// Where a secret actually goes.
///
/// The OS store is the only backend a shipped Starkbot uses. The file backend
/// exists for **development and tests**, because a Keychain read from an
/// unsigned binary raises a modal authorization prompt, and a test binary's
/// identity changes on every `cargo build` — so a test suite that touched the
/// login Keychain asked the developer for their password dozens of times per
/// run, with no way to make it stop.
enum Backend {
    /// The platform's own store: the user's login Keychain through `SecItem*`
    /// on macOS, the session's Secret Service collection on Linux.
    Os,
    /// A JSON file, owner-read/write only. Selected by `NEO_KEYCHAIN_FILE`,
    /// and never the default.
    File(PathBuf),
}

/// Point `neo-keys` at a file instead of the login Keychain.
///
/// Set this in a dev shell (or a `.env`) to stop the authorization prompts
/// while iterating; every test in the workspace sets it to a temporary path.
/// The value is a path, and the file holds the secrets **in clear text** —
/// which is exactly why it is opt-in and why the shipped app never sets it.
pub const KEYCHAIN_FILE_ENV: &str = "NEO_KEYCHAIN_FILE";

/// Select the file backend and let the caller choose the path.
///
/// Set to `file`, this makes [`wanted_file_backend`] report that the OS store
/// must not be used, without naming a location — so each `Runtime` keeps its
/// keys beside its own store instead of every process on the machine sharing
/// one file. That sharing was a real bug: parallel tests performed concurrent
/// read-modify-write on one path and corrupted it.
///
/// The other accepted value is `login`, which forces the OS store: the login
/// Keychain on macOS, the login keyring (Secret Service) on Linux.
pub const KEYCHAIN_BACKEND_ENV: &str = "NEO_KEYCHAIN_BACKEND";

/// Does this process want the file backend, and where?
///
/// `Some(Some(path))` — a path was named. `Some(None)` — the file backend is
/// wanted but the caller picks the path. `None` — the OS store.
/// **A debug build defaults to the file backend; a release build defaults to
/// the OS store.** That is the whole point: on macOS a debug binary is unsigned
/// (or ad-hoc signed, which is the same thing here), its code-signing
/// identity changes on every `cargo build`, and so every login-Keychain read
/// raises a modal authorization prompt that no amount of "Always Allow" will
/// suppress. Developing against it meant typing the login password dozens of
/// times per run, and a modal dialog made an agent turn look like a hang.
/// Linux has no signing prompt, but the same default keeps `cargo test` and
/// `cargo run` out of the developer's real keyring, which is worth as much.
///
/// Either default can be overridden: `NEO_KEYCHAIN_BACKEND=login` puts a
/// debug build back on the real Keychain (to check the shipped path, after
/// `scripts/sign-dev.sh`), and `NEO_KEYCHAIN_FILE=<path>` selects a file
/// anywhere, in any build.
#[must_use]
pub fn wanted_file_backend() -> Option<Option<PathBuf>> {
    selected_backend(
        std::env::var_os(KEYCHAIN_FILE_ENV),
        std::env::var_os(KEYCHAIN_BACKEND_ENV),
        cfg!(debug_assertions),
    )
}

/// The selection rule itself, with the two variables and the build profile
/// passed in. Split out from [`wanted_file_backend`] because setting an
/// environment variable is `unsafe` in edition 2024 (and races every other
/// test in the binary), and this decision is too load-bearing to leave
/// untested: it is what stands between a developer's iteration loop and
/// their real keyring.
fn selected_backend(
    file: Option<std::ffi::OsString>,
    backend: Option<std::ffi::OsString>,
    debug_build: bool,
) -> Option<Option<PathBuf>> {
    if let Some(path) = file
        && !path.is_empty()
    {
        return Some(Some(PathBuf::from(path)));
    }
    match backend.as_deref().and_then(std::ffi::OsStr::to_str) {
        Some("file") => Some(None),
        Some("login") => None,
        // Unset, empty or unrecognised: the build profile decides.
        _ => debug_build.then_some(None),
    }
}

impl Default for Keychain {
    fn default() -> Self {
        Self::new(DEFAULT_SERVICE)
    }
}

impl Keychain {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            backend: match wanted_file_backend() {
                Some(Some(path)) => Backend::File(path),
                // Wanted, but nobody said where: fall back to a path beside
                // the user's data, which `Runtime::open` normally supplies.
                Some(None) => Backend::File(default_file_path()),
                None => Backend::Os,
            },
        }
    }

    /// A keychain backed by `path` rather than the OS store, whatever the
    /// environment says. For tests that must not depend on a variable.
    pub fn file(service: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            service: service.into(),
            backend: Backend::File(path.into()),
        }
    }

    /// The OS store — the login Keychain on macOS, the login keyring on Linux
    /// — whatever [`KEYCHAIN_FILE_ENV`] says.
    ///
    /// Only for moving secrets *out* of it: a dev shell that has selected the
    /// file backend still needs one way to read what the real app stored.
    pub fn login(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            backend: Backend::Os,
        }
    }

    pub fn service(&self) -> &str {
        &self.service
    }

    /// Whether this keychain is the platform's real store rather than a file.
    #[must_use]
    pub fn is_login_keychain(&self) -> bool {
        matches!(self.backend, Backend::Os)
    }

    pub fn get(&self, account: &str) -> Result<Option<Secret>, KeychainError> {
        let account = check_account(account)?;
        let mut bytes = match &self.backend {
            Backend::Os => match os::get(&self.service, account)? {
                Some(bytes) => bytes,
                None => return Ok(None),
            },
            Backend::File(path) => match read_file(path, &self.service)?.remove(account) {
                Some(value) => value.into_bytes(),
                None => return Ok(None),
            },
        };
        // A stored credential is always UTF-8 (a vendor key, or our own JSON);
        // anything else is not a secret this crate wrote.
        let mut value = match String::from_utf8(bytes.clone()) {
            Ok(value) => value,
            Err(_) => {
                bytes.zeroize();
                return Err(KeychainError::InvalidItem {
                    account: account.to_owned(),
                });
            }
        };
        bytes.zeroize();
        let secret = match Secret::new(&value) {
            Ok(secret) => secret,
            Err(SecretError::Empty) => {
                value.zeroize();
                return Err(KeychainError::InvalidItem {
                    account: account.to_owned(),
                });
            }
        };
        value.zeroize();
        Ok(Some(secret))
    }

    /// Store (or replace) the account's secret.
    ///
    /// Any length, any bytes: a line break or a 4 KB JSON blob are both fine,
    /// which is what the OAuth credentials of K7 need.
    pub fn set(&self, account: &str, secret: &Secret) -> Result<(), KeychainError> {
        let account = check_account(account)?;
        // The audited boundary: the value leaves `Secret` only to reach the
        // platform's store, and only as bytes.
        #[allow(clippy::disallowed_methods)]
        let exposed = secret.expose();
        match &self.backend {
            Backend::Os => os::set(&self.service, account, exposed.as_bytes()),
            Backend::File(path) => {
                let mut items = read_file(path, &self.service)?;
                items.insert(account.to_owned(), exposed.to_owned());
                write_file(path, &self.service, &items)
            }
        }
    }

    /// Delete the item. An account with nothing stored is already deleted.
    pub fn delete(&self, account: &str) -> Result<(), KeychainError> {
        let account = check_account(account)?;
        match &self.backend {
            Backend::Os => os::delete(&self.service, account),
            Backend::File(path) => {
                let mut items = read_file(path, &self.service)?;
                if items.remove(account).is_some() {
                    write_file(path, &self.service, &items)?;
                }
                Ok(())
            }
        }
    }

    /// Whether this account holds a usable secret. Whether a vendor accepts it
    /// is a separate question answered by a validator, not here.
    pub fn state(&self, account: &str) -> Result<KeyState, KeychainError> {
        match self.get(account) {
            Ok(Some(_)) => Ok(KeyState::Present),
            Ok(None) => Ok(KeyState::Missing),
            Err(KeychainError::InvalidItem { .. }) => Ok(KeyState::Invalid),
            Err(error) => Err(error),
        }
    }
}

/// Where the file backend goes when only `NEO_KEYCHAIN_BACKEND=file` is set
/// and no `Runtime` chose a path.
#[cfg(target_os = "macos")]
fn default_file_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join("Library/Application Support/com.starkbot.neo/keys.json")
}

/// Where the file backend goes when only `NEO_KEYCHAIN_BACKEND=file` is set
/// and no `Runtime` chose a path.
///
/// XDG, not `~/Library`: `$XDG_DATA_HOME` when the session sets it, and the
/// specification's own default of `~/.local/share` when it does not.
#[cfg(target_os = "linux")]
fn default_file_path() -> PathBuf {
    if let Some(data) = std::env::var_os("XDG_DATA_HOME").filter(|data| !data.is_empty()) {
        return PathBuf::from(data).join("com.starkbot.neo/keys.json");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".local/share/com.starkbot.neo/keys.json")
}

/// The file backend's contents for one service. Missing file means empty.
fn read_file(
    path: &std::path::Path,
    service: &str,
) -> Result<BTreeMap<String, String>, KeychainError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => {
            return Err(KeychainError::Store {
                operation: "read",
                account: service.to_owned(),
                detail: error.to_string(),
            });
        }
    };
    let all: BTreeMap<String, BTreeMap<String, String>> =
        serde_json::from_str(&text).map_err(|error| KeychainError::Store {
            operation: "read",
            account: service.to_owned(),
            detail: error.to_string(),
        })?;
    Ok(all.get(service).cloned().unwrap_or_default())
}

/// Replace one service's contents, owner-only.
fn write_file(
    path: &std::path::Path,
    service: &str,
    items: &BTreeMap<String, String>,
) -> Result<(), KeychainError> {
    let fail = |detail: String| KeychainError::Store {
        operation: "write",
        account: service.to_owned(),
        detail,
    };
    let mut all: BTreeMap<String, BTreeMap<String, String>> = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => BTreeMap::new(),
    };
    all.insert(service.to_owned(), items.clone());
    let encoded = serde_json::to_string_pretty(&all).map_err(|error| fail(error.to_string()))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| fail(error.to_string()))?;
    }
    // Write-then-rename, so a reader never sees a half-written file. Two
    // processes writing at once still resolve to one of the two whole files
    // rather than a corrupt mixture of both.
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&temporary, encoded).map_err(|error| fail(error.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| fail(error.to_string()))?;
    }
    std::fs::rename(&temporary, path).map_err(|error| fail(error.to_string()))?;
    // Clear text on disk, so at least no other user can read it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| fail(error.to_string()))?;
    }
    Ok(())
}

fn check_account(account: &str) -> Result<&str, KeychainError> {
    let usable =
        !account.is_empty() && !account.starts_with('-') && !account.chars().any(char::is_control);
    if usable {
        Ok(account)
    } else {
        Err(KeychainError::InvalidAccount(account.to_owned()))
    }
}

// Every test below drives the file backend, which behaves the same on both
// platforms, so they are the Linux coverage too — not a macOS-only suite.
#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    /// Deletes everything the test stored, however the test ends.
    /// A keychain for one test.
    ///
    /// **Always the file backend, never the login Keychain.** A test binary is
    /// unsigned and its identity changes on every build, so every login-
    /// Keychain read raised a modal authorization prompt — `cargo test` asked
    /// the developer for their password once per item per run. The behaviour
    /// under test (store, read back, replace, delete, length, bytes) is the
    /// same on both backends; the login Keychain itself is exercised by the
    /// `#[ignore]`d test below.
    struct TestKeychain {
        keychain: Keychain,
        accounts: Vec<String>,
        // Held so the file outlives the test.
        _directory: tempfile::TempDir,
    }

    impl TestKeychain {
        fn new(label: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default();
            let service = format!(
                "com.starkbot.neo.test.{label}.{}.{nanos}",
                std::process::id()
            );
            let directory = match tempfile::tempdir() {
                Ok(directory) => directory,
                Err(error) => panic!("a temporary directory: {error}"),
            };
            Self {
                keychain: Keychain::file(service, directory.path().join("keys.json")),
                accounts: Vec::new(),
                _directory: directory,
            }
        }

        fn account(&mut self, name: &str) -> String {
            self.accounts.push(name.to_owned());
            name.to_owned()
        }

        fn store(&self, account: &str, value: &str) {
            let secret = match Secret::new(value) {
                Ok(secret) => secret,
                Err(error) => panic!("{error}"),
            };
            if let Err(error) = self.keychain.set(account, &secret) {
                panic!("{error}");
            }
        }

        fn read(&self, account: &str) -> Option<String> {
            match self.keychain.get(account) {
                #[allow(clippy::disallowed_methods)]
                Ok(secret) => secret.map(|secret| secret.expose().to_owned()),
                Err(error) => panic!("{error}"),
            }
        }

        fn state(&self, account: &str) -> KeyState {
            match self.keychain.state(account) {
                Ok(state) => state,
                Err(error) => panic!("{error}"),
            }
        }
    }

    impl Drop for TestKeychain {
        fn drop(&mut self) {
            for account in &self.accounts {
                let _ = self.keychain.delete(account);
            }
        }
    }

    #[test]
    fn round_trips_a_secret_through_the_keychain() {
        let mut fixture = TestKeychain::new("roundtrip");
        let account = fixture.account("openai");

        assert_eq!(fixture.state(&account), KeyState::Missing);
        assert_eq!(fixture.read(&account), None);

        fixture.store(&account, "sk-first-value");
        assert_eq!(fixture.read(&account).as_deref(), Some("sk-first-value"));
        assert_eq!(fixture.state(&account), KeyState::Present);

        fixture.store(&account, "sk-second-value");
        assert_eq!(fixture.read(&account).as_deref(), Some("sk-second-value"));

        if let Err(error) = fixture.keychain.delete(&account) {
            panic!("{error}");
        }
        assert_eq!(fixture.state(&account), KeyState::Missing);
    }

    #[test]
    fn deleting_an_absent_account_succeeds() {
        let mut fixture = TestKeychain::new("absent");
        let account = fixture.account("typesafe");

        assert!(fixture.keychain.delete(&account).is_ok());
    }

    #[test]
    fn awkward_values_survive_storage() {
        let mut fixture = TestKeychain::new("awkward");
        for (index, value) in [
            "quote\"inside",
            "back\\slash",
            "tail-Ωé-ünicode",
            "-leading-dash",
            "  padded  ",
        ]
        .into_iter()
        .enumerate()
        {
            let account = fixture.account(&format!("awkward-{index}"));
            fixture.store(&account, value);
            assert_eq!(fixture.read(&account).as_deref(), Some(value));
            assert_eq!(fixture.state(&account), KeyState::Present);
        }
    }

    #[test]
    fn rejects_unusable_account_names() {
        let keychain = Keychain::new("com.starkbot.neo.test.rejects");
        let secret = match Secret::new("sk-value") {
            Ok(secret) => secret,
            Err(error) => panic!("{error}"),
        };
        for account in ["", "-flag", "new\nline", "bell\u{7}"] {
            assert!(matches!(
                keychain.get(account),
                Err(KeychainError::InvalidAccount(_))
            ));
            assert!(matches!(
                keychain.set(account, &secret),
                Err(KeychainError::InvalidAccount(_))
            ));
            assert!(matches!(
                keychain.delete(account),
                Err(KeychainError::InvalidAccount(_))
            ));
            assert!(matches!(
                keychain.state(account),
                Err(KeychainError::InvalidAccount(_))
            ));
        }
    }

    /// A line break used to be refused, because the old `security`-on-stdin
    /// write would have read it as the end of the password. `SecItemAdd` takes
    /// the bytes as they are.
    // Reading a stored value back is the whole point of these two tests, and
    // the value is a fixture this test wrote, not a credential.
    #[allow(clippy::disallowed_methods)]
    #[test]
    fn a_secret_may_contain_a_line_break() {
        let mut fixture = TestKeychain::new("linebreak");
        let account = fixture.account("openai");
        let secret = match Secret::new("first\nsecond") {
            Ok(secret) => secret,
            Err(error) => panic!("{error}"),
        };

        if let Err(error) = fixture.keychain.set(&account, &secret) {
            panic!("{error}");
        }
        match fixture.keychain.get(&account) {
            Ok(Some(read_back)) => assert_eq!(read_back.expose(), "first\nsecond"),
            other => panic!("expected the value back, got {other:?}"),
        }
    }

    /// The bug this rewrite exists for: driving `security add-generic-password
    /// -w` on stdin truncated at its 128-character `readpassphrase` buffer, so
    /// a 151-byte value came back 128 bytes long. Anything longer than a short
    /// API key was silently corrupted.
    #[allow(clippy::disallowed_methods)]
    #[test]
    fn a_secret_longer_than_the_old_128_byte_limit_round_trips() {
        let mut fixture = TestKeychain::new("long");
        let account = fixture.account("anthropic-oauth");
        // A realistic OAuth credential blob: well past 128 bytes.
        let value = format!(
            "{{\"access_token\":\"{}\",\"refresh_token\":\"{}\",\"expires_at_ms\":1789850000000}}",
            "a".repeat(600),
            "r".repeat(300)
        );
        let secret = match Secret::new(&value) {
            Ok(secret) => secret,
            Err(error) => panic!("{error}"),
        };

        if let Err(error) = fixture.keychain.set(&account, &secret) {
            panic!("{error}");
        }
        match fixture.keychain.get(&account) {
            Ok(Some(read_back)) => {
                assert_eq!(read_back.expose().len(), value.len());
                assert_eq!(read_back.expose(), value);
            }
            other => panic!("expected the whole value back, got {other:?}"),
        }
    }

    /// No error this module can build carries key material: the only free
    /// text in `KeychainError` comes from the framework's own code and
    /// message, and the account name.
    #[test]
    fn errors_never_quote_the_stored_value() {
        let error = KeychainError::Store {
            operation: "read",
            account: "openai".to_owned(),
            detail: "The specified item could not be found in the keychain. (-25300)".to_owned(),
        };

        let message = error.to_string();
        assert!(message.contains("-25300"));
        assert!(message.contains("openai"));
        assert!(!message.contains("sk-"));
    }

    /// The login Keychain itself, which the rest of this module deliberately
    /// avoids. Ignored because it prompts for authorization on an unsigned
    /// build, which is exactly the thing that made `cargo test` unusable.
    ///
    /// Run it deliberately after signing:
    /// `scripts/sign-dev.sh && cargo test -p neo-keys -- --ignored`
    #[test]
    #[ignore = "touches the real login Keychain and may prompt for authorization"]
    fn the_login_keychain_round_trips_a_long_secret() {
        let service = format!("com.starkbot.neo.test.login.{}", std::process::id());
        let keychain = Keychain::new(&service);
        assert!(keychain.is_login_keychain());
        let value = "k".repeat(700);
        let secret = match Secret::new(&value) {
            Ok(secret) => secret,
            Err(error) => panic!("{error}"),
        };
        if let Err(error) = keychain.set("openai", &secret) {
            panic!("{error}");
        }
        let read_back = keychain.get("openai");
        let _ = keychain.delete("openai");
        match read_back {
            #[allow(clippy::disallowed_methods)]
            Ok(Some(read_back)) => assert_eq!(read_back.expose().len(), value.len()),
            other => panic!("expected the value back, got {other:?}"),
        }
    }

    fn var(value: &str) -> Option<std::ffi::OsString> {
        Some(std::ffi::OsString::from(value))
    }

    /// The rule that decides where a developer's keys go. Each row is a
    /// different way to get it wrong: a release build quietly writing clear
    /// text to disk, or a debug build reaching into the real store and
    /// blocking on an unlock prompt.
    #[test]
    fn the_backend_is_chosen_by_the_two_variables_then_the_build_profile() {
        // A named file wins over everything, in either profile.
        assert_eq!(
            selected_backend(var("/tmp/keys.json"), var("login"), false),
            Some(Some(PathBuf::from("/tmp/keys.json")))
        );
        // An empty `NEO_KEYCHAIN_FILE` is not a path; it must not select a
        // file backend at the current directory.
        assert_eq!(selected_backend(var(""), None, false), None);
        assert_eq!(selected_backend(var(""), None, true), Some(None));
        // `file` without a path: the caller (a `Runtime`) picks one.
        assert_eq!(selected_backend(None, var("file"), false), Some(None));
        // `login` forces the OS store even in a debug build.
        assert_eq!(selected_backend(None, var("login"), true), None);
        // Nothing set: debug develops against a file, release ships to the OS.
        assert_eq!(selected_backend(None, None, true), Some(None));
        assert_eq!(selected_backend(None, None, false), None);
        // A typo is not a third backend; it falls back to the profile.
        assert_eq!(selected_backend(None, var("keyring"), false), None);
    }

    /// `login()` and `file()` are the two explicit constructors, and neither
    /// may consult the environment: `import_login_keychain` reads the OS store
    /// from a process whose own backend is a file.
    #[test]
    fn the_explicit_constructors_ignore_the_environment() {
        assert!(Keychain::login(DEFAULT_SERVICE).is_login_keychain());
        assert!(!Keychain::file(DEFAULT_SERVICE, "/tmp/keys.json").is_login_keychain());
    }

    /// A Linux box with no Secret Service is a machine state `doctor` reports,
    /// so the message has to carry the fix: what to install, and the way out
    /// for a machine that will never have a keyring.
    #[test]
    fn the_absent_secret_service_error_names_its_remedy() {
        let message = KeychainError::NoSecretService.to_string();

        assert!(message.contains("gnome-keyring"), "{message}");
        assert!(message.contains("NEO_KEYCHAIN_BACKEND=file"), "{message}");
        // Not a generic failure: it says what is missing.
        assert!(message.contains("Secret Service"), "{message}");
    }

    /// The locked keyring is the other nameable Linux condition, and it is a
    /// different remedy: unlock, not install.
    #[test]
    fn the_locked_store_error_names_the_account_and_the_remedy() {
        let message = KeychainError::SecretStoreLocked {
            operation: "read",
            account: "openai".to_owned(),
        }
        .to_string();

        assert!(message.contains("openai"), "{message}");
        assert!(message.contains("unlock"), "{message}");
    }
}
