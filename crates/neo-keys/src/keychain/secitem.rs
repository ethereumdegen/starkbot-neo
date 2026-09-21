//! macOS storage: the login Keychain through `SecItem*`.
//!
//! Every operation goes through the Security framework (`SecItemAdd`,
//! `SecItemCopyMatching`, `SecItemUpdate`, `SecItemDelete`) by way of the
//! `security-framework` crate's safe wrapper. An earlier implementation drove
//! `/usr/bin/security` instead, and **silently truncated every secret at 128
//! bytes**: to keep the value off argv it fed `add-generic-password -w` on
//! stdin, where `security` reads the password with a 128-character
//! `readpassphrase` buffer. A 151-byte value came back 128 bytes long, which
//! would quietly corrupt a long `sk-proj-…` key and makes a JSON credential
//! blob impossible. `SecItemAdd` takes arbitrary bytes and needs neither argv
//! nor a terminal.
//!
//! Nothing from `security_framework` crosses this module's edge: the three
//! functions below speak bytes and [`KeychainError`], so the shared code in
//! the parent module compiles identically on every platform.

use security_framework::base::Error as SecError;
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};

use super::KeychainError;

/// `errSecItemNotFound` — nothing is stored under that account.
const NOT_FOUND: i32 = -25300;

pub(super) fn get(service: &str, account: &str) -> Result<Option<Vec<u8>>, KeychainError> {
    match get_generic_password(service, account) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.code() == NOT_FOUND => Ok(None),
        Err(error) => Err(failed("read", account, &error)),
    }
}

pub(super) fn set(service: &str, account: &str, secret: &[u8]) -> Result<(), KeychainError> {
    set_generic_password(service, account, secret).map_err(|error| failed("write", account, &error))
}

/// An account with nothing stored is already deleted.
pub(super) fn delete(service: &str, account: &str) -> Result<(), KeychainError> {
    match delete_generic_password(service, account) {
        Ok(()) => Ok(()),
        Err(error) if error.code() == NOT_FOUND => Ok(()),
        Err(error) => Err(failed("delete", account, &error)),
    }
}

/// The framework's own code and message, neither of which can contain the
/// stored value.
fn failed(operation: &'static str, account: &str, error: &SecError) -> KeychainError {
    KeychainError::Store {
        operation,
        account: account.to_owned(),
        detail: format!("{} ({})", error.message().unwrap_or_default(), error.code()),
    }
}
