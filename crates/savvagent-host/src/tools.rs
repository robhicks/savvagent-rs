//! Tool MCP server registry.
//!
//! At startup, [`ToolRegistry::connect`] spawns each configured stdio tool
//! server as a child process, fetches its `tools/list`, and builds a routing
//! table from tool name to server. During the tool-use loop, the host calls
//! [`ToolRegistry::call`] with the model's chosen tool name and JSON
//! arguments; the registry dispatches the call and returns a normalized
//! [`ToolCallOutcome`].

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use rmcp::{
    RoleClient, ServiceExt, model::CallToolRequestParams, service::RunningService,
    transport::TokioChildProcess,
};
use savvagent_protocol::ToolDef;
use serde_json::Value;

use crate::config::ToolEndpoint;
use crate::sandbox::{SandboxConfig, SandboxWrapper, apply_sandbox};

/// Aggregate view of all connected tool servers.
pub(crate) struct ToolRegistry {
    servers: Vec<ToolServer>,
    /// Tool name → index into `servers`.
    routes: HashMap<String, usize>,
    /// Aggregated tool definitions, in the order they were discovered.
    pub(crate) defs: Vec<ToolDef>,
}

struct ToolServer {
    label: String,
    service: RunningService<RoleClient, ()>,
}

impl ToolRegistry {
    /// Spawn each configured tool server and aggregate their tool lists.
    ///
    /// `project_root` is forwarded to every spawned child via two parallel
    /// env vars — `SAVVAGENT_TOOL_FS_ROOT` and `SAVVAGENT_TOOL_BASH_ROOT` —
    /// so the bundled tool binaries confine themselves to the host's project
    /// root by default. Setting both on every tool is harmless: each tool
    /// reads only the var it cares about.
    ///
    /// `sandbox` is applied to each spawn when [`SandboxConfig::enabled`] is
    /// `true` and the platform wrapper binary (`bwrap` / `sandbox-exec`) is
    /// found on `$PATH`. If it's missing, the tool runs unwrapped with a
    /// warning — sandboxing is never a hard prerequisite.
    pub async fn connect(
        endpoints: &[ToolEndpoint],
        project_root: &Path,
        sandbox: &SandboxConfig,
    ) -> Result<Self> {
        let mut servers = Vec::with_capacity(endpoints.len());
        let mut routes: HashMap<String, usize> = HashMap::new();
        let mut defs = Vec::new();

        for (idx, ep) in endpoints.iter().enumerate() {
            match ep {
                ToolEndpoint::Stdio { command, args } => {
                    let label = command.display().to_string();
                    let mut cmd = tokio::process::Command::new(command);
                    cmd.args(args);
                    cmd.env("SAVVAGENT_TOOL_FS_ROOT", project_root);
                    cmd.env("SAVVAGENT_TOOL_BASH_ROOT", project_root);

                    // Apply the OS sandbox wrapper if configured.
                    let wrapper = apply_sandbox(&mut cmd, command, project_root, sandbox);
                    match wrapper {
                        SandboxWrapper::None => {}
                        SandboxWrapper::Bwrap => {
                            tracing::info!("sandbox[bwrap]: {label}");
                        }
                        SandboxWrapper::SandboxExec => {
                            tracing::info!("sandbox[sandbox-exec]: {label}");
                        }
                    }

                    let transport = TokioChildProcess::new(cmd)
                        .with_context(|| format!("spawn tool server: {label}"))?;
                    let service = ()
                        .serve(transport)
                        .await
                        .with_context(|| format!("init MCP session with {label}"))?;
                    let tools = service
                        .list_all_tools()
                        .await
                        .with_context(|| format!("list_tools on {label}"))?;
                    for t in tools {
                        let name = t.name.to_string();
                        if routes.insert(name.clone(), idx).is_some() {
                            anyhow::bail!(
                                "duplicate tool `{name}` advertised by {label} (already registered by another server)"
                            );
                        }
                        defs.push(ToolDef {
                            name,
                            description: t.description.as_deref().unwrap_or("").to_string(),
                            input_schema: Value::Object(input_schema_value(t.input_schema)),
                        });
                    }
                    servers.push(ToolServer { label, service });
                }
            }
        }

        tracing::debug!(
            "connected to {} tool server(s), {} tool(s) total",
            servers.len(),
            defs.len()
        );

        Ok(Self {
            servers,
            routes,
            defs,
        })
    }

    /// Call `name` on the appropriate tool server with `input` JSON arguments.
    pub async fn call(&self, name: &str, input: Value) -> ToolCallOutcome {
        let Some(&idx) = self.routes.get(name) else {
            return ToolCallOutcome::error(format!("unknown tool: {name}"));
        };
        let server = &self.servers[idx];
        let args = match input {
            Value::Object(m) => m,
            other => {
                return ToolCallOutcome::error(format!(
                    "tool `{name}` arguments must be a JSON object, got {}",
                    discriminant(&other)
                ));
            }
        };
        let params = CallToolRequestParams::new(name.to_string()).with_arguments(args);
        match server.service.call_tool(params).await {
            Ok(result) => {
                let is_error = result.is_error == Some(true);
                let payload = render_result_payload(&result);
                if is_error {
                    ToolCallOutcome::error(payload)
                } else {
                    ToolCallOutcome::success(payload)
                }
            }
            Err(e) => ToolCallOutcome::error(format!("tool transport error on {name}: {e}")),
        }
    }

    /// Cancel each tool server session, draining its child process.
    pub async fn shutdown(self) {
        for s in self.servers {
            if let Err(e) = s.service.cancel().await {
                tracing::warn!("error closing tool server {}: {e}", s.label);
            }
        }
    }
}

/// Normalize the shape of an MCP tool result into a single `String` payload
/// suitable for embedding in a `tool_result` content block.
fn render_result_payload(result: &rmcp::model::CallToolResult) -> String {
    if let Some(v) = &result.structured_content {
        return serde_json::to_string(v).unwrap_or_else(|_| String::from("<unrenderable JSON>"));
    }
    let mut out = String::new();
    for c in &result.content {
        if let Some(t) = c.as_text() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&t.text);
        }
    }
    out
}

fn input_schema_value(arc: Arc<rmcp::model::JsonObject>) -> rmcp::model::JsonObject {
    Arc::try_unwrap(arc).unwrap_or_else(|a| (*a).clone())
}

fn discriminant(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Outcome of one tool dispatch.
#[derive(Debug, Clone)]
pub(crate) struct ToolCallOutcome {
    /// True if the tool reported failure or transport error.
    pub is_error: bool,
    /// Text payload to embed in a `tool_result` content block.
    pub payload: String,
}

impl ToolCallOutcome {
    fn success(payload: String) -> Self {
        Self {
            is_error: false,
            payload,
        }
    }
    fn error(payload: String) -> Self {
        Self {
            is_error: true,
            payload,
        }
    }
}
