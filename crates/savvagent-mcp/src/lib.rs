//! Shared traits and helpers for SPP-over-MCP integration.
//!
//! This crate sits between [`savvagent_protocol`] (pure types) and the actual
//! MCP transport plumbing. It defines the host-facing [`ProviderClient`]
//! trait and the server-facing [`ProviderHandler`] trait so providers can be
//! implemented against a stable abstraction. Concrete `rmcp`-based
//! implementations (Streamable HTTP client, stdio server, etc.) layer on top
//! and live in `savvagent-host` and per-provider crates.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use async_trait::async_trait;
use savvagent_protocol::{CompleteRequest, CompleteResponse, ProviderError, StreamEvent};
use tokio::sync::mpsc;

/// Re-export of the MCP progress-notification discriminator.
pub use savvagent_protocol::STREAM_EVENT_KIND;

/// Host-side view of a provider MCP server.
///
/// Implementations of this trait drive an MCP `tools/call` for the
/// [`COMPLETE_TOOL_NAME`](savvagent_protocol::COMPLETE_TOOL_NAME) tool over
/// whatever transport they were constructed with (typically Streamable HTTP).
///
/// When the caller wants streaming, it passes `events = Some(sender)` and the
/// implementation forwards [`StreamEvent`]s as they arrive on the wire,
/// returning the final [`CompleteResponse`] when the stream ends.
#[async_trait]
pub trait ProviderClient: Send + Sync {
    /// Run a completion against the underlying provider server.
    async fn complete(
        &self,
        req: CompleteRequest,
        events: Option<mpsc::Sender<StreamEvent>>,
    ) -> Result<CompleteResponse, ProviderError>;
}

/// Server-side view of a provider implementation.
///
/// A concrete provider crate (e.g. `provider-anthropic`) implements this
/// trait, and the per-provider binary wraps the impl with an `rmcp`
/// Streamable HTTP server. Keeping the trait transport-agnostic means the
/// same impl can be unit-tested in-process and exercised over MCP without
/// modification.
#[async_trait]
pub trait ProviderHandler: Send + Sync {
    /// Handle a `complete` call. Implementations push streaming events into
    /// `emit` (when present) before returning the final response.
    async fn complete(
        &self,
        req: CompleteRequest,
        emit: Option<&dyn StreamEmitter>,
    ) -> Result<CompleteResponse, ProviderError>;
}

/// Sink the server-side handler uses to publish stream events. The concrete
/// MCP wrapper turns each call into an MCP `notifications/progress`.
#[async_trait]
pub trait StreamEmitter: Send + Sync {
    /// Send one event to the host. Errors here mean the host has gone away
    /// and the handler should abandon the call.
    async fn emit(&self, event: StreamEvent) -> Result<(), EmitError>;
}

/// Failure modes for [`StreamEmitter::emit`].
#[derive(Debug, thiserror::Error)]
pub enum EmitError {
    /// The host disconnected.
    #[error("host disconnected")]
    Disconnected,
    /// Transport-level failure.
    #[error("transport error: {0}")]
    Transport(String),
}

/// Convenience [`StreamEmitter`] backed by a tokio mpsc channel. Useful for
/// in-process tests and as the default plumbing inside the savvagent host.
pub struct ChannelEmitter {
    tx: mpsc::Sender<StreamEvent>,
}

impl ChannelEmitter {
    /// Wrap an mpsc sender as an emitter.
    pub fn new(tx: mpsc::Sender<StreamEvent>) -> Self {
        Self { tx }
    }
}

#[async_trait]
impl StreamEmitter for ChannelEmitter {
    async fn emit(&self, event: StreamEvent) -> Result<(), EmitError> {
        self.tx.send(event).await.map_err(|_| EmitError::Disconnected)
    }
}

/// [`ProviderClient`] adapter that calls a [`ProviderHandler`] directly,
/// without going through MCP. The host gets the same trait object it would
/// over the wire, but every `complete` call is just a function call and a
/// channel forward — no transport, no spawned binary.
pub struct InProcessProviderClient {
    handler: std::sync::Arc<dyn ProviderHandler>,
}

impl InProcessProviderClient {
    /// Wrap an existing handler.
    pub fn new(handler: std::sync::Arc<dyn ProviderHandler>) -> Self {
        Self { handler }
    }
}

#[async_trait]
impl ProviderClient for InProcessProviderClient {
    async fn complete(
        &self,
        req: CompleteRequest,
        events: Option<mpsc::Sender<StreamEvent>>,
    ) -> Result<CompleteResponse, ProviderError> {
        let emitter = events.map(ChannelEmitter::new);
        let emit_ref: Option<&dyn StreamEmitter> =
            emitter.as_ref().map(|e| e as &dyn StreamEmitter);
        self.handler.complete(req, emit_ref).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_protocol::{ContentBlock, Role, StopReason, Usage};

    struct EchoHandler;

    #[async_trait]
    impl ProviderHandler for EchoHandler {
        async fn complete(
            &self,
            req: CompleteRequest,
            emit: Option<&dyn StreamEmitter>,
        ) -> Result<CompleteResponse, ProviderError> {
            let last = req
                .messages
                .last()
                .and_then(|m| m.content.first())
                .and_then(|b| match b {
                    ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .unwrap_or_default();

            if let Some(emit) = emit {
                emit.emit(StreamEvent::MessageStop).await.unwrap();
            }

            Ok(CompleteResponse {
                id: "test".into(),
                model: req.model,
                content: vec![ContentBlock::Text { text: last }],
                stop_reason: StopReason::EndTurn,
                stop_sequence: None,
                usage: Usage::default(),
            })
        }
    }

    #[tokio::test]
    async fn handler_with_channel_emitter() {
        let (tx, mut rx) = mpsc::channel(8);
        let emitter = ChannelEmitter::new(tx);
        let handler = EchoHandler;
        let req = CompleteRequest::text("test", "hi", 16);

        let req_role = req.messages[0].role;
        assert_eq!(req_role, Role::User);

        let resp = handler.complete(req, Some(&emitter)).await.unwrap();
        assert_eq!(resp.stop_reason, StopReason::EndTurn);
        let evt = rx.recv().await.unwrap();
        assert!(matches!(evt, StreamEvent::MessageStop));
    }
}
