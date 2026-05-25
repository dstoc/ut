//! Facade over the status overlay session: the real Wayland-backed
//! implementation when the `ui` feature is enabled, and a no-op stub
//! otherwise. Both expose the same `OverlaySession` API consumed by the
//! session loop in `lib.rs`.

#[cfg(feature = "ui")]
mod real;
#[cfg(feature = "ui")]
pub(crate) use real::OverlaySession;

#[cfg(not(feature = "ui"))]
mod stub;
#[cfg(not(feature = "ui"))]
pub(crate) use stub::OverlaySession;
