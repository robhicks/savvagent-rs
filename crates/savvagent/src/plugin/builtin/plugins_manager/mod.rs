//! `internal:plugins-manager` — enable/disable Optional plugins.
//!
//! Task 8.1 ships only the persistence module
//! (`~/.savvagent/plugins.toml` round-trip). Task 8.2 adds the manager
//! screen and the `Plugin` impl.

// Task 8.2 wires `persistence::load`/`save` into the apply_effects path;
// allow dead_code here so the intermediate Task 8.1 state still passes
// `clippy -D warnings`.
#[allow(dead_code)]
pub mod persistence;
