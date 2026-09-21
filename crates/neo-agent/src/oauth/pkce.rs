//! PKCE material for an authorization-code login.
//!
//! The verifier is 32 bytes of operating-system randomness rendered as
//! base64url without padding (RFC 7636 §4.1), and the challenge is the
//! base64url-no-pad SHA-256 of that *string* — not of the raw bytes, which is
//! the mistake that makes a vendor answer `invalid_grant` at exchange time.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::Zeroize;

/// One login's `code_verifier`.
///
/// It is single-use and worthless without the matching authorization code,
/// but it is still login material: it never reaches a log, an error or a
/// snapshot, its formatting is redacted, and it is wiped on drop.
pub struct Verifier(String);

impl Verifier {
    pub(crate) fn generate() -> Self {
        Self(URL_SAFE_NO_PAD.encode(random_bytes()))
    }

    /// The verifier as sent in a token request. Crate-internal on purpose:
    /// the only caller is [`super::OauthClient::exchange`].
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    /// `S256`: base64url-no-pad of the SHA-256 of the verifier's ASCII.
    pub(crate) fn challenge(&self) -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(self.0.as_bytes()))
    }
}

/// `PendingLogin::wait_for_code` consumes the login, so a caller has to keep
/// its own copy of the verifier for the exchange that follows. The clone is
/// wiped on drop like the original.
impl Clone for Verifier {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl Drop for Verifier {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for Verifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Verifier(••••)")
    }
}

impl fmt::Display for Verifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, formatter)
    }
}

/// A fresh `state`, as lowercase hex (both vendors use hex states).
pub(crate) fn state_hex() -> String {
    Uuid::new_v4().simple().to_string()
}

/// 32 uniform bytes from the OS CSPRNG.
///
/// `uuid`'s v4 generator draws from `getrandom`, i.e. `getentropy(2)` on
/// macOS. Two v4 values carry 244 random bits; folding them through SHA-256
/// yields 32 bytes with no version/variant structure left in them. This
/// avoids adding a `rand` dependency for a job the workspace can already do.
fn random_bytes() -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(Uuid::new_v4().as_bytes());
    hasher.update(Uuid::new_v4().as_bytes());
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_is_unreserved_and_long_enough() {
        let verifier = Verifier::generate();
        let value = verifier.as_str();
        // RFC 7636 §4.1: 43..=128 characters from the unreserved set. 32
        // bytes of base64url-no-pad is exactly 43.
        assert_eq!(value.len(), 43);
        assert!(
            value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~')),
            "verifier left the unreserved character set"
        );
    }

    #[test]
    fn challenge_is_the_s256_of_the_verifier_string() {
        let verifier = Verifier::generate();
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_str().as_bytes()));
        assert_eq!(verifier.challenge(), expected);
        assert_ne!(verifier.challenge(), verifier.as_str());
    }

    #[test]
    fn known_vector_matches_rfc_7636() {
        // RFC 7636 appendix B.
        let verifier = Verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".to_owned());
        assert_eq!(
            verifier.challenge(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn two_logins_do_not_share_material() {
        let first = Verifier::generate();
        let second = Verifier::generate();
        assert_ne!(first.as_str(), second.as_str());
        assert_ne!(state_hex(), state_hex());
    }

    #[test]
    fn state_is_hex() {
        let state = state_hex();
        assert_eq!(state.len(), 32);
        assert!(state.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn formatting_never_shows_the_verifier() {
        let verifier = Verifier::generate();
        let rendered = format!("{verifier:?} {verifier}");
        assert!(!rendered.contains(verifier.as_str()));
    }
}
