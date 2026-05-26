//! `plugin-static` world adapter.
//!
//! Loads a `.wasm` component, wires the two host imports (`log`,
//! `current-theme`), instantiates the component into a long-lived store,
//! caches the manifest and theme catalog at construction time, and exposes
//! a `Box<dyn savvagent_plugin::Plugin>` to the runtime.
//!
//! ## Concurrency model
//!
//! Per-store the wasm export calls require `&mut Store`, and `Store` is not
//! `Send`-safe to clone, so a single `tokio::sync::Mutex<StoreAndInstance>`
//! serializes every export call. The `Plugin` trait methods `&mut self`
//! (handle_slash, on_event) are themselves serial in the runtime, so this
//! mutex never contends in practice — it exists so the `&self`-only methods
//! (`manifest`, `render_slot`, `themes`) can read the cached snapshots that
//! were populated under the mutex at construction time.
//!
//! ## Cached values
//!
//! `manifest()` and `themes()` are `&self` + sync in the trait, but the
//! wasm exports are `&mut store` + async. Resolve at construction time and
//! cache the conversion result; subsequent reads are zero-cost.
//!
//! ## Recovery
//!
//! When a wasm call traps, the `Store` and the `PluginStatic` are
//! discarded; Task 8 will re-instantiate via the cached `InstancePre`. In
//! v0.18.0 (this task) we surface the trap as `PluginError::Internal` and
//! let the runtime decide; the recovery mechanism lands later.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;
use wasmtime::Store;
use wasmtime::component::{Component, HasSelf, Linker};

use savvagent_plugin::manifest::Manifest;
use savvagent_plugin::{Effect, HostEvent, Plugin, PluginError, Region, StyledLine, ThemeEntry};

use crate::convert::{
    effect_from_wit, manifest_from_wit, plugin_error_from_wit, theme_color_to_wit,
    theme_entry_from_wit,
};
use crate::engine::shared_engine;
use crate::error::WasmPluginError;
use crate::host_imports::{log as log_host, theme};
use crate::manifest::PluginManifest as DiskManifest;
use crate::static_world::{
    self, PluginStatic, PluginStaticImports, savvagent::plugin::types as wit,
};

/// Per-store state that lives inside the wasmtime [`Store`]. The host
/// imports project from `&mut StaticHostState` via [`HasSelf`].
pub(crate) struct StaticHostState {
    /// Stable id of this plugin, attached to every host-side log event.
    plugin_id: String,
    /// Shared, live theme snapshot. Read under `theme.read().await`.
    theme: theme::ThemeProvider,
}

// `PluginStaticImports` is the auto-generated trait the bindgen world emits
// for the `import log` + `import current-theme` functions. We implement it
// on `StaticHostState`; the bindgen-emitted blanket `impl<_T> for &mut _T`
// then satisfies the `for<'a> D::Data<'a>: PluginStaticImports` bound that
// `add_to_linker` requires when paired with `HasSelf<StaticHostState>`.
impl PluginStaticImports for StaticHostState {
    async fn log(&mut self, level: wit::LogLevel, msg: String) {
        log_host::emit(&self.plugin_id, level, &msg);
    }

    async fn current_theme(&mut self) -> Vec<(String, wit::ThemeColor)> {
        let snap = theme::snapshot(&self.theme).await;
        snap.into_iter()
            .map(|(name, color)| (name, theme_color_to_wit(color)))
            .collect()
    }
}

// The bindgen also requires a `Host` impl for the `types` interface. The
// generated trait is empty; this satisfies the `add_to_linker` bound.
impl static_world::savvagent::plugin::types::Host for StaticHostState {}

/// Holds the wasm `Store` + the loaded `PluginStatic` together so calls
/// that need both can borrow them under one lock.
struct StoreAndInstance {
    store: Store<StaticHostState>,
    instance: PluginStatic,
}

/// Adapter that wraps a `plugin-static` wasm component as a
/// `Box<dyn Plugin>`.
pub struct StaticAdapter {
    cached_manifest: Manifest,
    cached_themes: Vec<ThemeEntry>,
    inner: Arc<Mutex<StoreAndInstance>>,
    /// Held purely for trap-recovery in Task 8; the field is read indirectly
    /// in v0.18.0 only through [`StaticAdapter::disk_manifest`].
    disk_manifest: Arc<DiskManifest>,
}

impl StaticAdapter {
    /// Borrow the parsed `plugin.toml` this adapter was constructed from.
    /// Held purely so Task 8's recovery path can re-derive the wasm path
    /// + identity without re-walking the four-path discovery.
    pub fn disk_manifest(&self) -> &Arc<DiskManifest> {
        &self.disk_manifest
    }
}

impl std::fmt::Debug for StaticAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaticAdapter")
            .field("id", &self.cached_manifest.id.as_str())
            .field("name", &self.cached_manifest.name)
            .field("version", &self.cached_manifest.version)
            .finish()
    }
}

impl StaticAdapter {
    /// Construct a `StaticAdapter` by loading `plugin.wasm` from
    /// `plugin_dir`, wiring host imports, instantiating, and caching
    /// manifest + themes.
    ///
    /// `disk_manifest` is the parsed `plugin.toml`; it carries identity and
    /// is consulted by Task 8's recovery path. `theme` is the runtime's
    /// shared theme snapshot the host import `current-theme()` reads from.
    pub async fn new(
        disk_manifest: Arc<DiskManifest>,
        plugin_dir: &Path,
        theme: theme::ThemeProvider,
    ) -> Result<Self, WasmPluginError> {
        let engine = shared_engine()?;
        let wasm_path = plugin_dir.join("plugin.wasm");
        let component =
            Component::from_file(&engine, &wasm_path).map_err(WasmPluginError::Wasmtime)?;

        let mut linker: Linker<StaticHostState> = Linker::new(&engine);
        PluginStatic::add_to_linker::<_, HasSelf<StaticHostState>>(&mut linker, |s| s)
            .map_err(WasmPluginError::Wasmtime)?;

        let state = StaticHostState {
            plugin_id: disk_manifest.plugin.id.clone(),
            theme,
        };
        let mut store = Store::new(&engine, state);

        let instance = PluginStatic::instantiate_async(&mut store, &component, &linker)
            .await
            .map_err(WasmPluginError::Wasmtime)?;

        // Cache manifest at construction.
        let wit_manifest = instance
            .call_manifest(&mut store)
            .await
            .map_err(WasmPluginError::Wasmtime)?
            .map_err(|e| {
                WasmPluginError::Manifest(
                    wasm_path.clone(),
                    format!("plugin returned error: {e:?}"),
                )
            })?;
        let cached_manifest = manifest_from_wit(wit_manifest)?;

        // Cache themes at construction.
        let wit_themes = instance
            .call_themes(&mut store)
            .await
            .map_err(WasmPluginError::Wasmtime)?;
        let cached_themes: Vec<ThemeEntry> =
            wit_themes.into_iter().map(theme_entry_from_wit).collect();

        Ok(Self {
            cached_manifest,
            cached_themes,
            inner: Arc::new(Mutex::new(StoreAndInstance { store, instance })),
            disk_manifest,
        })
    }
}

#[async_trait]
impl Plugin for StaticAdapter {
    fn manifest(&self) -> Manifest {
        self.cached_manifest.clone()
    }

    async fn handle_slash(
        &mut self,
        name: &str,
        args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        let mut guard = self.inner.lock().await;
        let StoreAndInstance { store, instance } = &mut *guard;
        let result = instance
            .call_handle_slash(&mut *store, name, &args)
            .await
            .map_err(|e| PluginError::Internal(format!("wasm trap in handle_slash: {e}")))?;
        let wit_effects = result.map_err(plugin_error_from_wit)?;
        let mut effects = Vec::with_capacity(wit_effects.len());
        for e in wit_effects {
            effects.push(effect_from_wit(e).map_err(|err| PluginError::Internal(err.to_string()))?);
        }
        Ok(effects)
    }

    async fn on_event(&mut self, event: HostEvent) -> Result<Vec<Effect>, PluginError> {
        let event_json = serde_json::to_string(&event_to_json(&event))
            .map_err(|e| PluginError::Internal(format!("serialize HostEvent: {e}")))?;
        let mut guard = self.inner.lock().await;
        let StoreAndInstance { store, instance } = &mut *guard;
        let result = instance
            .call_on_event(&mut *store, &event_json)
            .await
            .map_err(|e| PluginError::Internal(format!("wasm trap in on_event: {e}")))?;
        let wit_effects = result.map_err(plugin_error_from_wit)?;
        let mut effects = Vec::with_capacity(wit_effects.len());
        for e in wit_effects {
            effects.push(effect_from_wit(e).map_err(|err| PluginError::Internal(err.to_string()))?);
        }
        Ok(effects)
    }

    fn render_slot(&self, _slot_id: &str, _region: Region) -> Vec<StyledLine> {
        // `render_slot` is `&self` + sync, but the wasm export is `&mut
        // store` + async. Bridging would require either `block_in_place`
        // (deadlock-prone if called from a single-threaded runtime) or a
        // background pumper that pre-renders slots — both meaningful
        // designs for a later release. v0.18.0 punts: external static
        // plugins simply cannot contribute render slots. Built-in
        // plugins, which still have direct `&self` access to their state,
        // continue to render slots as before.
        Vec::new()
    }

    fn themes(&self) -> Vec<ThemeEntry> {
        self.cached_themes.clone()
    }
}

/// Render a `HostEvent` as a JSON-shaped value the wasm guest can parse.
/// We hand-roll the projection here rather than relying on a `serde::Serialize`
/// on `HostEvent` (which the trait-surface crate intentionally does not
/// provide) so the wire shape is stable across savvagent versions.
fn event_to_json(event: &HostEvent) -> serde_json::Value {
    use serde_json::json;
    match event {
        HostEvent::HostStarting => json!({"kind": "host-starting"}),
        HostEvent::Connect { provider_id } => json!({
            "kind": "connect",
            "provider_id": provider_id.as_str(),
        }),
        HostEvent::Disconnect {
            provider_id,
            reason,
        } => json!({
            "kind": "disconnect",
            "provider_id": provider_id.as_str(),
            "reason": reason,
        }),
        HostEvent::TurnStart { turn_id } => json!({
            "kind": "turn-start",
            "turn_id": turn_id,
        }),
        HostEvent::TurnEnd { turn_id, success } => json!({
            "kind": "turn-end",
            "turn_id": turn_id,
            "success": success,
        }),
        HostEvent::ToolCallStart { call_id, tool } => json!({
            "kind": "tool-call-start",
            "call_id": call_id,
            "tool": tool,
        }),
        HostEvent::ToolCallEnd { call_id, success } => json!({
            "kind": "tool-call-end",
            "call_id": call_id,
            "success": success,
        }),
        HostEvent::PromptSubmitted { text } => json!({
            "kind": "prompt-submitted",
            "text": text,
        }),
        HostEvent::TranscriptSaved { path } => json!({
            "kind": "transcript-saved",
            "path": path,
        }),
        HostEvent::ProviderRegistered { id, display_name } => json!({
            "kind": "provider-registered",
            "id": id.as_str(),
            "display_name": display_name,
        }),
        HostEvent::ContextSizeChanged { tokens } => json!({
            "kind": "context-size-changed",
            "tokens": tokens,
        }),
        HostEvent::ActiveProviderChanged { id } => json!({
            "kind": "active-provider-changed",
            "id": id.as_str(),
        }),
        HostEvent::SubagentStop {
            agent_name,
            success,
        } => json!({
            "kind": "subagent-stop",
            "agent_name": agent_name,
            "success": success,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::ProviderId;

    #[test]
    fn event_to_json_covers_every_variant() {
        let pid = ProviderId::new("anthropic").unwrap();
        let cases = vec![
            HostEvent::HostStarting,
            HostEvent::Connect {
                provider_id: pid.clone(),
            },
            HostEvent::Disconnect {
                provider_id: pid.clone(),
                reason: "bye".into(),
            },
            HostEvent::TurnStart { turn_id: 1 },
            HostEvent::TurnEnd {
                turn_id: 2,
                success: true,
            },
            HostEvent::ToolCallStart {
                call_id: "c".into(),
                tool: "read_file".into(),
            },
            HostEvent::ToolCallEnd {
                call_id: "c".into(),
                success: false,
            },
            HostEvent::PromptSubmitted { text: "hi".into() },
            HostEvent::TranscriptSaved {
                path: "/tmp/t.json".into(),
            },
            HostEvent::ProviderRegistered {
                id: pid.clone(),
                display_name: "Anthropic".into(),
            },
            HostEvent::ContextSizeChanged { tokens: 42 },
            HostEvent::ActiveProviderChanged { id: pid },
            HostEvent::SubagentStop {
                agent_name: "code-reviewer".into(),
                success: true,
            },
        ];
        for e in cases {
            let v = event_to_json(&e);
            assert!(v.get("kind").is_some(), "every variant emits a `kind`");
        }
    }
}
