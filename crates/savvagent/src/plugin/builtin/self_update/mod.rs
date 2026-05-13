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
    Contributions, Effect, HookKind, HostEvent, Manifest, Plugin, PluginError, PluginId,
    PluginKind, Region, SlotSpec, StyledLine, StyledSpan, TextMods, ThemeColor,
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

/// Environment variable that disables the update check + `/update` apply
/// path entirely. Any non-empty value short-circuits the plugin to
/// [`UpdateState::Disabled`].
const OPT_OUT_ENV_VAR: &str = "SAVVAGENT_NO_UPDATE_CHECK";

/// CLI flag (parsed via `std::env::args`) with the same effect as the
/// env var. Cheaper than pulling in clap for one boolean.
const OPT_OUT_CLI_FLAG: &str = "--no-update-check";

/// Slot id the plugin contributes to. The TUI's `ui.rs` reserves a
/// one-row chunk for this slot above the existing `home.tips` row;
/// `render_slot` returns an empty Vec when there is no update available
/// so the row paints as theme background only.
const BANNER_SLOT_ID: &str = "home.banner";

/// Inspect the process environment + argv for the opt-out signal. Pure
/// helper (works on any iterator + env lookup) so unit tests can verify
/// both branches without mutating real env state.
fn opt_out_from(
    env_lookup: impl FnOnce(&str) -> Option<String>,
    args: impl IntoIterator<Item = String>,
) -> bool {
    if env_lookup(OPT_OUT_ENV_VAR)
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        return true;
    }
    args.into_iter().any(|a| a == OPT_OUT_CLI_FLAG)
}

/// Production wrapper that reads from `std::env` + `std::env::args`.
fn opt_out_active() -> bool {
    opt_out_from(|k| std::env::var(k).ok(), std::env::args())
}

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
    /// without touching the network. Honors the `SAVVAGENT_NO_UPDATE_CHECK`
    /// env var and `--no-update-check` CLI flag — when either is set the
    /// plugin starts in [`UpdateState::Disabled`] and `on_event` is a
    /// no-op.
    pub fn with_fetcher(fetcher: Arc<dyn ReleasesFetcher>) -> Self {
        let initial = if opt_out_active() {
            UpdateState::Disabled
        } else {
            UpdateState::Unknown
        };
        Self {
            install_method: detect(),
            state: Arc::new(Mutex::new(initial)),
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
        contributions.slots = vec![SlotSpec {
            slot_id: BANNER_SLOT_ID.into(),
            priority: 100,
        }];

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

        // If opt-out was set at construction the initial state is already
        // `Disabled` — skip the spawn so we don't even build a tokio task.
        if matches!(*self.state.lock().unwrap(), UpdateState::Disabled) {
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

    fn render_slot(&self, slot_id: &str, _region: Region) -> Vec<StyledLine> {
        if slot_id != BANNER_SLOT_ID {
            return vec![];
        }
        // `try_lock` keeps the render hot path non-blocking: if the
        // `HostStarting` task happens to hold the lock for a write, we
        // simply skip this frame and the banner shows on the next.
        let Ok(guard) = self.state.try_lock() else {
            return vec![];
        };
        let UpdateState::Available { current, latest } = &*guard else {
            return vec![];
        };
        let text = rust_i18n::t!(
            "self-update.banner-available",
            current = current.to_string(),
            latest = latest.to_string()
        )
        .to_string();
        vec![StyledLine {
            spans: vec![StyledSpan {
                text,
                fg: Some(ThemeColor::Accent),
                bg: None,
                modifiers: TextMods::default(),
            }],
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::HOME_LOCK;
    use async_trait::async_trait;
    use semver::Version;

    /// In-test releases fetcher that returns a fixed tag.
    struct FixedFetcher(&'static str);

    #[async_trait]
    impl ReleasesFetcher for FixedFetcher {
        async fn latest_tag(&self) -> anyhow::Result<String> {
            Ok(self.0.to_string())
        }
    }

    fn dummy_region() -> Region {
        Region {
            x: 0,
            y: 0,
            width: 80,
            height: 1,
        }
    }

    #[test]
    fn manifest_subscribes_to_host_starting_and_contributes_banner_slot() {
        let _lock = HOME_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");

        let p = SelfUpdatePlugin::new();
        let m = p.manifest();
        assert_eq!(m.id.as_str(), "internal:self-update");
        assert_eq!(m.contributions.hooks, vec![HookKind::HostStarting]);
        assert_eq!(m.contributions.slots.len(), 1);
        assert_eq!(m.contributions.slots[0].slot_id, BANNER_SLOT_ID);
        // Slash arrives in PR 4.
        assert!(m.contributions.slash_commands.is_empty());
    }

    #[test]
    fn initial_state_is_unknown_when_not_opted_out() {
        // The plugin reads real env/argv on construction; this test runs
        // under cargo test, whose argv does not include the opt-out flag
        // and whose env (in CI / typical local dev) does not set
        // SAVVAGENT_NO_UPDATE_CHECK. If the developer happens to have
        // that env var set, this assertion is a no-op rather than a
        // misleading failure.
        if std::env::var(OPT_OUT_ENV_VAR)
            .ok()
            .is_some_and(|v| !v.is_empty())
        {
            return;
        }
        let p = SelfUpdatePlugin::new();
        assert_eq!(p.state(), UpdateState::Unknown);
    }

    // --- opt_out_from pure helper ---

    #[test]
    fn opt_out_env_var_with_value_disables() {
        assert!(opt_out_from(
            |k| if k == OPT_OUT_ENV_VAR {
                Some("1".into())
            } else {
                None
            },
            std::iter::empty::<String>(),
        ));
    }

    #[test]
    fn opt_out_env_var_empty_does_not_disable() {
        assert!(!opt_out_from(
            |k| if k == OPT_OUT_ENV_VAR {
                Some(String::new())
            } else {
                None
            },
            std::iter::empty::<String>(),
        ));
    }

    #[test]
    fn opt_out_cli_flag_disables() {
        assert!(opt_out_from(
            |_| None,
            vec!["savvagent".to_string(), "--no-update-check".to_string(),],
        ));
    }

    #[test]
    fn opt_out_returns_false_when_neither_set() {
        assert!(!opt_out_from(|_| None, vec!["savvagent".to_string()],));
    }

    // --- render_slot ---

    #[test]
    fn render_slot_returns_empty_for_unknown_state() {
        let _lock = HOME_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");

        let p = SelfUpdatePlugin::with_fetcher(Arc::new(FixedFetcher("v99.99.99")));
        let lines = p.render_slot(BANNER_SLOT_ID, dummy_region());
        assert!(lines.is_empty());
    }

    #[test]
    fn render_slot_returns_empty_for_up_to_date() {
        let _lock = HOME_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");

        let p = SelfUpdatePlugin::with_fetcher(Arc::new(FixedFetcher("v99.99.99")));
        *p.state.lock().unwrap() = UpdateState::UpToDate;
        let lines = p.render_slot(BANNER_SLOT_ID, dummy_region());
        assert!(lines.is_empty());
    }

    #[test]
    fn render_slot_returns_empty_for_disabled() {
        let _lock = HOME_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");

        let p = SelfUpdatePlugin::with_fetcher(Arc::new(FixedFetcher("v99.99.99")));
        *p.state.lock().unwrap() = UpdateState::Disabled;
        let lines = p.render_slot(BANNER_SLOT_ID, dummy_region());
        assert!(lines.is_empty());
    }

    #[test]
    fn render_slot_renders_banner_when_available() {
        let _lock = HOME_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");

        let p = SelfUpdatePlugin::with_fetcher(Arc::new(FixedFetcher("v99.99.99")));
        *p.state.lock().unwrap() = UpdateState::Available {
            current: Version::parse("0.10.0").unwrap(),
            latest: Version::parse("0.11.0").unwrap(),
        };
        let lines = p.render_slot(BANNER_SLOT_ID, dummy_region());
        assert_eq!(lines.len(), 1);
        let text = &lines[0].spans[0].text;
        assert!(
            text.contains("0.10.0") && text.contains("0.11.0"),
            "expected both versions in banner, got: {text}"
        );
        assert!(
            text.contains("/update"),
            "expected /update hint, got: {text}"
        );
    }

    #[test]
    fn render_slot_ignores_other_slot_ids() {
        let _lock = HOME_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");

        let p = SelfUpdatePlugin::with_fetcher(Arc::new(FixedFetcher("v99.99.99")));
        *p.state.lock().unwrap() = UpdateState::Available {
            current: Version::parse("0.10.0").unwrap(),
            latest: Version::parse("0.11.0").unwrap(),
        };
        let lines = p.render_slot("home.tips", dummy_region());
        assert!(lines.is_empty());
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
