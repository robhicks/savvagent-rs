//! Per-provider and per-model capability metadata. Carried into the host
//! via `HostConfig::providers` (see config.rs::ProviderRegistration).
//! Plugins build these from their hardcoded model lists; the host treats
//! them as read-only data and never mutates capability records itself.

use savvagent_protocol::ProviderId;

/// Coarse cost category for a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CostTier {
    /// No usage charge (e.g. local models).
    Free,
    /// Below-average cost per token.
    Cheap,
    /// Typical commercial model pricing.
    Standard,
    /// High-capability frontier model pricing.
    Premium,
}

/// Per-model capability metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCapabilities {
    /// Bare model id (e.g. `"claude-opus-4-7"`).
    pub id: String,
    /// Human-readable name for display in the UI.
    pub display_name: String,
    /// Whether the model accepts image inputs.
    pub supports_vision: bool,
    /// Whether the model accepts audio inputs.
    pub supports_audio: bool,
    /// Maximum input context window in tokens.
    pub context_window: usize,
    /// Rough cost category for the model.
    pub cost_tier: CostTier,
}

/// A short alias that maps a bare name to a specific provider + model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelAlias {
    /// Short name (e.g. `"opus"`).
    pub alias: String,
    /// Provider that owns this model.
    pub provider: ProviderId,
    /// Fully-qualified model id on that provider.
    pub model: String,
}

/// All capability metadata for one provider.
#[derive(Debug, Clone)]
pub struct ProviderCapabilities {
    /// Ordered list of models offered by the provider.
    pub models: Vec<ModelCapabilities>,
    /// Id of the model to use when none is specified.
    pub default_model: String,
}

impl ProviderCapabilities {
    /// Look up a model by id, returning `None` if not found.
    pub fn model(&self, id: &str) -> Option<&ModelCapabilities> {
        self.models.iter().find(|m| m.id == id)
    }

    /// Return the default model's capabilities.
    ///
    /// # Panics
    ///
    /// Panics if `default_model` is not present in `models`. Callers are
    /// responsible for constructing consistent `ProviderCapabilities` values.
    pub fn default(&self) -> &ModelCapabilities {
        self.model(&self.default_model)
            .expect("default_model must exist in models")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_protocol::ProviderId;

    fn anthropic() -> ProviderId {
        ProviderId::new("anthropic").unwrap()
    }

    fn opus_caps() -> ModelCapabilities {
        ModelCapabilities {
            id: "claude-opus-4-7".into(),
            display_name: "Claude Opus 4.7".into(),
            supports_vision: true,
            supports_audio: false,
            context_window: 200_000,
            cost_tier: CostTier::Premium,
        }
    }

    #[test]
    fn provider_caps_lookup_by_id() {
        let caps = ProviderCapabilities {
            models: vec![opus_caps()],
            default_model: "claude-opus-4-7".into(),
        };
        assert_eq!(caps.model("claude-opus-4-7").unwrap().id, "claude-opus-4-7");
        assert!(caps.model("not-a-model").is_none());
        assert_eq!(caps.default().id, "claude-opus-4-7");
    }

    #[test]
    fn model_alias_struct_is_constructible() {
        let alias = ModelAlias {
            alias: "opus".into(),
            provider: anthropic(),
            model: "claude-opus-4-7".into(),
        };
        assert_eq!(alias.alias, "opus");
    }

    #[test]
    #[should_panic(expected = "default_model must exist in models")]
    fn default_panics_when_unset() {
        let caps = ProviderCapabilities {
            models: vec![],
            default_model: "missing".into(),
        };
        let _ = caps.default();
    }
}
