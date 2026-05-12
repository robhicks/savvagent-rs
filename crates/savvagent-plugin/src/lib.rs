//! `savvagent-plugin` — trait surface and WIT-portable data types.
//!
//! This crate has zero runtime behavior. It defines the data shape that
//! crosses plugin boundaries; the runtime lives in the `savvagent` crate.
//!
//! See `docs/superpowers/specs/2026-05-12-v0.9.0-plugin-system-design.md`.

#![deny(rust_2018_idioms)]
#![warn(missing_debug_implementations)]
