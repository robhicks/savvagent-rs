//! Adapters that bridge wasm components to `Box<dyn savvagent_plugin::Plugin>`.
//!
//! Each adapter handles one WIT world:
//!
//! - [`static_::StaticAdapter`] — `plugin-static`, the simplest world:
//!   slash commands, hooks, themes, render slots. One long-lived store per
//!   adapter.
//!
//! - [`interactive::InteractiveAdapter`] — `plugin-interactive`,
//!   per-screen-open Store + a `screen-instance` resource that owns
//!   instance-local state. The trait surface for `Screen::render`/`tips`
//!   is sync-returns-Vec<StyledLine>; the adapter caches the most recent
//!   wasm render output and re-issues the wasm call after every key/event.
//!
//! Task 6 adds `provider::ProviderAdapter` (one store per turn, HTTP /
//! keyring / streaming progress capabilities).

pub mod interactive;
pub mod static_;

pub use interactive::InteractiveAdapter;
pub use static_::StaticAdapter;
