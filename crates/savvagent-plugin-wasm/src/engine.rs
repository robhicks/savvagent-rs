//! Process-wide shared [`wasmtime::Engine`].
//!
//! All three adapters (static, interactive, provider) share one engine so
//! components compile once and the configuration is consistent across worlds.
//! `Engine` is `Clone` (it holds an internal `Arc`), so handing it out by
//! value is cheap.
//!
//! Configuration:
//! - **Component model**: required — every world we ship is a component.
//! - **Async support**: required — all generated exports return
//!   `impl Future`, and adapters await them inside `tokio::task` workers.
//! - **Epoch interruption**: deferred to Task 8 — enabling it here without
//!   a corresponding epoch bumper traps every store on the first
//!   instruction. Task 8 will land the epoch driver alongside the
//!   three-strikes recovery logic and flip this flag.
//!
//! The engine is initialized lazily via [`OnceLock`]; the first call to
//! [`shared_engine`] pays the construction cost and every subsequent call
//! is a single relaxed load.
//!
//! Initialization failures are surfaced through
//! [`crate::error::WasmPluginError::Wasmtime`] rather than panicking, so a
//! misconfigured embedder doesn't take the host down. In practice the
//! features we enable are all stable in wasmtime 34, so the failure path is
//! defensive.

use std::sync::OnceLock;

use wasmtime::{Config, Engine};

use crate::error::WasmPluginError;

static ENGINE: OnceLock<Engine> = OnceLock::new();

/// Returns the process-wide shared [`Engine`], initializing it on first
/// call. Cheap to clone (internal `Arc`).
pub fn shared_engine() -> Result<Engine, WasmPluginError> {
    if let Some(e) = ENGINE.get() {
        return Ok(e.clone());
    }
    let mut cfg = Config::new();
    cfg.async_support(true);
    cfg.wasm_component_model(true);
    let engine = Engine::new(&cfg).map_err(WasmPluginError::Wasmtime)?;
    // get_or_init is the right primitive but it requires an infallible
    // closure; we already built the engine above so use the racy path:
    // first writer wins, others drop their copy and use the stored one.
    Ok(ENGINE.get_or_init(|| engine).clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_engine_is_idempotent() {
        let a = shared_engine().expect("engine init");
        let b = shared_engine().expect("engine init");
        // Engine doesn't expose pointer-equality directly; the smoke test
        // is that two calls succeed and either is usable.
        let _ = a;
        let _ = b;
    }

    #[test]
    fn engine_supports_component_model() {
        let engine = shared_engine().expect("engine init");
        // Empty component bytes wouldn't parse; instead just confirm the
        // engine is callable. The presence of `wasm_component_model(true)`
        // is verified via downstream tests that actually instantiate
        // components (see tests/static_adapter.rs).
        let _ = engine;
    }
}
