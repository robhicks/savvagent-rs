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
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use async_trait::async_trait;
use rmcp::{
    RoleClient, ServiceExt,
    model::CallToolRequestParams,
    service::{RunningService, ServiceError},
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

/// Per-call override of `tool-bash`'s network access. Replaces the older
/// `Option<bool>` plumbing so the three states have names and the
/// "explicit override short-circuits the cache" semantics are structural
/// rather than implicit in callers.
///
/// | Variant      | Meaning                                                 |
/// |--------------|---------------------------------------------------------|
/// | `Inherit`    | Defer to the resolver's policy (may park on a prompt).  |
/// | `ForceAllow` | Grant network access regardless of policy or cache.     |
/// | `ForceDeny`  | Deny network access regardless of policy or cache.      |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetOverride {
    /// Defer to the resolver's policy. The resolver may emit a
    /// [`crate::session::TurnEvent::BashNetworkRequested`] prompt.
    #[default]
    Inherit,
    /// Bypass the resolver and grant network access for this call's spawn.
    ForceAllow,
    /// Bypass the resolver and deny network access for this call's spawn.
    ForceDeny,
}

/// Resolver invoked by [`ToolRegistry::call_with_bash_net_override`] when a
/// bash dispatch needs to know what `allow_net` to spawn with.
///
/// The trait's required method, [`resolve_policy`], is the no-override case
/// — implementations consult their permission state and may park on a user
/// prompt. The default [`resolve`] method short-circuits explicit
/// [`NetOverride::ForceAllow`] / [`NetOverride::ForceDeny`] without
/// touching the session cache; only [`NetOverride::Inherit`] reaches
/// [`resolve_policy`]. Callers should always invoke [`resolve`] — the
/// short-circuit logic stays in one place.
///
/// [`resolve_policy`]: BashNetResolver::resolve_policy
/// [`resolve`]: BashNetResolver::resolve
#[async_trait]
pub trait BashNetResolver: Send + Sync + 'static {
    /// Resolve the policy-level `allow_net`. May park on a user prompt.
    async fn resolve_policy(&self) -> bool;

    /// Resolve with override consideration. The default implementation
    /// short-circuits explicit overrides; `Inherit` defers to
    /// [`resolve_policy`]. Override only if you need different
    /// short-circuit semantics — most callers should not.
    ///
    /// [`resolve_policy`]: BashNetResolver::resolve_policy
    async fn resolve(&self, over: NetOverride) -> bool {
        match over {
            NetOverride::ForceAllow => true,
            NetOverride::ForceDeny => false,
            NetOverride::Inherit => self.resolve_policy().await,
        }
    }
}

/// Shorthand for the trait-object handle the registry stores. Held behind
/// an `RwLock` so the host can swap in the real resolver after construction
/// (the temporary one used at `connect` time defers all decisions to a
/// hard-coded false).
pub(crate) type BashNetResolverHandle = Arc<dyn BashNetResolver>;

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
    resolver: Arc<RwLock<BashNetResolverHandle>>,
    /// Currently-active spawned server, if any.
    ///
    /// The lock guards only the (reuse-or-respawn → dispatch) sequence
    /// — the resolver runs unlocked (it may park on a user prompt, and
    /// we don't want to serialize that across all bash dispatches).
    ///
    /// Today's flow is serial-by-construction at the TUI layer
    /// (`app.is_loading` gate prevents concurrent `/bash` invocations
    /// and model-driven calls run sequentially through the turn loop),
    /// so concurrent dispatches don't race in practice. If a future
    /// caller breaks that, both could park on the same prompt's
    /// `oneshot`.
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

/// Spawn-determining parameters of an active `tool-bash` child. Two
/// `BashSpawnKey`s compare equal iff the cached server is suitable for
/// the new call without a respawn. Today the only spawn-determining
/// parameter is `allow_net`; v0.9's domain-allowlist work will extend
/// the key with an `allowed_domains: Vec<String>` field, at which point
/// every existing site that asks "does the cache still satisfy this
/// call?" already routes through the key's `==` and gets the new field
/// for free.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BashSpawnKey {
    allow_net: bool,
}

/// An active, lazily-spawned `tool-bash` child plus the [`BashSpawnKey`]
/// it was spawned with. Killed (via `service.cancel()`) before a respawn
/// or at registry shutdown.
struct ActiveBashServer {
    label: String,
    service: RunningService<RoleClient, ()>,
    spawn_key: BashSpawnKey,
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
        bash_net_resolver: BashNetResolverHandle,
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
                                anyhow::bail!("duplicate tool `{name}` advertised by {label}");
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
                            tracing::warn!("tool-bash probe shutdown error (ignored): {e}");
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
                        let allow_net = sandbox.net_allowed_for(command);
                        log_sandbox_wrapper(&label, &wrapper, allow_net);

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
                                anyhow::bail!("duplicate tool `{name}` advertised by {label}");
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

    /// Call `name` with a per-call bash network override.
    ///
    /// For non-bash tools, `net_override` is ignored. For bash tools, the
    /// override is passed to the resolver and to the spawn logic; see
    /// [`LazyBash`] for the spawn-vs-reuse decision.
    pub async fn call_with_bash_net_override(
        &self,
        name: &str,
        input: Value,
        net_override: NetOverride,
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
        ToolCallOutcome::from_call_result(name, server.service.call_tool(params).await)
    }

    /// Replace the bash network resolver. Used by [`crate::session::Host`]
    /// after construction so the resolver can capture `Arc`-shared
    /// handles to the host's permission state and emit
    /// [`crate::session::TurnEvent::BashNetworkRequested`].
    ///
    /// No-op when no `tool-bash` endpoint is configured.
    pub(crate) fn install_bash_net_resolver(&self, resolver: BashNetResolverHandle) {
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
        net_override: NetOverride,
    ) -> ToolCallOutcome {
        // Step 1: resolve the per-call allow_net via the host-supplied
        // resolver. This may emit a prompt and block until the user
        // answers — hence why we run it before taking the active-server
        // lock. We snapshot the current resolver under a brief read lock,
        // then drop the lock before awaiting so the host can swap the
        // resolver freely. The trait's default `resolve` impl handles the
        // explicit-override short-circuit, so we don't need to repeat that
        // logic here.
        let resolver = self
            .resolver
            .read()
            .expect("resolver lock poisoned")
            .clone();
        let allow_net = resolver.resolve(net_override).await;
        let new_key = BashSpawnKey { allow_net };

        // Step 2: lock the active server slot so we get a single
        // ordering for the (reuse-or-respawn → dispatch) sequence.
        let mut guard = self.active.lock().await;
        let cached_key = guard.as_ref().map(|a| a.spawn_key.clone());
        let must_respawn = cached_key.as_ref() != Some(&new_key);

        if must_respawn {
            if let Some(prev) = &cached_key {
                tracing::info!(
                    "tool-bash: spawn key changed ({:?} -> {new_key:?}); respawning",
                    prev
                );
            }

            // Blue-green respawn: spawn the new server FIRST. If it
            // fails we keep the old one — the alternative (kill first,
            // then fail to spawn) would leave the slot empty and force
            // every subsequent call to attempt a fresh spawn from cold.
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
            let new_service = match ().serve(transport).await {
                Ok(s) => s,
                Err(e) => {
                    return ToolCallOutcome::error(format!(
                        "init MCP session with tool-bash ({label}, allow_net={allow_net}): {e}"
                    ));
                }
            };

            // New server is up. Now we can safely retire the old one.
            if let Some(prev) = guard.take()
                && let Err(e) = prev.service.cancel().await
            {
                tracing::warn!(
                    "lazy tool-bash respawn: error cancelling previous server {} \
                     ({:?}): {e} (ignored — new server already up)",
                    prev.label,
                    prev.spawn_key,
                );
            }

            *guard = Some(ActiveBashServer {
                label,
                service: new_service,
                spawn_key: new_key,
            });
            tracing::debug!("lazy tool-bash: (re)spawned with allow_net={allow_net}");
        }

        // Step 3: dispatch the call. `guard.as_ref().unwrap()` is safe — we
        // just populated it above (or confirmed an existing entry).
        let active = guard.as_ref().expect("active bash server present");
        let params = CallToolRequestParams::new(name.to_string()).with_arguments(args);
        ToolCallOutcome::from_call_result(name, active.service.call_tool(params).await)
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
    log_sandbox_wrapper(&label, &wrapper, allow_net);
    cmd
}

/// Log the resolved sandbox wrapper for a freshly built tool command.
/// Single source of truth for the eager and bash spawn paths so the log
/// format stays consistent.
fn log_sandbox_wrapper(label: &str, wrapper: &SandboxWrapper, allow_net: bool) {
    match wrapper {
        SandboxWrapper::None => {}
        SandboxWrapper::Bwrap => {
            tracing::info!("sandbox[bwrap]: {label} (allow_net={allow_net})");
        }
        SandboxWrapper::SandboxExec => {
            tracing::info!("sandbox[sandbox-exec]: {label} (allow_net={allow_net})");
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

    /// Normalize the outcome of `RunningService::call_tool` into a
    /// [`ToolCallOutcome`]. Shared by the eager and lazy bash dispatch
    /// paths — `name` is the tool name, used for the transport-error
    /// message only.
    pub(crate) fn from_call_result(
        name: &str,
        result: std::result::Result<rmcp::model::CallToolResult, ServiceError>,
    ) -> Self {
        match result {
            Ok(r) => {
                let payload = render_result_payload(&r);
                if r.is_error == Some(true) {
                    Self::error(payload)
                } else {
                    Self::success(payload)
                }
            }
            Err(e) => Self::error(format!("tool transport error on {name}: {e}")),
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

    /// Resolver that returns a fixed `allow_net` policy and counts invocations.
    /// The trait's default `resolve` short-circuits explicit overrides, so
    /// `resolve_policy` only runs when the caller passes `NetOverride::Inherit`.
    struct FixedResolver {
        policy: bool,
        invocations: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl BashNetResolver for FixedResolver {
        async fn resolve_policy(&self) -> bool {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            self.policy
        }
    }

    fn fixed_resolver(policy: bool, counter: Arc<AtomicUsize>) -> BashNetResolverHandle {
        Arc::new(FixedResolver {
            policy,
            invocations: counter,
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
        resolver: BashNetResolverHandle,
        active: Mutex<Option<BashSpawnKey>>,
        spawn_count: AtomicUsize,
    }

    impl CountingBash {
        async fn dispatch(&self, net_override: NetOverride) -> bool {
            let resolver = self.resolver.clone();
            let allow_net = resolver.resolve(net_override).await;
            let new_key = BashSpawnKey { allow_net };
            let mut guard = self.active.lock().await;
            let must_respawn = guard.as_ref() != Some(&new_key);
            if must_respawn {
                self.spawn_count.fetch_add(1, Ordering::SeqCst);
                *guard = Some(new_key);
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

        assert!(bash.dispatch(NetOverride::Inherit).await);
        assert!(bash.dispatch(NetOverride::Inherit).await);

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
            // Resolver policy is `true`; per-call ForceDeny flips it.
            resolver: fixed_resolver(true, counter.clone()),
            active: Mutex::new(None),
            spawn_count: AtomicUsize::new(0),
        };

        // Call 1: ForceDeny → spawn with allow_net=false.
        assert!(!bash.dispatch(NetOverride::ForceDeny).await);
        // Call 2: Inherit → resolver returns true → respawn.
        assert!(bash.dispatch(NetOverride::Inherit).await);

        assert_eq!(
            bash.spawn_count.load(Ordering::SeqCst),
            2,
            "flipping allow_net between calls must force a respawn"
        );
    }

    #[tokio::test]
    async fn force_allow_short_circuits_policy() {
        let counter = Arc::new(AtomicUsize::new(0));
        let bash = CountingBash {
            // Resolver policy is `false` — but ForceAllow never asks.
            resolver: fixed_resolver(false, counter.clone()),
            active: Mutex::new(None),
            spawn_count: AtomicUsize::new(0),
        };

        assert!(bash.dispatch(NetOverride::ForceAllow).await);

        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "ForceAllow must NOT call resolve_policy — the override short-circuits"
        );
    }

    #[tokio::test]
    async fn force_deny_short_circuits_policy() {
        let counter = Arc::new(AtomicUsize::new(0));
        let bash = CountingBash {
            resolver: fixed_resolver(true, counter.clone()),
            active: Mutex::new(None),
            spawn_count: AtomicUsize::new(0),
        };

        assert!(!bash.dispatch(NetOverride::ForceDeny).await);

        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "ForceDeny must NOT call resolve_policy — the override short-circuits"
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

        // Call 1: Inherit → resolver true → spawn(true).
        assert!(bash.dispatch(NetOverride::Inherit).await);
        // Call 2: ForceAllow → matches active → reuse.
        assert!(bash.dispatch(NetOverride::ForceAllow).await);

        assert_eq!(
            bash.spawn_count.load(Ordering::SeqCst),
            1,
            "matching override should reuse the cached spawn"
        );
    }
}
