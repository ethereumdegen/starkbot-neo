//! `neo-ax` — the macOS Accessibility actor.
//!
//! This is the only code in the product that touches the AX API or posts a
//! `CGEvent`. It reads native apps as an *element table* (at most 250 rows,
//! each with an index, a role, a label and the operations Jev may ask for)
//! and executes one chosen action at a time, AX first.
//!
//! # Shape
//!
//! `AXUIElement` is not `Send`, so every AX call runs on one dedicated OS
//! thread that owns a `CFRunLoop`. [`AxHandle`] is the `Clone + Send` handle
//! onto it; elements never leave the thread and callers hold plain ids
//! ([`Ref`]) and fingerprints.
//!
//! ```no_run
//! # async fn demo() -> Result<(), neo_ax::AxError> {
//! use neo_ax::{AppSel, AxHandle};
//!
//! if !AxHandle::trusted() {
//!     AxHandle::request_trust();
//! }
//! let ax = AxHandle::spawn()?;
//! let app = ax.activate(&AppSel::Name("TextEdit".into())).await?;
//! let table = ax.table(&AppSel::Pid(app.pid)).await?;
//! println!("{} rows", table.elements.len());
//! # Ok(()) }
//! ```
//!
//! # What this crate refuses
//!
//! * Terminal-class apps, editors with integrated terminals, scripting hosts
//!   and secret stores are on a deny list enforced below every entry point
//!   ([`AxPolicy`]); this is not a shell and not a coding agent (P3).
//! * A secure text field's value is dropped before it is stored anywhere: it
//!   appears as `securefield` with no operations so Jev can answer `BLOCKED`.
//! * No screenshot is ever taken and Screen Recording is never requested
//!   (P10).
//! * Chrome-family browsers belong to the CDP observer, not here.

#![deny(missing_docs)]

mod deny;
mod error;
mod mapping;
mod raw;
mod table;
mod types;

pub use deny::AxPolicy;
pub use error::{AxError, Freshness, StaleReason};
pub use types::{
    ActOutcome, AppInfo, AppSel, AxAction, Checked, Control, Element, ElementTable, Fingerprint,
    Guard, Key, Method, Modifier, Operation, Rect, Ref, ScrollDir, State, WindowInfo,
};

#[cfg(target_os = "macos")]
mod actor;
#[cfg(target_os = "macos")]
mod apps;
#[cfg(target_os = "macos")]
mod input;
#[cfg(target_os = "macos")]
pub mod perm;
#[cfg(target_os = "macos")]
mod sys;

#[cfg(target_os = "macos")]
pub use actor::AxHandle;
#[cfg(target_os = "macos")]
pub use perm::{ACCESSIBILITY_SETTINGS_URL, can_post_events, request_trust, trusted};
