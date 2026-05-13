//! `internal:language` — language catalog + `/language` slash + language picker screen.
//!
//! Mirrors the structure of `internal:themes`: catalog + persistence in
//! `catalog.rs`, picker state machine in `picker.rs` (PR 3), screen
//! adapter in `screen.rs` (PR 3), Plugin impl in this file (PR 3).

pub mod catalog;
pub mod picker;
