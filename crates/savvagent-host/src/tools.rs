//! Tool MCP server registry.
//!
//! At startup, [`ToolRegistry::connect`] sets up each configured stdio tool
//! server. Most tools are spawned eagerly: the registry connects to them,
//! fetches their `tools/list`, and routes calls to the cached process. The
//! `tool-bash` server is special — its OS sandbox needs to bake in an
//! `allow_net` decision that we can't make until the host's event loop is
//! running and (for `BashNetworkPolicy::Ask`) the user has answered a prompt.
//! For that one tool we still do a *one-shot probe spawn* at connect time
//! to scrape the tool list (with `allow_net = false` — the spawn never
//! actually runs any shell command, just speaks MCP). After the probe we
//! shut the process down and lazy-spawn it on the first call.
//!
//! During the tool-use loop, the host calls
//! [`ToolRegistry::call_with_bash_net_override`] with the model's chosen
//! tool name, JSON arguments, and an optional per-call `net_override`. The
//! registry dispatches the call and returns a normalized
//! [`ToolCallOutcome`]. The bash slot caches a single active server keyed
//! on the `allow_net` it was spawned with: subsequent calls reuse it, and
//! a per-call override that disagrees with the active server's
//! `allow_net` kills the old child and respawns. Per-call overrides do
//! **not** mutate the session-cached bash network decision.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use rmcp::{
    RoleClient, ServiceExt, model::CallToolRequestParams, service::RunningService,
    transport::TokioChildProcess,
};
use savvagent_protocol::ToolDef;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::config::ToolEndpoint;
use crate::sandbox::{SandboxConfig, SandboxWrapper, apply_sandbox};

/// The substring we use to identify a `tool-bash` binary path. Mirrors the
/// detection scheme used in `sandbox.rs::net_allowed_for`.
const TOOL_BASH_MARKER: &str = "tool-bash";

/// Resolver invoked by [`ToolRegistry::call_with_bash_net_override`] when a
/// bash dispatch needs to know what `allow_net` to spawn with.
///
/// `Some(v)` is the per-call `--net`/`--no-net` override; the resolver must
/// return it directly without touching the session cache. `None` means
/// "resolve via permission policy" (which may emit a `BashNetworkRequested`
/// prompt and await the user's answer).
pub(crate) type BashNetResolver = Arc<
    dyn Fn(Option<bool>) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync + 'static,
>;

/// Aggregate view of all connected tool servers.
pub(crate) struct ToolRegistry {
    /// Eager (always-spawned) tool servers. Indices in `routes` for
    /// non-bash tools point into this vector.
    eager_servers: Vec<ToolServer>,
    /// Tool name → index into `eager_servers`. Bash tools (currently `run`)
    /// are NOT in this map; they hit the lazy path below instead.
    routes: HashMap<String, usize>,
    /// Aggregated tool definitions, in the order they were discovered.
    /// Includes bash's `run` (scraped via a probe spawn at connect time).
    pub(crate) defs: Vec<ToolDef>,
    /// Optional lazy slot for the configured `tool-bash` endpoint. `None`
    /// when no bash endpoint was supplied (e.g. tests).
    lazy_bash: Option<LazyBash>,
}

struct ToolServer {
    label: String,
    service: RunningService<RoleClient, ()>,
}

/// Lazy-spawn slot for `tool-bash`. The server is spawned on demand by
/// [`ToolRegistry::call_with_bash_net_override`] and cached until either
/// the registry shuts down or a call arrives with an `allow_net` that
/// disagrees with the cached spawn (in which case the cached process is
/// killed and a fresh one is spawned).
struct LazyBash {
    /// Set of tool names this lazy slot is responsible for dispatching.
    /// Populated by the probe spawn at connect time. In practice this is
    /// `{"run"}` today but storing the full set keeps the registry
    /// honest if tool-bash ever advertises more tools.
    tool_names: std::collections::HashSet<String>,
    /// Static spawn parameters captured at `connect` time. Cloned and
    /// mutated (per-spawn `allow_net` injected into `sandbox_template`)
    /// each time we (re)spawn the child.
    config: BashSpawnConfig,
    /// Resolves the runtime `allow_net` for a given per-call override.
    /// Invoked once per call before we look at the cached active server.
    /// Held behind `RwLock` so the host can install a real resolver
    /// (one that calls back into the host's permission state) after
    /// `Host` construction completes — at `connect` time we can't yet
    /// capture `self` into the closure.
    resolver: Arc<RwLock<BashNetResolver>>,
    /// Currently-active spawned server, if any. Lock guards the entire
    /// (resolve → reuse-or-respawn → dispatch) sequence for one call so
    /// concurrent calls can't race to spawn two children.
    active: Mutex<Option<ActiveBashServer>>,
}

/// Captured at `ToolRegistry::connect` time and reused for every (re)spawn
/// of the bash child. The only field that varies per spawn is the
/// `allow_net` injected into `sandbox_template.tool_overrides["tool-bash"]`.
#[derive(Clone)]
struct BashSpawnConfig {
    command: PathBuf,
    args: Vec<String>,
    project_root: PathBuf,
    /// Base sandbox config; the per-spawn `allow_net` is injected as a
    /// `tool_overrides[TOOL_BASH_MARKER]` entry before calling
    /// `apply_sandbox`. Cloned per spawn so we never permanently mutate
    /// the host's `SandboxConfig`.
    sandbox_template: SandboxConfig,
}

/// An active, lazily-spawned `tool-bash` child plus the `allow_net` it
/// was spawned with. Killed (via `service.cancel()`) before a respawn or
/// at registry shutdown.
struct ActiveBashServer {
    label: String,
    service: RunningService<RoleClient, ()>,
    allow_net: bool,
}

impl ToolRegistry {
    /// Spawn each non-bash tool server eagerly, probe-spawn the bash
    /// tool (if configured) just long enough to scrape its tool list,
    /// then aggregate everything.
    ///
    /// `project_root` is forwarded to every spawned child via three parallel
    /// env vars — `SAVVAGENT_TOOL_FS_ROOT`, `SAVVAGENT_TOOL_BASH_ROOT`, and
    /// `SAVVAGENT_TOOL_GREP_ROOT` — so the bundled tool binaries confine
    /// themselves to the host's project root by default. Setting all three
    /// on every tool is harmless: each tool reads only the var it cares about.
    ///
    /// `sandbox` is applied to each spawn when [`SandboxConfig::enabled`] is
    /// `true` and the platform wrapper binary (`bwrap` / `sandbox-exec`) is
    /// found on `$PATH`. If it's missing, the tool runs unwrapped with a
    /// warning — sandboxing is never a hard prerequisite.
    ///
    /// `bash_net_resolver` is invoked on every `tool-bash` call to resolve
    /// the `allow_net` for that call's spawn. See [`BashNetResolver`].
    pub async fn connect(
        endpoints: &[ToolEndpoint],
        project_root: &Path,
        sandbox: &SandboxConfig,
        bash_net_resolver: BashNetResolver,
    ) -> Result<Self> {
        let mut eager_servers = Vec::new();
        let mut routes: HashMap<String, usize> = HashMap::new();
        let mut defs = Vec::new();
        let mut lazy_bash: Option<LazyBash> = None;

        for ep in endpoints {
            match ep {
                ToolEndpoint::Stdio { command, args } => {
                    let is_bash = command.to_string_lossy().contains(TOOL_BASH_MARKER);
                    if is_bash {
                        // Probe spawn: spawn bash with allow_net=false just
                        // long enough to scrape its tool list, then drop it
                        // before any user command can run. The session's
                        // first real call lazily respawns with the runtime
                        // allow_net decision.
                        let probe_cmd = build_bash_command(
                            command,
                            args,
                            project_root,
                            sandbox,
                            /* allow_net = */ false,
                        );
                        let label = command.display().to_string();
                        let transport = TokioChildProcess::new(probe_cmd)
                            .with_context(|| format!("spawn tool-bash probe: {label}"))?;
                        let service = ()
                            .serve(transport)
                            .await
                            .with_context(|| format!("init MCP session with {label}"))?;
                        let tools = service
                            .list_all_tools()
                            .await
                            .with_context(|| format!("list_tools on {label}"))?;
                        let mut tool_names = std::collections::HashSet::new();
                        for t in tools {
                            let name = t.name.to_string();
                            if routes.contains_key(&name) || tool_names.contains(&name) {
                                anyhow::bail!(
                                    "duplicate tool `{name}` advertised by {label}"
                                );
                            }
                            tool_names.insert(name.clone());
                            defs.push(ToolDef {
                                name,
                                description: t.description.as_deref().unwrap_or("").to_string(),
                                input_schema: Value::Object(input_schema_value(t.input_schema)),
                            });
                        }
                        // Probe done — shut it down so the lazy spawn gets
                        // a clean slate at first call.
                        if let Err(e) = service.cancel().await {
                            tracing::warn!(
                                "tool-bash probe shutdown error (ignored): {e}"
                            );
                        }
                        if lazy_bash.is_some() {
                            anyhow::bail!(
                                "multiple tool-bash endpoints configured; only one is supported"
                            );
                        }
                        lazy_bash = Some(LazyBash {
                            tool_names,
                            config: BashSpawnConfig {
                                command: command.clone(),
                                args: args.clone(),
                                project_root: project_root.to_path_buf(),
                                sandbox_template: sandbox.clone(),
                            },
                            resolver: Arc::new(RwLock::new(bash_net_resolver.clone())),
                            active: Mutex::new(None),
                        });
                    } else {
                        let label = command.display().to_string();
                        let mut cmd = tokio::process::Command::new(command);
                        cmd.args(args);
                        cmd.env("SAVVAGENT_TOOL_FS_ROOT", project_root);
                        cmd.env("SAVVAGENT_TOOL_BASH_ROOT", project_root);
                        cmd.env("SAVVAGENT_TOOL_GREP_ROOT", project_root);

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
                        let idx = eager_servers.len();
                        for t in tools {
                            let name = t.name.to_string();
                            if routes.insert(name.clone(), idx).is_some() {
                                anyhow::bail!(
                                    "duplicate tool `{name}` advertised by {label}"
                                );
                            }
                            defs.push(ToolDef {
                                name,
                                description: t.description.as_deref().unwrap_or("").to_string(),
                                input_schema: Value::Object(input_schema_value(t.input_schema)),
                            });
                        }
                        eager_servers.push(ToolServer { label, service });
                    }
                }
            }
        }

        tracing::debug!(
            "connected to {} eager tool server(s){}, {} tool(s) total",
            eager_servers.len(),
            if lazy_bash.is_some() {
                " + lazy tool-bash"
            } else {
                ""
            },
            defs.len()
        );

        Ok(Self {
            eager_servers,
            routes,
            defs,
            lazy_bash,
        })
    }

    /// Call `name` with an optional per-call bash network override.
    ///
    /// For non-bash tools, `net_override` is ignored. For bash tools, the
    /// override is passed to the resolver and to the spawn logic; see
    /// [`LazyBash`] for the spawn-vs-reuse decision.
    pub async fn call_with_bash_net_override(
        &self,
        name: &str,
        input: Value,
        net_override: Option<bool>,
    ) -> ToolCallOutcome {
        // Validate args shape up-front; both paths need it as an object.
        let args = match input {
            Value::Object(m) => m,
            other => {
                return ToolCallOutcome::error(format!(
                    "tool `{name}` arguments must be a JSON object, got {}",
                    discriminant(&other)
                ));
            }
        };

        // Lazy bash path first — it owns the `run` tool name.
        if let Some(lazy) = self.lazy_bash.as_ref()
            && lazy.tool_names.contains(name)
        {
            return lazy.dispatch(name, args, net_override).await;
        }

        // Eager path for everything else.
        let Some(&idx) = self.routes.get(name) else {
            return ToolCallOutcome::error(format!("unknown tool: {name}"));
        };
        let server = &self.eager_servers[idx];
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

    /// Replace the bash network resolver. Used by [`crate::session::Host`]
    /// after construction so the resolver can capture `Arc`-shared
    /// handles to the host's permission state and emit
    /// [`crate::session::TurnEvent::BashNetworkRequested`].
    ///
    /// No-op when no `tool-bash` endpoint is configured.
    pub(crate) fn install_bash_net_resolver(&self, resolver: BashNetResolver) {
        if let Some(lazy) = self.lazy_bash.as_ref() {
            *lazy.resolver.write().expect("resolver lock poisoned") = resolver;
        }
    }

    /// Cancel each tool server session, draining its child process.
    pub async fn shutdown(self) {
        for s in self.eager_servers {
            if let Err(e) = s.service.cancel().await {
                tracing::warn!("error closing tool server {}: {e}", s.label);
            }
        }
        if let Some(lazy) = self.lazy_bash {
            let mut guard = lazy.active.lock().await;
            if let Some(active) = guard.take()
                && let Err(e) = active.service.cancel().await
            {
                tracing::warn!("error closing lazy tool-bash {}: {e}", active.label);
            }
        }
    }
}

impl LazyBash {
    /// Resolve `allow_net` for this call, (re)spawn the bash server if
    /// needed, and dispatch the tool call to it.
    async fn dispatch(
        &self,
        name: &str,
        args: serde_json::Map<String, Value>,
        net_override: Option<bool>,
    ) -> ToolCallOutcome {
        // Step 1: resolve the per-call allow_net via the host-supplied
        // resolver. This may emit a prompt and block until the user
        // answers — hence why we run it before taking the active-server
        // lock. We snapshot the current resolver under a brief read lock,
        // then drop the lock before awaiting so the host can swap the
        // resolver freely.
        let resolver = self.resolver.read().expect("resolver lock poisoned").clone();
        let allow_net = (resolver)(net_override).await;

        // Step 2: lock the active server slot so we get a single
        // ordering for the (reuse-or-respawn → dispatch) sequence.
        let mut guard = self.active.lock().await;
        let must_respawn = match guard.as_ref() {
            None => true,
            Some(active) => active.allow_net != allow_net,
        };

        if must_respawn {
            // Kill the existing process (if any) before spawning a new one.
            if let Some(prev) = guard.take()
                && let Err(e) = prev.service.cancel().await
            {
                tracing::warn!(
                    "lazy tool-bash respawn: error cancelling previous server {}: {e}",
                    prev.label
                );
            }

            let cmd = build_bash_command(
                &self.config.command,
                &self.config.args,
                &self.config.project_root,
                &self.config.sandbox_template,
                allow_net,
            );
            let label = self.config.command.display().to_string();
            let transport = match TokioChildProcess::new(cmd) {
                Ok(t) => t,
                Err(e) => {
                    return ToolCallOutcome::error(format!(
                        "spawn tool-bash ({label}, allow_net={allow_net}): {e}"
                    ));
                }
            };
            let service = match ().serve(transport).await {
                Ok(s) => s,
                Err(e) => {
                    return ToolCallOutcome::error(format!(
                        "init MCP session with tool-bash ({label}, allow_net={allow_net}): {e}"
                    ));
                }
            };
            *guard = Some(ActiveBashServer {
                label,
                service,
                allow_net,
            });
            tracing::debug!(
                "lazy tool-bash: (re)spawned with allow_net={allow_net}"
            );
        }

        // Step 3: dispatch the call. `guard.as_ref().unwrap()` is safe — we
        // just populated it above (or confirmed an existing entry).
        let active = guard.as_ref().expect("active bash server present");
        let params = CallToolRequestParams::new(name.to_string()).with_arguments(args);
        match active.service.call_tool(params).await {
            Ok(result) => {
                let is_error = result.is_error == Some(true);
                let payload = render_result_payload(&result);
                if is_error {
                    ToolCallOutcome::error(payload)
                } else {
                    ToolCallOutcome::success(payload)
                }
            }
            Err(e) => ToolCallOutcome::error(format!(
                "tool transport error on {name}: {e}"
            )),
        }
    }
}

/// Build a sandboxed `tokio::process::Command` for tool-bash with the
/// given `allow_net`. The injected per-spawn `tool_overrides[tool-bash]
/// .allow_net = Some(allow_net)` wins over the built-in default-deny
/// fallback in `SandboxConfig::net_allowed_for`.
fn build_bash_command(
    command: &Path,
    args: &[String],
    project_root: &Path,
    sandbox_template: &SandboxConfig,
    allow_net: bool,
) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args);
    cmd.env("SAVVAGENT_TOOL_FS_ROOT", project_root);
    cmd.env("SAVVAGENT_TOOL_BASH_ROOT", project_root);
    cmd.env("SAVVAGENT_TOOL_GREP_ROOT", project_root);

    // Clone the template and inject the per-spawn allow_net override. We
    // merge with any user-pinned `[tool_overrides.tool-bash]` entry —
    // user pin wins (matches the "user config overrides host runtime"
    // invariant). We only inject ours when the user did NOT pin one.
    let mut sandbox = sandbox_template.clone();
    let needs_inject = sandbox
        .tool_overrides
        .get(TOOL_BASH_MARKER)
        .map(|ov| ov.allow_net.is_none())
        .unwrap_or(true);
    if needs_inject {
        sandbox
            .tool_overrides
            .entry(TOOL_BASH_MARKER.to_string())
            .or_default()
            .allow_net = Some(allow_net);
    }

    let wrapper = apply_sandbox(&mut cmd, command, project_root, &sandbox);
    let label = command.display().to_string();
    match wrapper {
        SandboxWrapper::None => {}
        SandboxWrapper::Bwrap => {
            tracing::info!("sandbox[bwrap]: {label} (allow_net={allow_net})");
        }
        SandboxWrapper::SandboxExec => {
            tracing::info!("sandbox[sandbox-exec]: {label} (allow_net={allow_net})");
        }
    }
    cmd
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod lazy_bash_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Resolver that returns a fixed `allow_net` and counts invocations.
    fn fixed_resolver(value: bool, counter: Arc<AtomicUsize>) -> BashNetResolver {
        Arc::new(move |over: Option<bool>| {
            counter.fetch_add(1, Ordering::SeqCst);
            let v = over.unwrap_or(value);
            Box::pin(async move { v })
        })
    }

    /// A degenerate `LazyBash` whose `dispatch` never actually spawns —
    /// it intercepts the spawn step so we can verify the spawn/respawn
    /// counting logic without requiring a real tool-bash binary on disk.
    ///
    /// We do this by hand-rolling a thin wrapper around the spawn
    /// decision. The real `LazyBash::dispatch` is exercised end-to-end
    /// in the integration test in `session.rs`.
    struct CountingBash {
        resolver: BashNetResolver,
        active: Mutex<Option<bool>>,
        spawn_count: AtomicUsize,
    }

    impl CountingBash {
        async fn dispatch(&self, net_override: Option<bool>) -> bool {
            let resolver = self.resolver.clone();
            let allow_net = (resolver)(net_override).await;
            let mut guard = self.active.lock().await;
            let must_respawn = match guard.as_ref() {
                None => true,
                Some(prev) => *prev != allow_net,
            };
            if must_respawn {
                self.spawn_count.fetch_add(1, Ordering::SeqCst);
                *guard = Some(allow_net);
            }
            allow_net
        }
    }

    #[tokio::test]
    async fn two_calls_same_allow_net_spawn_once() {
        let counter = Arc::new(AtomicUsize::new(0));
        let bash = CountingBash {
            resolver: fixed_resolver(true, counter.clone()),
            active: Mutex::new(None),
            spawn_count: AtomicUsize::new(0),
        };

        assert!(bash.dispatch(None).await);
        assert!(bash.dispatch(None).await);

        assert_eq!(
            bash.spawn_count.load(Ordering::SeqCst),
            1,
            "two calls with the same resolved allow_net must reuse the cached spawn"
        );
    }

    #[tokio::test]
    async fn override_flip_respawns() {
        let counter = Arc::new(AtomicUsize::new(0));
        let bash = CountingBash {
            // Resolver default is `true`; per-call override(false) flips it.
            resolver: fixed_resolver(true, counter.clone()),
            active: Mutex::new(None),
            spawn_count: AtomicUsize::new(0),
        };

        // Call 1: override = Some(false) → spawn with allow_net=false.
        assert!(!bash.dispatch(Some(false)).await);
        // Call 2: no override → resolver returns true → respawn.
        assert!(bash.dispatch(None).await);

        assert_eq!(
            bash.spawn_count.load(Ordering::SeqCst),
            2,
            "flipping allow_net between calls must force a respawn"
        );
    }

    #[tokio::test]
    async fn override_matches_cached_no_respawn() {
        let counter = Arc::new(AtomicUsize::new(0));
        let bash = CountingBash {
            resolver: fixed_resolver(true, counter.clone()),
            active: Mutex::new(None),
            spawn_count: AtomicUsize::new(0),
        };

        // Call 1: no override → resolver true → spawn(true).
        assert!(bash.dispatch(None).await);
        // Call 2: override=Some(true) → matches active → reuse.
        assert!(bash.dispatch(Some(true)).await);

        assert_eq!(
            bash.spawn_count.load(Ordering::SeqCst),
            1,
            "matching override should reuse the cached spawn"
        );
    }
}
