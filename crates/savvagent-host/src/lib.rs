//! Savvagent agent engine.
//!
//! `savvagent-host` is the runtime that the TUI links as a library. It owns
//! conversation state, drives the tool-use loop, and orchestrates connections
//! to one provider MCP server (over Streamable HTTP) and any number of tool
//! MCP servers (over stdio).
//!
//! Public surface:
//!
//! - [`HostConfig`] — declarative configuration: provider endpoint, tool
//!   endpoints, model, project root, system prompt overrides.
//! - [`Host`] — connect once via [`Host::start`], then call [`Host::run_turn`]
//!   for each user message. [`Host::shutdown`] cleans up child processes.
//! - [`TurnOutcome`] — final assistant response plus a per-turn trace of
//!   tool calls.
//! - [`HostError`] — top-level error type.

#![cfg_attr(not(test), forbid(unsafe_code))]
#![warn(missing_docs)]

mod config;
mod permissions;
mod project;
mod provider;
mod sandbox;
pub mod sensitive_paths;
mod session;
mod tools;

pub use config::{HostConfig, ProviderEndpoint, ToolEndpoint};
pub use permissions::{
    ArgPattern, FrontMatterPermissions, PermissionDecision, PermissionPolicy, PermissionsToml,
    Rule, SerializableRule, Verdict,
};
pub use sandbox::{SandboxConfig, SandboxWrapper, ToolSandboxOverride};
pub use savvagent_protocol::{ListModelsResponse, ModelInfo, ToolDef};
pub use session::{
    Host, HostError, TRANSCRIPT_SCHEMA_VERSION, ToolCall, ToolCallStatus, TranscriptError,
    TranscriptFile, TurnEvent, TurnOutcome,
};

#[doc(hidden)]
pub use provider::RmcpProviderClient;
