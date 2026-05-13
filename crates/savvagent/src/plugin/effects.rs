//! `apply_effects` is the single mutation surface for `App` in response
//! to plugin output. Every Effect variant maps to one App method (or
//! recurses for Stack). Slash re-entrancy depth is enforced here so that
//! the depth cap cannot be bypassed by constructing a fresh SlashRouter.

use std::collections::HashMap;

use savvagent_plugin::{Effect, HostEvent, PluginId, PluginKind, ScreenArgs};

use crate::app::App;
use crate::plugin::builtin::command_palette::screen::{PaletteCommand, PaletteScreen};
use crate::plugin::builtin::plugins_manager::screen::{PluginRow, PluginsManagerScreen};
use crate::plugin::builtin::plugins_manager::{persistence, summarize_contributions};
use crate::plugin::hooks::HookDispatcher;
use crate::plugin::manifests::Indexes;
use crate::plugin::slash::SlashRouter;

/// Maximum number of nested dispatch entries before an error is returned.
/// Gates both `Effect::RunSlash` re-entries from `apply_effects` and the
/// `dispatch_host_event` recursion that fires when a `RegisterProvider`
/// effect emits a `ProviderRegistered` host event subscribers might react
/// to with more effects. Sharing one counter prevents a subscriber that
/// emits hook-triggering effects from `on_event` from recursing unboundedly
/// (the previous code only capped RunSlash).
const MAX_DISPATCH_DEPTH: u8 = 4;

/// Apply each effect in sequence, mutating `App` state. Used by the event
/// loop after dispatching key events, hook events, or slash commands.
///
/// Returns `Err(String)` if an effect referenced an unknown screen or slash
/// command, or if dispatch recursion exceeded [`MAX_DISPATCH_DEPTH`].
pub async fn apply_effects(app: &mut App, effects: Vec<Effect>) -> Result<(), String> {
    apply_effects_with_depth(app, effects, 0).await
}

async fn apply_effects_with_depth(
    app: &mut App,
    effects: Vec<Effect>,
    depth: u8,
) -> Result<(), String> {
    for eff in effects {
        apply_one(app, eff, depth).await?;
    }
    Ok(())
}

async fn apply_one(app: &mut App, eff: Effect, depth: u8) -> Result<(), String> {
    match eff {
        Effect::PushNote { line } => app.push_styled_note(line),
        Effect::OpenScreen { id, args } => open_screen(app, &id, args).await?,
        Effect::CloseScreen => {
            app.screen_stack.pop();
        }
        Effect::SetActiveTheme { slug, persist } => {
            app.set_active_theme_by_slug(slug);
            if persist {
                app.persist_config();
            }
        }
        Effect::SetActiveLocale { code, persist } => {
            let changed = app.set_active_language(code);
            if changed && persist {
                app.persist_language();
            }
        }
        Effect::SetActiveProvider { id, persist } => {
            app.set_active_provider(id);
            if persist {
                app.persist_config();
            }
        }
        Effect::RegisterProvider { id, display_name } => {
            // Map ProviderId → owning PluginId by convention. Every built-in
            // provider plugin uses the id pattern `internal:provider-<provider>`.
            let plugin_id = match PluginId::new(format!("internal:provider-{}", id.as_str())) {
                Ok(pid) => pid,
                Err(e) => {
                    tracing::warn!(error = %e, provider_id = %id.as_str(),
                        "RegisterProvider with invalid provider id; skipping");
                    app.push_styled_note(savvagent_plugin::StyledLine::plain(format!(
                        "Invalid provider id from plugin: {}",
                        id.as_str()
                    )));
                    return Ok(());
                }
            };
            let registry = match &app.plugin_registry {
                Some(r) => r.clone(),
                None => {
                    tracing::error!(
                        "RegisterProvider before plugin runtime installed; \
                         the user-facing /connect flow cannot complete until \
                         app startup finishes installing the runtime."
                    );
                    app.push_styled_note(savvagent_plugin::StyledLine::plain(
                        "Plugin runtime not yet installed; /connect requires app startup to complete.",
                    ));
                    return Ok(());
                }
            };
            let client = {
                let reg = registry.read().await;
                reg.take_provider_client(&plugin_id).await
            };
            if let Some(client) = client {
                app.register_provider(id.clone(), display_name.clone(), client);
                // Notify subscribers via HostEvent::ProviderRegistered so e.g.
                // internal:connect can refresh its candidate list, then fire
                // HostEvent::Connect so HUD subscribers (notably
                // internal:splash) flip to "connected". Both events go
                // through the same depth-tracked path; using
                // `depth.saturating_add(1)` keeps the shared
                // MAX_DISPATCH_DEPTH cap honest across the two emissions.
                Box::pin(dispatch_host_event(
                    app,
                    HostEvent::ProviderRegistered {
                        id: id.clone(),
                        display_name,
                    },
                    depth.saturating_add(1),
                ))
                .await?;
                Box::pin(dispatch_host_event(
                    app,
                    HostEvent::Connect {
                        provider_id: id.clone(),
                    },
                    depth.saturating_add(1),
                ))
                .await?;
            } else {
                tracing::error!(
                    provider_id = %id.as_str(),
                    plugin_id = %plugin_id.as_str(),
                    "Effect::RegisterProvider: take_provider_client returned None — \
                     possible dual-instance bug or plugin lifecycle issue."
                );
                app.push_styled_note(savvagent_plugin::StyledLine::plain(format!(
                    "provider `{}` announced but no client was constructed",
                    id.as_str()
                )));
            }
        }
        Effect::SaveTranscript { path } => match app.save_transcript_to(path.clone()) {
            Ok(()) => {
                app.push_styled_note(savvagent_plugin::StyledLine::plain(format!(
                    "Transcript saved to {path}"
                )));
                // Notify subscribers (e.g. `internal:resume`) that a
                // transcript was just persisted. The autosave path in
                // `main.rs` already dispatches `TranscriptSaved` after
                // each `TurnComplete`; user-initiated saves via `/save`
                // must do the same so `/resume`'s candidate list
                // refreshes without waiting for the next autosave.
                if let Err(err) = Box::pin(dispatch_host_event(
                    app,
                    HostEvent::TranscriptSaved { path: path.clone() },
                    depth.saturating_add(1),
                ))
                .await
                {
                    tracing::warn!(error = %err, path = %path,
                        "TranscriptSaved dispatch from Effect::SaveTranscript failed");
                }
            }
            Err(e) => {
                tracing::error!(error = %e, path = %path, "save_transcript failed");
                app.push_styled_note(savvagent_plugin::StyledLine::plain(format!(
                    "Save failed ({path}): {e}"
                )));
            }
        },
        Effect::PromptSend { text } => app.submit_prompt(text),
        Effect::RunSlash { name, args } => {
            if depth >= MAX_DISPATCH_DEPTH {
                return Err(format!(
                    "Effect::RunSlash depth limit ({MAX_DISPATCH_DEPTH}) exceeded by slash '{name}'"
                ));
            }
            run_slash(app, name, args, depth + 1).await?;
        }
        Effect::ClearLog => app.clear_log(),
        Effect::PrefillInput { text } => app.prefill_input(text),
        Effect::Quit => app.request_quit(),
        Effect::TogglePlugin { id, enabled } => {
            apply_toggle_plugin(app, id, enabled).await?;
        }
        Effect::Stack(children) => {
            // Recurse via Box::pin so the future has a known size.
            Box::pin(apply_effects_with_depth(app, children, depth)).await?;
        }
        // The Effect enum is #[non_exhaustive]; unhandled variants are logged
        // so implementers of future PRs can spot missing wiring.
        other => {
            tracing::warn!(
                effect = ?other,
                "apply_one: unhandled Effect variant (likely needs wiring in a later PR)"
            );
        }
    }
    Ok(())
}

/// Apply [`Effect::TogglePlugin`]: refuse Core toggles, update the registry's
/// enabled-set, rebuild derived indexes, and persist Optional state to
/// `~/.savvagent/plugins.toml`. Errors here are returned to the caller so
/// the event-loop can surface them; persistence failures surface as a
/// styled note rather than an error so the in-memory toggle still takes
/// effect.
async fn apply_toggle_plugin(app: &mut App, id: PluginId, enabled: bool) -> Result<(), String> {
    let reg_handle = match app.plugin_registry.as_ref().cloned() {
        Some(h) => h,
        None => {
            tracing::error!("Effect::TogglePlugin: plugin runtime not installed");
            app.push_styled_note(savvagent_plugin::StyledLine::plain(
                "Plugin runtime not yet installed; /plugins requires app startup to complete.",
            ));
            return Ok(());
        }
    };

    // Phase 1: refuse Core toggles + mutate the enabled-set under the
    // registry's write lock. The lock is released before we rebuild
    // indexes so Indexes::build's per-plugin manifest locks don't fight
    // the outer write guard.
    //
    // Defense-in-depth: the plugins-manager screen already refuses Core
    // toggles + surfaces a note before emitting TogglePlugin, and the
    // command palette filters Core plugins out of its action list. But
    // any other path that synthesises TogglePlugin (a future
    // slash-driven `/plugin disable <id>` flow, a hook subscriber, a
    // misbehaving plugin) would silently no-op here. Both branches now
    // push a user-visible note in addition to the warn-log.
    {
        let mut reg = reg_handle.write().await;
        if let Some(plugin) = reg.get(&id) {
            let kind = plugin.lock().await.manifest().kind;
            if matches!(kind, PluginKind::Core) {
                tracing::warn!(plugin = %id.as_str(), "refusing to toggle Core plugin");
                app.push_styled_note(savvagent_plugin::StyledLine::plain(format!(
                    "Cannot disable Core plugin: {}",
                    id.as_str()
                )));
                return Ok(());
            }
        } else {
            tracing::warn!(plugin = %id.as_str(), "TogglePlugin: unknown plugin id");
            app.push_styled_note(savvagent_plugin::StyledLine::plain(format!(
                "Cannot toggle unknown plugin: {}",
                id.as_str()
            )));
            return Ok(());
        }
        reg.set_enabled(&id, enabled);
    }

    // Phase 2: rebuild the derived indexes so the new enabled set is
    // visible to slash/screen/hook dispatch.
    //
    // If `Indexes::build` fails (e.g. enabling this plugin introduces a
    // slash/screen/keybinding conflict with another enabled plugin),
    // Phase 1's registry mutation has already committed. Propagating
    // the error via `?` would leave the registry diverged from the
    // indexes AND skip Phase 3 (persistence) — so the user would see a
    // toggle that "worked" in the row state but didn't actually take
    // effect, with no visibility beyond a tracing line. Instead, we
    // explicitly roll the registry mutation back to the prior state,
    // log at error level, surface a user-visible note, and return Ok —
    // leaving the indexes (which were never replaced) coherent with
    // the rolled-back registry.
    let new_idx = {
        let reg_read = reg_handle.read().await;
        match Indexes::build(&reg_read).await {
            Ok(i) => i,
            Err(e) => {
                drop(reg_read);
                let mut reg = reg_handle.write().await;
                reg.set_enabled(&id, !enabled);
                tracing::error!(
                    plugin = %id.as_str(),
                    error = %e,
                    "TogglePlugin: Indexes::build failed; rolled back registry mutation",
                );
                app.push_styled_note(savvagent_plugin::StyledLine::plain(format!(
                    "Couldn't toggle {}: rebuilding plugin indexes failed ({}); change reverted.",
                    id.as_str(),
                    e
                )));
                return Ok(());
            }
        }
    };
    if let Some(idx_handle) = app.plugin_indexes.as_ref() {
        let mut idx = idx_handle.write().await;
        *idx = new_idx;
    }

    // Phase 3: persist Optional rows. Core plugins are never written, so
    // hand-edits that disable Core are still ignored on next load.
    {
        let reg = reg_handle.read().await;
        let mut entries: HashMap<PluginId, bool> = HashMap::new();
        // Snapshot ids first so we don't borrow `reg` across the manifest
        // lock acquisitions below.
        let ids: Vec<PluginId> = reg.all_ids().cloned().collect();
        for pid in ids {
            let Some(plugin) = reg.get(&pid) else {
                continue;
            };
            let kind = plugin.lock().await.manifest().kind;
            if matches!(kind, PluginKind::Optional) {
                entries.insert(pid.clone(), reg.is_enabled(&pid));
            }
        }
        if let Err(e) = persistence::save(&entries) {
            tracing::error!(error = %e, "plugins.toml save failed");
            app.push_styled_note(savvagent_plugin::StyledLine::plain(format!(
                "Could not save plugins.toml ({e}); toggle applied for this session only — \
                 will revert at next start."
            )));
        }
    }
    Ok(())
}

async fn open_screen(app: &mut App, id: &str, args: ScreenArgs) -> Result<(), String> {
    // Per-screen open arguments may need values that only `App` knows
    // (active theme slug, registered provider list, etc.). Plugins emit
    // a placeholder; apply_effects patches it in here so plugin code
    // doesn't need read access to App state.
    let args = match (id, args) {
        ("themes.picker", _) => ScreenArgs::ThemePicker {
            current_slug: app.active_theme.name().to_string(),
        },
        ("language.picker", _) => ScreenArgs::LanguagePicker {
            current_code: app.active_language.clone(),
        },
        ("connect.picker", _) => ScreenArgs::ConnectPicker,
        (_, other) => other,
    };
    let (reg, idx) = match (&app.plugin_registry, &app.plugin_indexes) {
        (Some(r), Some(i)) => (r.clone(), i.clone()),
        _ => return Err("plugin runtime not installed".into()),
    };
    let idx_guard = idx.read().await;
    let pid = idx_guard
        .screens
        .get(id)
        .cloned()
        .ok_or_else(|| format!("unknown screen id: {id}"))?;
    // Layout lookup: walk the plugin's manifest screen list to find this id's layout.
    // (We don't cache a screen_layouts map in PR 3 — defer that optimization to PR 8
    // when the manager screen needs to query manifests anyway.)
    let reg_guard = reg.read().await;
    let handle = reg_guard
        .get(&pid)
        .ok_or_else(|| format!("unknown plugin id: {}", pid.as_str()))?;
    drop(reg_guard);
    drop(idx_guard);

    // For screens whose row/data list lives in `App`/the registry rather
    // than in `ScreenArgs`, build the populated screen ourselves and skip
    // the plugin's `create_screen` (which would just allocate an empty
    // placeholder we'd immediately discard). Today that's `plugins.manager`
    // and `palette`; future screens with the same shape (e.g. a hooks
    // inspector) should follow the same pattern rather than smuggling
    // state through ScreenArgs.
    let (screen, layout) = if id == "plugins.manager" {
        let layout = {
            let plugin = handle.lock().await;
            let manifest = plugin.manifest();
            manifest
                .contributions
                .screens
                .iter()
                .find(|s| s.id == id)
                .ok_or_else(|| format!("plugin {} doesn't declare screen {id}", pid.as_str()))?
                .layout
                .clone()
        };
        let rows = build_plugins_manager_rows(&reg).await;
        let screen: Box<dyn savvagent_plugin::Screen> =
            Box::new(PluginsManagerScreen::with_rows(rows));
        (screen, layout)
    } else if id == "palette" {
        let layout = {
            let plugin = handle.lock().await;
            let manifest = plugin.manifest();
            manifest
                .contributions
                .screens
                .iter()
                .find(|s| s.id == id)
                .ok_or_else(|| format!("plugin {} doesn't declare screen {id}", pid.as_str()))?
                .layout
                .clone()
        };
        let commands = build_palette_commands(&reg, &idx).await;
        let screen: Box<dyn savvagent_plugin::Screen> =
            Box::new(PaletteScreen::with_commands(commands));
        (screen, layout)
    } else {
        let plugin = handle.lock().await;
        let manifest = plugin.manifest();
        let layout = manifest
            .contributions
            .screens
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| format!("plugin {} doesn't declare screen {id}", pid.as_str()))?
            .layout
            .clone();
        let screen = plugin.create_screen(id, args).map_err(|e| e.to_string())?;
        (screen, layout)
    };

    app.screen_stack.push(screen, layout);
    Ok(())
}

/// Build one [`PaletteCommand`] per slash command in the runtime's slash
/// index. Only enabled plugins' slashes appear (the index is rebuilt on
/// enable/disable). Sorted alphabetically by name so the palette has a
/// stable ordering across runs.
async fn build_palette_commands(
    reg_handle: &std::sync::Arc<tokio::sync::RwLock<crate::plugin::registry::PluginRegistry>>,
    idx_handle: &std::sync::Arc<tokio::sync::RwLock<crate::plugin::manifests::Indexes>>,
) -> Vec<PaletteCommand> {
    let idx = idx_handle.read().await;
    let mut entries: Vec<(String, PluginId)> = idx
        .slash
        .iter()
        .map(|(name, pid)| (name.clone(), pid.clone()))
        .collect();
    drop(idx);
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let reg = reg_handle.read().await;
    let mut commands = Vec::with_capacity(entries.len());
    for (name, pid) in entries {
        let Some(plugin) = reg.get(&pid) else {
            continue;
        };
        let manifest = plugin.lock().await.manifest();
        let Some(spec) = manifest
            .contributions
            .slash_commands
            .iter()
            .find(|s| s.name == name)
        else {
            continue;
        };
        commands.push(PaletteCommand {
            name: spec.name.clone(),
            description: spec.summary.clone(),
            needs_arg: spec.args_hint.is_some(),
        });
    }
    commands
}

/// Build one [`PluginRow`] per registered plugin by walking the registry's
/// `all_ids` and locking each plugin's manifest. Sorted alphabetically by
/// id so the manager screen has a stable ordering across runs.
async fn build_plugins_manager_rows(
    reg_handle: &std::sync::Arc<tokio::sync::RwLock<crate::plugin::registry::PluginRegistry>>,
) -> Vec<PluginRow> {
    let reg = reg_handle.read().await;
    let mut ids: Vec<PluginId> = reg.all_ids().cloned().collect();
    ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));

    let mut rows = Vec::with_capacity(ids.len());
    for pid in ids {
        let Some(plugin) = reg.get(&pid) else {
            continue;
        };
        let manifest = plugin.lock().await.manifest();
        let summary = summarize_contributions(&manifest.contributions);
        rows.push(PluginRow {
            id: pid.clone(),
            name: manifest.name,
            version: manifest.version,
            kind: manifest.kind,
            enabled: reg.is_enabled(&pid),
            contribution_summary: summary,
        });
    }
    rows
}

/// Deliver a [`HostEvent`] to every plugin that subscribed to its
/// [`savvagent_plugin::HookKind`], then apply each subscriber's returned
/// effects independently in subscriber-registration order.
///
/// `depth` is forwarded so [`Effect::RunSlash`] re-entries and
/// `Effect::RegisterProvider`-triggered re-entries from hook handlers
/// share one cap ([`MAX_DISPATCH_DEPTH`]).
///
/// Subscriber lookup + `on_event` calls are delegated to
/// [`HookDispatcher::emit`]; that layer already logs and skips plugins
/// whose `on_event` errors. We then iterate the returned
/// `Vec<(PluginId, Vec<Effect>)>` and run [`apply_effects_with_depth`]
/// once per subscriber, warn-logging on failure and continuing — so a
/// single subscriber's bad effect (e.g. `Effect::OpenScreen { id:
/// "unknown" }`) cannot starve later subscribers' effects. The function
/// itself still returns `Ok(())` in that case; only depth-limit errors
/// propagate up.
///
/// Ordering: every subscriber's `on_event` sees the same pre-event app
/// state (fan-out happens before any apply). Effect application then
/// runs in subscriber-registration order, so later subscribers' effects
/// observe earlier subscribers' mutations.
///
/// Used by:
///
/// 1. [`apply_one`]'s `Effect::RegisterProvider` branch, to fire
///    `ProviderRegistered` + `Connect` after a successful registration.
/// 2. The TUI event loop, to forward host-originated events
///    (`TurnStart`, `TurnEnd`, `ToolCallStart`, `ToolCallEnd`,
///    `PromptSubmitted`, `TranscriptSaved`) translated from the host's
///    existing [`savvagent_host::TurnEvent`] stream.
pub(crate) async fn dispatch_host_event(
    app: &mut App,
    event: HostEvent,
    depth: u8,
) -> Result<(), String> {
    if depth >= MAX_DISPATCH_DEPTH {
        // Bottom out the recursion; emit a single warning so the user can
        // diagnose a subscriber that fires hook-triggering effects from its
        // own `on_event` handler.
        tracing::warn!(
            depth,
            cap = MAX_DISPATCH_DEPTH,
            event = ?event.kind(),
            "dispatch_host_event depth limit reached; dropping event"
        );
        return Err(format!(
            "dispatch depth limit ({MAX_DISPATCH_DEPTH}) exceeded by host event {:?}",
            event.kind()
        ));
    }
    let (reg, idx) = match (&app.plugin_registry, &app.plugin_indexes) {
        (Some(r), Some(i)) => (r.clone(), i.clone()),
        _ => return Ok(()),
    };
    let batches = {
        // Hold both guards only across the dispatcher's emit so the
        // lock surface mirrors `open_screen` / `run_slash`. The
        // HookDispatcher itself awaits each plugin's `on_event` while
        // holding the per-plugin Mutex (one-at-a-time delivery); the
        // outer RwLocks just gate the indexes/registry view.
        let reg_guard = reg.read().await;
        let idx_guard = idx.read().await;
        let dispatcher = HookDispatcher::new(&idx_guard, &reg_guard);
        dispatcher.emit(event).await
    };
    for (pid, effs) in batches {
        if let Err(e) = Box::pin(apply_effects_with_depth(app, effs, depth)).await {
            tracing::warn!(
                plugin_id = %pid.as_str(),
                error = %e,
                "applying effects from on_event failed; continuing dispatch"
            );
        }
    }
    Ok(())
}

async fn run_slash(
    app: &mut App,
    name: String,
    args: Vec<String>,
    depth: u8,
) -> Result<(), String> {
    let (reg, idx) = match (&app.plugin_registry, &app.plugin_indexes) {
        (Some(r), Some(i)) => (r.clone(), i.clone()),
        _ => return Err("plugin runtime not installed".into()),
    };
    let effs = {
        let reg_guard = reg.read().await;
        let idx_guard = idx.read().await;
        let router = SlashRouter::new(&idx_guard, &reg_guard);
        router
            .dispatch(&name, args)
            .await
            .map_err(|e| e.to_string())?
    };
    Box::pin(apply_effects_with_depth(app, effs, depth)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{HOME_LOCK, HomeGuard};
    use savvagent_plugin::StyledLine;
    use std::path::PathBuf;

    fn fresh_app() -> crate::app::App {
        crate::app::App::new("test-model".into(), PathBuf::from("/tmp"), "en".to_string())
    }

    /// RunSlash at depth >= MAX_DISPATCH_DEPTH must return a depth-limit error
    /// without panicking or spinning. This is the core assertion for Fix 1.
    #[tokio::test]
    async fn runslash_at_max_depth_returns_error() {
        // Acquire HOME_LOCK only while constructing App (which reads $HOME via
        // config paths). Drop both guards before any await point so clippy's
        // await_holding_lock lint stays green.
        let mut app = {
            let _lock = HOME_LOCK.lock().unwrap();
            let _home = HomeGuard::new();
            fresh_app()
        };

        let effect = Effect::RunSlash {
            name: "any".into(),
            args: vec![],
        };
        // Apply at depth == MAX_DISPATCH_DEPTH (4) — must error immediately.
        let result = Box::pin(apply_effects_with_depth(
            &mut app,
            vec![effect],
            MAX_DISPATCH_DEPTH,
        ))
        .await;
        assert!(result.is_err(), "expected depth-limit error, got Ok(())");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("depth limit"),
            "error message should mention depth limit, got: {msg}"
        );
    }

    /// A subscriber that re-emits a dispatch-triggering effect from
    /// `on_event` must bottom out at MAX_DISPATCH_DEPTH rather than
    /// recursing unboundedly. Regression test for the post-review fix
    /// where `dispatch_host_event` forwarded `depth` unchanged.
    #[tokio::test]
    async fn dispatch_host_event_caps_recursion_depth() {
        let mut app = {
            let _lock = HOME_LOCK.lock().unwrap();
            let _home = HomeGuard::new();
            fresh_app()
        };

        // Call dispatch_host_event at depth == MAX_DISPATCH_DEPTH directly.
        // It must short-circuit with a depth-limit error, regardless of
        // whether any subscribers exist. (Empty hook map is the common
        // path here — the assertion is on the depth guard, not the loop.)
        let result = Box::pin(dispatch_host_event(
            &mut app,
            HostEvent::HostStarting,
            MAX_DISPATCH_DEPTH,
        ))
        .await;
        assert!(
            result.is_err(),
            "expected depth-limit error at max depth, got Ok(())"
        );
        let msg = result.unwrap_err();
        assert!(
            msg.contains("depth limit"),
            "error message should mention depth limit, got: {msg}"
        );
    }

    /// Regression test for the post-v0.9 hotfix that wired `Effect::Quit`
    /// (emitted by the new `internal:quit` plugin) into `App::request_quit`.
    /// Before the fix, `/quit` from the palette landed on the `_ => warn`
    /// arm and silently dropped the request.
    #[tokio::test]
    async fn quit_effect_sets_should_quit() {
        let mut app = {
            let _lock = HOME_LOCK.lock().unwrap();
            let _home = HomeGuard::new();
            fresh_app()
        };
        assert!(!app.should_quit, "precondition: app starts not-quitting");

        apply_effects(&mut app, vec![Effect::Quit])
            .await
            .expect("apply_effects must succeed");

        assert!(
            app.should_quit,
            "Effect::Quit must flip should_quit so the event loop exits"
        );
    }

    /// Regression test for the post-v0.9 hotfix that added
    /// `Effect::PrefillInput`. Applying it must replace the textarea
    /// contents with the literal text (no leading slash stripped, no
    /// extra newline).
    #[tokio::test]
    async fn prefill_input_replaces_textarea_contents() {
        let mut app = {
            let _lock = HOME_LOCK.lock().unwrap();
            let _home = HomeGuard::new();
            fresh_app()
        };

        apply_effects(
            &mut app,
            vec![Effect::PrefillInput {
                text: "/view ".into(),
            }],
        )
        .await
        .expect("apply_effects must succeed");

        assert_eq!(
            app.input_textarea.lines(),
            &["/view ".to_string()],
            "PrefillInput must install the literal text as a single line"
        );
    }

    /// Stack effect recurses through children in order; results are applied
    /// sequentially.
    #[tokio::test]
    async fn stack_applies_children_in_order() {
        let mut app = {
            let _lock = HOME_LOCK.lock().unwrap();
            let _home = HomeGuard::new();
            fresh_app()
        };
        let initial_len = app.entries.len();

        let effects = vec![Effect::Stack(vec![
            Effect::PushNote {
                line: StyledLine::plain("first"),
            },
            Effect::PushNote {
                line: StyledLine::plain("second"),
            },
        ])];
        apply_effects(&mut app, effects).await.unwrap();

        // Two new entries should have been appended.
        assert_eq!(app.entries.len(), initial_len + 2);
    }

    /// End-to-end regression test for the dual-instance provider-plugin
    /// bug. Before the fix, `register_builtins` allocated each provider
    /// plugin twice — once in the `plugins` map (where slash dispatch
    /// reached it) and once in the `providers` map (where
    /// `take_provider_client` looked). The slash handler installed a
    /// client into instance `B`; `apply_effects::Effect::RegisterProvider`
    /// then asked instance `A` to hand one over and got `None`, so every
    /// `/connect <provider>` landed in the "announced but no client was
    /// constructed" failure branch.
    ///
    /// This test exercises the full chain: SlashRouter → handle_slash →
    /// Effect::RegisterProvider → take_provider_client → register_provider.
    /// We pre-install a stub client via `with_test_client` so we don't
    /// touch the user's keyring — the goal is to pin the wiring, not to
    /// exercise the real credential store.
    #[tokio::test]
    async fn connect_anthropic_with_stub_client_registers_end_to_end() {
        use crate::plugin::builtin::provider_anthropic::ProviderAnthropicPlugin;
        use crate::plugin::builtin::provider_common::ProviderEntry;
        use crate::plugin::manifests::Indexes;
        use crate::plugin::registry::{BuiltinSet, PluginRegistry};
        use async_trait::async_trait;
        use savvagent_mcp::ProviderClient;
        use savvagent_protocol::{
            CompleteRequest, CompleteResponse, ListModelsResponse, ProviderError, StreamEvent,
        };
        use tokio::sync::mpsc;

        // Minimal stub client — never actually called by this test; we
        // only need to prove it travels from plugin → app.registered_providers.
        struct StubClient;
        #[async_trait]
        impl ProviderClient for StubClient {
            async fn complete(
                &self,
                _: CompleteRequest,
                _: Option<mpsc::Sender<StreamEvent>>,
            ) -> Result<CompleteResponse, ProviderError> {
                unreachable!("stub client never invoked in this test")
            }
            async fn list_models(&self) -> Result<ListModelsResponse, ProviderError> {
                unreachable!("stub client never invoked in this test")
            }
        }

        let mut app = {
            let _lock = HOME_LOCK.lock().unwrap();
            let _home = HomeGuard::new();
            fresh_app()
        };

        // Build a BuiltinSet with just the anthropic provider plugin
        // pre-loaded with our stub client.
        let entry = ProviderEntry::new(ProviderAnthropicPlugin::with_test_client(Box::new(
            StubClient,
        )));
        let set = BuiltinSet {
            plugins: vec![],
            providers: vec![entry],
        };
        let registry = PluginRegistry::new(set);
        let indexes = Indexes::build(&registry).await.expect("indexes build");

        app.install_plugin_runtime(registry, indexes);

        // Dispatch the slash command through the same path the live TUI
        // uses. Before the fix, this would fail with "no client constructed."
        let effects = vec![Effect::RunSlash {
            name: "connect anthropic".into(),
            args: vec![],
        }];
        apply_effects(&mut app, effects)
            .await
            .expect("apply_effects must succeed");

        // The registered_providers map should now contain "anthropic"
        // pointing at our stub client.
        assert!(
            app.registered_providers.contains_key("anthropic"),
            "expected anthropic provider to be registered end-to-end; got keys: {:?}",
            app.registered_providers.keys().collect::<Vec<_>>()
        );
    }

    /// PR 7 wired `Effect::RegisterProvider` to emit BOTH
    /// `HostEvent::ProviderRegistered` (so `internal:connect` refreshes
    /// its candidate list) AND `HostEvent::Connect` (so `internal:splash`
    /// flips its HUD). This test pins that dual emission by installing a
    /// counter plugin subscribed to each `HookKind` and asserting both
    /// fired exactly once after one `RegisterProvider`.
    #[tokio::test]
    async fn register_provider_apply_effects_emits_both_provider_registered_and_connect() {
        use crate::plugin::builtin::provider_anthropic::ProviderAnthropicPlugin;
        use crate::plugin::builtin::provider_common::ProviderEntry;
        use crate::plugin::manifests::Indexes;
        use crate::plugin::registry::{BuiltinSet, PluginRegistry};
        use async_trait::async_trait;
        use savvagent_mcp::ProviderClient;
        use savvagent_plugin::{
            Contributions, HookKind, HostEvent, Manifest, Plugin, PluginError, PluginId,
            PluginKind, ProviderId,
        };
        use savvagent_protocol::{
            CompleteRequest, CompleteResponse, ListModelsResponse, ProviderError, StreamEvent,
        };
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};
        use tokio::sync::mpsc;

        // Same stub client shape as the previous end-to-end test.
        struct StubClient;
        #[async_trait]
        impl ProviderClient for StubClient {
            async fn complete(
                &self,
                _: CompleteRequest,
                _: Option<mpsc::Sender<StreamEvent>>,
            ) -> Result<CompleteResponse, ProviderError> {
                unreachable!("stub client never invoked in this test")
            }
            async fn list_models(&self) -> Result<ListModelsResponse, ProviderError> {
                unreachable!("stub client never invoked in this test")
            }
        }

        // Test plugin: subscribes to ProviderRegistered + Connect and
        // increments a shared counter whenever each variant arrives, so
        // the test can assert the dual emission without rooting around
        // in splash internals.
        struct DualCounter {
            id: String,
            registered_calls: Arc<AtomicU32>,
            connect_calls: Arc<AtomicU32>,
        }

        #[async_trait]
        impl Plugin for DualCounter {
            fn manifest(&self) -> Manifest {
                let mut contributions = Contributions::default();
                contributions.hooks = vec![HookKind::ProviderRegistered, HookKind::Connect];
                Manifest {
                    id: PluginId::new(&self.id).expect("valid id"),
                    name: self.id.clone(),
                    version: "0".into(),
                    description: "".into(),
                    kind: PluginKind::Optional,
                    contributions,
                }
            }

            async fn on_event(&mut self, event: HostEvent) -> Result<Vec<Effect>, PluginError> {
                match event {
                    HostEvent::ProviderRegistered { .. } => {
                        self.registered_calls.fetch_add(1, Ordering::SeqCst);
                    }
                    HostEvent::Connect { .. } => {
                        self.connect_calls.fetch_add(1, Ordering::SeqCst);
                    }
                    _ => {}
                }
                Ok(vec![])
            }
        }

        let mut app = {
            let _lock = HOME_LOCK.lock().unwrap();
            let _home = HomeGuard::new();
            fresh_app()
        };

        let registered_calls = Arc::new(AtomicU32::new(0));
        let connect_calls = Arc::new(AtomicU32::new(0));

        let counter = DualCounter {
            id: "internal:test-dual-counter".into(),
            registered_calls: registered_calls.clone(),
            connect_calls: connect_calls.clone(),
        };

        // Pair the counter with the anthropic provider plugin so
        // RegisterProvider has a valid take_provider_client target.
        let provider_entry = ProviderEntry::new(ProviderAnthropicPlugin::with_test_client(
            Box::new(StubClient),
        ));
        let set = BuiltinSet {
            plugins: vec![Box::new(counter)],
            providers: vec![provider_entry],
        };
        let registry = PluginRegistry::new(set);
        let indexes = Indexes::build(&registry).await.expect("indexes build");
        app.install_plugin_runtime(registry, indexes);

        // Drive a single RegisterProvider effect — the same shape
        // ConnectPlugin emits after handle_slash succeeds.
        let effects = vec![Effect::RegisterProvider {
            id: ProviderId::new("anthropic").expect("valid id"),
            display_name: "Anthropic".into(),
        }];
        apply_effects(&mut app, effects)
            .await
            .expect("RegisterProvider must succeed");

        assert_eq!(
            registered_calls.load(Ordering::SeqCst),
            1,
            "ProviderRegistered should fire exactly once"
        );
        assert_eq!(
            connect_calls.load(Ordering::SeqCst),
            1,
            "Connect should fire exactly once after RegisterProvider"
        );
        assert!(
            app.registered_providers.contains_key("anthropic"),
            "provider client travels into App as part of the same chain"
        );
    }

    /// Per-subscriber error isolation regression test (post-review fix).
    ///
    /// Before the fix, `dispatch_host_event` batched every subscriber's
    /// effects into one `Vec<Effect>` and called
    /// `apply_effects_with_depth` once. The `?` short-circuit on the
    /// first failing effect (e.g. `Effect::OpenScreen { id: "unknown" }`)
    /// silently starved every later subscriber's effects of being
    /// applied. The dispatcher now applies per-subscriber with
    /// log-and-continue on apply failure.
    ///
    /// This test installs two subscribers on `HostStarting`:
    ///   - "bad" returns `Effect::OpenScreen { id: "definitely-unknown" }`
    ///     — apply fails with "unknown screen id".
    ///   - "good" returns `Effect::PushNote { line: "made it" }`.
    /// After dispatch, the good subscriber's note must be present in the
    /// app's entries despite the bad subscriber's apply failure.
    #[tokio::test]
    async fn dispatch_continues_when_one_subscribers_effect_fails() {
        use crate::plugin::manifests::Indexes;
        use crate::plugin::registry::{BuiltinSet, PluginRegistry};
        use async_trait::async_trait;
        use savvagent_plugin::{
            Contributions, HookKind, HostEvent, Manifest, Plugin, PluginError, PluginId,
            PluginKind, ScreenArgs,
        };

        /// Subscriber whose effect always fails to apply.
        struct BadEffectSub {
            id: String,
        }
        #[async_trait]
        impl Plugin for BadEffectSub {
            fn manifest(&self) -> Manifest {
                let mut contributions = Contributions::default();
                contributions.hooks = vec![HookKind::HostStarting];
                Manifest {
                    id: PluginId::new(&self.id).expect("valid id"),
                    name: self.id.clone(),
                    version: "0".into(),
                    description: "".into(),
                    kind: PluginKind::Optional,
                    contributions,
                }
            }
            async fn on_event(&mut self, _: HostEvent) -> Result<Vec<Effect>, PluginError> {
                Ok(vec![Effect::OpenScreen {
                    id: "definitely-unknown-screen".into(),
                    args: ScreenArgs::None,
                }])
            }
        }

        /// Subscriber whose effect always succeeds — pushes a note we
        /// can look for in `app.entries`.
        struct GoodNoteSub {
            id: String,
            note: String,
        }
        #[async_trait]
        impl Plugin for GoodNoteSub {
            fn manifest(&self) -> Manifest {
                let mut contributions = Contributions::default();
                contributions.hooks = vec![HookKind::HostStarting];
                Manifest {
                    id: PluginId::new(&self.id).expect("valid id"),
                    name: self.id.clone(),
                    version: "0".into(),
                    description: "".into(),
                    kind: PluginKind::Optional,
                    contributions,
                }
            }
            async fn on_event(&mut self, _: HostEvent) -> Result<Vec<Effect>, PluginError> {
                Ok(vec![Effect::PushNote {
                    line: StyledLine::plain(self.note.clone()),
                }])
            }
        }

        let mut app = {
            let _lock = HOME_LOCK.lock().unwrap();
            let _home = HomeGuard::new();
            fresh_app()
        };

        let bad = BadEffectSub {
            id: "internal:test-bad-effect".into(),
        };
        let good = GoodNoteSub {
            id: "internal:test-good-note".into(),
            note: "good-subscriber-fired".into(),
        };

        let set = BuiltinSet {
            plugins: vec![Box::new(bad), Box::new(good)],
            providers: vec![],
        };
        let registry = PluginRegistry::new(set);
        let indexes = Indexes::build(&registry).await.expect("indexes build");
        app.install_plugin_runtime(registry, indexes);

        // Dispatch HostStarting. The dispatcher itself must return Ok(()),
        // and the good subscriber's note must be present even though the
        // bad subscriber's effect failed to apply.
        dispatch_host_event(&mut app, HostEvent::HostStarting, 0)
            .await
            .expect("dispatch_host_event must return Ok despite a subscriber's apply failure");

        let found = app.entries.iter().any(|e| match e {
            crate::app::Entry::Note(text) => text.contains("good-subscriber-fired"),
            _ => false,
        });
        assert!(
            found,
            "good subscriber's note must be applied despite earlier subscriber's apply failure; \
             entries: {:?}",
            app.entries
        );
    }

    /// `Effect::SaveTranscript` must fire `HostEvent::TranscriptSaved`
    /// from its Ok arm so subscribers (notably `internal:resume`) see
    /// user-initiated saves, not just autosaves. Regression test for the
    /// post-review fix.
    #[tokio::test]
    async fn save_transcript_effect_emits_transcript_saved_event() {
        use crate::plugin::manifests::Indexes;
        use crate::plugin::registry::{BuiltinSet, PluginRegistry};
        use async_trait::async_trait;
        use savvagent_plugin::{
            Contributions, HookKind, HostEvent, Manifest, Plugin, PluginError, PluginId, PluginKind,
        };
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};

        /// Counts TranscriptSaved invocations and records the most
        /// recent path so the test can assert payload pass-through.
        struct SavedCounter {
            id: String,
            calls: Arc<AtomicU32>,
            last_path: Arc<std::sync::Mutex<Option<String>>>,
        }
        #[async_trait]
        impl Plugin for SavedCounter {
            fn manifest(&self) -> Manifest {
                let mut contributions = Contributions::default();
                contributions.hooks = vec![HookKind::TranscriptSaved];
                Manifest {
                    id: PluginId::new(&self.id).expect("valid id"),
                    name: self.id.clone(),
                    version: "0".into(),
                    description: "".into(),
                    kind: PluginKind::Optional,
                    contributions,
                }
            }
            async fn on_event(&mut self, event: HostEvent) -> Result<Vec<Effect>, PluginError> {
                if let HostEvent::TranscriptSaved { path } = event {
                    self.calls.fetch_add(1, Ordering::SeqCst);
                    *self.last_path.lock().unwrap() = Some(path);
                }
                Ok(vec![])
            }
        }

        let mut app = {
            let _lock = HOME_LOCK.lock().unwrap();
            let _home = HomeGuard::new();
            fresh_app()
        };

        let calls = Arc::new(AtomicU32::new(0));
        let last_path = Arc::new(std::sync::Mutex::new(None));
        let counter = SavedCounter {
            id: "internal:test-transcript-saved".into(),
            calls: calls.clone(),
            last_path: last_path.clone(),
        };

        let set = BuiltinSet {
            plugins: vec![Box::new(counter)],
            providers: vec![],
        };
        let registry = PluginRegistry::new(set);
        let indexes = Indexes::build(&registry).await.expect("indexes build");
        app.install_plugin_runtime(registry, indexes);

        // Save somewhere in a tempdir so we don't pollute the working tree.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("test-transcript.md");
        let path_str = path.to_string_lossy().into_owned();

        apply_effects(
            &mut app,
            vec![Effect::SaveTranscript {
                path: path_str.clone(),
            }],
        )
        .await
        .expect("apply_effects must succeed");

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "TranscriptSaved must fire exactly once for one Effect::SaveTranscript"
        );
        assert_eq!(
            last_path.lock().unwrap().as_deref(),
            Some(path_str.as_str()),
            "TranscriptSaved payload must carry the saved path"
        );
    }

    /// `Effect::TogglePlugin` for an Optional plugin must (a) flip the
    /// registry's enabled bit, (b) rebuild the indexes so the new state
    /// is observable to dispatch, and (c) persist the change to
    /// `~/.savvagent/plugins.toml`.
    ///
    /// HOME_LOCK is a `std::sync::Mutex` and the `HomeGuard` holds the
    /// `$HOME` redirect; both need to span the awaiting toggle so the
    /// persistence::save path lands in the per-test tempdir, not the
    /// developer's real `~/.savvagent/`. Tokio's `current_thread` flavor
    /// keeps the future pinned to one OS thread, so holding a std Mutex
    /// across `.await` is safe — we silence the lint that catches the
    /// more general (multi-thread runtime) case.
    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn toggle_plugin_optional_updates_registry_and_persists() {
        use crate::plugin::builtin::plugins_manager::persistence as plugins_toml;
        use crate::plugin::manifests::Indexes;
        use crate::plugin::registry::{BuiltinSet, PluginRegistry};
        use async_trait::async_trait;
        use savvagent_plugin::{Contributions, Manifest, Plugin, PluginId, PluginKind};

        // Minimal Optional plugin — the toggle target.
        struct Optional;
        #[async_trait]
        impl Plugin for Optional {
            fn manifest(&self) -> Manifest {
                Manifest {
                    id: PluginId::new("internal:test-optional").expect("valid"),
                    name: "Test Optional".into(),
                    version: "0".into(),
                    description: "".into(),
                    kind: PluginKind::Optional,
                    contributions: Contributions::default(),
                }
            }
        }

        let _lock = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let mut app = fresh_app();

        let set = BuiltinSet {
            plugins: vec![Box::new(Optional)],
            providers: vec![],
        };
        let registry = PluginRegistry::new(set);
        let indexes = Indexes::build(&registry).await.expect("indexes build");
        app.install_plugin_runtime(registry, indexes);

        let pid = PluginId::new("internal:test-optional").expect("valid");

        // Pre-condition: plugin is enabled, and the registry says so.
        {
            let reg = app.plugin_registry.as_ref().unwrap().read().await;
            assert!(reg.is_enabled(&pid));
        }

        apply_effects(
            &mut app,
            vec![Effect::TogglePlugin {
                id: pid.clone(),
                enabled: false,
            }],
        )
        .await
        .expect("apply_effects must succeed");

        // Post: registry now says disabled.
        {
            let reg = app.plugin_registry.as_ref().unwrap().read().await;
            assert!(
                !reg.is_enabled(&pid),
                "registry should mark Optional plugin disabled after TogglePlugin(false)"
            );
        }

        // And ~/.savvagent/plugins.toml carries the override.
        let loaded = plugins_toml::load();
        assert_eq!(loaded.get(&pid), Some(&false));
    }

    /// Regression test for the CRITICAL post-review fix: if Phase 2's
    /// `Indexes::build` fails after Phase 1's registry mutation has
    /// committed, the previous code returned the error via `?` —
    /// leaving the registry diverged from the indexes AND skipping
    /// Phase 3 (persistence). The user saw a successful-looking toggle
    /// that didn't take effect.
    ///
    /// Setup: two Optional plugins that contribute the same slash name
    /// (`dup`). At install time only one is enabled, so
    /// `Indexes::build` succeeds. We then emit
    /// `TogglePlugin { id: B, enabled: true }`; that flip would make
    /// `Indexes::build` fail with a `SlashConflict`. The handler must
    /// roll the registry's enabled bit on B back to `false`, surface a
    /// note, and return `Ok(())`.
    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn toggle_plugin_rolls_back_on_indexes_build_failure() {
        use crate::plugin::manifests::Indexes;
        use crate::plugin::registry::{BuiltinSet, PluginRegistry};
        use async_trait::async_trait;
        use savvagent_plugin::{Contributions, Manifest, Plugin, PluginId, PluginKind, SlashSpec};

        /// Two distinct plugin types that contribute the same slash name
        /// `dup` so enabling both at once causes `Indexes::build` to
        /// fail with `SlashConflict`.
        struct SlashDup {
            id: String,
        }
        #[async_trait]
        impl Plugin for SlashDup {
            fn manifest(&self) -> Manifest {
                let mut contributions = Contributions::default();
                contributions.slash_commands = vec![SlashSpec {
                    name: "dup".into(),
                    summary: "".into(),
                    args_hint: None,
                }];
                Manifest {
                    id: PluginId::new(&self.id).expect("valid id"),
                    name: self.id.clone(),
                    version: "0".into(),
                    description: "".into(),
                    kind: PluginKind::Optional,
                    contributions,
                }
            }
        }

        let _lock = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let mut app = fresh_app();

        let a = SlashDup {
            id: "internal:test-slash-dup-a".into(),
        };
        let b = SlashDup {
            id: "internal:test-slash-dup-b".into(),
        };

        let set = BuiltinSet {
            plugins: vec![Box::new(a), Box::new(b)],
            providers: vec![],
        };
        let mut registry = PluginRegistry::new(set);

        // Disable B before the initial Indexes::build so it succeeds —
        // only A's `dup` is in the slash index at install time.
        let pid_b = PluginId::new("internal:test-slash-dup-b").expect("valid");
        registry.set_enabled(&pid_b, false);

        let indexes = Indexes::build(&registry).await.expect("indexes build");
        app.install_plugin_runtime(registry, indexes);

        // Sanity: pre-toggle, B is disabled.
        {
            let reg = app.plugin_registry.as_ref().unwrap().read().await;
            assert!(!reg.is_enabled(&pid_b), "precondition: B starts disabled");
        }

        // Drive the toggle. Phase 1 sets B enabled; Phase 2's
        // Indexes::build fails on the dup-slash conflict; the handler
        // must roll Phase 1 back. Returns Ok(()) because the rollback
        // path surfaces the failure via a PushNote, not an error.
        apply_effects(
            &mut app,
            vec![Effect::TogglePlugin {
                id: pid_b.clone(),
                enabled: true,
            }],
        )
        .await
        .expect("apply_effects must return Ok despite the build failure");

        // Post: B is still disabled (rollback worked). Without the
        // rollback, the registry would now hold B as enabled while the
        // indexes still reflected the prior (working) state.
        let reg = app.plugin_registry.as_ref().unwrap().read().await;
        assert!(
            !reg.is_enabled(&pid_b),
            "registry must be rolled back to disabled after Indexes::build failure"
        );

        // And a user-visible note explaining the revert was pushed.
        let found = app.entries.iter().any(|e| match e {
            crate::app::Entry::Note(text) => text.contains("change reverted"),
            _ => false,
        });
        assert!(
            found,
            "expected a 'change reverted' note after the failed toggle; entries: {:?}",
            app.entries
        );
    }

    /// Toggling a Core plugin is a no-op at the apply_effects level: the
    /// registry remains unchanged and nothing is written.
    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn toggle_plugin_core_is_refused() {
        use crate::plugin::manifests::Indexes;
        use crate::plugin::registry::{BuiltinSet, PluginRegistry};
        use async_trait::async_trait;
        use savvagent_plugin::{Contributions, Manifest, Plugin, PluginId, PluginKind};

        struct Core;
        #[async_trait]
        impl Plugin for Core {
            fn manifest(&self) -> Manifest {
                Manifest {
                    id: PluginId::new("internal:test-core").expect("valid"),
                    name: "Test Core".into(),
                    version: "0".into(),
                    description: "".into(),
                    kind: PluginKind::Core,
                    contributions: Contributions::default(),
                }
            }
        }

        let _lock = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let mut app = fresh_app();

        let set = BuiltinSet {
            plugins: vec![Box::new(Core)],
            providers: vec![],
        };
        let registry = PluginRegistry::new(set);
        let indexes = Indexes::build(&registry).await.expect("indexes build");
        app.install_plugin_runtime(registry, indexes);

        let pid = PluginId::new("internal:test-core").expect("valid");

        apply_effects(
            &mut app,
            vec![Effect::TogglePlugin {
                id: pid.clone(),
                enabled: false,
            }],
        )
        .await
        .expect("apply_effects must succeed");

        // Registry should still mark the Core plugin enabled.
        {
            let reg = app.plugin_registry.as_ref().unwrap().read().await;
            assert!(
                reg.is_enabled(&pid),
                "Core plugin must remain enabled after a refused TogglePlugin"
            );
        }

        // Defense-in-depth: the apply layer pushes a user-visible note
        // so a non-screen-driven `TogglePlugin` for a Core plugin
        // (future slash-driven flow, hook subscriber, etc.) doesn't
        // silently no-op.
        let found = app.entries.iter().any(|e| match e {
            crate::app::Entry::Note(text) => text.contains("Cannot disable Core plugin"),
            _ => false,
        });
        assert!(
            found,
            "expected a 'Cannot disable Core plugin' note; entries: {:?}",
            app.entries
        );
    }

    /// Defense-in-depth for fix #5: an unknown id in `TogglePlugin`
    /// must push a user-visible note in addition to the warn-log. The
    /// plugins-manager screen only emits TogglePlugin for ids it
    /// already knows about, so this branch is exercised only by
    /// non-screen-driven emitters; we still pin the user feedback so
    /// future emitters don't silently no-op.
    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn toggle_plugin_unknown_id_pushes_note() {
        use crate::plugin::manifests::Indexes;
        use crate::plugin::registry::{BuiltinSet, PluginRegistry};
        use savvagent_plugin::PluginId;

        let _lock = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let mut app = fresh_app();

        let set = BuiltinSet {
            plugins: vec![],
            providers: vec![],
        };
        let registry = PluginRegistry::new(set);
        let indexes = Indexes::build(&registry).await.expect("indexes build");
        app.install_plugin_runtime(registry, indexes);

        let pid = PluginId::new("internal:nonexistent").expect("valid id");

        apply_effects(
            &mut app,
            vec![Effect::TogglePlugin {
                id: pid.clone(),
                enabled: false,
            }],
        )
        .await
        .expect("apply_effects must succeed even for unknown id");

        let found = app.entries.iter().any(|e| match e {
            crate::app::Entry::Note(text) => text.contains("Cannot toggle unknown plugin"),
            _ => false,
        });
        assert!(
            found,
            "expected a 'Cannot toggle unknown plugin' note; entries: {:?}",
            app.entries
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn set_active_locale_persist_true_switches_rust_i18n_and_writes_file() {
        let _lock = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();

        let mut app = fresh_app();
        apply_effects(
            &mut app,
            vec![savvagent_plugin::Effect::SetActiveLocale {
                code: "es".into(),
                persist: true,
            }],
        )
        .await
        .unwrap();

        assert_eq!(&*rust_i18n::locale(), "es");
        assert_eq!(app.active_language, "es");
        let path = crate::plugin::builtin::language::catalog::config_path().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#"language = "es""#));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn set_active_locale_persist_false_does_not_write_file() {
        let _lock = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();

        let mut app = fresh_app();
        apply_effects(
            &mut app,
            vec![savvagent_plugin::Effect::SetActiveLocale {
                code: "pt".into(),
                persist: false,
            }],
        )
        .await
        .unwrap();

        assert_eq!(&*rust_i18n::locale(), "pt");
        assert_eq!(app.active_language, "pt");
        let path = crate::plugin::builtin::language::catalog::config_path().unwrap();
        assert!(
            !path.exists(),
            "persist=false must not create language.toml"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn set_active_locale_unknown_code_is_a_noop_with_note() {
        let _lock = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        // Reset locale to a known baseline before capturing so the assertion
        // is deterministic even though rust_i18n::set_locale is global state.
        rust_i18n::set_locale("en");
        let mut app = fresh_app();
        let before_locale = rust_i18n::locale().to_string();
        let before_active = app.active_language.clone();

        apply_effects(
            &mut app,
            vec![savvagent_plugin::Effect::SetActiveLocale {
                code: "xx".into(),
                persist: true,
            }],
        )
        .await
        .unwrap();

        assert_eq!(&*rust_i18n::locale(), before_locale.as_str());
        assert_eq!(app.active_language, before_active);

        // persist must not fire when the code was rejected — the file
        // must not exist in the HomeGuard tempdir.
        let path = crate::plugin::builtin::language::catalog::config_path().unwrap();
        assert!(
            !path.exists(),
            "persist must not fire when set_active_language rejected the code"
        );
    }
}
