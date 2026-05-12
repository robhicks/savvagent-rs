//! Savvagent-internal trait that provider plugins implement in addition to
//! [`Plugin`]. See spec §6: this is the explicit non-WIT-portable seam where
//! the `Box<dyn ProviderClient>` hand-off happens. v1.0 will redesign this
//! as proper WIT resource ownership; for now the trait lives in the
//! `savvagent` crate (NOT in `savvagent-plugin`) precisely because it
//! traffics in `Box<dyn ProviderClient>`.

use std::sync::Arc;

use savvagent_mcp::ProviderClient;
use savvagent_plugin::Plugin;
use tokio::sync::Mutex;

/// Provider plugins implement this in addition to [`Plugin`]. The runtime
/// calls [`take_client`] after observing an [`savvagent_plugin::Effect::RegisterProvider`]
/// emitted by the same plugin's [`Plugin::handle_slash`] or
/// [`Plugin::on_event`] output.
///
/// Returning [`None`] means "credentials not yet available" — the plugin
/// should have emitted a [`savvagent_plugin::Effect::PushNote`] explaining
/// the situation alongside its (premature) `RegisterProvider` emission, or
/// — more typically — should have avoided emitting `RegisterProvider` at
/// all until it had a client to hand off.
pub(crate) trait BuiltinProviderPlugin: Plugin {
    /// Take the constructed provider client out of the plugin, leaving
    /// `None` behind. Called by the runtime after observing
    /// [`savvagent_plugin::Effect::RegisterProvider`] from the same plugin.
    fn take_client(&mut self) -> Option<Box<dyn ProviderClient>>;
}

/// A single provider-plugin instance exposed as two trait-object Arcs that
/// share the same underlying state. Constructed once at startup via
/// [`ProviderEntry::new`] from the concrete plugin type; Rust's unsize
/// coercion then yields both views from the same `Arc<Mutex<T>>` so the
/// slash router (which sees `dyn Plugin`) and `take_provider_client` (which
/// sees `dyn BuiltinProviderPlugin`) read and mutate the **same** instance.
///
/// Previously the runtime allocated two separate instances per provider
/// plugin — one in the `plugins` map, one in the `providers` map — which
/// caused every `/connect <provider>` to fail with "no client constructed"
/// because the slash handler built a client into the plugins-side instance
/// while `take_provider_client` consulted the providers-side instance.
pub(crate) struct ProviderEntry {
    /// Provider-trait view used by the runtime's `take_client` call site.
    pub as_provider: Arc<Mutex<dyn BuiltinProviderPlugin>>,
    /// Plugin-trait view used by the slash/render/hook dispatch paths.
    pub as_plugin: Arc<Mutex<dyn Plugin>>,
}

impl ProviderEntry {
    /// Build a [`ProviderEntry`] from a concrete provider plugin type.
    /// Both trait-object Arcs point at the same `Arc<Mutex<T>>` under the
    /// hood — there is exactly one instance per call.
    pub fn new<T>(plugin: T) -> Self
    where
        T: BuiltinProviderPlugin + 'static,
    {
        let concrete: Arc<Mutex<T>> = Arc::new(Mutex::new(plugin));
        // Unsize coercion from the concrete `Arc<Mutex<T>>` to each
        // trait-object Arc. Both views share the same allocation.
        let as_provider: Arc<Mutex<dyn BuiltinProviderPlugin>> = concrete.clone();
        let as_plugin: Arc<Mutex<dyn Plugin>> = concrete;
        Self {
            as_provider,
            as_plugin,
        }
    }
}
