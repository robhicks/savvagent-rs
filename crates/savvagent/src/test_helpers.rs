//! Test-only utilities shared across the savvagent crate's unit test modules.
//!
//! `HOME_LOCK` serialises access to the global `$HOME` env variable, so any
//! test that uses [`HomeGuard`] (which rewrites `$HOME` to a temp dir) holds
//! the lock for its lifetime. Both `app::tests` and `theme_command_tests`
//! must import this single mutex — keeping per-module copies would let
//! tests in one module race tests in the other on the process-wide `$HOME`.

#![cfg(test)]

use std::sync::Mutex;

/// Process-wide lock serialising every test that mutates `$HOME`.
pub static HOME_LOCK: Mutex<()> = Mutex::new(());

/// RAII guard: on construction, redirects `$HOME` to a fresh tmpdir; on drop,
/// restores the previous `$HOME` (or unsets it). Must be held while the test
/// touches `$HOME`-rooted paths (e.g. `~/.savvagent/theme.toml`).
pub struct HomeGuard {
    _td: tempfile::TempDir,
    prev: Option<std::ffi::OsString>,
}

impl HomeGuard {
    pub fn new() -> Self {
        let td = tempfile::TempDir::new().expect("tempdir");
        let prev = std::env::var_os("HOME");
        // SAFETY: setting $HOME is unsafe in Rust 2024 because it mutates
        // process-global state. We hold HOME_LOCK for the lifetime of the
        // guard, so no other test reads $HOME concurrently.
        unsafe { std::env::set_var("HOME", td.path()) };
        Self { _td: td, prev }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        // SAFETY: see HomeGuard::new — we still hold HOME_LOCK here.
        unsafe {
            match &self.prev {
                Some(p) => std::env::set_var("HOME", p),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}
