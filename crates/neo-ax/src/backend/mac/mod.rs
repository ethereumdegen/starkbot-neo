//! The macOS backend: `AXUIElement` through `objc2-application-services`,
//! with `CGEvent` as the input fallback.
//!
//! The whole of it runs on one dedicated OS thread owning a `CFRunLoop`
//! (`actor`), because `AXUIElement` is not `Send`. `sys` is the FFI surface,
//! `apps` is `NSWorkspace`, `input` is `CGEvent`, and `perm` is TCC.

mod actor;
mod apps;
mod input;
pub mod perm;
mod sys;

pub use actor::AxHandle;
