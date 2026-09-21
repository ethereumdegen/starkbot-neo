//! The observer seam: the one surface the navigator loop drives.
//!
//! A web page behind CDP (`web::CdpObserver`) and a native app behind the macOS
//! accessibility API answer the same three questions — what is here, is it still
//! here, do this — so the loop in `lib.rs` never names either one.

use serde_json::Value;

use crate::policy::Action;

#[derive(Debug, thiserror::Error)]
pub enum ObserveError {
    /// The decision no longer refers to the observed surface: observe again, decide again.
    #[error("stale page: {0}")]
    Stale(&'static str),
    /// A mutation may already have happened; never retried blindly.
    ///
    /// Owned rather than `&'static str` because the accessibility path's
    /// reasons come from the actor at runtime — "the app may need a commit
    /// key", "`Numbers` is not responding" — and flattening all of them into
    /// one fixed sentence left every failed native run saying only "the
    /// accessibility actor could not answer", which names nothing a person or
    /// a model can act on.
    #[error("uncertain mutation: {0}")]
    Uncertain(std::borrow::Cow<'static, str>),
    #[error(transparent)]
    Cdp(#[from] neo_cdp::CdpError),
}

/// One observable, actionable surface.
///
/// `&mut self` throughout: an observer owns its transport and its post-action
/// bookkeeping, so a step is never concurrent with another step.
#[async_trait::async_trait]
pub trait Observer: Send {
    /// One atomic observation of the surface: `actions`, `text`, `marker`,
    /// `guards`, `scroll` and whatever identifies it (`url`/`title`) — the shape
    /// `policy::build_request` and `policy::action_space` consume.
    async fn observe(&mut self) -> Result<Value, ObserveError>;

    /// Is the surface still the one the decision was made on? With an `action`,
    /// only that target and its context need be unchanged; without one, the
    /// whole surface must be.
    async fn fresh(
        &mut self,
        observation: &Value,
        action: Option<&Action>,
    ) -> Result<bool, ObserveError>;

    /// Execute one chosen action; `text` is the value for a fill.
    ///
    /// Guards on `fresh` first: `ObserveError::Stale` means nothing happened and
    /// the caller should observe again, `Uncertain` means it may have.
    async fn act(
        &mut self,
        action: &Action,
        observation: &Value,
        text: Option<&str>,
    ) -> Result<(), ObserveError>;
}
