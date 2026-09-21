//! The platform backends.
//!
//! Everything else in this crate is portable. [`crate::ElementTable`],
//! [`crate::Guard`], [`crate::Freshness`] and [`crate::ActOutcome`] are plain
//! data; `mapping` and `table` turn a walked tree into them. A backend is the
//! part that talks to one operating system's accessibility API, produces that
//! tree, and executes one action against it.
//!
//! # Adding one
//!
//! `unsupported` is what a platform with no backend gets: it refuses at
//! [`AxHandle::spawn`] and names the platform, rather than reporting a
//! permission problem that does not exist there.
//!
//! `mac` is `AXUIElement` plus `CGEvent`; `atspi` is AT-SPI2 over D-Bus
//! plus Hyprland IPC plus the two Wayland virtual-input protocols. They
//! share every line above this module — `mapping`, `table`, `deny`, `raw`
//! and `types` — and neither of them is named anywhere else in the
//! workspace: `lib.rs`, `jev-nav` and `neo-agent` see one `AxHandle`.

#[cfg(target_os = "linux")]
pub mod atspi;
#[cfg(target_os = "macos")]
pub mod mac;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub mod unsupported;

#[cfg(target_os = "linux")]
pub use atspi::{AxHandle, perm};
#[cfg(target_os = "macos")]
pub use mac::{AxHandle, perm};
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub use unsupported::{AxHandle, perm};
