#![forbid(unsafe_code)]
//! Everything that asks Jev a question outside a navigator step: intake,
//! routing, gates and the verdict log (05 §1).
//!
//! Today it holds the one piece the credential screens need: the TypeSafe key
//! validator. `neo-keys` declares the trait and owns no URL; `jev-nav::wire`
//! owns the endpoint; this crate joins them, which is the arrangement 05 §1
//! prescribes.

mod typesafe;

pub use typesafe::TypeSafeKeyValidator;
