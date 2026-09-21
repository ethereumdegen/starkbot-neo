//! The session's Secret Service (`org.freedesktop.secrets`) as a keychain
//! backend — Linux's login Keychain.
//!
//! One item per account in the **default** collection, carrying the two
//! attributes `service` and `account` and nothing else. Those are libsecret's
//! own names, so the item this crate writes is exactly the item
//! `secret-tool lookup service com.starkbot.neo account openai` finds: a
//! lookup matches an item whose attributes are a superset of the query, so a
//! third attribute here would still be found, while a third one on the
//! command line would not find ours.
//!
//! Every call runs on a thread of its own. `secret-service`'s blocking API
//! drives zbus through `tokio::runtime::Runtime::block_on` — that is what
//! zbus's `tokio` feature does — and `block_on` panics when the calling
//! thread is already driving a runtime, which `Runtime::secret` and the OAuth
//! store both are. zbus keeps its own reactor, so the thread spawned here
//! only parks until the round trip returns.

use std::collections::HashMap;
use std::sync::LazyLock;

use secret_service::blocking::{Collection, SecretService};
use secret_service::{EncryptionType, Error};

use super::KeychainError;

/// The bus name a Secret Service provider owns.
const BUS_NAME: &str = "org.freedesktop.secrets";

/// The two attributes `secret-tool` takes on its command line.
const ATTRIBUTE_SERVICE: &str = "service";
const ATTRIBUTE_ACCOUNT: &str = "account";

/// A stored credential is always UTF-8 ([`super::Keychain::get`] refuses
/// anything else), so the item is text — which is what `secret-tool lookup`
/// prints and what another keyring front end will show the user.
const CONTENT_TYPE: &str = "text/plain";

/// Is there a Secret Service provider on this session's bus?
///
/// Memoized: the answer cannot change without the session restarting, and
/// every `Keychain::new` asks.
pub(super) fn available() -> bool {
    static AVAILABLE: LazyLock<bool> = LazyLock::new(|| off_thread(probe));
    *AVAILABLE
}

fn probe() -> bool {
    let Ok(connection) = zbus::blocking::Connection::session() else {
        return false;
    };
    let Ok(bus) = zbus::blocking::fdo::DBusProxy::new(&connection) else {
        return false;
    };
    let Ok(name) = zbus::names::BusName::try_from(BUS_NAME) else {
        return false;
    };
    if let Ok(true) = bus.name_has_owner(name) {
        return true;
    }
    // A provider that is activatable but not started yet is still a keyring:
    // the first `connect` starts it. Only a session with no provider at all
    // falls back to the file.
    match bus.list_activatable_names() {
        Ok(names) => names.iter().any(|name| name.as_str() == BUS_NAME),
        Err(_) => false,
    }
}

pub(super) fn get(service: &str, account: &str) -> Result<Option<Vec<u8>>, KeychainError> {
    run("read", account, || {
        let bus = SecretService::connect(EncryptionType::Dh)?;
        let collection = unlocked(&bus)?;
        let items = collection.search_items(attributes(service, account))?;
        match items.first() {
            // The collection is unlocked by now, which is all gnome-keyring
            // locks; the spec lets a provider lock an item on its own, and
            // KeePassXC does. Same prompt, same mapping — asking first costs
            // one property read.
            Some(item) => {
                if item.is_locked()? {
                    item.unlock()?;
                }
                Ok(Some(item.get_secret()?))
            }
            None => Ok(None),
        }
    })
}

pub(super) fn set(service: &str, account: &str, secret: &[u8]) -> Result<(), KeychainError> {
    let label = format!("Starkbot Neo — {account}");
    run("write", account, || {
        let bus = SecretService::connect(EncryptionType::Dh)?;
        let collection = unlocked(&bus)?;
        // `replace` makes this the upsert `set_generic_password` is on macOS:
        // storing twice leaves one item, not two items one of which `get`
        // will never reach.
        collection.create_item(
            &label,
            attributes(service, account),
            secret,
            true,
            CONTENT_TYPE,
        )?;
        Ok(())
    })
}

pub(super) fn delete(service: &str, account: &str) -> Result<(), KeychainError> {
    run("delete", account, || {
        let bus = SecretService::connect(EncryptionType::Dh)?;
        let collection = unlocked(&bus)?;
        // Every match, not the first. Two items can carry the same attributes
        // — another front end can write one — and leaving one behind would
        // have `get` keep returning a credential the user deleted.
        for item in collection.search_items(attributes(service, account))? {
            item.delete()?;
        }
        Ok(())
    })
}

/// The default collection, unlocked.
///
/// `ensure_unlocked` only reports the state; `unlock` is what raises the
/// provider's own prompt, which is how a locked login keyring is meant to be
/// opened. Dismissing that prompt arrives as [`Error::Prompt`] and becomes
/// [`KeychainError::Cancelled`] — never "no such account", which is the one
/// confusion a caller cannot recover from: it would tell the user to store a
/// key they had already stored.
fn unlocked<'a>(bus: &'a SecretService<'a>) -> Result<Collection<'a>, Error> {
    let collection = bus.get_default_collection()?;
    if collection.is_locked()? {
        collection.unlock()?;
        collection.ensure_unlocked()?;
    }
    Ok(collection)
}

fn attributes<'a>(service: &'a str, account: &'a str) -> HashMap<&'a str, &'a str> {
    HashMap::from([(ATTRIBUTE_SERVICE, service), (ATTRIBUTE_ACCOUNT, account)])
}

fn run<T: Send>(
    operation: &'static str,
    account: &str,
    work: impl FnOnce() -> Result<T, Error> + Send,
) -> Result<T, KeychainError> {
    off_thread(work).map_err(|error| map(operation, account, &error))
}

/// Run `work` where no tokio runtime is entered, and wait for it.
fn off_thread<T: Send>(work: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| match scope.spawn(work).join() {
        Ok(value) => value,
        // `thread::scope` re-raises it when this closure returns anyway;
        // doing it here keeps the panic at the call site it came from.
        Err(panic) => std::panic::resume_unwind(panic),
    })
}

/// The same three outcomes the macOS backend maps, from the same three
/// situations: the keyring is locked (`errSecInteractionNotAllowed`), the
/// provider refused (`errSecAuthFailed`), or the user dismissed the prompt
/// (`errUserCanceled`). A caller therefore never needs a `cfg` to decide what
/// to tell the user.
fn map(operation: &'static str, account: &str, error: &Error) -> KeychainError {
    let account = account.to_owned();
    match error {
        Error::Locked => KeychainError::Locked { operation, account },
        Error::Prompt => KeychainError::Cancelled { operation, account },
        Error::ZbusFdo(zbus::fdo::Error::AccessDenied(_)) => {
            KeychainError::Denied { operation, account }
        }
        // In this module `NoResult` can only come from the default-collection
        // lookup: an empty search is an empty `Vec`, not an error. So it means
        // the provider has no `default` alias — a keyring that was never set
        // up — and saying so is the difference between a user who runs
        // `seahorse` and a user who retypes a key that is already stored.
        Error::NoResult => KeychainError::Keychain {
            operation,
            account,
            detail: "the secret service has no default collection".to_owned(),
        },
        error => KeychainError::Keychain {
            operation,
            account,
            detail: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dismissal of an unlock prompt, a locked keyring and an empty
    /// keyring are three different situations, and the caller acts on each
    /// differently. Collapse `Prompt` into "not found" — the shape the
    /// Secret Service API makes easiest, since a missing item is also a
    /// missing item — and `bootstrap()` asks the user to store a credential
    /// they already have.
    #[test]
    fn a_dismissed_prompt_is_not_a_missing_account() {
        assert!(matches!(
            map("read", "openai", &Error::Prompt),
            KeychainError::Cancelled { .. }
        ));
        assert!(matches!(
            map("read", "openai", &Error::Locked),
            KeychainError::Locked { .. }
        ));
        let denied = Error::ZbusFdo(zbus::fdo::Error::AccessDenied(String::new()));
        assert!(matches!(
            map("write", "openai", &denied),
            KeychainError::Denied { .. }
        ));
        match map("read", "openai", &Error::NoResult) {
            KeychainError::Keychain { detail, .. } => {
                assert!(detail.contains("default collection"));
            }
            other => panic!("expected a keychain error, got {other:?}"),
        }
    }

    /// No error built from a provider failure can carry the value: the only
    /// free text is the provider's own message.
    #[test]
    fn errors_never_quote_the_stored_value() {
        let error = map("read", "openai", &Error::Crypto("bad padding"));

        let message = error.to_string();
        assert!(message.contains("openai"));
        assert!(!message.contains("sk-"));
    }

    /// The real provider on this session, which the rest of the suite must not
    /// touch: it writes to the user's own default collection, and a locked
    /// keyring raises a prompt that would look like a hang behind
    /// `cargo test`.
    ///
    /// **Inside a multi-threaded runtime on purpose.** Every caller of
    /// `Keychain::get` is: `Runtime::secret` and the OAuth store are both
    /// reached from async code. `zbus`'s `tokio` feature makes its blocking
    /// API call `Runtime::block_on`, which panics outright when the calling
    /// thread is already driving a runtime — so without [`off_thread`] this
    /// test panics on the first bus call while a plain `#[test]` would pass,
    /// and the panic would only show up in the shipped app.
    ///
    /// Run it deliberately: `cargo test -p neo-keys -- --ignored`
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "writes to the session's real keyring and may raise an unlock prompt"]
    async fn the_session_keyring_round_trips_a_long_secret() {
        let service = format!("com.starkbot.neo.test.ss.{}", std::process::id());
        let value = "k".repeat(700);

        if let Err(error) = set(&service, "openai", value.as_bytes()) {
            panic!("{error}");
        }
        let read_back = get(&service, "openai");
        let deleted = delete(&service, "openai");
        let after = get(&service, "openai");

        match read_back {
            Ok(Some(bytes)) => assert_eq!(bytes, value.as_bytes()),
            other => panic!("expected the value back, got {other:?}"),
        }
        if let Err(error) = deleted {
            panic!("{error}");
        }
        match after {
            Ok(None) => {}
            other => panic!("expected the item to be gone, got {other:?}"),
        }
    }
}
