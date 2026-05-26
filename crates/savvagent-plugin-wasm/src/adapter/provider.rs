//! `plugin-provider` world adapter.
//!
//! Bridges a wasm component implementing the `plugin-provider` world to
//! [`savvagent_mcp::ProviderClient`] — the host-facing trait every provider
//! impl (in-process, MCP-over-HTTP, or now wasm) presents.
//!
//! ## Concurrency model
//!
//! Each `ProviderClient` method (`complete`, `list_models`) constructs a
//! **fresh** `Store<ProviderHostState>` and instantiates the component from
//! a cached `InstancePre`. This is the simplest correct design:
//!
//! - No store reuse across calls means no per-store state can leak between
//!   turns (api keys read into a global, partial streaming state from a
//!   crashed turn, …).
//! - Per-call `Store` ownership means each call's `mpsc::Sender` for
//!   streaming events lives only for the duration of that call.
//! - `InstancePre` does the export-shape typecheck once at construction;
//!   per-call instantiation only pays the wasm-module-instantiation cost,
//!   not the typecheck.
//!
//! The plan sketched a per-store pool to reduce instantiation latency.
//! v0.18.0 ships without one — measurement first, optimize later. The
//! pool slot is reserved in [`WasmProviderClient`] as a `Mutex<Vec<...>>`
//! that's never populated; a future revision can pop from it when
//! non-empty, falling back to fresh construction otherwise.
//!
//! ## `count_tokens`
//!
//! [`ProviderClient`] does **not** declare `count_tokens` — that method
//! lives only on the wasm side. We expose it here as an inherent method on
//! `WasmProviderClient` so callers that need it can dispatch through the
//! adapter; it is not part of the dyn-trait surface and therefore won't
//! flow into the runtime's `PROVIDERS` slot.
//!
//! ## Error mapping
//!
//! Three layers of failure can surface from this adapter:
//!
//! 1. **Plugin returned `provider-error`** → mapped via
//!    `From<wit::ProviderError> for spp::ProviderError` (defined in
//!    `spp_convert.rs`). The plugin owns the error taxonomy here.
//! 2. **Wasm trap / instantiation failure** → wrapped as a synthetic
//!    `ProviderError { kind: Transport, message: "wasmtime: ..." }` so
//!    the host sees a meaningful error class rather than a generic
//!    Internal.
//! 3. **`fetch-stream` or unimplemented capability** → not reachable from
//!    this adapter directly; the plugin would get the corresponding
//!    `HttpError`/`KeyringError` and surface it as its own
//!    `ProviderError`.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{Mutex, mpsc};
use wasmtime::Store;
use wasmtime::component::{Component, HasSelf, Linker};

use savvagent_mcp::ProviderClient;
use savvagent_protocol::{
    CompleteRequest, CompleteResponse, ErrorKind, ListModelsResponse, ProviderError, StreamEvent,
};

use crate::engine::shared_engine;
use crate::error::WasmPluginError;
use crate::host_imports::{
    http::HttpState, keyring::KeyringState, log as log_host, progress::ProgressState,
};
use crate::manifest::PluginManifest as DiskManifest;
use crate::provider_world::{
    PluginProvider, PluginProviderImports, PluginProviderPre,
    savvagent::plugin::{
        http_capability as http_wit, keyring_capability as keyring_wit,
        progress_capability as progress_wit, spp as spp_wit, types as wit,
    },
};

/// Per-store state for the provider-world wasm Store.
///
/// One fresh value per call — never shared. The four sub-fields are the
/// host implementations of the four declared imports (`log`, `http`,
/// `keyring`, `progress`).
pub(crate) struct ProviderHostState {
    /// Plugin id (`<vendor>:<rest>`) attached to every host-side log event.
    plugin_id: String,
    /// HTTP capability state. Holds the reqwest client + manifest-derived
    /// allow-list.
    http: HttpState,
    /// Keyring capability state. Holds the manifest-derived account
    /// allow-list.
    keyring: KeyringState,
    /// Progress capability state. Holds an optional `mpsc::Sender` for
    /// streaming-event forwarding.
    progress: ProgressState,
}

// ---- Host trait impls -----------------------------------------------
//
// The bindgen emits one trait per declared import (`log` is inline on
// the world; `http-capability`, `keyring-capability`, `progress-capability`
// each get a trait `Host` per interface). We implement each on
// `ProviderHostState` so `add_to_linker::<_, HasSelf<ProviderHostState>>`
// picks up all four in one shot.

// Inline `log` export on the world.
impl PluginProviderImports for ProviderHostState {
    async fn log(&mut self, level: wit::LogLevel, msg: String) {
        log_host::emit(&self.plugin_id, level, &msg);
    }
}

// `savvagent:plugin/types` is the shared (and now `with:`-aliased)
// types interface. Its bindgen trait surface is empty; the impl is a
// formality the linker needs.
impl wit::Host for ProviderHostState {}

// `savvagent:plugin/spp` — bindgen-generated empty Host trait. The spp
// interface is type-only (no functions imported by the world), so this
// is also a no-op marker.
impl spp_wit::Host for ProviderHostState {}

// `savvagent:plugin/http-capability`
impl http_wit::Host for ProviderHostState {
    async fn fetch(
        &mut self,
        req: http_wit::HttpRequest,
    ) -> Result<http_wit::HttpResponse, http_wit::HttpError> {
        self.http.fetch(req).await
    }

    async fn fetch_stream(
        &mut self,
        _req: http_wit::HttpRequest,
    ) -> Result<wasmtime::component::Resource<http_wit::HttpStream>, http_wit::HttpError> {
        // Reserved for v0.19.0. Returning a Transport error keeps the
        // failure path total without invoking unimplemented!/panic; see
        // the module docs for the rationale.
        Err(http_wit::HttpError::Transport(
            "fetch-stream is not supported by this host (savvagent v0.18.0)".to_string(),
        ))
    }
}

// `savvagent:plugin/http-capability/http-stream` — resource host impl.
// Required by the bindgen even though we never construct one: every
// method on the resource must be wired so the linker knows what to do
// if a plugin somehow obtains a handle. We return Transport for all of
// them, matching the `fetch-stream` denial above.
impl http_wit::HostHttpStream for ProviderHostState {
    async fn status(&mut self, _rep: wasmtime::component::Resource<http_wit::HttpStream>) -> u16 {
        0
    }

    async fn headers(
        &mut self,
        _rep: wasmtime::component::Resource<http_wit::HttpStream>,
    ) -> Vec<(String, String)> {
        Vec::new()
    }

    async fn next_chunk(
        &mut self,
        _rep: wasmtime::component::Resource<http_wit::HttpStream>,
    ) -> Result<Option<Vec<u8>>, http_wit::HttpError> {
        Err(http_wit::HttpError::Transport(
            "fetch-stream is not supported by this host (savvagent v0.18.0)".to_string(),
        ))
    }

    async fn drop(
        &mut self,
        _rep: wasmtime::component::Resource<http_wit::HttpStream>,
    ) -> wasmtime::Result<()> {
        Ok(())
    }
}

// `savvagent:plugin/keyring-capability`
impl keyring_wit::Host for ProviderHostState {
    async fn get(&mut self, account: String) -> Result<String, keyring_wit::KeyringError> {
        self.keyring.get(&account)
    }
}

// `savvagent:plugin/progress-capability`
impl progress_wit::Host for ProviderHostState {
    async fn emit_stream_event(&mut self, event: progress_wit::StreamEvent) {
        self.progress.emit(event).await;
    }
}

// ---- Adapter --------------------------------------------------------

/// Adapter that wraps a `plugin-provider` wasm component as a
/// `Box<dyn ProviderClient>`.
///
/// Construction does the bindgen typecheck and caches an `InstancePre`;
/// every `complete` / `list_models` / `count_tokens` call mints a fresh
/// Store, instantiates, calls, and drops the Store.
pub struct WasmProviderClient {
    /// Long-lived pre-instantiated component. Cloning is `Arc`-cheap.
    instance_pre: Arc<PluginProviderPre<ProviderHostState>>,
    /// Parsed `plugin.toml`. Needed at every call to construct the
    /// per-store `HttpState`/`KeyringState` (their allow-lists come from
    /// `[security]`).
    disk_manifest: Arc<DiskManifest>,
    /// Reserved for the per-call store pool (see module docs). Always
    /// empty in v0.18.0; the field is held so a future revision can wire
    /// a `try_pop`-or-new path without an ABI break.
    _store_pool: Mutex<Vec<Store<ProviderHostState>>>,
}

impl std::fmt::Debug for WasmProviderClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmProviderClient")
            .field("plugin_id", &self.disk_manifest.plugin.id.as_str())
            .field(
                "provider_id",
                &self.disk_manifest.exports.provider_id.as_deref(),
            )
            .finish()
    }
}

impl WasmProviderClient {
    /// Construct a `WasmProviderClient` by loading `plugin.wasm` from
    /// `plugin_dir`, wiring all four host imports, and pre-instantiating
    /// the component.
    ///
    /// The wasm file is loaded once; per-call instantiation reuses the
    /// `Component` indirectly via the `InstancePre`.
    pub async fn new(
        disk_manifest: Arc<DiskManifest>,
        plugin_dir: &Path,
    ) -> Result<Self, WasmPluginError> {
        let engine = shared_engine()?;
        let wasm_path = plugin_dir.join("plugin.wasm");
        let component =
            Component::from_file(&engine, &wasm_path).map_err(WasmPluginError::Wasmtime)?;

        let mut linker: Linker<ProviderHostState> = Linker::new(&engine);
        PluginProvider::add_to_linker::<_, HasSelf<ProviderHostState>>(&mut linker, |s| s)
            .map_err(WasmPluginError::Wasmtime)?;

        let pre = linker
            .instantiate_pre(&component)
            .map_err(WasmPluginError::Wasmtime)?;
        let plugin_pre = PluginProviderPre::new(pre).map_err(WasmPluginError::Wasmtime)?;

        Ok(Self {
            instance_pre: Arc::new(plugin_pre),
            disk_manifest,
            _store_pool: Mutex::new(Vec::new()),
        })
    }

    /// Borrow the parsed `plugin.toml` this adapter was constructed from.
    /// Used by tests and Task 9's `PROVIDERS` extender to read
    /// `[exports] provider-id` without re-parsing.
    pub fn disk_manifest(&self) -> &Arc<DiskManifest> {
        &self.disk_manifest
    }

    /// Build a fresh `ProviderHostState` for one call.
    ///
    /// `events` is `Some(sender)` for `complete` (when the caller asked
    /// for streaming) and `None` for `list_models` / `count_tokens`.
    /// The allow-lists are pulled out of the cached manifest every call;
    /// they're cheap `Vec<String>` clones (the manifest's `[security]`
    /// table is small).
    fn new_host_state(&self, events: Option<mpsc::Sender<StreamEvent>>) -> ProviderHostState {
        // `[security]` is provider-world-only (enforced by
        // `manifest.rs`), but the field is still `Option<...>` — an
        // absent section means empty allow-lists, which is the most
        // restrictive setting we can derive automatically.
        let (allowed_hosts, keyring_accounts) = match &self.disk_manifest.security {
            Some(s) => (s.allowed_hosts.clone(), s.keyring_accounts.clone()),
            None => (Vec::new(), Vec::new()),
        };
        let progress = match events {
            Some(tx) => ProgressState::enabled(tx),
            None => ProgressState::disabled(),
        };
        ProviderHostState {
            plugin_id: self.disk_manifest.plugin.id.clone(),
            http: HttpState::new(allowed_hosts),
            keyring: KeyringState::new(keyring_accounts),
            progress,
        }
    }

    /// Call `count-tokens` against the plugin. Not part of the
    /// `ProviderClient` trait surface — exposed here as an inherent
    /// method so callers that need it can dispatch through the adapter.
    ///
    /// `model` and `messages` are passed verbatim into the WIT-side
    /// `count-tokens-request`; this mirrors the request shape declared
    /// in `spp.wit`.
    pub async fn count_tokens(
        &self,
        req: CountTokensRequest,
    ) -> Result<CountTokensResponse, ProviderError> {
        let engine = shared_engine().map_err(|e| wasm_error_to_provider_error(&e.to_string()))?;
        let state = self.new_host_state(None);
        let mut store = Store::new(&engine, state);
        let instance = self
            .instance_pre
            .instantiate_async(&mut store)
            .await
            .map_err(|e| wasm_error_to_provider_error(&format!("instantiate: {e}")))?;
        let wit_req = spp_wit::CountTokensRequest {
            model: req.model,
            messages: req.messages.into_iter().map(Into::into).collect(),
        };
        let result = instance
            .call_count_tokens(&mut store, &wit_req)
            .await
            .map_err(|e| wasm_error_to_provider_error(&format!("count_tokens trap: {e}")))?;
        match result {
            Ok(resp) => Ok(CountTokensResponse {
                input_tokens: resp.input_tokens,
            }),
            Err(e) => Err(e.into()),
        }
    }
}

#[async_trait]
impl ProviderClient for WasmProviderClient {
    async fn complete(
        &self,
        req: CompleteRequest,
        events: Option<mpsc::Sender<StreamEvent>>,
    ) -> Result<CompleteResponse, ProviderError> {
        let engine = shared_engine().map_err(|e| wasm_error_to_provider_error(&e.to_string()))?;
        let state = self.new_host_state(events);
        let mut store = Store::new(&engine, state);
        let instance = self
            .instance_pre
            .instantiate_async(&mut store)
            .await
            .map_err(|e| wasm_error_to_provider_error(&format!("instantiate: {e}")))?;
        let wit_req: spp_wit::CompleteRequest = req.into();
        let result = instance
            .call_complete(&mut store, &wit_req)
            .await
            .map_err(|e| wasm_error_to_provider_error(&format!("complete trap: {e}")))?;
        result.map(Into::into).map_err(Into::into)
    }

    async fn list_models(&self) -> Result<ListModelsResponse, ProviderError> {
        let engine = shared_engine().map_err(|e| wasm_error_to_provider_error(&e.to_string()))?;
        let state = self.new_host_state(None);
        let mut store = Store::new(&engine, state);
        let instance = self
            .instance_pre
            .instantiate_async(&mut store)
            .await
            .map_err(|e| wasm_error_to_provider_error(&format!("instantiate: {e}")))?;
        let result = instance
            .call_list_models(&mut store)
            .await
            .map_err(|e| wasm_error_to_provider_error(&format!("list_models trap: {e}")))?;
        result.map(Into::into).map_err(Into::into)
    }
}

/// Free-form `count-tokens` request used by [`WasmProviderClient::count_tokens`].
///
/// `count-tokens` has no [`savvagent_protocol`] counterpart, so we
/// declare a small local type rather than dragging the WIT-level type
/// out into the public surface.
#[derive(Debug, Clone)]
pub struct CountTokensRequest {
    /// Model id the count is being computed against.
    pub model: String,
    /// Messages whose token count the plugin should compute.
    pub messages: Vec<savvagent_protocol::Message>,
}

/// Free-form `count-tokens` response. Mirrors the WIT-side record
/// field-for-field.
#[derive(Debug, Clone)]
pub struct CountTokensResponse {
    /// Total input-side token count.
    pub input_tokens: u32,
}

/// Wrap a wasmtime / instantiation / trap error string into the
/// `ProviderError` shape host code expects. We always use
/// `ErrorKind::Internal` — these failures aren't transport-layer or
/// vendor-side, they're host-side. Plugin authors see them in logs
/// regardless.
fn wasm_error_to_provider_error(msg: &str) -> ProviderError {
    ProviderError {
        kind: ErrorKind::Internal,
        message: format!("wasmtime: {msg}"),
        retry_after_ms: None,
        provider_code: None,
    }
}
