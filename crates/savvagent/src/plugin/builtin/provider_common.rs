//! Savvagent-internal trait that provider plugins implement in addition to
//! [`Plugin`]. See spec §6: this is the explicit non-WIT-portable seam where
//! the `Box<dyn ProviderClient>` hand-off happens. v1.0 will redesign this
//! as proper WIT resource ownership; for now the trait lives in the
//! `savvagent` crate (NOT in `savvagent-plugin`) precisely because it
//! traffics in `Box<dyn ProviderClient>`.

use std::collections::HashMap;
use std::sync::Arc;

use savvagent_host::{CostTier, ModelCapabilities, ProviderCapabilities};
use savvagent_mcp::ProviderClient;
use savvagent_plugin::Plugin;
use savvagent_protocol::ListModelsResponse;
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

/// Build a [`ProviderCapabilities`] from a live `list_models` response, reusing
/// hand-curated metadata (vision/audio support, context window, cost tier) for
/// any model whose id matches one in `static_fallback`. Models discovered only
/// at runtime get conservative defaults: `supports_vision=false`,
/// `supports_audio=false`, `context_window` from the response when present
/// else `0`, and `cost_tier = Standard`.
///
/// The default model preference cascade is:
/// 1. `resp.default_model_id`, if present in the dynamic list.
/// 2. The `static_fallback` default, if still present in the dynamic list.
/// 3. The first dynamic model.
///
/// Returns `None` when the dynamic list is empty (caller should fall back to
/// the static catalog).
pub(crate) fn caps_from_list_models(
    resp: ListModelsResponse,
    static_fallback: &ProviderCapabilities,
) -> Option<ProviderCapabilities> {
    if resp.models.is_empty() {
        return None;
    }

    let static_lookup: HashMap<&str, &ModelCapabilities> = static_fallback
        .models()
        .iter()
        .map(|m| (m.id.as_str(), m))
        .collect();

    let models: Vec<ModelCapabilities> = resp
        .models
        .into_iter()
        .map(|m| {
            if let Some(known) = static_lookup.get(m.id.as_str()) {
                (*known).clone()
            } else {
                ModelCapabilities {
                    id: m.id.clone(),
                    display_name: m.display_name.unwrap_or_else(|| m.id.clone()),
                    supports_vision: false,
                    supports_audio: false,
                    context_window: m.context_window.unwrap_or(0) as usize,
                    cost_tier: CostTier::Standard,
                }
            }
        })
        .collect();

    let chosen_default = resp
        .default_model_id
        .filter(|id| models.iter().any(|m| &m.id == id))
        .or_else(|| {
            let sd = static_fallback.default_model_id().to_string();
            models.iter().any(|m| m.id == sd).then_some(sd)
        })
        .unwrap_or_else(|| models[0].id.clone());

    ProviderCapabilities::new(models, chosen_default).ok()
}

/// Call `client.list_models()`, fall back to `static_fallback` on any error or
/// empty response. Emits a `tracing::warn` with the provider name and reason
/// when falling back so a user staring at the TUI can correlate a stale `/model`
/// picker with a network/credential issue.
pub(crate) async fn build_dynamic_caps(
    client: &(dyn ProviderClient + Send + Sync),
    static_fallback: ProviderCapabilities,
    provider_log_name: &str,
) -> ProviderCapabilities {
    match client.list_models().await {
        Ok(resp) => match caps_from_list_models(resp, &static_fallback) {
            Some(dynamic) => dynamic,
            None => {
                tracing::warn!(
                    provider = provider_log_name,
                    "list_models returned empty list; falling back to static catalog"
                );
                static_fallback
            }
        },
        Err(e) => {
            tracing::warn!(
                provider = provider_log_name,
                error = %e,
                "list_models failed; falling back to static catalog"
            );
            static_fallback
        }
    }
}

#[cfg(test)]
mod caps_helpers_tests {
    use super::*;
    use savvagent_host::{CostTier, ModelCapabilities};
    use savvagent_protocol::{ListModelsResponse, ModelInfo};

    fn static_caps() -> ProviderCapabilities {
        ProviderCapabilities::new(
            vec![
                ModelCapabilities {
                    id: "known-flash".into(),
                    display_name: "Known Flash".into(),
                    supports_vision: true,
                    supports_audio: false,
                    context_window: 1_000_000,
                    cost_tier: CostTier::Cheap,
                },
                ModelCapabilities {
                    id: "known-pro".into(),
                    display_name: "Known Pro".into(),
                    supports_vision: true,
                    supports_audio: false,
                    context_window: 1_000_000,
                    cost_tier: CostTier::Premium,
                },
            ],
            "known-flash".into(),
        )
        .expect("static caps must build")
    }

    #[test]
    fn known_models_reuse_static_metadata() {
        let resp = ListModelsResponse {
            models: vec![ModelInfo {
                id: "known-pro".into(),
                display_name: Some("Server-provided name".into()),
                context_window: None,
            }],
            default_model_id: None,
        };
        let caps = caps_from_list_models(resp, &static_caps()).expect("must build");
        let m = caps.model("known-pro").expect("present");
        // Static metadata wins over the server-provided display_name
        // because we trust the curated record.
        assert_eq!(m.display_name, "Known Pro");
        assert!(m.supports_vision);
        assert!(matches!(m.cost_tier, CostTier::Premium));
        assert_eq!(m.context_window, 1_000_000);
    }

    #[test]
    fn unknown_models_get_conservative_defaults() {
        let resp = ListModelsResponse {
            models: vec![ModelInfo {
                id: "brand-new".into(),
                display_name: Some("Brand New".into()),
                context_window: Some(8_192),
            }],
            default_model_id: None,
        };
        let caps = caps_from_list_models(resp, &static_caps()).expect("must build");
        let m = caps.model("brand-new").expect("present");
        assert_eq!(m.display_name, "Brand New");
        assert!(!m.supports_vision);
        assert!(!m.supports_audio);
        assert_eq!(m.context_window, 8_192);
        assert!(matches!(m.cost_tier, CostTier::Standard));
    }

    #[test]
    fn default_prefers_response_then_static_then_first() {
        // 1. response-supplied default wins.
        let resp = ListModelsResponse {
            models: vec![
                ModelInfo {
                    id: "known-pro".into(),
                    display_name: None,
                    context_window: None,
                },
                ModelInfo {
                    id: "known-flash".into(),
                    display_name: None,
                    context_window: None,
                },
            ],
            default_model_id: Some("known-pro".into()),
        };
        let caps = caps_from_list_models(resp, &static_caps()).expect("must build");
        assert_eq!(caps.default_model_id(), "known-pro");

        // 2. response default missing → static default still present → static wins.
        let resp = ListModelsResponse {
            models: vec![
                ModelInfo {
                    id: "known-pro".into(),
                    display_name: None,
                    context_window: None,
                },
                ModelInfo {
                    id: "known-flash".into(),
                    display_name: None,
                    context_window: None,
                },
            ],
            default_model_id: None,
        };
        let caps = caps_from_list_models(resp, &static_caps()).expect("must build");
        assert_eq!(caps.default_model_id(), "known-flash");

        // 3. neither in list → first wins.
        let resp = ListModelsResponse {
            models: vec![ModelInfo {
                id: "brand-new".into(),
                display_name: None,
                context_window: None,
            }],
            default_model_id: Some("not-in-list".into()),
        };
        let caps = caps_from_list_models(resp, &static_caps()).expect("must build");
        assert_eq!(caps.default_model_id(), "brand-new");
    }

    #[test]
    fn empty_response_returns_none() {
        let resp = ListModelsResponse {
            models: vec![],
            default_model_id: None,
        };
        assert!(caps_from_list_models(resp, &static_caps()).is_none());
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Process-wide in-memory keyring backend for tests.
    //!
    //! `keyring::mock` ships with the crate but each `Entry::new` builds a
    //! fresh credential with no shared state — so a test that calls
    //! `keyring::Entry::new(...).set_password(k)` can't be read back by the
    //! plugin's `creds::load`, which opens a separate `Entry`. On headless
    //! Linux CI the real `sync-secret-service` backend isn't reachable
    //! either (no DBus session), so the silent-reconnect path can never be
    //! exercised without this shim.
    //!
    //! [`SharedMockBuilder`] gives every entry it builds a view onto one
    //! `Mutex<HashMap<(service, user), secret>>`, so the test's set and the
    //! plugin's get hit the same bytes.
    use std::any::Any;
    use std::collections::HashMap;
    use std::sync::{Mutex, Once, OnceLock};

    use keyring::Error;
    use keyring::credential::{
        Credential, CredentialApi, CredentialBuilderApi, CredentialPersistence,
    };

    type Store = Mutex<HashMap<(String, String), Vec<u8>>>;

    fn store() -> &'static Store {
        static STORE: OnceLock<Store> = OnceLock::new();
        STORE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    #[derive(Debug)]
    struct SharedMockCredential {
        service: String,
        user: String,
    }

    impl CredentialApi for SharedMockCredential {
        fn set_secret(&self, secret: &[u8]) -> Result<(), Error> {
            store()
                .lock()
                .expect("shared mock store mutex poisoned")
                .insert((self.service.clone(), self.user.clone()), secret.to_vec());
            Ok(())
        }

        fn get_secret(&self) -> Result<Vec<u8>, Error> {
            store()
                .lock()
                .expect("shared mock store mutex poisoned")
                .get(&(self.service.clone(), self.user.clone()))
                .cloned()
                .ok_or(Error::NoEntry)
        }

        fn delete_credential(&self) -> Result<(), Error> {
            let removed = store()
                .lock()
                .expect("shared mock store mutex poisoned")
                .remove(&(self.service.clone(), self.user.clone()));
            if removed.is_some() {
                Ok(())
            } else {
                Err(Error::NoEntry)
            }
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[derive(Debug)]
    struct SharedMockBuilder;

    impl CredentialBuilderApi for SharedMockBuilder {
        fn build(
            &self,
            target: Option<&str>,
            service: &str,
            user: &str,
        ) -> Result<Box<Credential>, Error> {
            // The shim's `HashMap` key is `(service, user)`. If anyone
            // ever starts using `Entry::new_with_target` two distinct
            // targets would silently collapse into the same slot — fail
            // loudly so the shim is extended first.
            debug_assert!(
                target.is_none(),
                "SharedMockBuilder ignores `target`; got {target:?}. \
                 Extend the shim to key on (target, service, user) before \
                 introducing Entry::new_with_target in production code.",
            );
            Ok(Box::new(SharedMockCredential {
                service: service.to_owned(),
                user: user.to_owned(),
            }))
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn persistence(&self) -> CredentialPersistence {
            CredentialPersistence::ProcessOnly
        }
    }

    /// Idempotently install [`SharedMockBuilder`] as the keyring crate's
    /// process-wide default. Safe to call from every test that touches the
    /// keyring; only the first call swaps the builder.
    pub(crate) fn use_mock_keyring() {
        static MOCK: Once = Once::new();
        MOCK.call_once(|| {
            keyring::set_default_credential_builder(Box::new(SharedMockBuilder));
        });
    }

    #[cfg(test)]
    mod tests {
        use super::use_mock_keyring;
        use keyring::{Entry, Error};

        /// Pins the four behaviors the provider-plugin tests rely on but
        /// only exercise indirectly:
        ///
        /// - `get` on a missing key returns [`Error::NoEntry`].
        /// - `delete` on a missing key returns [`Error::NoEntry`] — this
        ///   DIVERGES from `keyring::mock`, which returns `Ok(())`. The
        ///   provider tests discard `delete_credential` results via
        ///   `let _ = ...`, so a regression here would otherwise pass
        ///   silently.
        /// - `set` then `get` round-trips bytes.
        /// - Distinct `(service, user)` tuples are isolated.
        #[test]
        #[serial_test::serial]
        fn shim_pins_keyring_contract() {
            use_mock_keyring();

            // Distinct service id so this test can't collide with the
            // provider-plugin tests' `("savvagent", PROVIDER_ID)` entries.
            let a = Entry::new("savvagent-shim-test", "user-a").expect("entry a");
            let b = Entry::new("savvagent-shim-test", "user-b").expect("entry b");

            // Clean slate even if a prior run of this test panicked
            // before its trailing cleanup.
            let _ = a.delete_credential();
            let _ = b.delete_credential();

            assert!(matches!(a.get_password(), Err(Error::NoEntry)));
            assert!(matches!(a.delete_credential(), Err(Error::NoEntry)));

            a.set_password("secret-a").expect("set a");
            assert_eq!(a.get_password().expect("get a"), "secret-a");

            assert!(matches!(b.get_password(), Err(Error::NoEntry)));
            b.set_password("secret-b").expect("set b");
            assert_eq!(a.get_password().expect("get a after b"), "secret-a");
            assert_eq!(b.get_password().expect("get b"), "secret-b");

            a.delete_credential().expect("delete a");
            assert!(matches!(a.get_password(), Err(Error::NoEntry)));

            let _ = b.delete_credential();
        }
    }
}
