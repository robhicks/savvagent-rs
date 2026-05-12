//! Sequential awaited dispatch of [`HostEvent`]s to subscribed plugins.
//!
//! [`HookDispatcher`] is the lookup-and-call surface that the runtime uses
//! to fan a single host event out to every plugin that subscribed to its
//! [`HookKind`]. Subscribers are awaited one at a time in registration
//! order (the order in which [`crate::plugin::manifests::Indexes::build`]
//! visited their manifests); a slow subscriber will stall later ones, by
//! design — plugins may need to observe ordering and the spec promises a
//! single-threaded delivery model.
//!
//! Plugin errors from `on_event` are logged via `tracing::warn` and the
//! offending plugin is skipped, but the loop continues so one buggy
//! subscriber cannot starve others of the event.
//!
//! Effect application happens **outside** this dispatcher. `emit` returns
//! the accumulated effects so the caller (`effects::dispatch_host_event`
//! for in-app dispatch, the TUI event loop for host-originated dispatch)
//! can apply them through the shared
//! [`crate::plugin::effects::apply_effects`] mutation surface — which is
//! where re-entrancy depth tracking lives. Keeping apply out of the
//! dispatcher avoids two competing depth counters.

use savvagent_plugin::{Effect, HostEvent};

use crate::plugin::manifests::Indexes;
use crate::plugin::registry::PluginRegistry;

/// Sequential awaited dispatcher for [`HostEvent`]s.
///
/// Borrows the [`Indexes`] (for subscriber lookup) and [`PluginRegistry`]
/// (for plugin handles) for the duration of a single `emit` call. The
/// dispatcher is otherwise stateless — depth tracking lives on the
/// effect-apply side.
pub struct HookDispatcher<'a> {
    indexes: &'a Indexes,
    registry: &'a PluginRegistry,
}

impl<'a> HookDispatcher<'a> {
    /// Construct a dispatcher backed by `indexes` (for `hooks` lookup) and
    /// `registry` (for plugin handles).
    pub fn new(indexes: &'a Indexes, registry: &'a PluginRegistry) -> Self {
        Self { indexes, registry }
    }

    /// Fan `event` out to every subscribed plugin and accumulate their
    /// returned effects.
    ///
    /// Subscribers are visited in registration order. A plugin whose
    /// `on_event` returns an error is logged via `tracing::warn` and
    /// skipped; later subscribers still see the event. Effects returned
    /// by each subscriber are appended in order — callers can apply the
    /// full vector with [`crate::plugin::effects::apply_effects`] (or its
    /// depth-tracking inner variant) when ready.
    pub async fn emit(&self, event: HostEvent) -> Vec<Effect> {
        let kind = event.kind();
        let subs = self.indexes.hooks.get(&kind).cloned().unwrap_or_default();
        let mut out = Vec::new();
        for pid in subs {
            let Some(handle) = self.registry.get(&pid) else {
                tracing::warn!(
                    plugin_id = %pid.as_str(),
                    event_kind = ?kind,
                    "hook subscriber present in Indexes but missing from registry; skipping"
                );
                continue;
            };
            let mut plugin = handle.lock().await;
            match plugin.on_event(event.clone()).await {
                Ok(effects) => out.extend(effects),
                Err(e) => {
                    tracing::warn!(
                        plugin_id = %pid.as_str(),
                        event_kind = ?kind,
                        error = %e,
                        "plugin on_event returned an error; skipping"
                    );
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use savvagent_plugin::{
        Contributions, HookKind, Manifest, Plugin, PluginError, PluginId, PluginKind, StyledLine,
    };

    /// Test plugin that counts `on_event` invocations and returns a single
    /// `PushNote` effect with its id, so the dispatcher's accumulation can
    /// be observed in the returned Vec.
    struct Counter {
        id: String,
        count: std::sync::atomic::AtomicU32,
    }

    #[async_trait]
    impl Plugin for Counter {
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
            self.count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(vec![Effect::PushNote {
                line: StyledLine::plain(self.id.clone()),
            }])
        }
    }

    /// Plugin that errors from `on_event` to exercise the warn-and-skip
    /// branch. Manifest subscribes to `HostStarting` so it shows up in the
    /// dispatch path.
    struct ErrorOnEvent {
        id: String,
    }

    #[async_trait]
    impl Plugin for ErrorOnEvent {
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
            Err(PluginError::Internal("boom".into()))
        }
    }

    #[tokio::test]
    async fn dispatch_calls_each_subscriber_once() {
        let reg = PluginRegistry::from_plugins(vec![
            Box::new(Counter {
                id: "internal:test-a".into(),
                count: Default::default(),
            }) as Box<dyn Plugin>,
            Box::new(Counter {
                id: "internal:test-b".into(),
                count: Default::default(),
            }),
        ]);
        let idx = Indexes::build(&reg).await.expect("indexes build");
        let d = HookDispatcher::new(&idx, &reg);
        let effs = d.emit(HostEvent::HostStarting).await;
        assert_eq!(
            effs.len(),
            2,
            "expected one effect from each subscriber; got: {effs:?}"
        );
    }

    #[tokio::test]
    async fn dispatch_skips_subscriber_that_errors() {
        // Errors must not abort fan-out — the second subscriber still gets
        // its effect into the output Vec.
        let reg = PluginRegistry::from_plugins(vec![
            Box::new(ErrorOnEvent {
                id: "internal:test-bad".into(),
            }) as Box<dyn Plugin>,
            Box::new(Counter {
                id: "internal:test-good".into(),
                count: Default::default(),
            }),
        ]);
        let idx = Indexes::build(&reg).await.expect("indexes build");
        let d = HookDispatcher::new(&idx, &reg);
        let effs = d.emit(HostEvent::HostStarting).await;
        assert_eq!(
            effs.len(),
            1,
            "good subscriber must still contribute despite earlier error"
        );
    }

    #[tokio::test]
    async fn dispatch_with_no_subscribers_returns_empty() {
        let reg = PluginRegistry::from_plugins(vec![]);
        let idx = Indexes::build(&reg).await.expect("indexes build");
        let d = HookDispatcher::new(&idx, &reg);
        let effs = d.emit(HostEvent::HostStarting).await;
        assert!(effs.is_empty());
    }
}
