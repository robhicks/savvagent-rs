//! Inline HTML canvas renderer for savvagent.
//!
//! Wraps Blitz to expose a [`HtmlCanvas`] implementing
//! [`savvagent_plugin::ContentRenderer`]. Phase 1 implements only
//! `render`; the eventing surface lands in Phase 2.
//!
//! See `docs/superpowers/specs/2026-05-21-inline-html-canvas-design.md`.

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]
#![warn(missing_debug_implementations)]
#![warn(missing_docs)]

mod canvas;
// Consumed by Phase 2 Task 7 (`HtmlCanvas::focusable_elements`); allow
// dead_code in the interim so CI (-D warnings) stays green between
// per-task PRs in this phase.
#[allow(dead_code)]
mod focus;
mod subset;

/// Cell ↔ pixel coordinate translation helpers.
pub mod coords;
pub use canvas::HtmlCanvas;
pub use coords::{cell_to_pixel, contains_cell, CellPixelSize, CellRect};
