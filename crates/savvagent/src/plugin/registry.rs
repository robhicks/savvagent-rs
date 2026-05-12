//! In-memory registry of constructed plugin instances + enabled-set.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use savvagent_mcp::ProviderClient;
use savvagent_plugin::{Plugin, PluginId};
use tokio::sync::Mutex;

use crate::plugin::builtin::provider_common::BuiltinProviderPlugin;

/// Plugin instances + provider-plugin parallel map handed to
/// [`PluginRegistry::new`].
///
/// Returned by [`crate::plugin::register_builtins`]. The two vectors are
/// parallel rather than merged because [`BuiltinProviderPlugin`] is *not*
/// part of `savvagent-plugin` — see `provider_common.rs` for the rationale.
#[derive(Default)]
pub struct BuiltinSet {
    /// Every plugin (including provider plugins, double-allocated) that
    /// should appear in the runtime's `dyn Plugin` registry.
    pub plugins: Vec<Box<dyn Plugin>>,
    /// Provider plugins, indexed separately so the runtime can call
    /// [`BuiltinProviderPlugin::take_client`] on them after observing
    /// [`savvagent_plugin::Effect::RegisterProvider`].
    pub providers: Vec<Box<dyn BuiltinProviderPlugin>>,
}

/// Stores plugin instances behind `Arc<Mutex<Box<dyn Plugin>>>` keyed by
/// `PluginId`; tracks an enabled-set. Provider plugins are additionally
/// stored in a parallel map keyed by [`PluginId`] so the runtime can call
/// [`BuiltinProviderPlugin::take_client`] without trait-object downcasting.
pub struct PluginRegistry {
    plugins: HashMap<PluginId, Arc<Mutex<Box<dyn Plugin>>>>,
    providers: HashMap<PluginId, Arc<Mutex<Box<dyn BuiltinProviderPlugin>>>>,
    enabled: HashSet<PluginId>,
}

impl PluginRegistry {
    /// Test-only convenience: construct from a vector of plugins (no
    /// provider-plugin parallel map). Production code uses
    /// [`PluginRegistry::new`] with the [`BuiltinSet`] returned by
    /// [`crate::plugin::register_builtins`].
    #[cfg(test)]
    pub fn from_plugins(plugins: Vec<Box<dyn Plugin>>) -> Self {
        Self::new(BuiltinSet {
            plugins,
            providers: vec![],
        })
    }

    /// Construct from a [`BuiltinSet`]. Every plugin is inserted into the
    /// registry and added to the enabled set; every provider plugin is
    /// additionally indexed in the parallel provider map. The enabled
    /// set is rewound at startup by reading `plugins.toml` (PR 8).
    pub fn new(set: BuiltinSet) -> Self {
        let BuiltinSet { plugins, providers } = set;
        let mut map = HashMap::with_capacity(plugins.len());
        let mut enabled = HashSet::with_capacity(plugins.len());
        for p in plugins {
            let id = p.manifest().id.clone();
            enabled.insert(id.clone());
            map.insert(id, Arc::new(Mutex::new(p)));
        }
        let mut provider_map = HashMap::with_capacity(providers.len());
        for p in providers {
            let id = p.manifest().id.clone();
            provider_map.insert(id, Arc::new(Mutex::new(p)));
        }
        Self {
            plugins: map,
            providers: provider_map,
            enabled,
        }
    }

    /// Returns the total number of registered plugins (enabled and disabled).
    #[allow(dead_code)] // used by the plugins-manager screen in PR 8
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    /// Returns `true` if no plugins are registered.
    #[allow(dead_code)] // satisfies the is_empty/len lint pair; used in PR 8
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Iterates over the [`PluginId`]s of every currently-enabled plugin.
    pub fn enabled_ids(&self) -> impl Iterator<Item = &PluginId> {
        self.plugins.keys().filter(|id| self.enabled.contains(id))
    }

    /// Returns the shared plugin handle for `id`, or `None` if not registered.
    pub fn get(&self, id: &PluginId) -> Option<Arc<Mutex<Box<dyn Plugin>>>> {
        self.plugins.get(id).cloned()
    }

    /// Returns `true` if `id` is in the enabled set.
    #[allow(dead_code)] // used by the plugins-manager screen in PR 8
    pub fn is_enabled(&self, id: &PluginId) -> bool {
        self.enabled.contains(id)
    }

    /// Adds or removes `id` from the enabled set. Enabling an unregistered id
    /// is a no-op at query time but is otherwise stored; callers should only
    /// enable ids that are registered.
    #[allow(dead_code)] // used by the plugins-manager screen in PR 8
    pub fn set_enabled(&mut self, id: &PluginId, enabled: bool) {
        if enabled {
            self.enabled.insert(id.clone());
        } else {
            self.enabled.remove(id);
        }
    }

    /// Take the constructed provider client out of the provider plugin
    /// associated with `id`, leaving the plugin's slot empty. Returns
    /// `None` if `id` is unknown or the plugin doesn't currently hold a
    /// client. Called by [`crate::plugin::effects::apply_effects`] after
    /// observing [`savvagent_plugin::Effect::RegisterProvider`].
    pub async fn take_provider_client(&self, id: &PluginId) -> Option<Box<dyn ProviderClient>> {
        let handle = self.providers.get(id)?.clone();
        let mut guard = handle.lock().await;
        guard.take_client()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use savvagent_plugin::{Contributions, Manifest, PluginKind};

    /// Test plugin that takes a vendor-prefixed id and produces a Manifest
    /// using PluginId::new() (validated).
    struct Empty(String);

    #[async_trait]
    impl Plugin for Empty {
        fn manifest(&self) -> Manifest {
            Manifest {
                id: PluginId::new(&self.0).expect("test ids must be valid"),
                name: self.0.clone(),
                version: "0.0.0".into(),
                description: "test".into(),
                kind: PluginKind::Optional,
                contributions: Contributions::default(),
            }
        }
    }

    #[test]
    fn registry_indexes_by_id() {
        let reg = PluginRegistry::from_plugins(vec![
            Box::new(Empty("test:a".into())),
            Box::new(Empty("test:b".into())),
        ]);
        assert_eq!(reg.len(), 2);
        let id_a = PluginId::new("test:a").unwrap();
        let id_b = PluginId::new("test:b").unwrap();
        assert!(reg.is_enabled(&id_a));
        assert!(reg.is_enabled(&id_b));
    }

    #[test]
    fn disable_removes_from_enabled_set() {
        let mut reg = PluginRegistry::from_plugins(vec![Box::new(Empty("test:x".into()))]);
        let id = PluginId::new("test:x").unwrap();
        assert!(reg.is_enabled(&id));
        reg.set_enabled(&id, false);
        assert!(!reg.is_enabled(&id));
    }
}
