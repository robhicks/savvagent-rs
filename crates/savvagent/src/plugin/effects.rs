//! `apply_effects` is the single mutation surface for `App` in response
//! to plugin output. Every Effect variant maps to one App method (or
//! recurses for Stack). Slash re-entrancy depth is enforced here so that
//! the depth cap cannot be bypassed by constructing a fresh SlashRouter.

use savvagent_plugin::{Effect, HostEvent, PluginId, ScreenArgs};

use crate::app::App;
use crate::plugin::slash::SlashRouter;

/// Maximum number of nested `Effect::RunSlash` re-entries before an error
/// is returned. Prevents infinite loops when a plugin's slash handler emits
/// another `RunSlash` effect.
const MAX_RUNSLASH_DEPTH: u8 = 4;

/// Apply each effect in sequence, mutating `App` state. Used by the event
/// loop after dispatching key events, hook events, or slash commands.
///
/// Returns `Err(String)` if an effect referenced an unknown screen or slash
/// command, or if recursion via `Effect::RunSlash` exceeded `MAX_RUNSLASH_DEPTH`.
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
                    return Ok(());
                }
            };
            let registry = match &app.plugin_registry {
                Some(r) => r.clone(),
                None => {
                    tracing::error!("RegisterProvider before plugin runtime installed");
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
                // internal:connect can refresh its candidate list.
                Box::pin(dispatch_host_event(
                    app,
                    HostEvent::ProviderRegistered { id, display_name },
                    depth,
                ))
                .await?;
            } else {
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
            if depth >= MAX_RUNSLASH_DEPTH {
                return Err(format!(
                    "Effect::RunSlash depth limit ({MAX_RUNSLASH_DEPTH}) exceeded by slash '{name}'"
                ));
            }
            run_slash(app, name, args, depth + 1).await?;
        }
        Effect::ClearLog => app.clear_log(),
        Effect::Quit => app.request_quit(),
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

async fn open_screen(app: &mut App, id: &str, args: ScreenArgs) -> Result<(), String> {
    // Per-screen open arguments may need values that only `App` knows
    // (active theme slug, registered provider list, etc.). Plugins emit
    // a placeholder; apply_effects patches it in here so plugin code
    // doesn't need read access to App state.
    let args = match (id, args) {
        ("themes.picker", _) => ScreenArgs::ThemePicker {
            current_slug: app.active_theme.name().to_string(),
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

    let (screen, layout) = {
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

/// Deliver a [`HostEvent`] to every plugin that subscribed to its
/// [`savvagent_plugin::HookKind`], applying each plugin's returned effects
/// in order. `depth` is forwarded so [`Effect::RunSlash`] re-entries from
/// hook handlers cannot bypass the depth cap. PR 7 will replace this with
/// the full Host-side dispatcher.
pub(crate) async fn dispatch_host_event(
    app: &mut App,
    event: HostEvent,
    depth: u8,
) -> Result<(), String> {
    let (reg, idx) = match (&app.plugin_registry, &app.plugin_indexes) {
        (Some(r), Some(i)) => (r.clone(), i.clone()),
        _ => return Ok(()),
    };
    let kind = event.kind();
    let subscriber_ids = {
        let idx_guard = idx.read().await;
        idx_guard.hooks.get(&kind).cloned().unwrap_or_default()
    };
    for pid in subscriber_ids {
        let handle = {
            let reg_guard = reg.read().await;
            reg_guard.get(&pid)
        };
        let Some(handle) = handle else { continue };
        let effects = {
            let mut plugin = handle.lock().await;
            match plugin.on_event(event.clone()).await {
                Ok(effs) => effs,
                Err(e) => {
                    tracing::warn!(plugin_id = %pid.as_str(), error = %e,
                        "plugin on_event returned an error; skipping");
                    continue;
                }
            }
        };
        Box::pin(apply_effects_with_depth(app, effects, depth)).await?;
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
        crate::app::App::new("test-model".into(), PathBuf::from("/tmp"))
    }

    /// RunSlash at depth >= MAX_RUNSLASH_DEPTH must return a depth-limit error
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
        // Apply at depth == MAX_RUNSLASH_DEPTH (4) — must error immediately.
        let result = Box::pin(apply_effects_with_depth(
            &mut app,
            vec![effect],
            MAX_RUNSLASH_DEPTH,
        ))
        .await;
        assert!(result.is_err(), "expected depth-limit error, got Ok(())");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("depth limit"),
            "error message should mention depth limit, got: {msg}"
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
}
