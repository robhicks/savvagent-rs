//! ~/.savvagent/config.toml schema, load, save, and migration marker.
//! Single source of truth for non-routing knobs (startup connect policy,
//! per-provider connect timeout, migration_v1_done marker).

use std::path::{Path, PathBuf};

use savvagent_host::StartupConnectPolicy;
use savvagent_protocol::ProviderId;
use serde::{Deserialize, Serialize};

/// Typed version of the `startup.policy` config key. Serialises to/from
/// kebab-case strings (`"opt-in"`, `"all"`, `"none"`, `"last-used"`).
/// An unrecognised string in the TOML will fail to deserialise; the
/// `load_or_default` caller logs a warning and falls back to the default
/// rather than silently choosing `OptIn`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum StartupPolicyKind {
    /// Only providers in `startup_providers` are auto-connected.
    #[default]
    OptIn,
    /// Every registered provider is auto-connected.
    All,
    /// No auto-connect; pool starts empty.
    None,
    /// Auto-connect the provider(s) from last session.
    LastUsed,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ConfigFile {
    #[serde(default)]
    pub startup: StartupSection,
    #[serde(default)]
    pub migration: MigrationSection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartupSection {
    #[serde(default)]
    pub policy: StartupPolicyKind,
    #[serde(default)]
    pub startup_providers: Vec<String>,
    #[serde(default = "default_timeout")]
    pub connect_timeout_ms: u64,
}

impl Default for StartupSection {
    fn default() -> Self {
        Self {
            policy: StartupPolicyKind::default(),
            startup_providers: Vec::new(),
            connect_timeout_ms: default_timeout(),
        }
    }
}

fn default_timeout() -> u64 {
    3000
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct MigrationSection {
    #[serde(default)]
    pub v1_done: bool,
}

impl ConfigFile {
    pub fn default_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".savvagent")
            .join("config.toml")
    }

    /// Load from `path`, falling back to [`Self::default`] on file-not-found
    /// or parse error. Parse errors are logged at `warn` level.
    pub fn load_or_default(path: &Path) -> Self {
        let Ok(contents) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        match toml::from_str::<ConfigFile>(&contents) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "config.toml parse failed; falling back to defaults"
                );
                Self::default()
            }
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, text)
    }

    pub fn to_startup_policy(&self) -> StartupConnectPolicy {
        let ids: Vec<ProviderId> = self
            .startup
            .startup_providers
            .iter()
            .filter_map(|s| ProviderId::new(s).ok())
            .collect();
        match self.startup.policy {
            StartupPolicyKind::All => StartupConnectPolicy::All,
            StartupPolicyKind::None => StartupConnectPolicy::None,
            StartupPolicyKind::LastUsed => StartupConnectPolicy::LastUsed(ids),
            StartupPolicyKind::OptIn => StartupConnectPolicy::OptIn(ids),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn missing_file_returns_default() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        let cfg = ConfigFile::load_or_default(&path);
        assert_eq!(cfg.startup.policy, StartupPolicyKind::OptIn);
        assert!(cfg.startup.startup_providers.is_empty());
        assert!(!cfg.migration.v1_done);
    }

    #[test]
    fn round_trip_preserves_fields() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        let mut cfg = ConfigFile::default();
        cfg.startup.policy = StartupPolicyKind::OptIn;
        cfg.startup.startup_providers = vec!["anthropic".into(), "gemini".into()];
        cfg.startup.connect_timeout_ms = 4000;
        cfg.migration.v1_done = true;
        cfg.save(&path).unwrap();

        let loaded = ConfigFile::load_or_default(&path);
        assert_eq!(loaded.startup.policy, StartupPolicyKind::OptIn);
        assert_eq!(
            loaded.startup.startup_providers,
            vec!["anthropic", "gemini"]
        );
        assert_eq!(loaded.startup.connect_timeout_ms, 4000);
        assert!(loaded.migration.v1_done);
    }

    #[test]
    fn policy_string_maps_correctly() {
        let mut cfg = ConfigFile::default();
        cfg.startup.policy = StartupPolicyKind::All;
        assert!(matches!(cfg.to_startup_policy(), StartupConnectPolicy::All));
        cfg.startup.policy = StartupPolicyKind::None;
        assert!(matches!(
            cfg.to_startup_policy(),
            StartupConnectPolicy::None
        ));
        cfg.startup.policy = StartupPolicyKind::OptIn;
        cfg.startup.startup_providers = vec!["anthropic".into()];
        match cfg.to_startup_policy() {
            StartupConnectPolicy::OptIn(ids) => assert_eq!(ids.len(), 1),
            _ => panic!(),
        }
    }

    #[test]
    fn invalid_policy_string_falls_back_to_default() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            "[startup]\npolicy = \"invalid-typo\"\nconnect_timeout_ms = 5000\n",
        )
        .unwrap();
        let cfg = ConfigFile::load_or_default(&path);
        // Falls back entirely to default on parse error.
        assert_eq!(cfg.startup.policy, StartupPolicyKind::OptIn);
        assert_eq!(cfg.startup.connect_timeout_ms, default_timeout());
    }
}
