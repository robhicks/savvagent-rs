//! Theme selection and persistence for the TUI.
//!
//! Themes are runtime-selectable palettes applied at the render-path
//! boundary; switching themes does not restructure the widget tree.
//! Selection is loaded from `~/.savvagent/theme.toml` at startup and
//! persisted on every successful `/theme <name>` invocation. The render
//! path itself stays in `app.rs` / `ui.rs` (see Tasks 16.5-16.6); this
//! module owns only the type, serialization, and disk I/O.
//!
//! Contract: unknown theme names never crash. [`Theme::from_name`]
//! returns `None`; callers fall back to the active theme and emit a
//! warning line.

// Tasks 16.5-16.6 wire `load`, `save`, and the `Theme` type into the
// render path and `/theme` slash command. Until those land in the same
// PR the public API is unused at the crate level (tests cover the
// internals). The `allow(dead_code)` here is removed by 16.5-16.6.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Built-in theme variants. Wire identifiers (`dark` / `light` /
/// `high-contrast`) are stable and persisted to `theme.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Theme {
    #[default]
    Dark,
    Light,
    HighContrast,
}

impl Theme {
    /// Stable wire name used in `theme.toml` and `/theme <name>`.
    pub fn name(self) -> &'static str {
        match self {
            Theme::Dark => "dark",
            Theme::Light => "light",
            Theme::HighContrast => "high-contrast",
        }
    }

    /// Parse a wire name. Returns `None` for unknown inputs (case-sensitive).
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "dark" => Some(Theme::Dark),
            "light" => Some(Theme::Light),
            "high-contrast" => Some(Theme::HighContrast),
            _ => None,
        }
    }

    /// Every built-in theme, in display order.
    pub fn all() -> [Theme; 3] {
        [Theme::Dark, Theme::Light, Theme::HighContrast]
    }
}

/// On-disk shape of `~/.savvagent/theme.toml`. Single key
/// (`theme = "<wire-name>"`) so a missing file or missing key falls back
/// to [`Theme::default()`].
#[derive(Debug, Serialize, Deserialize)]
struct ThemeConfig {
    #[serde(default)]
    theme: Theme,
}

/// Compute `~/.savvagent/theme.toml`. Returns `None` if `$HOME` is unset
/// or empty (matches the convention in `sandbox.rs::sandbox_toml_path`).
fn config_path() -> Option<PathBuf> {
    let raw = std::env::var_os("HOME")?;
    if raw.is_empty() {
        return None;
    }
    Some(PathBuf::from(raw).join(".savvagent").join("theme.toml"))
}

/// Load the user's theme selection. Returns [`Theme::default()`] if the
/// file is absent, unparseable, or `$HOME` is unset.
///
/// Parse errors are logged at `warn!` level. Missing-file is silent
/// (expected on first run).
pub fn load() -> Theme {
    match config_path() {
        Some(path) => load_from_path(&path),
        None => Theme::default(),
    }
}

/// Load the theme selection from an explicit path. Pure inner used by
/// [`load`] and tests.
pub(crate) fn load_from_path(path: &Path) -> Theme {
    match std::fs::read_to_string(path) {
        Ok(text) => match toml::from_str::<ThemeConfig>(&text) {
            Ok(cfg) => cfg.theme,
            Err(e) => {
                tracing::warn!(
                    "theme.toml at {} failed to parse: {e}. Falling back to default.",
                    path.display()
                );
                Theme::default()
            }
        },
        Err(_) => Theme::default(),
    }
}

/// Persist the selected theme. Returns `Ok(())` on success or if `$HOME`
/// is unset (silent no-op; matches `sandbox.rs::save` behavior for now,
/// though follow-up #20 may tighten this to an error).
pub fn save(theme: Theme) -> std::io::Result<()> {
    match config_path() {
        Some(path) => save_to_path(&path, theme),
        None => Ok(()),
    }
}

pub(crate) fn save_to_path(path: &Path, theme: Theme) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let config = ThemeConfig { theme };
    let text =
        toml::to_string(&config).expect("ThemeConfig serialization is infallible");
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // --- Task 16.2: Theme::from_name + Theme::all ---

    #[test]
    fn from_name_recognises_built_ins() {
        assert_eq!(Theme::from_name("dark"), Some(Theme::Dark));
        assert_eq!(Theme::from_name("light"), Some(Theme::Light));
        assert_eq!(Theme::from_name("high-contrast"), Some(Theme::HighContrast));
    }

    #[test]
    fn from_name_rejects_unknown() {
        assert_eq!(Theme::from_name("solarized"), None);
        assert_eq!(Theme::from_name(""), None);
        assert_eq!(Theme::from_name("DARK"), None);
        assert_eq!(Theme::from_name("HIGH-CONTRAST"), None);
    }

    #[test]
    fn all_returns_three_built_ins() {
        let themes = Theme::all();
        assert_eq!(themes.len(), 3);
        assert!(themes.contains(&Theme::Dark));
        assert!(themes.contains(&Theme::Light));
        assert!(themes.contains(&Theme::HighContrast));
    }

    #[test]
    fn name_round_trips_through_from_name() {
        for t in Theme::all() {
            assert_eq!(Theme::from_name(t.name()), Some(t));
        }
    }

    // --- Task 16.3: theme.toml round-trip ---

    #[test]
    fn theme_config_serializes_and_deserializes() {
        for t in Theme::all() {
            let config = ThemeConfig { theme: t };
            let text = toml::to_string(&config).unwrap();
            let parsed: ThemeConfig = toml::from_str(&text).unwrap();
            assert_eq!(parsed.theme, t);
        }
    }

    #[test]
    fn theme_config_missing_field_defaults_to_dark() {
        let parsed: ThemeConfig = toml::from_str("").unwrap();
        assert_eq!(parsed.theme, Theme::Dark);
    }

    #[test]
    fn theme_config_with_unknown_name_is_a_parse_error() {
        // serde's untagged-enum-via-rename matches strictly; an unknown
        // wire name should fail deserialization, NOT silently default.
        let r: Result<ThemeConfig, _> = toml::from_str(r#"theme = "solarized""#);
        assert!(r.is_err(), "unknown theme name should fail to parse");
    }

    // --- Task 16.4: load_from_path + save_to_path round-trip ---

    #[test]
    fn load_from_path_returns_default_when_file_absent() {
        let td = TempDir::new().unwrap();
        let missing = td.path().join("theme.toml");
        assert_eq!(load_from_path(&missing), Theme::Dark);
    }

    #[test]
    fn load_from_path_returns_default_on_parse_error() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("theme.toml");
        std::fs::write(&path, r#"theme = "totally-bogus""#).unwrap();
        assert_eq!(
            load_from_path(&path),
            Theme::Dark,
            "parse failure must fall back to default, not crash"
        );
    }

    #[test]
    fn save_then_load_round_trips() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("nested").join("theme.toml");
        // Parent dir does not exist yet — save_to_path must create it.
        save_to_path(&path, Theme::HighContrast).unwrap();
        assert_eq!(load_from_path(&path), Theme::HighContrast);
    }

    #[test]
    fn save_overwrites_previous_value() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("theme.toml");
        save_to_path(&path, Theme::Light).unwrap();
        save_to_path(&path, Theme::Dark).unwrap();
        assert_eq!(load_from_path(&path), Theme::Dark);
    }
}
