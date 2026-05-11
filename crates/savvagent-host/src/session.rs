//! Conversation state and the tool-use loop.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use savvagent_mcp::ProviderClient;
use savvagent_protocol::{
    BlockDelta, CompleteRequest, ContentBlock, Message, ProviderError, Role, StopReason,
    StreamEvent, ToolDef,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::config::{HostConfig, ProviderEndpoint};
use crate::permissions::{
    BashNetworkChoice, BashNetworkPolicy, PermissionDecision, PermissionPolicy, Verdict,
};
use crate::project;
use crate::provider::RmcpProviderClient;
use crate::sandbox::SandboxConfig;
use crate::tools::{BashNetResolver, ToolRegistry};

/// Current transcript file schema version.
///
/// Increment when the on-disk shape changes incompatibly. The loader rejects
/// files whose `schema_version` doesn't match this constant with
/// [`TranscriptError::SchemaMismatch`], which lets callers surface a clear
/// error rather than silently misinterpreting old data.
///
/// **Pre-resume files** (written before this version field was introduced)
/// lack the `schema_version` field entirely. They are accepted as v1
/// transcripts — the only field they carry is the raw `Vec<Message>` array,
/// which is identical in shape to `TranscriptFile::messages` in v1.
pub const TRANSCRIPT_SCHEMA_VERSION: u32 = 1;

/// On-disk transcript format.
///
/// The file is pretty-printed JSON. Older files written by
/// `Host::save_transcript` before session-resume was added lack the wrapper
/// object; they are a bare `[...]` array of [`Message`]s. The loader handles
/// both shapes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptFile {
    /// Schema version; used to detect incompatible on-disk formats.
    pub schema_version: u32,
    /// Recorded model identifier. May differ from the active connection.
    pub model: String,
    /// Unix timestamp (seconds) of when the transcript was saved.
    pub saved_at: u64,
    /// Conversation messages in chronological order.
    pub messages: Vec<Message>,
}

/// Errors produced by transcript load / save operations.
#[derive(Debug, Error)]
pub enum TranscriptError {
    /// The file could not be read.
    #[error("io error reading transcript: {0}")]
    Io(#[from] std::io::Error),
    /// JSON was malformed or didn't match the expected shape.
    #[error("malformed transcript JSON: {0}")]
    Malformed(String),
    /// The on-disk schema version differs from the version this binary expects.
    #[error("transcript schema v{found}, expected v{expected}")]
    SchemaMismatch {
        /// Version found in the file.
        found: u32,
        /// Version this build expects.
        expected: u32,
    },
}

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
    /// Policy returned [`Verdict::Ask`] for a tool call. The turn is paused
    /// until the embedder calls [`Host::resolve_permission`] with the matching
    /// `id`. Emitted before any `ToolCallStarted` for this call.
    PermissionRequested {
        /// Opaque request id; pass back to [`Host::resolve_permission`].
        id: u64,
        /// Tool the model wants to invoke.
        name: String,
        /// Short, human-readable summary for the modal.
        summary: String,
        /// Full argument JSON, in case the UI wants to render it expanded.
        args: Value,
    },
    /// Tool-bash is about to be spawned and the configured
    /// [`BashNetworkPolicy`] is `Ask` with no cached decision for this
    /// session. The host pauses the spawn until the embedder calls
    /// [`Host::resolve_bash_network_decision`] with `id`.
    BashNetworkRequested {
        /// Opaque request id; pass back to
        /// [`Host::resolve_bash_network_decision`].
        id: u64,
        /// Human-readable summary of the bash invocation, suitable for
        /// display in a modal (e.g., the first ~80 chars of the command).
        summary: String,
    },
    /// A tool call was refused (by policy or by the user). No `ToolCallStarted`
    /// or `ToolCallFinished` is emitted for this call — only this event plus a
    /// synthetic error `tool_result` appended to the conversation.
    ToolCallDenied {
        /// Tool name.
        name: String,
        /// Reason that's also embedded in the synthetic `tool_result`.
        reason: String,
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
    /// Layered permission policy: sensitive-path floor + SAVVAGENT.md
    /// front-matter rules + persisted Always/Never rules from
    /// `~/.savvagent/permissions.toml` + built-in defaults. See the
    /// `permissions` module docs.
    policy: PermissionPolicy,
    /// Active Layer-3 sandbox configuration. Resolved from
    /// `HostConfig::sandbox` at startup (or loaded from disk when unset).
    sandbox: SandboxConfig,
    /// Outstanding permission requests. Inserted when the loop emits
    /// `PermissionRequested`; the matching `oneshot` is consumed by
    /// [`Host::resolve_permission`].
    pending: Mutex<HashMap<u64, oneshot::Sender<PermissionDecision>>>,
    /// Outstanding `BashNetworkRequested` prompts, keyed by event id. The
    /// matching `oneshot` is consumed by
    /// [`Host::resolve_bash_network_decision`]. `Arc`-shared so the
    /// lazy bash-net resolver closure can hold a reference too.
    pending_bash_network: Arc<Mutex<HashMap<u64, oneshot::Sender<BashNetworkChoice>>>>,
    /// Current turn's event channel, exposed so the lazy `tool-bash`
    /// spawn resolver (which is invoked from inside `run_turn_inner` via
    /// `ToolRegistry::call_with_bash_net_override`) can emit
    /// [`TurnEvent::BashNetworkRequested`] without having to take the
    /// channel through every call layer. Set at the start of each
    /// streaming turn and cleared at the end; the non-streaming code
    /// path leaves it `None`. Held behind `Arc<std::sync::Mutex<_>>`
    /// so the resolver closure can clone its handle.
    current_turn_events: Arc<std::sync::Mutex<Option<mpsc::Sender<TurnEvent>>>>,
    /// Monotonic source for permission-request ids. `Arc`-shared so the
    /// resolver closure can mint ids.
    next_request_id: Arc<AtomicU64>,
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
        let sandbox = config.sandbox.clone().unwrap_or_else(SandboxConfig::load);
        // The real resolver — which calls back into the host's
        // permission state — captures `&self` and so can
        // only be installed once we own the `Host`. The bootstrap resolver
        // here is a safe default that returns the per-call override (if
        // any) or falls back to `false`. `wire_self_into_resolver` swaps
        // in the real one below.
        let resolver = bootstrap_bash_net_resolver();
        let tools =
            ToolRegistry::connect(&config.tools, &config.project_root, &sandbox, resolver).await?;
        let system_prompt =
            project::system_prompt(&config.project_root, config.system_prompt.as_deref());
        let policy = config
            .policy
            .clone()
            .unwrap_or_else(|| PermissionPolicy::default_for(&config.project_root));
        let host = Self {
            config,
            provider,
            tools: Mutex::new(Some(tools)),
            state: Mutex::new(SessionState {
                messages: Vec::new(),
            }),
            system_prompt,
            policy,
            sandbox,
            pending: Mutex::new(HashMap::new()),
            pending_bash_network: Arc::new(Mutex::new(HashMap::new())),
            current_turn_events: Arc::new(std::sync::Mutex::new(None)),
            next_request_id: Arc::new(AtomicU64::new(1)),
        };
        host.wire_self_into_resolver().await;
        Ok(host)
    }

    /// Construct a host directly from a (possibly mock) [`ProviderClient`] and
    /// a pre-connected tool registry. Used by tests and embedders that want to
    /// bypass the standard transport layer.
    #[doc(hidden)]
    pub async fn with_components(
        config: HostConfig,
        provider: Box<dyn ProviderClient + Send + Sync>,
    ) -> Result<Self, HostError> {
        let sandbox = config.sandbox.clone().unwrap_or_else(SandboxConfig::load);
        let resolver = bootstrap_bash_net_resolver();
        let tools =
            ToolRegistry::connect(&config.tools, &config.project_root, &sandbox, resolver).await?;
        let system_prompt =
            project::system_prompt(&config.project_root, config.system_prompt.as_deref());
        let policy = config
            .policy
            .clone()
            .unwrap_or_else(|| PermissionPolicy::default_for(&config.project_root));
        let host = Self {
            config,
            provider,
            tools: Mutex::new(Some(tools)),
            state: Mutex::new(SessionState {
                messages: Vec::new(),
            }),
            system_prompt,
            policy,
            sandbox,
            pending: Mutex::new(HashMap::new()),
            pending_bash_network: Arc::new(Mutex::new(HashMap::new())),
            current_turn_events: Arc::new(std::sync::Mutex::new(None)),
            next_request_id: Arc::new(AtomicU64::new(1)),
        };
        host.wire_self_into_resolver().await;
        Ok(host)
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

    /// Ask the active provider for its model list.
    ///
    /// Returns the provider's default error when `list_models` is not
    /// advertised; callers should treat that case as "fall through to
    /// optimistic `/model` selection".
    pub async fn list_models(
        &self,
    ) -> Result<savvagent_protocol::ListModelsResponse, savvagent_protocol::ProviderError> {
        self.provider.list_models().await
    }

    async fn run_turn_inner(
        &self,
        user_input: String,
        events: Option<mpsc::Sender<TurnEvent>>,
    ) -> Result<TurnOutcome, HostError> {
        // Publish the events channel (if any) for the lazy bash-net
        // resolver. Clear it via a guard on exit so an early-return path
        // doesn't leak a stale Sender that outlives the turn.
        let _events_guard = CurrentTurnEventsGuard::install(&self.current_turn_events, &events);
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
            for (tool_use_id, name, input) in tool_uses {
                let gate = self.gate_tool_call(&name, &input, events.as_ref()).await;
                match gate {
                    Ok(()) => {
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
                            registry
                                .call_with_bash_net_override(&name, input.clone(), None)
                                .await
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
                            tool_use_id,
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
                    Err(reason) => {
                        if let Some(tx) = &events {
                            let _ = tx
                                .send(TurnEvent::ToolCallDenied {
                                    name: name.clone(),
                                    reason: reason.clone(),
                                })
                                .await;
                        }
                        let payload = format!("denied by policy: {reason}");
                        tool_results.push(ContentBlock::ToolResult {
                            tool_use_id,
                            content: vec![ContentBlock::Text {
                                text: payload.clone(),
                            }],
                            is_error: true,
                        });
                        tool_calls.push(ToolCall {
                            name,
                            arguments: input,
                            status: ToolCallStatus::Errored,
                            result: payload,
                        });
                    }
                }
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
    ///
    /// Creates parent directories as needed. The file is written in the
    /// [`TranscriptFile`] versioned format so [`Self::load_transcript`] can
    /// round-trip it unambiguously.
    pub async fn save_transcript(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let messages = self.messages().await;
        let saved_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let record = TranscriptFile {
            schema_version: TRANSCRIPT_SCHEMA_VERSION,
            model: self.config.model.clone(),
            saved_at,
            messages,
        };
        let json = serde_json::to_vec_pretty(&record)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        tokio::fs::write(path, json).await
    }

    /// Re-hydrate the message history from a previously-saved transcript file.
    ///
    /// Accepts two on-disk shapes:
    ///
    /// - **Versioned** (`TranscriptFile` object with a `schema_version` field):
    ///   the schema version must equal [`TRANSCRIPT_SCHEMA_VERSION`]; a mismatch
    ///   returns [`TranscriptError::SchemaMismatch`].
    /// - **Legacy bare array** (`[...]`): files written before session-resume
    ///   was introduced. They are interpreted as v1 transcripts; the messages
    ///   are loaded as-is.
    ///
    /// Existing conversation history is **replaced** by the loaded messages.
    /// The caller is expected to have verified that a provider is connected
    /// before calling this; the host does not re-connect automatically.
    ///
    /// Returns the loaded [`TranscriptFile`] so callers can surface metadata
    /// (model, saved_at) in the UI.
    ///
    /// # Invariant: must not be called during an in-flight turn
    ///
    /// [`Self::run_turn_inner`] snapshots `state.messages` into a local `Vec`
    /// at turn start and commits that local clone back to `state.messages`
    /// when the turn completes. Calling `load_transcript` while a turn is
    /// running therefore appears to succeed, but the in-flight turn will
    /// silently overwrite the resumed history at the moment it finishes.
    /// The TUI enforces this by gating `/resume` on its `is_loading` flag.
    pub async fn load_transcript(&self, path: &Path) -> Result<TranscriptFile, TranscriptError> {
        let bytes = tokio::fs::read(path).await?;
        let root: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| TranscriptError::Malformed(e.to_string()))?;

        let transcript = match &root {
            // Versioned format: top-level object with schema_version.
            serde_json::Value::Object(map) if map.contains_key("schema_version") => {
                let record: TranscriptFile = serde_json::from_value(root)
                    .map_err(|e| TranscriptError::Malformed(e.to_string()))?;
                if record.schema_version != TRANSCRIPT_SCHEMA_VERSION {
                    return Err(TranscriptError::SchemaMismatch {
                        found: record.schema_version,
                        expected: TRANSCRIPT_SCHEMA_VERSION,
                    });
                }
                record
            }
            // Legacy bare array: treat as v1, synthesize metadata.
            serde_json::Value::Array(_) => {
                let messages: Vec<Message> = serde_json::from_value(root)
                    .map_err(|e| TranscriptError::Malformed(e.to_string()))?;
                TranscriptFile {
                    schema_version: TRANSCRIPT_SCHEMA_VERSION,
                    model: self.config.model.clone(),
                    saved_at: 0,
                    messages,
                }
            }
            _ => {
                return Err(TranscriptError::Malformed(
                    "expected a JSON object or array at the root".into(),
                ));
            }
        };

        {
            let mut state = self.state.lock().await;
            state.messages = transcript.messages.clone();
        }
        Ok(transcript)
    }

    /// Drop the per-turn message history without touching connections.
    pub async fn clear_history(&self) {
        self.state.lock().await.messages.clear();
    }

    /// Borrow the active config.
    pub fn config(&self) -> &HostConfig {
        &self.config
    }

    /// Borrow the active sandbox configuration.
    pub fn sandbox_config(&self) -> &SandboxConfig {
        &self.sandbox
    }

    /// Resolve a previously-emitted [`TurnEvent::PermissionRequested`].
    ///
    /// Called by the embedder (TUI) once the user picks Allow / Deny in the
    /// modal. A no-op if `id` is unknown — that handles double-resolves and
    /// races where the turn was cancelled before the user answered.
    pub async fn resolve_permission(&self, id: u64, decision: PermissionDecision) {
        let sender = self.pending.lock().await.remove(&id);
        if let Some(tx) = sender {
            let _ = tx.send(decision);
        }
    }

    /// Resolve a previously-emitted
    /// [`TurnEvent::BashNetworkRequested`]. The embedder (TUI) calls this
    /// after the user picks Once / AlwaysThisSession / DenyOnce /
    /// DenyAlways from the modal. The corresponding spawn resumes.
    ///
    /// A no-op if `id` is unknown — that handles double-resolves and
    /// races where the spawn was cancelled before the user answered.
    pub async fn resolve_bash_network_decision(&self, id: u64, choice: BashNetworkChoice) {
        let tx = self.pending_bash_network.lock().await.remove(&id);
        if let Some(tx) = tx {
            let _ = tx.send(choice);
        }
    }

    /// Invoke the registered `tool-bash` `run` tool directly, outside the
    /// model-driven tool-use loop. Used by the TUI's `/bash` slash command
    /// so a user can run a shell command without round-tripping through
    /// the provider.
    ///
    /// `net_override` is forwarded to
    /// [`crate::tools::ToolRegistry::call_with_bash_net_override`]:
    /// `Some(true)` / `Some(false)` short-circuit the bash-network policy
    /// for this call; `None` defers to the policy (which may emit
    /// [`TurnEvent::BashNetworkRequested`] on `events` if one is
    /// supplied).
    ///
    /// Returns `Ok((is_error, payload))` — `is_error == true` means the
    /// underlying tool reported failure (non-zero exit, transport error,
    /// etc.); `payload` is the textual tool-result. Returns
    /// `Err("tool-bash not registered")` when no bash endpoint was
    /// configured on the host.
    pub async fn run_bash_command(
        &self,
        command: &str,
        net_override: Option<bool>,
        events: Option<mpsc::Sender<TurnEvent>>,
    ) -> Result<(bool, String), String> {
        let _events_guard = CurrentTurnEventsGuard::install(&self.current_turn_events, &events);
        let input = serde_json::json!({ "command": command });
        let guard = self.tools.lock().await;
        let registry = guard
            .as_ref()
            .ok_or_else(|| "tool registry unavailable".to_string())?;
        let outcome = registry
            .call_with_bash_net_override("run", input, net_override)
            .await;
        Ok((outcome.is_error, outcome.payload))
    }

    /// Install the real bash-net resolver into the tool registry. The
    /// resolver captures `Arc`-shared handles to the permission policy,
    /// pending-prompt map, current-turn events, and request-id counter
    /// — exactly the state [`resolve_bash_network_with_state`] needs
    /// — so the closure can emit `BashNetworkRequested` and await the
    /// user's answer without holding a reference to `Host` itself.
    ///
    /// Idempotent — replacing the resolver multiple times is fine; the
    /// `ToolRegistry`'s lazy-bash slot reads it under a lock.
    async fn wire_self_into_resolver(&self) {
        let policy = self.policy.clone();
        let pending = self.pending_bash_network.clone();
        let next_id = self.next_request_id.clone();
        let current_events = self.current_turn_events.clone();
        let resolver: crate::tools::BashNetResolver = Arc::new(move |over: Option<bool>| {
            // Per-call override short-circuits: never touch the cache.
            if let Some(v) = over {
                return Box::pin(async move { v });
            }
            let policy = policy.clone();
            let pending = pending.clone();
            let next_id = next_id.clone();
            // Snapshot the current-turn events Sender (if any) so the
            // resolver can emit a prompt without holding a lock across
            // the await.
            let events = current_events
                .lock()
                .expect("current_turn_events poisoned")
                .clone();
            Box::pin(async move {
                let summary = "tool-bash spawn requests network access".to_string();
                resolve_bash_network_with_state(
                    &policy, &pending, &next_id, events.as_ref(), summary,
                )
                .await
                .unwrap_or(false)
            })
        });
        let guard = self.tools.lock().await;
        if let Some(reg) = guard.as_ref() {
            reg.install_bash_net_resolver(resolver);
        }
    }

    /// Persist a user-recorded Always/Never decision via the policy.
    ///
    /// Builds an [`crate::permissions::ArgPattern`] from `(tool_name, args)`
    /// and writes through to `~/.savvagent/permissions.toml`. Subsequent
    /// matching calls — in this session and future ones — short-circuit
    /// the modal. I/O errors are logged but not surfaced; the in-memory
    /// rule update still wins immediately.
    pub async fn add_session_rule(
        &self,
        tool_name: &str,
        args: &Value,
        decision: PermissionDecision,
    ) {
        if let Err(e) = self.policy.add_rule(tool_name, args, decision).await {
            tracing::warn!(
                tool = tool_name,
                error = %e,
                "failed to persist permission rule to ~/.savvagent/permissions.toml",
            );
        }
    }

    /// Snapshot of every tool advertised by the connected tool servers, in
    /// the order they were registered. Used by the TUI's `/tools` command.
    pub async fn tool_defs(&self) -> Vec<ToolDef> {
        let guard = self.tools.lock().await;
        guard.as_ref().map(|t| t.defs.clone()).unwrap_or_default()
    }

    /// What the policy would return for `tool_name` with empty arguments —
    /// a coarse "verdict at a glance" used by the TUI's `/tools` listing.
    /// Path-conditional verdicts (e.g. `write_file`) collapse to the
    /// no-path branch, which is intentionally the more conservative choice.
    pub fn default_verdict_for(&self, tool_name: &str) -> Verdict {
        self.policy
            .evaluate(tool_name, &Value::Object(serde_json::Map::new()))
    }

    /// Run policy against `(name, input)` and either return `Ok(())` (caller
    /// proceeds with the call) or `Err(reason)` (caller synthesizes a denied
    /// `tool_result`). For [`Verdict::Ask`], emits a
    /// [`TurnEvent::PermissionRequested`] and awaits the matching
    /// [`Self::resolve_permission`] via a oneshot.
    async fn gate_tool_call(
        &self,
        name: &str,
        input: &Value,
        events: Option<&mpsc::Sender<TurnEvent>>,
    ) -> Result<(), String> {
        match self.policy.evaluate(name, input) {
            Verdict::Allow => Ok(()),
            Verdict::Deny { reason } => Err(reason),
            Verdict::Ask { summary } => {
                let Some(tx) = events else {
                    // No interactive surface — Ask collapses to Deny so the
                    // model gets a clear "non-interactive turn" tool_result
                    // instead of hanging on a oneshot that nobody resolves.
                    return Err("non-interactive turn".into());
                };
                let req_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
                let (resolve_tx, resolve_rx) = oneshot::channel();
                self.pending.lock().await.insert(req_id, resolve_tx);
                let send_result = tx
                    .send(TurnEvent::PermissionRequested {
                        id: req_id,
                        name: name.to_string(),
                        summary,
                        args: input.clone(),
                    })
                    .await;
                if send_result.is_err() {
                    self.pending.lock().await.remove(&req_id);
                    return Err("event channel closed before permission could be requested".into());
                }
                match resolve_rx.await {
                    Ok(PermissionDecision::Allow) => Ok(()),
                    Ok(PermissionDecision::Deny) => Err("denied by user".into()),
                    Err(_) => Err("permission channel dropped".into()),
                }
            }
        }
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

/// RAII guard that publishes the per-turn events `Sender` to
/// `Host::current_turn_events` for the lifetime of a turn, and clears
/// it on drop. Lets the lazy bash-net resolver closure pick up the
/// channel without having to thread it through every call layer.
struct CurrentTurnEventsGuard<'a> {
    slot: &'a std::sync::Mutex<Option<mpsc::Sender<TurnEvent>>>,
}

impl<'a> CurrentTurnEventsGuard<'a> {
    fn install(
        slot: &'a std::sync::Mutex<Option<mpsc::Sender<TurnEvent>>>,
        events: &Option<mpsc::Sender<TurnEvent>>,
    ) -> Self {
        *slot.lock().expect("current_turn_events poisoned") = events.clone();
        Self { slot }
    }
}

impl Drop for CurrentTurnEventsGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut g) = self.slot.lock() {
            *g = None;
        }
    }
}

/// Bootstrap resolver used while the registry is being constructed —
/// before we have an `Arc<Host>` we can capture into a closure that
/// calls back into the host's permission state. It returns the
/// per-call override if any, otherwise `false` (deny). The real
/// resolver is installed in [`Host::wire_self_into_resolver`] right
/// after `Host` construction.
fn bootstrap_bash_net_resolver() -> BashNetResolver {
    std::sync::Arc::new(|over: Option<bool>| {
        let v = over.unwrap_or(false);
        Box::pin(async move { v })
    })
}

/// Shared resolver-state logic. Pure-function over its inputs so the
/// lazy-bash resolver closure (which can't borrow `&self`) and any
/// future direct callers can both use it. Emits
/// [`TurnEvent::BashNetworkRequested`] on the supplied channel when
/// the policy is `Ask` and no cached decision exists, awaits the
/// matching [`Host::resolve_bash_network_decision`] call, and
/// returns the resolved `allow_net`.
async fn resolve_bash_network_with_state(
    policy: &PermissionPolicy,
    pending: &Mutex<HashMap<u64, oneshot::Sender<BashNetworkChoice>>>,
    next_request_id: &AtomicU64,
    events: Option<&mpsc::Sender<TurnEvent>>,
    summary: String,
) -> Result<bool, ()> {
    match policy.bash_network() {
        BashNetworkPolicy::Always => Ok(true),
        BashNetworkPolicy::Never => Ok(false),
        BashNetworkPolicy::Ask => {
            if let Some(cached) = policy.bash_network_cached() {
                return Ok(cached);
            }
            // No cache — emit a prompt and await.
            let Some(events) = events else {
                // No interactive surface — collapse to deny so the
                // spawn doesn't hang on a oneshot that nobody resolves.
                return Ok(false);
            };
            let id = next_request_id.fetch_add(1, Ordering::Relaxed);
            let (tx, rx) = oneshot::channel();
            pending.lock().await.insert(id, tx);
            if events
                .send(TurnEvent::BashNetworkRequested { id, summary })
                .await
                .is_err()
            {
                pending.lock().await.remove(&id);
                return Err(());
            }
            let choice = rx.await.map_err(|_| ())?;
            // Update cache via the same sync resolver — pass a closure
            // that returns the choice we already have.
            let allow = policy.resolve_bash_network(|| choice);
            Ok(allow)
        }
    }
}

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

#[cfg(test)]
mod policy_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    use async_trait::async_trait;
    use savvagent_mcp::ProviderClient;
    use savvagent_protocol::{
        CompleteRequest, CompleteResponse, ContentBlock, ProviderError, StopReason, Usage,
    };
    use serde_json::json;
    use tokio::sync::mpsc;

    use super::*;
    use crate::config::{HostConfig, ProviderEndpoint};

    /// Mock provider that on the first `complete` returns one `tool_use` and
    /// on every subsequent call returns `end_turn`. The first response asks
    /// for `tool_name` with the supplied JSON `tool_args`.
    struct ScriptedProvider {
        calls: AtomicUsize,
        tool_name: String,
        tool_args: Value,
    }

    impl ScriptedProvider {
        fn new(tool_name: impl Into<String>, tool_args: Value) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                tool_name: tool_name.into(),
                tool_args,
            }
        }
    }

    #[async_trait]
    impl ProviderClient for ScriptedProvider {
        async fn complete(
            &self,
            req: CompleteRequest,
            _events: Option<mpsc::Sender<StreamEvent>>,
        ) -> Result<CompleteResponse, ProviderError> {
            let n = self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            let (content, stop_reason) = if n == 0 {
                (
                    vec![ContentBlock::ToolUse {
                        id: "tu_1".into(),
                        name: self.tool_name.clone(),
                        input: self.tool_args.clone(),
                    }],
                    StopReason::ToolUse,
                )
            } else {
                (
                    vec![ContentBlock::Text {
                        text: "done".into(),
                    }],
                    StopReason::EndTurn,
                )
            };
            Ok(CompleteResponse {
                id: format!("resp-{n}"),
                model: req.model,
                content,
                stop_reason,
                stop_sequence: None,
                usage: Usage::default(),
            })
        }
    }

    fn config_no_tools() -> HostConfig {
        let project_root = std::env::temp_dir().join("savvagent-policy-test");
        HostConfig::new(
            ProviderEndpoint::StreamableHttp {
                url: "inproc://test".into(),
            },
            "test-model".to_string(),
        )
        .with_project_root(project_root.clone())
        // Use a transient (in-memory only) policy so tests don't touch the
        // real `~/.savvagent/permissions.toml`.
        .with_policy(PermissionPolicy::transient(project_root))
    }

    /// `read_file` against `.env` is a default-Deny — host must synthesize an
    /// error tool_result and emit `ToolCallDenied`, never touching the
    /// registry.
    #[tokio::test]
    async fn deny_path_synthesizes_error_and_emits_event() {
        let provider: Box<dyn ProviderClient + Send + Sync> =
            Box::new(ScriptedProvider::new("read_file", json!({"path": ".env"})));
        let host = Host::with_components(config_no_tools(), provider)
            .await
            .unwrap();

        let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
        let outcome = host.run_turn_streaming("hi", tx).await.unwrap();

        // The model thought a tool ran; we report it as Errored with a
        // policy-denied payload.
        assert_eq!(outcome.tool_calls.len(), 1);
        let call = &outcome.tool_calls[0];
        assert_eq!(call.name, "read_file");
        assert_eq!(call.status, ToolCallStatus::Errored);
        assert!(call.result.contains("denied by policy"), "{}", call.result);

        let mut saw_denied = false;
        let mut saw_started = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                TurnEvent::ToolCallDenied { ref name, .. } if name == "read_file" => {
                    saw_denied = true;
                }
                TurnEvent::ToolCallStarted { .. } => {
                    saw_started = true;
                }
                _ => {}
            }
        }
        assert!(saw_denied, "expected a ToolCallDenied event");
        assert!(
            !saw_started,
            "ToolCallStarted should not fire for a denied call"
        );
    }

    /// `run` is default-Ask — host must emit `PermissionRequested` and wait
    /// for `resolve_permission`. After Allow, the registry runs (returns
    /// "unknown tool" since none registered, which is fine — we're testing
    /// the gating, not the call).
    #[tokio::test]
    async fn ask_path_blocks_until_resolved() {
        let provider: Box<dyn ProviderClient + Send + Sync> =
            Box::new(ScriptedProvider::new("run", json!({"command": "echo hi"})));
        let host = Arc::new(
            Host::with_components(config_no_tools(), provider)
                .await
                .unwrap(),
        );

        let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
        let host_for_run = Arc::clone(&host);
        let runner = tokio::spawn(async move { host_for_run.run_turn_streaming("hi", tx).await });

        // Drain events until we see PermissionRequested, then resolve Allow.
        let mut request_id: Option<u64> = None;
        while let Some(ev) = rx.recv().await {
            if let TurnEvent::PermissionRequested { id, ref name, .. } = ev {
                assert_eq!(name, "run");
                request_id = Some(id);
                break;
            }
        }
        let id = request_id.expect("PermissionRequested never arrived");
        host.resolve_permission(id, PermissionDecision::Allow).await;

        // Drain the rest.
        while rx.recv().await.is_some() {}

        let outcome = runner.await.unwrap().unwrap();
        // The Allow let the call reach the (empty) registry, which returns an
        // "unknown tool" error. That's the contract we want — the gate said
        // "go" and the call dispatched.
        assert_eq!(outcome.tool_calls.len(), 1);
        let call = &outcome.tool_calls[0];
        assert_eq!(call.name, "run");
        assert_eq!(call.status, ToolCallStatus::Errored);
        assert!(
            call.result.contains("unknown tool"),
            "expected dispatch to reach registry, got: {}",
            call.result
        );
    }

    /// A pre-registered Always rule short-circuits the policy: the modal
    /// never fires and `gate_tool_call` returns the rule's decision. After
    /// M9 PR 4 the rule is keyed on a normalized [`ArgPattern`] (here:
    /// command first-word `echo`) and stored in the policy's `toml_rules`
    /// layer — disk I/O is suppressed because the test policy is transient.
    #[tokio::test]
    async fn session_rule_short_circuits_policy() {
        let provider: Box<dyn ProviderClient + Send + Sync> =
            Box::new(ScriptedProvider::new("run", json!({"command": "echo hi"})));
        let host = Host::with_components(config_no_tools(), provider)
            .await
            .unwrap();
        host.add_session_rule(
            "run",
            &json!({"command": "echo hi"}),
            PermissionDecision::Allow,
        )
        .await;

        let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
        let outcome = host.run_turn_streaming("hi", tx).await.unwrap();

        let mut saw_permission = false;
        while let Some(ev) = rx.recv().await {
            if matches!(ev, TurnEvent::PermissionRequested { .. }) {
                saw_permission = true;
            }
        }
        assert!(
            !saw_permission,
            "session rule should suppress PermissionRequested"
        );
        // Allow lets the call reach the empty registry → "unknown tool".
        assert!(outcome.tool_calls[0].result.contains("unknown tool"));
    }

    /// Lazy-spawn end-to-end (no real bash binary): build the exact
    /// resolver closure `Host::wire_self_into_resolver` builds, drive it
    /// twice, and confirm only the first call emits a prompt — the
    /// second hits the session cache after `AlwaysThisSession`. This is
    /// the contract the lazy `tool-bash` spawn relies on for "prompt
    /// once per session", and the regression is what made this PR
    /// necessary: with the old eager spawn the question was already
    /// settled at registry-connect time.
    #[tokio::test]
    async fn lazy_bash_resolver_prompts_once_then_caches_session() {
        use std::sync::Arc;

        use crate::permissions::BashNetworkPolicy;
        use crate::tools::BashNetResolver;

        // Build a transient Ask-policy host and reach into its state to
        // construct the same resolver closure
        // `wire_self_into_resolver` produces. We test through the
        // resolver because the registry's lazy-bash dispatch path goes
        // through exactly this closure on every call.
        let provider: Box<dyn ProviderClient + Send + Sync> =
            Box::new(ScriptedProvider::new("noop", json!({})));
        let project_root = std::env::temp_dir().join("savvagent-lazy-bash-test");
        let config = HostConfig::new(
            ProviderEndpoint::StreamableHttp {
                url: "inproc://test".into(),
            },
            "test-model".to_string(),
        )
        .with_project_root(project_root.clone())
        .with_policy(
            PermissionPolicy::transient(project_root).with_bash_network(BashNetworkPolicy::Ask),
        );
        let host = Arc::new(Host::with_components(config, provider).await.unwrap());

        // Recreate exactly the resolver closure
        // `Host::wire_self_into_resolver` installs into the registry's
        // lazy slot. We can't easily reach into a real lazy slot
        // without a working tool-bash binary in the test environment;
        // building the resolver by hand exercises the same code path.
        let policy = host.policy.clone();
        let pending = host.pending_bash_network.clone();
        let next_id = host.next_request_id.clone();
        let current_events = host.current_turn_events.clone();
        let resolver: BashNetResolver = Arc::new(move |over: Option<bool>| {
            if let Some(v) = over {
                return Box::pin(async move { v });
            }
            let policy = policy.clone();
            let pending = pending.clone();
            let next_id = next_id.clone();
            let events = current_events
                .lock()
                .expect("current_turn_events poisoned")
                .clone();
            Box::pin(async move {
                let summary = "tool-bash spawn requests network access".to_string();
                super::resolve_bash_network_with_state(
                    &policy, &pending, &next_id, events.as_ref(), summary,
                )
                .await
                .unwrap_or(false)
            })
        });

        // Publish a per-turn events channel into the host's slot — same
        // as `CurrentTurnEventsGuard::install` would do at turn start.
        let (events_tx, mut events_rx) = mpsc::channel::<TurnEvent>(8);
        *host.current_turn_events.lock().unwrap() = Some(events_tx);

        // Spawn the first resolve in a task so we can pump events and
        // call resolve_bash_network_decision concurrently. Note: the
        // resolver future is `Send + 'static` because all state is Arc.
        let resolver_clone = resolver.clone();
        let first = tokio::spawn(async move { (resolver_clone)(None).await });

        // We expect a single BashNetworkRequested. Pluck its id, then
        // answer AlwaysThisSession so the cache populates.
        let id = loop {
            match events_rx.recv().await {
                Some(TurnEvent::BashNetworkRequested { id, .. }) => break id,
                Some(_other) => continue,
                None => panic!("events channel closed before BashNetworkRequested arrived"),
            }
        };
        host.resolve_bash_network_decision(id, BashNetworkChoice::AlwaysThisSession)
            .await;

        let allow_first = first.await.unwrap();
        assert!(
            allow_first,
            "AlwaysThisSession must resolve allow_net=true"
        );

        // Second resolve: should NOT emit another prompt (cache hit).
        // Drop the existing receiver's stash by trying a non-blocking
        // recv after the second resolve.
        let allow_second = (resolver)(None).await;
        assert!(
            allow_second,
            "second resolve must reuse the cached AlwaysThisSession decision"
        );

        // Drain any pending events with a tiny window and assert none
        // are BashNetworkRequested. We use try_recv repeatedly with no
        // sleep — the channel either has the message already or it
        // never will because the resolve_bash_network_with_state path
        // short-circuited via `policy.bash_network_cached()`.
        loop {
            match events_rx.try_recv() {
                Ok(TurnEvent::BashNetworkRequested { .. }) => {
                    panic!("second resolve must NOT emit a BashNetworkRequested");
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }

        // Drop the resolver so the test exits cleanly.
        drop(resolver);
    }

    /// `non-streaming` `run_turn` has no event channel, so any Ask collapses
    /// to a Deny rather than hanging on a oneshot that nobody resolves.
    #[tokio::test]
    async fn ask_with_no_events_collapses_to_deny() {
        let provider: Box<dyn ProviderClient + Send + Sync> =
            Box::new(ScriptedProvider::new("run", json!({"command": "echo hi"})));
        let host = Host::with_components(config_no_tools(), provider)
            .await
            .unwrap();

        let outcome = host.run_turn("hi").await.unwrap();
        assert_eq!(outcome.tool_calls.len(), 1);
        assert_eq!(outcome.tool_calls[0].status, ToolCallStatus::Errored);
        assert!(
            outcome.tool_calls[0]
                .result
                .contains("non-interactive turn"),
            "{}",
            outcome.tool_calls[0].result
        );
    }
}

#[cfg(test)]
mod list_models_tests {
    use async_trait::async_trait;
    use savvagent_mcp::{InProcessProviderClient, ProviderClient, ProviderHandler, StreamEmitter};
    use savvagent_protocol::{
        CompleteRequest, CompleteResponse, ErrorKind, ListModelsResponse, ModelInfo, ProviderError,
    };

    use super::*;
    use crate::config::{HostConfig, ProviderEndpoint};

    /// Handler that advertises a tiny curated list. Used to exercise the
    /// `Host::list_models` facade end-to-end via the in-process bridge.
    struct CuratedHandler;

    #[async_trait]
    impl ProviderHandler for CuratedHandler {
        async fn complete(
            &self,
            _req: CompleteRequest,
            _emit: Option<&dyn StreamEmitter>,
        ) -> Result<CompleteResponse, ProviderError> {
            unreachable!("complete is not exercised in list_models tests")
        }
        async fn list_models(&self) -> Result<ListModelsResponse, ProviderError> {
            Ok(ListModelsResponse {
                models: vec![
                    ModelInfo {
                        id: "alpha".into(),
                        display_name: Some("Alpha".into()),
                        context_window: Some(8192),
                    },
                    ModelInfo {
                        id: "beta".into(),
                        display_name: None,
                        context_window: None,
                    },
                ],
                default_model_id: Some("alpha".into()),
            })
        }
    }

    /// Handler that inherits the default `list_models` trait impl, signaling
    /// "not advertised" to host callers.
    struct SilentHandler;

    #[async_trait]
    impl ProviderHandler for SilentHandler {
        async fn complete(
            &self,
            _req: CompleteRequest,
            _emit: Option<&dyn StreamEmitter>,
        ) -> Result<CompleteResponse, ProviderError> {
            unreachable!("complete is not exercised in list_models tests")
        }
    }

    fn config() -> HostConfig {
        let project_root = std::env::temp_dir().join("savvagent-list-models-test");
        HostConfig::new(
            ProviderEndpoint::StreamableHttp {
                url: "inproc://test".into(),
            },
            "alpha".to_string(),
        )
        .with_project_root(project_root.clone())
        .with_policy(PermissionPolicy::transient(project_root))
    }

    #[tokio::test]
    async fn host_list_models_returns_provider_list() {
        let provider: Box<dyn ProviderClient + Send + Sync> = Box::new(
            InProcessProviderClient::new(std::sync::Arc::new(CuratedHandler)),
        );
        let host = Host::with_components(config(), provider).await.unwrap();
        let resp = host
            .list_models()
            .await
            .expect("list_models should succeed");
        let ids: Vec<_> = resp.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["alpha", "beta"]);
        assert_eq!(resp.default_model_id, Some("alpha".into()));
    }

    #[tokio::test]
    async fn host_list_models_surfaces_not_advertised_error() {
        let provider: Box<dyn ProviderClient + Send + Sync> = Box::new(
            InProcessProviderClient::new(std::sync::Arc::new(SilentHandler)),
        );
        let host = Host::with_components(config(), provider).await.unwrap();
        let err = host
            .list_models()
            .await
            .expect_err("default impl must error");
        assert!(matches!(err.kind, ErrorKind::NotImplemented));
        assert!(err.message.contains("list_models"), "msg: {}", err.message);
    }
}

#[cfg(test)]
mod transcript_tests {
    use async_trait::async_trait;
    use savvagent_mcp::ProviderClient;
    use savvagent_protocol::{
        CompleteRequest, CompleteResponse, ContentBlock, Message, ProviderError, Role, StopReason,
        Usage,
    };
    use tempfile::tempdir;
    use tokio::sync::mpsc;

    use super::*;
    use crate::config::{HostConfig, ProviderEndpoint};
    use crate::permissions::PermissionPolicy;

    /// Minimal provider that immediately returns end_turn with no content.
    struct NoopProvider;

    #[async_trait]
    impl ProviderClient for NoopProvider {
        async fn complete(
            &self,
            req: CompleteRequest,
            _events: Option<mpsc::Sender<StreamEvent>>,
        ) -> Result<CompleteResponse, ProviderError> {
            Ok(CompleteResponse {
                id: "noop".into(),
                model: req.model,
                content: vec![ContentBlock::Text { text: "ok".into() }],
                stop_reason: StopReason::EndTurn,
                stop_sequence: None,
                usage: Usage::default(),
            })
        }
    }

    fn tmp_config(dir: &std::path::Path) -> HostConfig {
        HostConfig::new(
            ProviderEndpoint::StreamableHttp {
                url: "inproc://test".into(),
            },
            "test-model".to_string(),
        )
        .with_project_root(dir.to_path_buf())
        .with_policy(PermissionPolicy::transient(dir.to_path_buf()))
    }

    /// Round-trip: save then load recovers the same message history.
    #[tokio::test]
    async fn round_trip_recovers_messages() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("t.json");

        // Build a host with some history.
        let host1 = Host::with_components(
            tmp_config(dir.path()),
            Box::new(NoopProvider) as Box<dyn ProviderClient + Send + Sync>,
        )
        .await
        .unwrap();
        host1.run_turn("hello").await.unwrap();
        host1.save_transcript(&path).await.unwrap();
        let saved = host1.messages().await;

        // Load into a fresh host.
        let host2 = Host::with_components(
            tmp_config(dir.path()),
            Box::new(NoopProvider) as Box<dyn ProviderClient + Send + Sync>,
        )
        .await
        .unwrap();
        let record = host2.load_transcript(&path).await.unwrap();
        let loaded = host2.messages().await;

        assert_eq!(saved, loaded, "message history must survive round-trip");
        assert_eq!(record.schema_version, TRANSCRIPT_SCHEMA_VERSION);
        assert_eq!(record.model, "test-model");
    }

    /// Schema version mismatch yields a typed error, not a panic.
    #[tokio::test]
    async fn schema_mismatch_returns_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("future.json");

        let future_file = serde_json::json!({
            "schema_version": TRANSCRIPT_SCHEMA_VERSION + 1,
            "model": "some-model",
            "saved_at": 0,
            "messages": []
        });
        tokio::fs::write(&path, serde_json::to_vec(&future_file).unwrap())
            .await
            .unwrap();

        let host = Host::with_components(
            tmp_config(dir.path()),
            Box::new(NoopProvider) as Box<dyn ProviderClient + Send + Sync>,
        )
        .await
        .unwrap();
        let err = host.load_transcript(&path).await.unwrap_err();
        assert!(
            matches!(err, TranscriptError::SchemaMismatch { .. }),
            "expected SchemaMismatch, got {err:?}"
        );
    }

    /// Malformed JSON returns an error without panicking.
    #[tokio::test]
    async fn malformed_json_returns_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad.json");
        tokio::fs::write(&path, b"{ not valid json !!!")
            .await
            .unwrap();

        let host = Host::with_components(
            tmp_config(dir.path()),
            Box::new(NoopProvider) as Box<dyn ProviderClient + Send + Sync>,
        )
        .await
        .unwrap();
        let err = host.load_transcript(&path).await.unwrap_err();
        assert!(
            matches!(err, TranscriptError::Malformed(_)),
            "expected Malformed, got {err:?}"
        );
    }

    /// Legacy bare-array transcripts (pre-resume) are accepted transparently.
    #[tokio::test]
    async fn legacy_bare_array_loads_ok() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("legacy.json");

        let messages = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "legacy message".into(),
            }],
        }];
        tokio::fs::write(&path, serde_json::to_vec_pretty(&messages).unwrap())
            .await
            .unwrap();

        let host = Host::with_components(
            tmp_config(dir.path()),
            Box::new(NoopProvider) as Box<dyn ProviderClient + Send + Sync>,
        )
        .await
        .unwrap();
        let record = host.load_transcript(&path).await.unwrap();
        assert_eq!(record.messages, messages);
        assert_eq!(record.schema_version, TRANSCRIPT_SCHEMA_VERSION);
    }
}
