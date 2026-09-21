//! The Linux backend: AT-SPI2 over D-Bus, with Wayland virtual input as the
//! fallback (17 §3).
//!
//! `bus` is the transport — one accessibility-bus connection, one call at a
//! time, each with a deadline. `walk` turns a tree into the `RawNode`s the
//! pure layers already know how to prune and budget. `wm` enumerates windows
//! and moves focus through the compositor, `apps` resolves and launches
//! applications by desktop entry, `input` is the `zwp_virtual_keyboard_v1` /
//! `zwlr_virtual_pointer_v1` fallback, and `perm` answers the three
//! permission questions the macOS callers ask with the truth: there is no
//! permission here.
//!
//! Everything above this module is shared. `mapping`, `table`, `deny`,
//! `raw` and `types` compile on both platforms and neither of them learns
//! which backend produced a tree.

mod actor;
pub(crate) mod apps;
mod bus;
mod input;
pub mod perm;
mod walk;
mod wm;

pub use actor::AxHandle;
