//! `SubHost` — a subagent execution context. Owns its own session
//! state, system prompt, model selection, and tool filter; shares the
//! parent's `ProviderClient`, `ToolRegistry`, `PreToolUseGate`,
//! permissions, and sandbox config via `Arc`.
//!
//! See `docs/superpowers/specs/2026-05-23-user-agents-design.md` §2.

use std::collections::HashSet;
use std::sync::Arc;

use savvagent_protocol::ToolDef;
use tokio_util::sync::CancellationToken;

use crate::Host;
use crate::scoped_registry::ScopedToolRegistry;
use crate::tools::SubagentContext;

/// Sub-Host configuration. Built by `TaskToolHandler` from an
/// `AgentSpec` and a parent `ToolCallContext`.
///
/// Fields are `pub(crate)` because the subagent loop and helpers live
/// inside `savvagent-host`; external constructors go through
/// [`SubHost::new`].
#[allow(dead_code)] // Fields consumed in Task 7+.
pub struct SubHost {
    pub(crate) parent: Arc<Host>,
    pub(crate) ctx: SubagentContext,
    pub(crate) system_prompt: String,
    pub(crate) model: Option<String>,
    pub(crate) tools: ScopedToolRegistry,
    pub(crate) tool_defs: Vec<ToolDef>,
    pub(crate) cancellation: CancellationToken,
}

impl SubHost {
    /// Build a `SubHost` over `parent`'s shared resources.
    ///
    /// `allowed_names` is the per-subagent tool allowlist, applied at
    /// dispatch time by [`ScopedToolRegistry`]. `tool_defs` is the
    /// pre-filtered slice of `ToolDef`s exposed to the model — callers
    /// (typically `TaskToolHandler`) own the filtering policy.
    ///
    /// Async because pulling the parent's `Arc<ToolRegistry>` out of
    /// its `Mutex<Option<_>>` requires the async lock. If the parent
    /// has already been shut down this returns `None`.
    #[allow(dead_code)] // Constructed by TaskToolHandler in Task 20.
    pub async fn new(
        parent: Arc<Host>,
        ctx: SubagentContext,
        system_prompt: String,
        model: Option<String>,
        allowed_names: HashSet<String>,
        tool_defs: Vec<ToolDef>,
        cancellation: CancellationToken,
    ) -> Option<Self> {
        let registry = parent.tool_registry_arc().await?;
        let tools = ScopedToolRegistry::new(registry, allowed_names);
        Some(Self {
            parent,
            ctx,
            system_prompt,
            model,
            tools,
            tool_defs,
            cancellation,
        })
    }

    /// Drive the subagent loop to its `end_turn`. Returns the final
    /// assistant text or an error.
    #[allow(dead_code)] // Wired up in Task 7.
    pub async fn run_subagent(&self, prompt: String) -> Result<String, SubHostError> {
        let _ = prompt;
        Err(SubHostError::Unimplemented)
    }
}

/// Errors produced by [`SubHost::run_subagent`].
#[derive(Debug, thiserror::Error)]
pub enum SubHostError {
    /// Placeholder for Task 6 — the loop body lands in Task 7.
    #[error("subagent loop not yet implemented")]
    Unimplemented,
    /// The subagent's `CancellationToken` was tripped.
    #[error("subagent cancelled")]
    Cancelled,
    /// `SAVVAGENT_AGENT_MAX_DEPTH` would be exceeded by this dispatch.
    #[error("subagent depth limit exceeded")]
    DepthExceeded,
    /// The subagent reached `end_turn` without producing assistant text.
    #[error("subagent produced no output")]
    EmptyOutput,
    /// The provider client returned an error.
    #[error("provider error: {0}")]
    Provider(String),
    /// A tool dispatch (or its allowlist gate) returned an error.
    #[error("tool error: {0}")]
    Tool(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sub_host_error_variants_compile() {
        // Smoke test: each variant constructs.
        let _ = SubHostError::Unimplemented;
        let _ = SubHostError::Cancelled;
        let _ = SubHostError::DepthExceeded;
        let _ = SubHostError::EmptyOutput;
        let _ = SubHostError::Provider("p".into());
        let _ = SubHostError::Tool("t".into());
    }
}
