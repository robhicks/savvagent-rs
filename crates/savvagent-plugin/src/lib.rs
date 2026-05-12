//! `savvagent-plugin` — trait surface and WIT-portable data types.
//!
//! This crate has zero runtime behavior. It defines the data shape that
//! crosses plugin boundaries; the runtime lives in the `savvagent` crate.
//!
//! See `docs/superpowers/specs/2026-05-12-v0.9.0-plugin-system-design.md`.

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]
#![warn(missing_debug_implementations)]
#![warn(missing_docs)]

/// Concrete error type returned by plugin trait methods.
pub mod error;

pub use error::PluginError;

/// ID newtypes and small structural types that cross plugin boundaries.
pub mod types;

/// Owned styled-text types returned by plugin render methods.
pub mod styled;

pub use types::{
    ChordPortable, KeyCodePortable, KeyEventPortable, KeyMods, PluginId,
    ProviderId, Region, ScreenInstanceId, ThemeEntry, ThemePalette, Timestamp,
};

pub use styled::{StyledLine, StyledSpan, TextMods, ThemeColor};
