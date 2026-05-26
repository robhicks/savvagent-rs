//! Wasmtime-backed runtime for savvagent external plugins.
//!
//! This crate adapts WASM components implementing one of three WIT worlds
//! (plugin-static / plugin-interactive / plugin-provider) to
//! `Box<dyn savvagent_plugin::Plugin>` — making them indistinguishable from
//! built-ins to the rest of the host.
//!
//! Tasks 1–2 landed the WIT contract and the host-side bindgen output.
//! Task 3 (this revision) adds the runtime's discovery + trust layer:
//!
//! - [`error`] — the runtime's error enum, `WasmPluginError`.
//! - [`manifest`] — parser/validator for `plugin.toml`.
//! - [`discovery`] — walks the four well-known directories and dedupes
//!   plugins first-wins by id, mirroring sub-projects A/B/C.
//! - [`trust`] — `~/.savvagent/plugin-trust.toml` ledger + the SHA-256
//!   `tree_hash` that anchors it.
//!
//! Discovery + trust enforcement, capability host impls, and the actual
//! adapter glue all land in Tasks 4–6.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod discovery;
pub mod error;
pub mod manifest;
pub mod spp_convert;
pub mod trust;

/// Re-export of the WIT-resources crate so downstream callers don't have to
/// pull it in separately when they want the canonical `WIT_DIR` path.
pub use savvagent_plugin_wit as wit;

// ---- Host-side bindings ------------------------------------------------
//
// `wasmtime::component::bindgen!` expands the `.wit` tree at the given
// `path:` into Rust types, traits, and a `World` struct. The macro is a
// proc-macro and therefore requires a *string-literal* path at the call
// site — it cannot read `savvagent_plugin_wit::WIT_DIR` even though that
// would resolve to the same directory. The relative path below resolves
// from this crate's `src/` to the sibling crate's `wit/` directory.
//
// Each world gets its own module to keep the three sets of generated
// types from colliding. `async: true` is required so the generated traits
// match the async wasmtime store the adapters will use in Tasks 4–6.

/// Host bindings for the `plugin-static` world.
#[allow(missing_docs, clippy::needless_lifetimes)]
pub mod static_world {
    wasmtime::component::bindgen!({
        path: "../savvagent-plugin-wit/wit",
        world: "plugin-static",
        async: true,
    });
}

/// Host bindings for the `plugin-interactive` world.
#[allow(missing_docs, clippy::needless_lifetimes)]
pub mod interactive_world {
    wasmtime::component::bindgen!({
        path: "../savvagent-plugin-wit/wit",
        world: "plugin-interactive",
        async: true,
    });
}

/// Host bindings for the `plugin-provider` world.
#[allow(missing_docs, clippy::needless_lifetimes)]
pub mod provider_world {
    wasmtime::component::bindgen!({
        path: "../savvagent-plugin-wit/wit",
        world: "plugin-provider",
        async: true,
    });
}
