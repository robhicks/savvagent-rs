//! Canonical list of sensitive paths that tool spawns must not be able to
//! read, write, or otherwise touch.
//!
//! Two consumers:
//!
//! - **In-process permission layer** ([`is_sensitive_path`]). Evaluated by
//!   `permissions.rs::evaluate` against the path string passed in a tool
//!   call. Catches the case where the agent tries to call
//!   `tool-fs/read_file { path: ".env" }` or
//!   `tool-fs/read_file { path: "/home/user/.ssh/id_rsa" }`.
//!
//! - **OS sandbox layer** ([`sensitive_paths_for_user`]). Returns absolute
//!   paths under `$HOME` that the sandbox should overlay with empty mounts
//!   (`bwrap --tmpfs` / `--ro-bind /dev/null` on Linux,
//!   `(deny file-read* …)` on macOS). Defense in depth: even if a
//!   compromised tool bypasses the in-process check, the kernel refuses
//!   the read.
//!
//! The OS sandbox list lives under `$HOME`; the in-process check is a
//! superset that also covers project-relative `.env*` files.
//!
//! Single source of truth — both consumers import from here. Drift
//! between layers is impossible by construction.

use std::path::PathBuf;

/// Sensitive directories under `$HOME` whose contents must not be readable
/// by tool spawns. Returned paths are absolute and pre-expanded against
/// the current user's home directory. Paths that do not exist on disk are
/// silently filtered out — there is nothing to overlay.
pub fn sensitive_paths_for_user() -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let mut candidates: Vec<PathBuf> = vec![
        home.join(".ssh"),
        home.join(".aws"),
        home.join(".gnupg"),
        home.join(".netrc"),
        home.join(".config").join("gh"),
        home.join(".mozilla"),
        home.join(".config").join("google-chrome"),
    ];
    if cfg!(target_os = "macos") {
        candidates.push(
            home.join("Library")
                .join("Application Support")
                .join("Firefox"),
        );
        candidates.push(
            home.join("Library")
                .join("Application Support")
                .join("Google")
                .join("Chrome"),
        );
    }
    candidates.into_iter().filter(|p| p.exists()).collect()
}

/// Returns `true` if `path` names a sensitive resource that tool spawns
/// must not read or write. Covers:
///
/// - `.env` and `.env.*` files anywhere in the path.
/// - Any path whose components include one of the sensitive home
///   directories listed in [`sensitive_paths_for_user`] (`.ssh`, `.aws`,
///   `.gnupg`, `gh`, `google-chrome`, `.mozilla`).
/// - Absolute paths that fall *under* any of those directories.
pub fn is_sensitive_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    if dotenv_match(&normalized) {
        return true;
    }
    sensitive_segment_match(&normalized)
}

fn dotenv_match(path: &str) -> bool {
    let last = path.rsplit('/').next().unwrap_or("");
    last == ".env" || last.starts_with(".env.")
}

fn sensitive_segment_match(path: &str) -> bool {
    const SEGMENTS: &[&str] = &[
        ".ssh", ".aws", ".gnupg", ".netrc", ".mozilla", "google-chrome",
    ];
    for seg in SEGMENTS {
        if path == *seg
            || path.starts_with(&format!("{seg}/"))
            || path.contains(&format!("/{seg}/"))
            || path.ends_with(&format!("/{seg}"))
        {
            return true;
        }
    }
    // Special-case ~/.config/gh — `gh` is too short to match
    // unqualified, so require the `.config/gh` segment combination.
    path == ".config/gh"
        || path.starts_with(".config/gh/")
        || path.contains("/.config/gh/")
        || path.ends_with("/.config/gh")
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
