//! Read/write `~/.savvagent/plugins.toml`. Atomic via tempfile + rename.
//!
//! Robustness rules:
//! * missing file -> empty map (first run / no overrides).
//! * malformed TOML -> warn + empty map (don't crash the TUI on a hand-edit).
//! * `schema_version` mismatch -> warn + empty map (forward-compat; we
//!   don't try to upgrade older shapes silently).
//! * unknown plugin id -> warn + skip that entry.
//!
//! Layout convention matches `theme.toml` in the same directory: keyed by
//! the full plugin id (e.g. `"internal:provider-anthropic"`) so the file
//! stays stable across releases that add/remove plugins.

use std::collections::HashMap;
use std::path::PathBuf;

use savvagent_plugin::PluginId;
use serde::{Deserialize, Serialize};

/// On-disk shape of `~/.savvagent/plugins.toml`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PluginsToml {
    /// Forward-compatibility marker. Bump when the file shape changes
    /// incompatibly so older binaries can refuse to interpret it.
    pub schema_version: u32,
    /// Per-plugin override entries, keyed by the plugin's full id.
    #[serde(default)]
    pub plugins: HashMap<String, PluginEntry>,
}

/// One row in the `[plugins.<id>]` table.
#[derive(Debug, Serialize, Deserialize)]
pub struct PluginEntry {
    /// Whether this Optional plugin should be enabled at startup.
    pub enabled: bool,
}

/// Current `schema_version` written by [`save`]. Bumped only on incompatible
/// shape changes — additive fields stay at this value.
const SCHEMA_VERSION: u32 = 1;

/// Compute the path to `~/.savvagent/plugins.toml`. Returns a sentinel
/// (`/.savvagent/plugins.toml`) if `$HOME` is unset or empty so callers can
/// still proceed; the `load`/`save` callers treat missing reads as empty
/// and silently log on write failures.
pub fn config_path() -> PathBuf {
    let home = match std::env::var_os("HOME") {
        Some(s) if !s.is_empty() => PathBuf::from(s),
        _ => PathBuf::from("/"),
    };
    home.join(".savvagent").join("plugins.toml")
}

/// Load the persisted enabled-state map. See module docs for robustness rules.
pub fn load() -> HashMap<PluginId, bool> {
    let path = config_path();
    let mut out = HashMap::new();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return out;
    };
    let parsed: PluginsToml = match toml::from_str(&text) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "plugins.toml malformed; ignoring");
            return out;
        }
    };
    if parsed.schema_version != SCHEMA_VERSION {
        tracing::warn!(
            seen = parsed.schema_version,
            expected = SCHEMA_VERSION,
            "plugins.toml schema_version mismatch; ignoring file"
        );
        return out;
    }
    for (id_str, entry) in parsed.plugins {
        match PluginId::new(&id_str) {
            Ok(pid) => {
                out.insert(pid, entry.enabled);
            }
            Err(e) => tracing::warn!(
                id = %id_str,
                error = %e,
                "plugins.toml: invalid plugin id; skipping"
            ),
        }
    }
    out
}

/// Persist the enabled-state map atomically. Creates the parent directory
/// with `0o700`, writes via a sibling `.tmp` file with `0o600`, then
/// renames it into place. Only Optional plugins should appear in the
/// passed-in map — Core plugins are never written.
pub fn save(entries: &HashMap<PluginId, bool>) -> std::io::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }
    let toml = PluginsToml {
        schema_version: SCHEMA_VERSION,
        plugins: entries
            .iter()
            .map(|(id, enabled)| (id.as_str().to_string(), PluginEntry { enabled: *enabled }))
            .collect(),
    };
    let serialized = toml::to_string_pretty(&toml).map_err(io_err)?;

    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, serialized)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn io_err(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{HOME_LOCK, HomeGuard};

    #[test]
    fn missing_file_yields_empty_map() {
        let _g = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let entries = load();
        assert!(entries.is_empty());
    }

    #[test]
    fn schema_version_mismatch_returns_empty() {
        let _g = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        // Materialise the directory under the per-test $HOME so the
        // mismatched file actually lives at the path `load` reads.
        let path = config_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"schema_version = 99

[plugins."internal:provider-anthropic"]
enabled = false
"#,
        )
        .unwrap();
        let entries = load();
        assert!(
            entries.is_empty(),
            "schema-version mismatch should drop the entire file"
        );
    }

    #[test]
    fn malformed_toml_returns_empty() {
        let _g = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let path = config_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "this is not valid toml ===").unwrap();
        let entries = load();
        assert!(entries.is_empty());
    }

    #[test]
    fn unknown_plugin_id_is_skipped() {
        let _g = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let path = config_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"schema_version = 1

[plugins."NOT A VALID ID"]
enabled = false

[plugins."internal:provider-anthropic"]
enabled = true
"#,
        )
        .unwrap();
        let entries = load();
        // The bad id is skipped; the good id is loaded.
        assert_eq!(entries.len(), 1);
        let pid = PluginId::new("internal:provider-anthropic").unwrap();
        assert!(entries[&pid]);
    }

    #[test]
    fn round_trip_preserves_enabled_state() {
        let _g = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let mut input = HashMap::new();
        input.insert(
            PluginId::new("internal:provider-anthropic").expect("valid"),
            true,
        );
        input.insert(
            PluginId::new("internal:provider-local").expect("valid"),
            false,
        );
        save(&input).unwrap();
        let loaded = load();
        assert_eq!(loaded.len(), 2);
        assert!(loaded[&PluginId::new("internal:provider-anthropic").expect("valid")]);
        assert!(!loaded[&PluginId::new("internal:provider-local").expect("valid")]);
    }
}
