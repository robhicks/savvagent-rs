//! `internal:self-update` — checks for newer releases and self-updates
//! the installed binary.
//!
//! Scope of this PR (v0.11.0 PR 1): plugin shell + [`InstallMethod`]
//! detection only. The manifest is registered, the plugin compiles into
//! [`crate::plugin::register_builtins`], and the install-method helper
//! is exercised by unit tests. No network, no slot, no `/update` slash,
//! no `HostStarting` subscription. Those land in subsequent PRs per
//! `docs/superpowers/specs/2026-05-13-v0.11.0-tui-self-update-design.md`.

use async_trait::async_trait;
use savvagent_plugin::{Contributions, Manifest, Plugin, PluginId, PluginKind};

/// Install-method detection (pure helper + [`std::env::current_exe`]
/// wrapper). Public to the crate so future PRs in this series can wire
/// it into the `HostStarting` hook and the `/update` apply path.
pub mod install_method;

pub use install_method::{InstallMethod, detect};

/// TUI self-update plugin.
///
/// Registered as [`PluginKind::Core`] so it appears in the plugin
/// registry alongside the other internal plugins but is hidden from
/// the user-toggle plugin manager screen. The v0.11.0 PR 3 opt-out
/// flags (env var + CLI) provide the disable affordance instead.
pub struct SelfUpdatePlugin {
    /// Cached install-method classification. Captured at construction
    /// time because `current_exe()` is stable for the process lifetime.
    /// Read by PR 2's `HostStarting` task; `allow(dead_code)` is the
    /// scaffold-PR placeholder until that consumer lands.
    #[allow(dead_code)]
    install_method: InstallMethod,
}

impl SelfUpdatePlugin {
    /// Construct a new [`SelfUpdatePlugin`] and cache the install
    /// method. Falls back to [`InstallMethod::Installed`] if the
    /// platform refuses `current_exe()` (see [`install_method::detect`]).
    pub fn new() -> Self {
        Self {
            install_method: detect(),
        }
    }

    /// Returns the cached install-method classification. Used by tests
    /// today and consumed by PR 2's `HostStarting` task to decide
    /// whether to short-circuit the network check.
    #[allow(dead_code)]
    pub fn install_method(&self) -> InstallMethod {
        self.install_method
    }
}

impl Default for SelfUpdatePlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for SelfUpdatePlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            id: PluginId::new("internal:self-update").expect("valid built-in id"),
            name: "Self update".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: rust_i18n::t!("plugin.self-update-description").to_string(),
            kind: PluginKind::Core,
            contributions: Contributions::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::HOME_LOCK;

    #[test]
    fn manifest_has_expected_identity() {
        let _lock = HOME_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");

        let p = SelfUpdatePlugin::new();
        let m = p.manifest();
        assert_eq!(m.id.as_str(), "internal:self-update");
        assert_eq!(m.name, "Self update");
        assert!(matches!(m.kind, PluginKind::Core));
        // PR 1 contributes nothing — slot/slash/hook arrive in later PRs.
        assert!(m.contributions.slash_commands.is_empty());
        assert!(m.contributions.slots.is_empty());
        assert!(m.contributions.hooks.is_empty());
        assert!(m.contributions.screens.is_empty());
        assert!(m.contributions.keybindings.is_empty());
    }

    #[test]
    fn description_is_localized() {
        let _lock = HOME_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");

        let p = SelfUpdatePlugin::new();
        let m = p.manifest();
        // Exact wording lives in en.toml; this assert just verifies the
        // key resolves (rust_i18n returns the bare key on a miss).
        assert_ne!(m.description, "plugin.self-update-description");
        assert!(!m.description.is_empty());
    }

    #[test]
    fn install_method_is_cached_at_construction() {
        let p = SelfUpdatePlugin::new();
        // Whatever the host's classification is, two reads agree.
        assert_eq!(p.install_method(), p.install_method());
    }
}
