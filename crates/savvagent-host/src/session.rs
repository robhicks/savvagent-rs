//! Conversation state and the tool-use loop.

use std::path::Path;

use savvagent_mcp::ProviderClient;
use savvagent_protocol::{
    BlockDelta, CompleteRequest, ContentBlock, Message, ProviderError, Role, StopReason,
    StreamEvent,
};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::{Mutex, mpsc};

use crate::config::{HostConfig, ProviderEndpoint};
use crate::project;
use crate::provider::RmcpProviderClient;
use crate::tools::ToolRegistry;

/// Top-level error surfaced from [`Host`] operations.
#[derive(Debug, Error)]
pub enum HostError {
    /// Connecting to a provider or tool MCP server failed at startup.
    #[error("{0}")]
    Startup(#[from] anyhow::Error),
    /// The provider returned an SPP-level error.
    #[error("provider error: {kind:?}: {message}", kind = .0.kind, message = .0.message)]
    Provider(ProviderError),
    /// Loop ran past [`HostConfig::max_iterations`] without reaching `end_turn`.
    #[error("tool-use loop exceeded {0} iterations")]
    LoopLimit(u32),
    /// Tool routing produced a malformed `tool_use` block.
    #[error("malformed assistant response: {0}")]
    MalformedResponse(String),
}

/// Status of one tool call inside a [`TurnOutcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallStatus {
    /// The tool returned successfully.
    Ok,
    /// The tool returned `is_error: true` or a transport-level error.
    Errored,
}

/// Trace of one tool call performed during a turn.
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Tool name as chosen by the model.
    pub name: String,
    /// Arguments the model produced.
    pub arguments: Value,
    /// Outcome status.
    pub status: ToolCallStatus,
    /// String payload returned to the model in `tool_result`.
    pub result: String,
}

/// What [`Host::run_turn`] returns when the loop reaches `end_turn`.
#[derive(Debug, Clone)]
pub struct TurnOutcome {
    /// Concatenated assistant text from the final response.
    pub text: String,
    /// Tool calls executed during this turn, in order.
    pub tool_calls: Vec<ToolCall>,
    /// Number of provider round-trips this turn took (including the final one).
    pub iterations: u32,
}

/// Streaming event the host emits while running a turn. The TUI consumes
/// these to render incremental output.
#[derive(Debug, Clone)]
pub enum TurnEvent {
    /// One iteration of the loop began. `iteration` is 1-based.
    IterationStarted {
        /// 1-based iteration index.
        iteration: u32,
    },
    /// A token-delta arrived for the assistant's in-progress response.
    TextDelta {
        /// Text fragment to append to the live buffer.
        text: String,
    },
    /// The model decided to invoke a tool. Emitted *before* the tool runs.
    ToolCallStarted {
        /// Tool name as chosen by the model.
        name: String,
        /// Arguments the model produced.
        arguments: Value,
    },
    /// A tool call finished and its result was appended to history.
    ToolCallFinished {
        /// Tool name (echoed for matching with the `Started` event).
        name: String,
        /// Outcome status.
        status: ToolCallStatus,
        /// String payload that was returned to the model.
        result: String,
    },
    /// The whole turn finished.
    TurnComplete {
        /// Final outcome — same value `run_turn_streaming` returns.
        outcome: TurnOutcome,
    },
}

/// The agent host. Connects once, then handles turns. `Host` is `Send + Sync`
/// behind shared state so the TUI can hand it to background tasks.
pub struct Host {
    config: HostConfig,
    provider: Box<dyn ProviderClient + Send + Sync>,
    tools: Mutex<Option<ToolRegistry>>,
    state: Mutex<SessionState>,
    system_prompt: Option<String>,
}

struct SessionState {
    messages: Vec<Message>,
}

impl Host {
    /// Connect to the configured provider and tool servers, perform any MCP
    /// handshakes, and load the project context file.
    pub async fn start(config: HostConfig) -> Result<Self, HostError> {
        let provider: Box<dyn ProviderClient + Send + Sync> = match &config.provider {
            ProviderEndpoint::StreamableHttp { url } => {
                Box::new(RmcpProviderClient::connect(url).await?)
            }
        };
        let tools = ToolRegistry::connect(&config.tools, &config.project_root).await?;
        let system_prompt =
            project::system_prompt(&config.project_root, config.system_prompt.as_deref());
        Ok(Self {
            config,
            provider,
            tools: Mutex::new(Some(tools)),
            state: Mutex::new(SessionState {
                messages: Vec::new(),
            }),
            system_prompt,
        })
    }

    /// Construct a host directly from a (possibly mock) [`ProviderClient`] and
    /// a pre-connected tool registry. Used by tests and embedders that want to
    /// bypass the standard transport layer.
    #[doc(hidden)]
    pub async fn with_components(
        config: HostConfig,
        provider: Box<dyn ProviderClient + Send + Sync>,
    ) -> Result<Self, HostError> {
        let tools = ToolRegistry::connect(&config.tools, &config.project_root).await?;
        let system_prompt =
            project::system_prompt(&config.project_root, config.system_prompt.as_deref());
        Ok(Self {
            config,
            provider,
            tools: Mutex::new(Some(tools)),
            state: Mutex::new(SessionState {
                messages: Vec::new(),
            }),
            system_prompt,
        })
    }

    /// Send `user_input` as a user turn and run the tool-use loop until the
    /// model emits `end_turn` (or some other terminal stop reason). No
    /// streaming events are emitted; use [`Self::run_turn_streaming`] for the
    /// TUI's incremental-render path.
    pub async fn run_turn(&self, user_input: impl Into<String>) -> Result<TurnOutcome, HostError> {
        self.run_turn_inner(user_input.into(), None).await
    }

    /// Run a turn while emitting [`TurnEvent`]s onto `events`. Token-level
    /// `TextDelta`s are forwarded as the provider streams them. The final
    /// [`TurnOutcome`] is also returned for callers that want both.
    pub async fn run_turn_streaming(
        &self,
        user_input: impl Into<String>,
        events: mpsc::Sender<TurnEvent>,
    ) -> Result<TurnOutcome, HostError> {
        self.run_turn_inner(user_input.into(), Some(events)).await
    }

    async fn run_turn_inner(
        &self,
        user_input: String,
        events: Option<mpsc::Sender<TurnEvent>>,
    ) -> Result<TurnOutcome, HostError> {
        // Snapshot existing history and append the user message. We keep a
        // local working copy and only commit it back to `state` once the loop
        // succeeds — that way a failed turn doesn't corrupt the conversation.
        let mut messages = {
            let s = self.state.lock().await;
            s.messages.clone()
        };
        messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: user_input }],
        });

        let tool_defs = {
            let guard = self.tools.lock().await;
            guard.as_ref().map(|t| t.defs.clone()).unwrap_or_default()
        };

        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut iterations: u32 = 0;
        let want_stream = events.is_some();

        loop {
            if iterations >= self.config.max_iterations {
                return Err(HostError::LoopLimit(self.config.max_iterations));
            }
            iterations += 1;

            if let Some(tx) = &events {
                let _ = tx
                    .send(TurnEvent::IterationStarted {
                        iteration: iterations,
                    })
                    .await;
            }

            let req = CompleteRequest {
                model: self.config.model.clone(),
                messages: messages.clone(),
                system: self.system_prompt.clone(),
                tools: tool_defs.clone(),
                temperature: None,
                top_p: None,
                max_tokens: self.config.max_tokens,
                stop_sequences: Vec::new(),
                stream: want_stream,
                thinking: None,
                metadata: None,
            };

            // Wire token-delta forwarding only when the caller asked to
            // stream; otherwise skip the channel + task entirely.
            let (provider_tx, forwarder) = if let Some(events_tx) = events.clone() {
                let (tx, rx) = mpsc::channel::<StreamEvent>(64);
                let task = tokio::spawn(forward_text_deltas(rx, events_tx));
                (Some(tx), Some(task))
            } else {
                (None, None)
            };

            tracing::debug!(
                iteration = iterations,
                stream = want_stream,
                msg_count = messages.len(),
                "dispatching provider.complete"
            );
            let resp_result = self.provider.complete(req, provider_tx).await;
            // Drop the sender side (if any) so the forwarder drains and exits.
            if let Some(task) = forwarder {
                let _ = task.await;
            }
            let resp = resp_result.map_err(HostError::Provider)?;
            tracing::debug!(
                iteration = iterations,
                stop_reason = ?resp.stop_reason,
                blocks = resp.content.len(),
                "provider.complete returned"
            );

            // Append the assistant turn verbatim — the provider's content
            // blocks (including tool_use ids) round-trip back unchanged.
            messages.push(Message {
                role: Role::Assistant,
                content: resp.content.clone(),
            });

            let mut tool_uses: Vec<(String, String, Value)> = Vec::new();
            let mut text_buf = String::new();
            for block in &resp.content {
                match block {
                    ContentBlock::Text { text } => {
                        if !text_buf.is_empty() {
                            text_buf.push('\n');
                        }
                        text_buf.push_str(text);
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        tool_uses.push((id.clone(), name.clone(), input.clone()));
                    }
                    _ => {}
                }
            }

            // Loop terminates when there are no tool uses, or the model
            // explicitly ended the turn.
            if tool_uses.is_empty() || matches!(resp.stop_reason, StopReason::EndTurn) {
                if !tool_uses.is_empty() {
                    return Err(HostError::MalformedResponse(format!(
                        "stop_reason=end_turn but {} tool_use block(s) present",
                        tool_uses.len()
                    )));
                }
                let outcome = TurnOutcome {
                    text: text_buf,
                    tool_calls,
                    iterations,
                };
                {
                    let mut state = self.state.lock().await;
                    state.messages = messages;
                }
                if let Some(tx) = events {
                    let _ = tx
                        .send(TurnEvent::TurnComplete {
                            outcome: outcome.clone(),
                        })
                        .await;
                }
                return Ok(outcome);
            }

            // Execute every requested tool call and append a single user
            // turn of tool_result blocks (Anthropic's expected shape).
            let mut tool_results: Vec<ContentBlock> = Vec::with_capacity(tool_uses.len());
            for (id, name, input) in tool_uses {
                if let Some(tx) = &events {
                    let _ = tx
                        .send(TurnEvent::ToolCallStarted {
                            name: name.clone(),
                            arguments: input.clone(),
                        })
                        .await;
                }
                let outcome = {
                    let guard = self.tools.lock().await;
                    let registry = guard.as_ref().expect("tools registry present");
                    registry.call(&name, input.clone()).await
                };
                let status = if outcome.is_error {
                    ToolCallStatus::Errored
                } else {
                    ToolCallStatus::Ok
                };
                if let Some(tx) = &events {
                    let _ = tx
                        .send(TurnEvent::ToolCallFinished {
                            name: name.clone(),
                            status,
                            result: outcome.payload.clone(),
                        })
                        .await;
                }
                tool_results.push(ContentBlock::ToolResult {
                    tool_use_id: id,
                    content: vec![ContentBlock::Text {
                        text: outcome.payload.clone(),
                    }],
                    is_error: outcome.is_error,
                });
                tool_calls.push(ToolCall {
                    name,
                    arguments: input,
                    status,
                    result: outcome.payload,
                });
            }
            messages.push(Message {
                role: Role::User,
                content: tool_results,
            });
        }
    }

    /// Read-only snapshot of the conversation history.
    pub async fn messages(&self) -> Vec<Message> {
        self.state.lock().await.messages.clone()
    }

    /// Persist the current message history as pretty-printed JSON to `path`.
    /// Creates parent directories as needed. Schema is the SPP `Vec<Message>`
    /// produced by [`Self::messages`] — re-loadable as-is.
    pub async fn save_transcript(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let messages = self.messages().await;
        let json = serde_json::to_vec_pretty(&messages)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        tokio::fs::write(path, json).await
    }

    /// Drop the per-turn message history without touching connections.
    pub async fn clear_history(&self) {
        self.state.lock().await.messages.clear();
    }

    /// Borrow the active config.
    pub fn config(&self) -> &HostConfig {
        &self.config
    }

    /// Cleanly shut down tool-server children. The provider session is
    /// dropped along with `self`. Idempotent — calling twice is a no-op for
    /// the second call.
    pub async fn shutdown(&self) {
        let registry = {
            let mut guard = self.tools.lock().await;
            guard.take()
        };
        if let Some(r) = registry {
            r.shutdown().await;
        }
    }
}

// We hold a `Box<dyn ProviderClient + Send + Sync>`. The trait object is
// Send + Sync; the rest of `Host` is too. Help the compiler verify it.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Host>();
};

/// Convert a stream of provider [`StreamEvent`]s into [`TurnEvent::TextDelta`]s
/// and forward them to the host caller. Non-text events are dropped (they're
/// re-derivable from the final response, which the loop already has).
async fn forward_text_deltas(mut rx: mpsc::Receiver<StreamEvent>, out: mpsc::Sender<TurnEvent>) {
    while let Some(ev) = rx.recv().await {
        if let StreamEvent::ContentBlockDelta {
            delta: BlockDelta::TextDelta { text },
            ..
        } = ev
        {
            if out.send(TurnEvent::TextDelta { text }).await.is_err() {
                break;
            }
        }
    }
}
