//! Savvagent-internal trait that provider plugins implement in addition to
//! [`Plugin`]. See spec §6: this is the explicit non-WIT-portable seam where
//! the `Box<dyn ProviderClient>` hand-off happens. v1.0 will redesign this
//! as proper WIT resource ownership; for now the trait lives in the
//! `savvagent` crate (NOT in `savvagent-plugin`) precisely because it
//! traffics in `Box<dyn ProviderClient>`.

use savvagent_mcp::ProviderClient;
use savvagent_plugin::Plugin;

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
pub trait BuiltinProviderPlugin: Plugin {
    /// Take the constructed provider client out of the plugin, leaving
    /// `None` behind. Called by the runtime after observing
    /// [`savvagent_plugin::Effect::RegisterProvider`] from the same plugin.
    fn take_client(&mut self) -> Option<Box<dyn ProviderClient>>;
}
