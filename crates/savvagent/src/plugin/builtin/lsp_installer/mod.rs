//! `internal:lsp-installer` — `/lsp` slash command, multi-select picker,
//! and one-shot LSP-binary installer.
//!
//! See `docs/superpowers/specs/2026-05-20-lsp-installer-design.md` and
//! `docs/superpowers/plans/2026-05-20-lsp-installer.md`.

pub mod catalog;
pub mod config_writer;
pub mod installer;
pub mod picker;
pub mod screen;
