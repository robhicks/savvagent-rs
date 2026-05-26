//! Host-side implementations of the capabilities every WIT world imports.
//!
//! Each capability lives in its own submodule and is consumed by the
//! corresponding adapter in `crate::adapter` via `Linker::func_wrap_async`.
//! The host-import surface is deliberately small in v0.18.0:
//!
//! - [`log`] — `log(level, msg)`: structured-logging passthrough used by
//!   all three worlds.
//! - [`theme`] — `current-theme()`: snapshot of the active palette's
//!   (name, color) tuples; used by static and interactive worlds.
//!
//! Capabilities specific to the interactive world (`draw-text`,
//! `draw-block`, `draw-line`, `clear-area`) and the provider world
//! (`http-capability`, `keyring-capability`, `progress-capability`) land in
//! Tasks 5 and 6 as sibling submodules.

pub mod log;
pub mod theme;
