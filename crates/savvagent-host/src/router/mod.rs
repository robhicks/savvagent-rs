//! Routing layers. Phase 1 ships only `legacy_model`; subsequent
//! phases (override prefix, modality, rules, heuristics) add siblings.

pub mod legacy_model;

pub use legacy_model::{LegacyModelResolution, ProviderView, resolve_legacy_model};
