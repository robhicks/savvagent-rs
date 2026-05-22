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
mod subset;

pub use canvas::HtmlCanvas;
