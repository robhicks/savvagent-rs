//! Adapters that bridge wasm components to `Box<dyn savvagent_plugin::Plugin>`.
//!
//! Each adapter handles one WIT world:
//!
//! - [`static_::StaticAdapter`] — `plugin-static`, the simplest world:
//!   slash commands, hooks, themes, render slots. One long-lived store per
//!   adapter.
//!
//! Tasks 5 and 6 add `interactive::InteractiveAdapter` (per-screen-open
//! store, draw primitives) and `provider::ProviderAdapter` (one store per
//! turn, HTTP / keyring / streaming progress capabilities) respectively;
//! they're omitted here so this module compiles standalone in v0.18.0.

pub mod static_;

pub use static_::StaticAdapter;
