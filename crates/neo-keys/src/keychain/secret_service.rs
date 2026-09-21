//! Linux storage: the session's Secret Service provider, over D-Bus.
//!
//! Secret Service is the freedesktop API that gnome-keyring, KWallet and
//! KeePassXC all implement, so one backend covers every desktop a Starkbot
//! user is likely to run. We talk to it with the `secret-service` crate rather
//! than `keyring`: `keyring`'s Linux path is this same crate plus a
//! credential-store abstraction we would not use (we already have our own
//! `Keychain` seam and our own file backend), and `libsecret-sys` would put a
//! C library and its headers on the build machine. `secret-service` with
//! `rt-async-io-crypto-rust` is pure Rust — a stock Ubuntu box needs no
//! `-dev` package to build Starkbot.
//!
//! The session is DH-encrypted (`EncryptionType::Dh`), not plain: a plain
//! session would push the key material through the session bus in clear text,
//! where any process that can talk to the bus could watch it go past.
//!
//! Nothing from `secret_service` crosses this module's edge: the three
//! functions below speak bytes and [`KeychainError`], so the shared code in
//! the parent module compiles identically on every platform.

use std::collections::HashMap;

use secret_service::blocking::{Collection, SecretService};
use secret_service::{EncryptionType, Error as ServiceError};

use super::KeychainError;

/// Lookup attributes. `service`/`account` mirror `kSecAttrService` and
/// `kSecAttrAccount` on macOS so both backends address an item the same way.
const SERVICE_ATTRIBUTE: &str = "service";
const ACCOUNT_ATTRIBUTE: &str = "account";

/// What the stored bytes are. Every Starkbot secret is UTF-8 — a vendor key or
/// our own JSON credential blob.
const CONTENT_TYPE: &str = "text/plain";

pub(super) fn get(service: &str, account: &str) -> Result<Option<Vec<u8>>, KeychainError> {
    with_collection("read", account, |collection| {
        match collection
            .search_items(attributes(service, account))?
            .first()
        {
            Some(item) => {
                item.ensure_unlocked()?;
                Ok(Some(item.get_secret()?))
            }
            None => Ok(None),
        }
    })
}

pub(super) fn set(service: &str, account: &str, secret: &[u8]) -> Result<(), KeychainError> {
    let label = format!("{service}: {account}");
    with_collection("write", account, |collection| {
        // `replace` matches `SecItemUpdate`-or-add: one account, one item, so a
        // re-entered key does not leave the old one behind in Seahorse.
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

/// An account with nothing stored is already deleted.
pub(super) fn delete(service: &str, account: &str) -> Result<(), KeychainError> {
    with_collection("delete", account, |collection| {
        for item in collection.search_items(attributes(service, account))? {
            item.delete()?;
        }
        Ok(())
    })
}

fn attributes<'a>(service: &'a str, account: &'a str) -> HashMap<&'a str, &'a str> {
    HashMap::from([(SERVICE_ATTRIBUTE, service), (ACCOUNT_ATTRIBUTE, account)])
}

/// Run one operation against the default collection, on a thread of its own.
///
/// The `secret-service` blocking API is a `block_on` around zbus, and zbus
/// says plainly that blocking on an async executor's own thread may stall that
/// executor. Every caller of `Keychain` is potentially inside the Tokio
/// runtime — the TUI frame loop and `Runtime`'s key checks both are — so the
/// D-Bus round trip happens on a scoped thread that belongs to no runtime. The
/// thread also keeps the connection's lifetime to one operation, which is what
/// makes `Keychain` `Send + Sync` without a lock.
fn with_collection<T: Send>(
    operation: &'static str,
    account: &str,
    task: impl FnOnce(&Collection<'_>) -> Result<T, ServiceError> + Send,
) -> Result<T, KeychainError> {
    let outcome = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let service = SecretService::connect(EncryptionType::Dh)?;
                let collection = service.get_any_collection()?;
                collection.ensure_unlocked()?;
                task(&collection)
            })
            .join()
    });
    match outcome {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(translate(operation, account, &error)),
        // A panic in the D-Bus thread is a bug, not a user-fixable condition;
        // it still must not take the process down with it.
        Err(_) => Err(KeychainError::Store {
            operation,
            account: account.to_owned(),
            detail: "the secret store thread panicked".to_owned(),
        }),
    }
}

/// Name the two conditions a user can actually fix, and pass everything else
/// through as a diagnostic. Neither path can carry the stored value: a D-Bus
/// error is a name and a message from the provider, never the payload.
fn translate(operation: &'static str, account: &str, error: &ServiceError) -> KeychainError {
    if is_absent(error) {
        return KeychainError::NoSecretService;
    }
    match error {
        // `Locked` is a collection that refused to open; `Prompt` is the user
        // dismissing the unlock dialog; `PromptDisconnected` is that dialog
        // dying. All three mean the same thing to the user: unlock the keyring.
        ServiceError::Locked | ServiceError::Prompt | ServiceError::PromptDisconnected => {
            KeychainError::SecretStoreLocked {
                operation,
                account: account.to_owned(),
            }
        }
        other => KeychainError::Store {
            operation,
            account: account.to_owned(),
            detail: other.to_string(),
        },
    }
}

/// Is this "nothing is listening"?
///
/// Two shapes reach us. `Unavailable` is no session bus at all (no
/// `DBUS_SESSION_BUS_ADDRESS`, or its socket is gone) — an ssh session or a
/// container. A bus that *is* running but has no Secret Service provider — a
/// stock Ubuntu Server, which ships no keyring daemon — answers the first
/// method call with the D-Bus error `ServiceUnknown`, and that is the case
/// `doctor` sees most often, so it must not fall through to a generic failure.
fn is_absent(error: &ServiceError) -> bool {
    match error {
        ServiceError::Unavailable => true,
        ServiceError::Zbus(zbus::Error::MethodError(name, _, _)) => {
            matches!(
                name.as_str(),
                "org.freedesktop.DBus.Error.ServiceUnknown"
                    | "org.freedesktop.DBus.Error.NameHasNoOwner"
            )
        }
        ServiceError::ZbusFdo(error) => matches!(
            error,
            zbus::fdo::Error::ServiceUnknown(_) | zbus::fdo::Error::NameHasNoOwner(_)
        ),
        _ => false,
    }
}
