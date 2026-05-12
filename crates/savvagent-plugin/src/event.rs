//! HostEvent payloads + HookKind discriminants.

use crate::types::ProviderId;

/// Discriminant-only mirror of [`HostEvent`] used as a hook-registry key.
///
/// Each variant corresponds to exactly one [`HostEvent`] variant. Plugins
/// register interest by providing a set of `HookKind`s; the runtime uses
/// these to route fired events without cloning the full payload.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HookKind {
    /// Emitted once at startup before any other hook.
    HostStarting,
    /// Emitted when a provider connection is established.
    Connect,
    /// Emitted when a provider connection is torn down.
    Disconnect,
    /// Emitted at the beginning of each agent turn.
    TurnStart,
    /// Emitted at the end of each agent turn, carrying success/failure status.
    TurnEnd,
    /// Emitted just before the host dispatches a tool call.
    ToolCallStart,
    /// Emitted after a tool call completes, carrying success/failure status.
    ToolCallEnd,
    /// Emitted when the user submits a prompt to the agent.
    PromptSubmitted,
    /// Emitted after a conversation transcript has been persisted to disk.
    TranscriptSaved,
}

/// Typed host-lifecycle events that the runtime fires into the plugin bus.
///
/// Each variant carries the minimal payload needed for plugins to react to
/// that lifecycle moment. Use [`HostEvent::kind`] to obtain a [`HookKind`]
/// suitable for hash-map dispatch without consuming the event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostEvent {
    /// The host is starting; fired once before any provider connection.
    HostStarting,
    /// A provider was successfully connected.
    Connect {
        /// Identifier of the provider that connected.
        provider_id: ProviderId,
    },
    /// A provider connection was closed or lost.
    Disconnect {
        /// Identifier of the provider that disconnected.
        provider_id: ProviderId,
        /// Human-readable explanation of why the connection ended.
        reason: String,
    },
    /// An agent turn has begun.
    TurnStart {
        /// Monotonic per-session turn counter, starting at 1.
        turn_id: u32,
    },
    /// An agent turn has completed.
    TurnEnd {
        /// Monotonic per-session turn counter matching the corresponding [`HostEvent::TurnStart`].
        turn_id: u32,
        /// `true` if the turn completed without error; `false` otherwise.
        success: bool,
    },
    /// A tool call has been dispatched to the tool registry.
    ToolCallStart {
        /// Opaque identifier that correlates this event with its [`HostEvent::ToolCallEnd`].
        call_id: String,
        /// Name of the tool being invoked.
        tool: String,
    },
    /// A tool call has returned.
    ToolCallEnd {
        /// Opaque identifier matching the corresponding [`HostEvent::ToolCallStart`].
        call_id: String,
        /// `true` if the tool returned without error; `false` otherwise.
        success: bool,
    },
    /// The user submitted a prompt to the agent.
    PromptSubmitted {
        /// The full text of the submitted prompt.
        text: String,
    },
    /// A conversation transcript was written to disk.
    TranscriptSaved {
        /// Absolute path to the saved transcript file.
        path: String,
    },
}

impl HostEvent {
    /// Returns the [`HookKind`] discriminant for this event.
    ///
    /// Useful for index lookups in the plugin hook registry without
    /// requiring ownership of the full payload.
    pub fn kind(&self) -> HookKind {
        match self {
            HostEvent::HostStarting => HookKind::HostStarting,
            HostEvent::Connect { .. } => HookKind::Connect,
            HostEvent::Disconnect { .. } => HookKind::Disconnect,
            HostEvent::TurnStart { .. } => HookKind::TurnStart,
            HostEvent::TurnEnd { .. } => HookKind::TurnEnd,
            HostEvent::ToolCallStart { .. } => HookKind::ToolCallStart,
            HostEvent::ToolCallEnd { .. } => HookKind::ToolCallEnd,
            HostEvent::PromptSubmitted { .. } => HookKind::PromptSubmitted,
            HostEvent::TranscriptSaved { .. } => HookKind::TranscriptSaved,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ProviderId;

    #[test]
    fn kind_matches_variant() {
        let e = HostEvent::Connect { provider_id: ProviderId("anthropic".into()) };
        assert_eq!(e.kind(), HookKind::Connect);

        let e = HostEvent::TurnStart { turn_id: 7 };
        assert_eq!(e.kind(), HookKind::TurnStart);
    }

    #[test]
    fn host_starting_carries_no_payload() {
        let e = HostEvent::HostStarting;
        assert_eq!(e.kind(), HookKind::HostStarting);
    }
}
