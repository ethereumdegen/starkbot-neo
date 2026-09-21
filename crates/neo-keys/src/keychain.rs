//! Storage for Starkbot-owned secrets, in whichever store the platform has.
//!
//! Three backends sit behind one [`Keychain`]: the login Keychain through
//! `SecItem*` on macOS ([`secitem`]), the session's Secret Service provider
//! over D-Bus on Linux ([`secret_service`]), and a clear-text JSON file under
//! both. The platform backend is chosen at compile time, the file backend at
//! run time by [`KEYCHAIN_BACKEND_ENV`] / [`KEYCHAIN_FILE_ENV`] — and, on a
//! session that offers no keyring at all, by [`os_keyring_available`].
//!
//! The file backend is pure Rust, so it is the only backend on a machine with
//! neither of the other two — which is what lets `neo-core` and `neo-store`,
//! and therefore most of this workspace's testable logic, run on a CI runner
//! with no keyring. It is also what the tests in this module exercise: a test
//! binary is unsigned and its code-signing identity changes on every build,
//! so touching the real login Keychain prompts for authorization once per
//! item, and touching the user's own keyring is not a thing a test suite may
//! do. The two tests that reach a real keyring are `#[ignore]`d.
//!
//! What the backend never changes: [`Secret`]'s zeroization, the redaction of
//! its `Debug`/`Display`, and the `expose()` audit points. Only the
//! destination of the bytes is platform-specific, and each platform module
//! speaks bytes and [`KeychainError`] — no `security_framework` or
//! `secret_service` type reaches a signature in this file, so the logic here
//! compiles and is tested identically on both platforms.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::PathBuf;

use rustix::fs::{FlockOperation, flock};
use zeroize::Zeroize;

use crate::{KeyState, Secret, SecretError};

#[cfg(target_os = "macos")]
mod secitem;
#[cfg(target_os = "linux")]
mod secret_service;

/// The platform's store, under one name. Both modules expose the same four
/// functions over bytes, so [`Keychain`] never names a platform type.
#[cfg(target_os = "macos")]
use secitem as os;
#[cfg(target_os = "linux")]
use secret_service as os;

/// Keychain service every Starkbot-owned credential lives under.
pub const DEFAULT_SERVICE: &str = "com.starkbot.neo";

/// Why a keychain operation did not happen.
///
/// `Locked`, `Denied` and `Cancelled` exist because they used to collapse into
/// `Keychain`, and a caller that cannot tell them apart can only say "keychain
/// read failed" — which turned a locked Keychain into a `bootstrap()` the user
/// had no way to act on. They are three different situations with three
/// different remedies: unlock it, re-sign or reset the item's access list, or
/// simply answer the prompt next time. Each platform module maps its own
/// failures onto them, so a caller never needs a `cfg` to decide what to tell
/// the user.
#[derive(Debug, thiserror::Error)]
pub enum KeychainError {
    #[error("keychain account name `{0}` is not usable")]
    InvalidAccount(String),
    #[error("the keychain item for account `{account}` is not a usable secret")]
    InvalidItem { account: String },
    /// `errSecInteractionNotAllowed` on macOS, a locked collection on Linux.
    /// Retrying changes nothing until the keyring is unlocked.
    #[error(
        "the keychain is locked; unlock it to {operation} account `{account}` \
         (its password is your login password)"
    )]
    Locked {
        operation: &'static str,
        account: String,
    },
    /// `errSecAuthFailed`, or a Secret Service provider answering
    /// `AccessDenied`. The binary's code signature does not match the item's
    /// access list, or the user refused. Retrying changes nothing.
    #[error("the keychain denied {operation} for account `{account}`")]
    Denied {
        operation: &'static str,
        account: String,
    },
    /// `errUserCanceled`, or a dismissed unlock prompt. Not a fault: a retry
    /// after the user decides is the whole remedy.
    #[error("the keychain prompt to {operation} account `{account}` was cancelled")]
    Cancelled {
        operation: &'static str,
        account: String,
    },
    /// Linux only, and a real machine state rather than a failure: a session
    /// with no Secret Service provider — a server install, a bare ssh session,
    /// a container — where the OS keyring was asked for by name anyway
    /// (`NEO_KEYCHAIN_BACKEND=login`, or [`Keychain::login`]). `doctor` shows
    /// this row, so the message must name the one command that fixes it and
    /// the escape hatch for a machine that will never have a keyring.
    #[error(
        "no secret store is running: this session has no Secret Service provider on D-Bus. \
         Install and start one — `sudo apt install gnome-keyring` (or `kwalletmanager5` on KDE) \
         and log in again — or set NEO_KEYCHAIN_BACKEND=file to keep keys in a file beside your data"
    )]
    NoSecretService,
    /// The store refused for a reason this crate does not model. Carries the
    /// platform's own code and message, neither of which can contain the
    /// value.
    #[error("keychain {operation} for account `{account}` failed: {detail}")]
    Keychain {
        operation: &'static str,
        account: String,
        detail: String,
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
    /// Set only when the file backend is a fallback rather than a choice, so
    /// [`Keychain::fallback_warning`] can say so once instead of every caller
    /// re-deriving it.
    fell_back: bool,
}

/// Where a secret actually goes.
///
/// The OS keyring is the only backend a shipped Starkbot uses. The file
/// backend exists for **development and tests**, because a Keychain read from
/// an unsigned binary raises a modal authorization prompt, and a test binary's
/// identity changes on every `cargo build` — so a test suite that touched the
/// login Keychain asked the developer for their password dozens of times per
/// run, with no way to make it stop.
enum Backend {
    /// The platform's own store: the user's login Keychain through `SecItem*`
    /// on macOS, the session's Secret Service collection on Linux.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    Os,
    /// A JSON file, owner-read/write only. Selected by `NEO_KEYCHAIN_FILE`,
    /// the default in a debug build, and the only backend on a machine whose
    /// session offers no keyring.
    File(PathBuf),
}

/// Point `neo-keys` at a file instead of the OS keyring.
///
/// Set this in a dev shell (or a `.env`) to stop the authorization prompts
/// while iterating; every test in the workspace sets it to a temporary path.
/// The value is a path, and the file holds the secrets **in clear text** —
/// which is exactly why it is opt-in and why the shipped app never sets it.
pub const KEYCHAIN_FILE_ENV: &str = "NEO_KEYCHAIN_FILE";

/// Select the file backend and let the caller choose the path.
///
/// Set to `file`, this makes [`wanted_file_backend`] report that the OS
/// keyring must not be used, without naming a location — so each `Runtime`
/// keeps its keys beside its own store instead of every process on the
/// machine sharing one file. That sharing was a real bug: parallel tests
/// performed concurrent read-modify-write on one path and corrupted it.
///
/// The other accepted value is `login`, which forces the OS keyring: the
/// login Keychain on macOS, the login keyring (Secret Service) on Linux.
pub const KEYCHAIN_BACKEND_ENV: &str = "NEO_KEYCHAIN_BACKEND";

/// What this process should store secrets in.
#[derive(Debug, PartialEq, Eq)]
enum Wanted {
    /// A file at a named path.
    FileAt(PathBuf),
    /// A file, path up to the caller.
    File,
    /// A file, path up to the caller, because this session has no keyring at
    /// all — which is nobody's choice and which the user has to be told
    /// about, since it means their credentials are in clear text.
    FileWithoutAKeyring,
    /// The OS keyring: the login Keychain on macOS, the session's Secret
    /// Service on Linux.
    Keyring,
}

/// The one place the storage policy lives.
///
/// **A debug build defaults to the file backend; a release build defaults to
/// the OS keyring, and falls back to the file when the machine has none.**
/// The debug default is the whole point: on macOS a debug binary is unsigned
/// (or ad-hoc signed, which is the same thing here), its code-signing
/// identity changes on every `cargo build`, and so every login-Keychain read
/// raises a modal authorization prompt that no amount of "Always Allow" will
/// suppress — developing against it meant typing the login password dozens of
/// times per run, and a modal dialog made an agent turn look like a hang. On
/// Linux the prompts are not the problem; the user's own keyring is. A
/// `cargo test --workspace` constructs dozens of `Runtime`s, and every one of
/// them writing into the collection that holds the user's real passwords is
/// not a thing a test suite may do. Same default, two reasons.
///
/// Either default can be overridden: `NEO_KEYCHAIN_BACKEND=login` puts a
/// debug build back on the real keyring (on macOS, after
/// `scripts/sign-dev.sh`), and `NEO_KEYCHAIN_FILE=<path>` selects a file
/// anywhere, in any build.
fn wanted_backend() -> Wanted {
    selected_backend(
        std::env::var_os(KEYCHAIN_FILE_ENV),
        std::env::var_os(KEYCHAIN_BACKEND_ENV),
        cfg!(debug_assertions),
        os_keyring_available,
    )
}

/// The selection rule itself, with the two variables, the build profile and
/// the keyring probe passed in. Split out from [`wanted_backend`] because
/// setting an environment variable is `unsafe` in edition 2024 (and races
/// every other test in the binary), and this decision is too load-bearing to
/// leave untested: it is what stands between a developer's iteration loop and
/// their real keyring.
///
/// `keyring_available` is a closure rather than a `bool` so that it is only
/// called when the answer can still change the outcome — which is what keeps
/// `cargo test` off the session bus.
fn selected_backend(
    file: Option<std::ffi::OsString>,
    backend: Option<std::ffi::OsString>,
    debug_build: bool,
    keyring_available: impl FnOnce() -> bool,
) -> Wanted {
    if let Some(path) = file
        && !path.is_empty()
    {
        return Wanted::FileAt(PathBuf::from(path));
    }
    match backend.as_deref().and_then(std::ffi::OsStr::to_str) {
        Some("file") => return Wanted::File,
        Some("login") => {}
        // Unset, empty, or a value this crate does not define: the build
        // profile decides.
        _ if debug_build => return Wanted::File,
        _ => {}
    }
    if keyring_available() {
        Wanted::Keyring
    } else {
        Wanted::FileWithoutAKeyring
    }
}

/// Does this process want the file backend, and where?
///
/// `Some(Some(path))` — a path was named. `Some(None)` — the file backend is
/// wanted but the caller picks the path. `None` — the OS keyring. See
/// [`wanted_backend`] for the policy; a caller that only has to open a
/// keychain wants [`Keychain::new`] instead.
#[must_use]
pub fn wanted_file_backend() -> Option<Option<PathBuf>> {
    match wanted_backend() {
        Wanted::FileAt(path) => Some(Some(path)),
        Wanted::File | Wanted::FileWithoutAKeyring => Some(None),
        Wanted::Keyring => None,
    }
}

/// Can this machine keep a secret out of a clear-text file?
///
/// macOS always can. On Linux it depends on the session: the answer is
/// whether anything owns — or can be started to own — `org.freedesktop.secrets`
/// on the session bus. `neo doctor` reports it, and it needs no `cfg` to do
/// so.
#[must_use]
pub fn os_keyring_available() -> bool {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        os::available()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

/// What [`Keychain::fallback_warning`] says.
pub const NO_KEYRING_WARNING: &str = "this session offers no OS keyring (on Linux: nothing owns `org.freedesktop.secrets` on the session bus), so Starkbot's credentials are stored in a clear-text file instead";

impl Default for Keychain {
    fn default() -> Self {
        Self::new(DEFAULT_SERVICE)
    }
}

impl Keychain {
    pub fn new(service: impl Into<String>) -> Self {
        let (backend, fell_back) = match wanted_backend() {
            Wanted::FileAt(path) => (Backend::File(path), false),
            // Wanted, but nobody said where: fall back to a path beside
            // the user's data, which `Runtime::open` normally supplies.
            Wanted::File => (Backend::File(default_file_path()), false),
            Wanted::FileWithoutAKeyring => (Backend::File(default_file_path()), true),
            Wanted::Keyring => (Self::keyring_backend(), false),
        };
        Self {
            service: service.into(),
            backend,
            fell_back,
        }
    }

    /// A keychain backed by `path` rather than the OS keyring, whatever the
    /// environment says. For tests that must not depend on a variable.
    pub fn file(service: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            service: service.into(),
            backend: Backend::File(path.into()),
            // Asked for by name, so there is nothing to warn about.
            fell_back: false,
        }
    }

    /// The OS keyring — the login Keychain on macOS, the login keyring on
    /// Linux — whatever [`KEYCHAIN_FILE_ENV`] says.
    ///
    /// Only for moving secrets *out* of it: a dev shell that has selected the
    /// file backend still needs one way to read what the real app stored.
    /// Absent where there is no keyring to name, so a caller cannot ask for
    /// one that cannot exist.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub fn login(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            backend: Self::keyring_backend(),
            fell_back: false,
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn keyring_backend() -> Backend {
        Backend::Os
    }

    /// Unreachable: [`wanted_backend`] never asks for a keyring on a machine
    /// [`os_keyring_available`] says has none. Spelled out rather than
    /// panicked.
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn keyring_backend() -> Backend {
        Backend::File(default_file_path())
    }

    pub fn service(&self) -> &str {
        &self.service
    }

    /// Whether this keychain is the platform's real store rather than a file.
    #[must_use]
    pub fn is_login_keychain(&self) -> bool {
        match &self.backend {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Backend::Os => true,
            Backend::File(_) => false,
        }
    }

    /// Present only when the credentials are in a clear-text file because
    /// this session has no keyring — not when a file was chosen. `neo doctor`
    /// and the Settings screen surface it; storage behaves the same either
    /// way, which is exactly why nothing else would notice.
    #[must_use]
    pub fn fallback_warning(&self) -> Option<&'static str> {
        self.fell_back.then_some(NO_KEYRING_WARNING)
    }

    pub fn get(&self, account: &str) -> Result<Option<Secret>, KeychainError> {
        let account = check_account(account)?;
        let mut bytes = match &self.backend {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
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
        // keyring, and only as the bytes it stores.
        #[allow(clippy::disallowed_methods)]
        let exposed = secret.expose();
        match &self.backend {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Backend::Os => os::set(&self.service, account, exposed.as_bytes()),
            Backend::File(path) => {
                // The lock spans the read *and* the write: two processes
                // storing two different accounts both used to read the old
                // file, and the second `rename` won — so one credential the
                // user believed they had stored was gone.
                let _lock = FileLock::exclusive(path, &self.service)?;
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
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Backend::Os => os::delete(&self.service, account),
            Backend::File(path) => {
                let _lock = FileLock::exclusive(path, &self.service)?;
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
///
/// The same rule `neo_core::paths` applies, spelled out again here because
/// `neo-core` depends on `neo-keys` and cannot be depended on back: macOS
/// keeps it under `~/Library/Application Support`, Linux under
/// `$XDG_DATA_HOME` (`~/.local/share` when that is unset).
fn default_file_path() -> PathBuf {
    // An absent or relative `HOME` used to yield `PathBuf::default()`, which
    // made this a *relative* path: a clear-text `keys.json` landed in whatever
    // directory the binary was launched from, and two processes started
    // elsewhere kept two different key stores without either noticing.
    let home = || {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
            .unwrap_or_else(std::env::temp_dir)
    };
    #[cfg(target_os = "macos")]
    {
        home().join("Library/Application Support/com.starkbot.neo/keys.json")
    }
    #[cfg(not(target_os = "macos"))]
    {
        // `XDG_DATA_HOME` is only honoured when it is absolute; the spec says
        // a relative value must be ignored, and honouring one would put the
        // clear-text store wherever the process was launched from.
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .unwrap_or_else(|| home().join(".local/share"))
            .join("starkbot-neo/keys.json")
    }
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
            return Err(KeychainError::Keychain {
                operation: "read",
                account: service.to_owned(),
                detail: error.to_string(),
            });
        }
    };
    let all: BTreeMap<String, BTreeMap<String, String>> =
        serde_json::from_str(&text).map_err(|error| KeychainError::Keychain {
            operation: "read",
            account: service.to_owned(),
            detail: error.to_string(),
        })?;
    Ok(all.get(service).cloned().unwrap_or_default())
}

/// Exclusive hold on the file backend across a read-modify-write.
///
/// `flock` is the same mechanism the screen lease uses, for the same reason:
/// the kernel releases it however the holding process dies, so a crash cannot
/// wedge every other process out of its own credentials. The lock lives on a
/// sibling `.lock` file rather than on the store itself, because the store is
/// replaced by `rename` — two processes would end up holding locks on two
/// different inodes and excluding nothing.
struct FileLock {
    // Closing the descriptor releases the flock.
    _file: std::fs::File,
}

impl FileLock {
    fn exclusive(path: &std::path::Path, service: &str) -> Result<Self, KeychainError> {
        let fail = |detail: String| KeychainError::Keychain {
            operation: "lock",
            account: service.to_owned(),
            detail,
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| fail(error.to_string()))?;
        }
        let mut name = path.as_os_str().to_owned();
        name.push(".lock");
        // The lock file carries no content, only the `flock`; truncating it
        // would be a write the holder never asked for.
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(PathBuf::from(name))
            .map_err(|error| fail(error.to_string()))?;
        flock(&file, FlockOperation::LockExclusive).map_err(|error| fail(error.to_string()))?;
        Ok(Self { _file: file })
    }
}

/// Replace one service's contents, owner-only.
///
/// Call under [`FileLock`]: this re-reads the file it is about to replace, so
/// a writer slipping in between the two would be silently discarded.
fn write_file(
    path: &std::path::Path,
    service: &str,
    items: &BTreeMap<String, String>,
) -> Result<(), KeychainError> {
    let fail = |detail: String| KeychainError::Keychain {
        operation: "write",
        account: service.to_owned(),
        detail,
    };
    // Not `unwrap_or_default`. A truncated or hand-edited `keys.json` parsed
    // as "no services at all", and this function wrote that back — every
    // other service's accounts replaced by the one being stored, in the
    // backend a debug build uses by default. Today `set` and `delete` both
    // call `read_file` first, which refuses the same input, so the wipe is
    // out of reach through the public API; leaving the two halves
    // disagreeing about what a bad file means is how it would come back.
    let mut all: BTreeMap<String, BTreeMap<String, String>> = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|error| fail(error.to_string()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(error) => return Err(fail(error.to_string())),
    };
    all.insert(service.to_owned(), items.clone());
    let encoded = serde_json::to_string_pretty(&all).map_err(|error| fail(error.to_string()))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| fail(error.to_string()))?;
    }
    // Write-then-rename, so a reader never sees a half-written file. The mode
    // goes on the `open` rather than on a `set_permissions` afterwards:
    // in between, a file holding every secret in clear text was readable by
    // whoever the umask allowed.
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|error| fail(error.to_string()))?;
    file.write_all(encoded.as_bytes())
        .map_err(|error| fail(error.to_string()))?;
    // The rename is ordered against the data only if the data is on disk
    // first; otherwise a power loss leaves an empty `keys.json` where a
    // complete one used to be.
    file.sync_all().map_err(|error| fail(error.to_string()))?;
    drop(file);
    std::fs::rename(&temporary, path).map_err(|error| fail(error.to_string()))?;
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
    use std::sync::{Arc, Barrier};
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
        let error = KeychainError::Keychain {
            operation: "read",
            account: "openai".to_owned(),
            detail: "The specified item could not be found in the keychain. (-25300)".to_owned(),
        };

        let message = error.to_string();
        assert!(message.contains("-25300"));
        assert!(message.contains("openai"));
        assert!(!message.contains("sk-"));
    }

    /// A `keys.json` this crate cannot parse is never silently replaced —
    /// the store holds every account's credential, and replacing it loses
    /// all of them. Both halves have to agree for that to hold: `read_file`
    /// refuses the file, and `write_file` (which used to
    /// `unwrap_or_default()` it into "no services at all" and write that
    /// back) now refuses it too. Relax either one and a truncated or
    /// hand-edited file costs the user every key, in the backend a debug
    /// build uses by default.
    #[test]
    fn a_corrupt_store_is_refused_rather_than_replaced() {
        let directory = match tempfile::tempdir() {
            Ok(directory) => directory,
            Err(error) => panic!("a temporary directory: {error}"),
        };
        let path = directory.path().join("keys.json");
        let corrupt = "{\"com.starkbot.neo\":{\"openai\":\"sk-live\",";
        if let Err(error) = std::fs::write(&path, corrupt) {
            panic!("{error}");
        }

        let keychain = Keychain::file("com.starkbot.neo", &path);
        let secret = match Secret::new("sk-new") {
            Ok(secret) => secret,
            Err(error) => panic!("{error}"),
        };
        assert!(matches!(
            keychain.set("anthropic", &secret),
            Err(KeychainError::Keychain { .. })
        ));
        match std::fs::read_to_string(&path) {
            Ok(after) => assert_eq!(after, corrupt, "the unreadable store was replaced anyway"),
            Err(error) => panic!("{error}"),
        }
    }

    /// Two writers storing two different accounts both read the old file and
    /// the second `rename` won, so one credential the user believed they had
    /// stored was gone. `flock` is per open file description, so two threads
    /// each taking their own lock contend exactly as two processes do.
    #[test]
    fn concurrent_writes_to_different_accounts_all_survive() {
        let directory = match tempfile::tempdir() {
            Ok(directory) => directory,
            Err(error) => panic!("a temporary directory: {error}"),
        };
        let path = directory.path().join("keys.json");
        let rounds = 10;
        for round in 0..rounds {
            let barrier = Arc::new(Barrier::new(2));
            let writers = ["openai", "anthropic"].map(|account| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                let account = format!("{account}-{round}");
                std::thread::spawn(move || {
                    let keychain = Keychain::file("com.starkbot.neo", path);
                    let secret = match Secret::new(format!("sk-{account}")) {
                        Ok(secret) => secret,
                        Err(error) => panic!("{error}"),
                    };
                    barrier.wait();
                    keychain.set(&account, &secret)
                })
            });
            for writer in writers {
                match writer.join() {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => panic!("{error}"),
                    Err(_) => panic!("a writer panicked"),
                }
            }
        }

        let keychain = Keychain::file("com.starkbot.neo", &path);
        for round in 0..rounds {
            for account in [format!("openai-{round}"), format!("anthropic-{round}")] {
                match keychain.get(&account) {
                    #[allow(clippy::disallowed_methods)]
                    Ok(Some(secret)) => assert_eq!(secret.expose(), format!("sk-{account}")),
                    other => panic!("`{account}` was lost: {other:?}"),
                }
            }
        }
    }

    /// The three outcomes a user can actually act on: unlock the keyring,
    /// re-sign or reset the item's access list, or answer the prompt. They
    /// used to collapse into one opaque `Keychain` error, and `bootstrap()`
    /// refused to start with a message nobody could act on. Both backends map
    /// onto these three, so the caller needs no `cfg`.
    #[test]
    fn a_locked_keychain_reads_differently_from_a_denied_one() {
        let locked = KeychainError::Locked {
            operation: "read",
            account: "openai".to_owned(),
        };
        let denied = KeychainError::Denied {
            operation: "read",
            account: "openai".to_owned(),
        };
        let cancelled = KeychainError::Cancelled {
            operation: "read",
            account: "openai".to_owned(),
        };
        assert!(locked.to_string().contains("locked"));
        assert!(locked.to_string().contains("openai"));
        assert!(denied.to_string().contains("denied"));
        assert!(cancelled.to_string().contains("cancelled"));
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

    fn var(value: &str) -> Option<std::ffi::OsString> {
        Some(std::ffi::OsString::from(value))
    }

    /// The rule that decides where a developer's keys go. Each row is a
    /// different way to get it wrong: a release build quietly writing clear
    /// text to disk, or a debug build reaching into the real store and
    /// blocking on an unlock prompt.
    #[test]
    fn the_backend_is_chosen_by_the_two_variables_then_the_build_profile() {
        let keyring = || true;
        // A named file wins over everything, in either profile.
        assert_eq!(
            selected_backend(var("/tmp/keys.json"), var("login"), false, keyring),
            Wanted::FileAt(PathBuf::from("/tmp/keys.json"))
        );
        // An empty `NEO_KEYCHAIN_FILE` is not a path; it must not select a
        // file backend at the current directory.
        assert_eq!(
            selected_backend(var(""), None, false, keyring),
            Wanted::Keyring
        );
        assert_eq!(selected_backend(var(""), None, true, keyring), Wanted::File);
        // `file` without a path: the caller (a `Runtime`) picks one.
        assert_eq!(
            selected_backend(None, var("file"), false, keyring),
            Wanted::File
        );
        // `login` forces the OS keyring even in a debug build.
        assert_eq!(
            selected_backend(None, var("login"), true, keyring),
            Wanted::Keyring
        );
        // Nothing set: debug develops against a file, release ships to the OS.
        assert_eq!(selected_backend(None, None, true, keyring), Wanted::File);
        assert_eq!(
            selected_backend(None, None, false, keyring),
            Wanted::Keyring
        );
        // A typo is not a third backend; it falls back to the profile.
        assert_eq!(
            selected_backend(None, var("keyring"), false, keyring),
            Wanted::Keyring
        );
        // No keyring on the machine: the file, and the user gets told.
        assert_eq!(
            selected_backend(None, None, false, || false),
            Wanted::FileWithoutAKeyring
        );
        assert_eq!(
            selected_backend(None, var("login"), true, || false),
            Wanted::FileWithoutAKeyring
        );
    }

    /// The probe talks to the session bus, so it may not run when the answer
    /// cannot change the outcome — otherwise `cargo test` reaches for the
    /// developer's keyring on every `Keychain::new`.
    #[test]
    fn the_keyring_is_not_probed_when_a_file_was_asked_for() {
        let probed = std::cell::Cell::new(false);
        let probe = || {
            probed.set(true);
            true
        };
        let _ = selected_backend(None, var("file"), false, probe);
        assert!(!probed.get(), "the session bus was probed anyway");
    }

    /// `login()` and `file()` are the two explicit constructors, and neither
    /// may consult the environment: `import_login_keychain` reads the OS store
    /// from a process whose own backend is a file.
    #[test]
    fn the_explicit_constructors_ignore_the_environment() {
        assert!(Keychain::login(DEFAULT_SERVICE).is_login_keychain());
        assert!(!Keychain::file(DEFAULT_SERVICE, "/tmp/keys.json").is_login_keychain());
    }

    /// The login Keychain itself, which the rest of this module deliberately
    /// avoids. Ignored because it prompts for authorization on an unsigned
    /// build, which is exactly the thing that made `cargo test` unusable.
    ///
    /// Run it deliberately after signing:
    /// `scripts/sign-dev.sh && cargo test -p neo-keys -- --ignored`
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "touches the real login Keychain and may prompt for authorization"]
    fn the_login_keychain_round_trips_a_long_secret() {
        let service = format!("com.starkbot.neo.test.login.{}", std::process::id());
        let keychain = Keychain::login(&service);
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
}
