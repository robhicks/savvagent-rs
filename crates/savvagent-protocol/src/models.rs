//! `list_models` request and response types.
//!
//! Optional SPP tool. Hosts treat absence as "not advertised by this provider"
//! and fall through to optimistic model selection.

use serde::{Deserialize, Serialize};

/// Empty placeholder. Reserved for v0.2+ filters (e.g. by capability).
#[derive(Clone, Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ListModelsRequest {}

/// Result of `list_models`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ListModelsResponse {
    /// Available models, in display order.
    pub models: Vec<ModelInfo>,
}

/// One row in [`ListModelsResponse::models`].
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ModelInfo {
    /// Stable id passed back to `complete.model`.
    pub id: String,
    /// Optional human-readable name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Optional context-window size in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    /// True when this model is the provider's default.
    #[serde(default)]
    pub default: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_info_roundtrip_minimal() {
        let mi = ModelInfo {
            id: "x".into(),
            display_name: None,
            context_window: None,
            default: false,
        };
        let v = serde_json::to_value(&mi).unwrap();
        // Optional fields elided.
        assert_eq!(v.get("display_name"), None);
        assert_eq!(v.get("context_window"), None);
        // `default` is a bare bool serialized as `false`.
        assert_eq!(v.get("default"), Some(&serde_json::json!(false)));
        let back: ModelInfo = serde_json::from_value(v).unwrap();
        assert_eq!(back.id, "x");
        assert!(!back.default);
    }

    #[test]
    fn model_info_roundtrip_full() {
        let mi = ModelInfo {
            id: "claude-haiku-4-5".into(),
            display_name: Some("Claude Haiku 4.5".into()),
            context_window: Some(200_000),
            default: true,
        };
        let s = serde_json::to_string(&mi).unwrap();
        let back: ModelInfo = serde_json::from_str(&s).unwrap();
        assert_eq!(back.id, mi.id);
        assert_eq!(back.display_name, mi.display_name);
        assert_eq!(back.context_window, mi.context_window);
        assert!(back.default);
    }

    #[test]
    fn list_models_response_roundtrip() {
        let resp = ListModelsResponse {
            models: vec![
                ModelInfo {
                    id: "a".into(),
                    display_name: None,
                    context_window: None,
                    default: false,
                },
                ModelInfo {
                    id: "b".into(),
                    display_name: Some("B".into()),
                    context_window: Some(1024),
                    default: true,
                },
            ],
        };
        let s = serde_json::to_string(&resp).unwrap();
        let back: ListModelsResponse = serde_json::from_str(&s).unwrap();
        assert_eq!(back.models.len(), 2);
        assert_eq!(back.models[0].id, "a");
        assert!(back.models[1].default);
    }

    #[test]
    fn model_info_accepts_missing_default() {
        // Forward-compat: an older sender might omit `default` entirely.
        let v = serde_json::json!({"id": "x"});
        let mi: ModelInfo = serde_json::from_value(v).unwrap();
        assert_eq!(mi.id, "x");
        assert!(!mi.default);
        assert_eq!(mi.display_name, None);
    }

    #[test]
    fn list_models_request_default_is_empty_object() {
        let v = serde_json::to_value(ListModelsRequest::default()).unwrap();
        assert_eq!(v, serde_json::json!({}));
    }
}
