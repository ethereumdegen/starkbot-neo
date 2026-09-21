//! The backend for a platform that has none yet.
//!
//! [`AxHandle::spawn`] refuses and names the platform, so a caller is told
//! "no accessibility backend for linux" instead of being sent to a macOS
//! settings pane it cannot open. Every gate that would normally stand in
//! front of it — the Accessibility grant, the post-event preflight — reports
//! *open* here (see [`perm`]), because there is no such gate on these
//! platforms and a caller that stopped at one would never reach the message
//! that says what is actually missing.
//!
//! Nothing constructs a handle, so no method below can run. They are written
//! out rather than made unreachable by an uninhabited type because every
//! caller is compiled from the macOS shape: proving the rest of a run
//! unreachable turns a correct caller into a page of dead-code warnings.

pub mod perm;

use crate::deny::AxPolicy;
use crate::error::{AxError, Freshness};
use crate::types::{ActOutcome, AppInfo, AppSel, AxAction, ElementTable, Guard};

/// A `Clone + Send` handle onto the AX thread. Never constructed here:
/// [`AxHandle::spawn`] is its only constructor and it always refuses.
#[derive(Clone)]
pub struct AxHandle(());

/// The one answer this backend has.
fn refused() -> AxError {
    AxError::NoBackend {
        platform: std::env::consts::OS,
    }
}

impl AxHandle {
    /// Always fails: there is no accessibility backend on this platform.
    ///
    /// # Errors
    ///
    /// [`AxError::NoBackend`], always.
    pub fn spawn() -> Result<Self, AxError> {
        Err(refused())
    }

    /// Always fails, as [`AxHandle::spawn`]; the deny list never gets a
    /// chance to matter.
    ///
    /// # Errors
    ///
    /// [`AxError::NoBackend`], always.
    pub fn spawn_with_policy(_policy: AxPolicy) -> Result<Self, AxError> {
        Err(refused())
    }

    /// `true`: no accessibility grant gates this platform. What is missing
    /// is the backend, which [`AxHandle::spawn`] reports.
    #[must_use]
    pub fn trusted() -> bool {
        perm::trusted()
    }

    /// `true`, with nothing asked for: there is no prompt to raise.
    #[must_use]
    pub fn request_trust() -> bool {
        perm::request_trust()
    }

    /// Tell the actor whether the session is locked. There is no actor.
    pub fn set_locked(&self, _locked: bool) {}

    /// Stop this actor, for good. There is no actor.
    pub fn stop(&self) {}

    /// Whether [`AxHandle::stop`] has been called. Always `true`: this
    /// handle will never act, and a caller polling it must not spin.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        true
    }

    /// Every regular running application.
    ///
    /// # Errors
    ///
    /// [`AxError::NoBackend`], always.
    pub async fn apps(&self) -> Result<Vec<AppInfo>, AxError> {
        Err(refused())
    }

    /// Bring an app to the front and wait for it to get there.
    ///
    /// # Errors
    ///
    /// [`AxError::NoBackend`], always.
    pub async fn activate(&self, _app: &AppSel) -> Result<AppInfo, AxError> {
        Err(refused())
    }

    /// One observation of an app: the element table of 01 §Element table.
    ///
    /// # Errors
    ///
    /// [`AxError::NoBackend`], always.
    pub async fn table(&self, _app: &AppSel) -> Result<ElementTable, AxError> {
        Err(refused())
    }

    /// Like [`AxHandle::table`], ranking menu leaves by overlap with the
    /// goal.
    ///
    /// # Errors
    ///
    /// [`AxError::NoBackend`], always.
    pub async fn table_for_goal(
        &self,
        _app: &AppSel,
        _goal: Option<&str>,
    ) -> Result<ElementTable, AxError> {
        Err(refused())
    }

    /// Is the surface still the one the decision was made on?
    ///
    /// # Errors
    ///
    /// [`AxError::NoBackend`], always.
    pub async fn guard(&self, _guard: &Guard) -> Result<Freshness, AxError> {
        Err(refused())
    }

    /// Execute one chosen action.
    ///
    /// # Errors
    ///
    /// [`AxError::NoBackend`], always.
    pub async fn act(&self, _action: &AxAction) -> Result<ActOutcome, AxError> {
        Err(refused())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The message a Linux user sees when they ask for a native app run. It
    /// must name the platform and must not be about a permission: the whole
    /// point of this backend is that "not trusted" would be a lie here.
    #[test]
    fn spawning_refuses_by_naming_the_platform() {
        let Err(error) = AxHandle::spawn() else {
            panic!("a platform with no backend cannot spawn an actor");
        };
        let message = error.to_string();
        assert!(
            message.contains(std::env::consts::OS),
            "the refusal must name the platform: {message}"
        );
        assert!(
            AxHandle::trusted(),
            "reporting `not trusted` would send the user after a grant that does not exist"
        );
    }
}
