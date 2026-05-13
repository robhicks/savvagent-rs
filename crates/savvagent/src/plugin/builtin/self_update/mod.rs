//! `internal:self-update` — checks for newer releases and self-updates
//! the installed binary.
//!
//! v0.11.0 PR 1 landed the plugin shell + [`InstallMethod`] detection.
//! v0.11.0 PR 2 (this PR) wires the [`HostStarting`](savvagent_plugin::HookKind::HostStarting)
//! hook: on startup the plugin spawns a tokio task that queries the
//! GitHub Releases API, compares against the running binary's version,
//! and stores the result in shared plugin state. The state is read by
//! later PRs from `render_slot` (PR 3) and `handle_slash` (PR 4).
//!
//! See `docs/superpowers/specs/2026-05-13-v0.11.0-tui-self-update-design.md`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, HookKind, HostEvent, Manifest, Plugin, PluginError, PluginId, PluginKind,
};

/// Install-method detection (pure helper + [`std::env::current_exe`]
/// wrapper). Public to the crate so future PRs in this series can wire
/// it into the `/update` apply path.
pub mod install_method;

/// Version-check logic: GitHub Releases query, semver compare, and the
/// [`UpdateState`] enum that downstream UI surfaces (banner, `/update`)
/// read.
pub mod check;

pub use check::{GithubReleasesFetcher, ReleasesFetcher, UpdateState, check_for_update};
pub use install_method::{InstallMethod, detect};

/// TUI self-update plugin.
///
/// Holds the install-method classification (captured once at construction)
/// and the in-memory [`UpdateState`] mutated by the `HostStarting` task.
/// Registered as [`PluginKind::Core`] — the v0.11.0 PR 3 opt-out flags
/// (env var + CLI) provide the disable affordance instead of the
/// user-toggle plugin manager.
pub struct SelfUpdatePlugin {
    /// Cached install-method classification. Captured at construction
    /// because `current_exe()` is stable for the process lifetime.
    install_method: InstallMethod,
    /// Shared state mutated by the spawned `HostStarting` task and read
    /// by `render_slot` (PR 3) / `handle_slash` (PR 4). `std::sync::Mutex`
    /// suffices — writes happen once per launch; reads happen on the
    /// render hot path but only under `try_lock` (PR 3 wiring).
    state: Arc<Mutex<UpdateState>>,
    /// Fetcher used by the spawned check task. The default constructor
    /// installs [`GithubReleasesFetcher`]; tests inject a stub via
    /// [`SelfUpdatePlugin::with_fetcher`].
    fetcher: Arc<dyn ReleasesFetcher>,
}

impl SelfUpdatePlugin {
    /// Construct a new [`SelfUpdatePlugin`] backed by the production
    /// [`GithubReleasesFetcher`].
    pub fn new() -> Self {
        Self::with_fetcher(Arc::new(GithubReleasesFetcher))
    }

    /// Construct a [`SelfUpdatePlugin`] with a custom [`ReleasesFetcher`].
    /// Used by tests to inject a stub that returns canned tag values
    /// without touching the network.
    pub fn with_fetcher(fetcher: Arc<dyn ReleasesFetcher>) -> Self {
        Self {
            install_method: detect(),
            state: Arc::new(Mutex::new(UpdateState::Unknown)),
            fetcher,
        }
    }

    /// Returns the cached install-method classification.
    #[allow(dead_code)]
    pub fn install_method(&self) -> InstallMethod {
        self.install_method
    }

    /// Read the current [`UpdateState`]. Used by tests today; PR 3's
    /// `render_slot` impl reads via `state.try_lock()` directly on the
    /// shared `Arc` for the same reason.
    #[allow(dead_code)]
    pub fn state(&self) -> UpdateState {
        self.state.lock().unwrap().clone()
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
        let mut contributions = Contributions::default();
        contributions.hooks = vec![HookKind::HostStarting];

        Manifest {
            id: PluginId::new("internal:self-update").expect("valid built-in id"),
            name: "Self update".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: rust_i18n::t!("plugin.self-update-description").to_string(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    async fn on_event(&mut self, event: HostEvent) -> Result<Vec<Effect>, PluginError> {
        if !matches!(event, HostEvent::HostStarting) {
            return Ok(vec![]);
        }

        // Spawn the version check on the runtime so the hook dispatcher
        // returns immediately. The task writes the result back into the
        // shared state for `render_slot` / `handle_slash` to consume.
        let state = Arc::clone(&self.state);
        let fetcher = Arc::clone(&self.fetcher);
        let install_method = self.install_method;
        let current_version = env!("CARGO_PKG_VERSION").to_string();

        tokio::spawn(async move {
            let result = check_for_update(&current_version, install_method, fetcher.as_ref()).await;
            if let Ok(mut guard) = state.lock() {
                *guard = result;
            }
        });

        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::HOME_LOCK;
    use async_trait::async_trait;

    /// In-test releases fetcher that returns a fixed tag.
    struct FixedFetcher(&'static str);

    #[async_trait]
    impl ReleasesFetcher for FixedFetcher {
        async fn latest_tag(&self) -> anyhow::Result<String> {
            Ok(self.0.to_string())
        }
    }

    #[test]
    fn manifest_subscribes_to_host_starting() {
        let _lock = HOME_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");

        let p = SelfUpdatePlugin::new();
        let m = p.manifest();
        assert_eq!(m.id.as_str(), "internal:self-update");
        assert_eq!(m.contributions.hooks, vec![HookKind::HostStarting]);
        // Slot/slash arrive in PR 3/4.
        assert!(m.contributions.slots.is_empty());
        assert!(m.contributions.slash_commands.is_empty());
    }

    #[test]
    fn initial_state_is_unknown() {
        let p = SelfUpdatePlugin::new();
        assert_eq!(p.state(), UpdateState::Unknown);
    }

    #[tokio::test]
    async fn host_starting_spawns_check_that_updates_state() {
        // Inject a stub that reports a newer version so the resulting
        // state is `Available` (or `Disabled` if the runtime's
        // current_exe() detects a dev build).
        let fetcher = Arc::new(FixedFetcher("v99.99.99"));
        let mut p = SelfUpdatePlugin::with_fetcher(fetcher);
        let install_method = p.install_method();

        p.on_event(HostEvent::HostStarting).await.unwrap();

        // The spawned task runs on the tokio runtime; yield repeatedly
        // until the state transitions away from Unknown. Bound to a
        // sensible iteration cap so a regression doesn't hang the suite.
        for _ in 0..100 {
            tokio::task::yield_now().await;
            let s = p.state();
            if !matches!(s, UpdateState::Unknown) {
                match install_method {
                    InstallMethod::Dev => assert_eq!(s, UpdateState::Disabled),
                    InstallMethod::Installed => assert!(matches!(s, UpdateState::Available { .. })),
                }
                return;
            }
        }
        panic!("state never transitioned away from Unknown");
    }

    #[tokio::test]
    async fn other_events_are_ignored() {
        let fetcher = Arc::new(FixedFetcher("v99.99.99"));
        let mut p = SelfUpdatePlugin::with_fetcher(fetcher);

        // TurnStart should not touch the state.
        p.on_event(HostEvent::TurnStart { turn_id: 1 })
            .await
            .unwrap();
        tokio::task::yield_now().await;
        assert_eq!(p.state(), UpdateState::Unknown);
    }
}
