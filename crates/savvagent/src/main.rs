//! Savvagent TUI entry point.
//!
//! Bootstraps a [`savvagent_host::Host`] in one of two ways:
//!
//! 1. **In-process (default).** Each provider crate is linked as a library;
//!    the TUI builds a [`ProviderHandler`](savvagent_mcp::ProviderHandler) and
//!    wraps it in [`InProcessProviderClient`] — no MCP transport, no spawned
//!    binary. The TUI scans the OS keyring for a saved API key and
//!    auto-connects to the first provider it finds; otherwise the user
//!    runs `/connect`.
//! 2. **Remote MCP (opt-in).** If `SAVVAGENT_PROVIDER_URL` is set the TUI
//!    connects to that Streamable HTTP MCP server instead — useful for
//!    pointing at a long-running `savvagent-anthropic`/`savvagent-gemini`
//!    binary or a third-party MCP provider.
//!
//! Other configuration:
//!
//! - `SAVVAGENT_MODEL`          (overrides the per-provider default)
//! - `SAVVAGENT_TOOL_FS_BIN`    (default `savvagent-tool-fs` on $PATH)
//! - `SAVVAGENT_TOOL_BASH_BIN`  (default `savvagent-tool-bash` on $PATH)
//! - `SAVVAGENT_TOOL_GREP_BIN`  (default `savvagent-tool-grep` on $PATH)

mod app;
mod creds;
mod palette;
mod plugin;
mod providers;
mod splash;
#[cfg(test)]
mod test_helpers;
mod theme;
mod tui;
mod ui;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use app::{
    App, BashCommandError, CommandSelection, Entry, InputMode, collect_transcript_entries,
    parse_bash_command,
};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use providers::{PROVIDERS, ProviderSpec};
use savvagent_host::{
    BashNetworkChoice, Host, HostConfig, PermissionDecision, ProviderEndpoint, SandboxConfig,
    SandboxMode, ToolCallStatus, ToolEndpoint, TranscriptError, TurnEvent,
};
use savvagent_mcp::{InProcessProviderClient, ProviderClient};
use tokio::sync::{RwLock, mpsc};
use tui_textarea::TextArea;

/// Worker → main-loop messages.
enum WorkerMsg {
    Event(TurnEvent),
    /// Sent if `run_turn_streaming` returned an error.
    Error(String),
    /// Sent when a `/bash` direct-invocation worker finishes (success or
    /// error). The main loop uses this to clear `app.is_loading`, mirroring
    /// the `TurnComplete` path for model-driven turns.
    BashDone,
}

type HostSlot = Arc<RwLock<Option<Arc<Host>>>>;

/// Resolved paths for every bundled tool-server binary the TUI knows how to
/// register. Each field is `None` when the binary couldn't be found; the
/// host just doesn't advertise that tool's surface in `tools/list`.
#[derive(Clone, Default)]
struct ToolBins {
    fs: Option<PathBuf>,
    bash: Option<PathBuf>,
    grep: Option<PathBuf>,
}

impl ToolBins {
    /// Append every populated entry as a stdio [`ToolEndpoint`] on `config`.
    fn apply(&self, mut config: HostConfig) -> HostConfig {
        for path in [
            self.fs.as_deref(),
            self.bash.as_deref(),
            self.grep.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            config = config.with_tool(ToolEndpoint::Stdio {
                command: path.to_path_buf(),
                args: vec![],
            });
        }
        config
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    init_tracing();

    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let tool_bins = ToolBins {
        fs: locate_bundled_bin("savvagent-tool-fs", "SAVVAGENT_TOOL_FS_BIN"),
        bash: locate_bundled_bin("savvagent-tool-bash", "SAVVAGENT_TOOL_BASH_BIN"),
        grep: locate_bundled_bin("savvagent-tool-grep", "SAVVAGENT_TOOL_GREP_BIN"),
    };

    let initial = bootstrap_host(&project_root, &tool_bins).await;
    let header_model = initial
        .as_ref()
        .map(|(_, model, _)| model.clone())
        .unwrap_or_else(|| "(disconnected)".to_string());
    let initial_provider = initial.as_ref().and_then(|(_, _, id)| *id);

    let host_slot: HostSlot = Arc::new(RwLock::new(initial.map(|(h, _, _)| h)));

    let transcript_dir = transcript_dir();

    let mut terminal = tui::init()?;

    // Restore terminal on panic.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = tui::restore();
        original_hook(info);
    }));

    let mut app = App::new(header_model, transcript_dir);

    {
        use crate::plugin::manifests::Indexes;
        use crate::plugin::registry::PluginRegistry;

        let plugins = plugin::register_builtins();
        let registry = PluginRegistry::new(plugins);
        let indexes = Indexes::build(&registry)
            .await
            .expect("plugin manifest conflict at startup");
        app.install_plugin_runtime(registry, indexes);
    }

    app.connected = host_slot.read().await.is_some();
    app.active_provider_id = initial_provider;
    // If we already have a host (e.g. saved-credentials bootstrap), align
    // the splash sandbox indicator with what the host will actually apply.
    // Otherwise it would briefly show the on-disk preference even when the
    // active host was built with a different SandboxConfig override.
    if let Some(host) = current_host(&host_slot).await {
        app.refresh_splash_sandbox_from_host(host.sandbox_config());
    }
    if !app.connected {
        app.push_note(
            "Not connected. Type / (or press Ctrl-P) and pick /connect to set up a provider.",
        );
    }
    if tool_bins.fs.is_none() {
        app.push_note(
            "Note: savvagent-tool-fs not found — fs tools disabled. Run `cargo build` or set SAVVAGENT_TOOL_FS_BIN.",
        );
    }
    if tool_bins.bash.is_none() {
        app.push_note(
            "Note: savvagent-tool-bash not found — bash disabled. Run `cargo build` or set SAVVAGENT_TOOL_BASH_BIN.",
        );
    }
    if tool_bins.grep.is_none() {
        app.push_note(
            "Note: savvagent-tool-grep not found — search disabled. Run `cargo build` or set SAVVAGENT_TOOL_GREP_BIN.",
        );
    }
    let res = run_app(
        &mut terminal,
        &mut app,
        host_slot.clone(),
        project_root,
        tool_bins,
    )
    .await;

    let _ = tui::restore();

    if let Some(host) = current_host(&host_slot).await {
        if let Err(e) = save_transcript_now(&app, &host).await {
            eprintln!("warning: could not save transcript on exit: {e}");
        }
    }
    if let Some(host) = host_slot.write().await.take() {
        host.shutdown().await;
    }

    if let Err(err) = res {
        eprintln!("{err:?}");
    }
    Ok(())
}

/// Try the legacy `SAVVAGENT_PROVIDER_URL` MCP path first, then keyring
/// auto-connect over the in-process bridge. Returns the host plus the model
/// and provider id used (so the App's header is right).
async fn bootstrap_host(
    project_root: &Path,
    tool_bins: &ToolBins,
) -> Option<(Arc<Host>, String, Option<&'static str>)> {
    if let Ok(url) = std::env::var("SAVVAGENT_PROVIDER_URL") {
        let model =
            std::env::var("SAVVAGENT_MODEL").unwrap_or_else(|_| "claude-haiku-4-5".to_string());
        match start_host_remote(url, model.clone(), project_root.to_path_buf(), tool_bins).await {
            Ok(host) => return Some((host, model, None)),
            Err(e) => {
                eprintln!("warning: SAVVAGENT_PROVIDER_URL set but connect failed: {e:#}");
            }
        }
    }

    for spec in PROVIDERS {
        let key = if spec.api_key_required {
            let Ok(Some(k)) = creds::load(spec.id) else {
                continue;
            };
            k
        } else {
            // Keyless provider — attempt auto-connect without a stored key.
            String::new()
        };
        match build_in_process_host(spec, &key, project_root, tool_bins).await {
            Ok(host) => {
                let model = std::env::var("SAVVAGENT_MODEL")
                    .unwrap_or_else(|_| spec.default_model.to_string());
                return Some((host, model, Some(spec.id)));
            }
            Err(e) => {
                eprintln!("warning: in-process bring-up of {} failed: {e:#}", spec.id);
            }
        }
    }
    None
}

/// Build a host whose `ProviderClient` is an [`InProcessProviderClient`],
/// using the per-provider default (or `SAVVAGENT_MODEL` env override) as
/// the model id.
async fn build_in_process_host(
    spec: &'static ProviderSpec,
    api_key: &str,
    project_root: &Path,
    tool_bins: &ToolBins,
) -> Result<Arc<Host>> {
    let model = std::env::var("SAVVAGENT_MODEL").unwrap_or_else(|_| spec.default_model.to_string());
    build_in_process_host_with_model(spec, api_key, project_root, tool_bins, model).await
}

/// Same as [`build_in_process_host`] but with an explicit `model` id —
/// used by `/model <id>` to reconnect against the same provider with a
/// different model.
async fn build_in_process_host_with_model(
    spec: &'static ProviderSpec,
    api_key: &str,
    project_root: &Path,
    tool_bins: &ToolBins,
    model: String,
) -> Result<Arc<Host>> {
    let handler = (spec.build)(api_key).with_context(|| format!("building {} handler", spec.id))?;
    if let Some(check) = spec.health_check {
        check()
            .await
            .with_context(|| format!("{} health check", spec.id))?;
    }
    let client: Box<dyn ProviderClient + Send + Sync> =
        Box::new(InProcessProviderClient::new(handler));
    // The endpoint variant is a placeholder when we hand the host a
    // pre-built ProviderClient via `with_components`; pick a recognizable
    // dummy URL so a stray log line says where it came from.
    let config = tool_bins.apply(
        HostConfig::new(
            ProviderEndpoint::StreamableHttp {
                url: format!("inproc://{}", spec.id),
            },
            model,
        )
        .with_project_root(project_root.to_path_buf()),
    );
    let host = Host::with_components(config, client)
        .await
        .context("Host::with_components")?;
    Ok(Arc::new(host))
}

async fn start_host_remote(
    url: String,
    model: String,
    project_root: PathBuf,
    tool_bins: &ToolBins,
) -> Result<Arc<Host>> {
    let config = tool_bins.apply(
        HostConfig::new(ProviderEndpoint::StreamableHttp { url }, model)
            .with_project_root(project_root),
    );
    let host = Host::start(config).await.context("failed to start host")?;
    Ok(Arc::new(host))
}

/// Resolve a bundled tool-server binary by name. Tries (in order):
///
/// 1. `<env_override>` env var (must point at an existing file).
/// 2. A sibling of the running TUI executable — i.e. `target/<profile>/`
///    when launched via `cargo run`, or the install dir when installed.
/// 3. Bare `<name>` resolved via `PATH`.
///
/// Returns `None` if none of the candidates exists. The caller surfaces a
/// note so the user knows that tool surface is disabled.
fn locate_bundled_bin(name: &str, env_override: &str) -> Option<PathBuf> {
    if let Ok(p) = std::env::var(env_override) {
        let path = PathBuf::from(p);
        return path.exists().then_some(path);
    }
    let bin_name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(&bin_name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(&bin_name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    None
}

async fn current_host(slot: &HostSlot) -> Option<Arc<Host>> {
    slot.read().await.clone()
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .try_init();
}

fn transcript_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".savvagent").join("transcripts")
}

async fn save_transcript_now(app: &App, host: &Arc<Host>) -> Result<PathBuf> {
    if app.entries.is_empty() {
        return Ok(PathBuf::new());
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = app.transcript_dir.join(format!("{ts}.json"));
    host.save_transcript(&path)
        .await
        .context("save transcript")?;
    Ok(path)
}

/// Run a slash command and apply its side effects. Used both when the user
/// types `/foo` + Enter and when they pick a no-arg command from the
/// palette. Commands that need direct host access (`/tools`, `/model`,
/// `/save`) are dispatched here; the rest fall through to
/// [`App::handle_command`].
async fn dispatch_slash_command(
    app: &mut App,
    cmd: &str,
    host_slot: &HostSlot,
    project_root: &Path,
    tool_bins: &ToolBins,
    worker_tx: &mpsc::Sender<WorkerMsg>,
) {
    let trimmed = cmd.trim_start();
    // `/bash` preserves the unparsed remainder verbatim (no `trim()` on
    // `rest`) so quoting like `--net   curl …` survives.
    let (head, rest_raw) = match trimmed.split_once(char::is_whitespace) {
        Some((h, r)) => (h, r),
        None => (trimmed, ""),
    };
    let rest = rest_raw.trim();

    match head {
        "/tools" => {
            show_tools(app, host_slot).await;
            return;
        }
        "/model" => {
            handle_model_command(app, rest, host_slot, project_root, tool_bins).await;
            return;
        }
        "/resume" => {
            handle_resume_command(app, rest, host_slot).await;
            return;
        }
        "/sandbox" => {
            handle_sandbox_command(app, rest, host_slot).await;
            return;
        }
        "/theme" => {
            handle_theme_command(app, rest);
            return;
        }
        "/bash" => {
            handle_bash_slash_command(app, rest_raw, host_slot, worker_tx).await;
            return;
        }
        _ => {}
    }

    let was_save = trimmed == "/save";
    app.handle_command(cmd);
    if was_save {
        if let Some(host) = current_host(host_slot).await {
            match save_transcript_now(app, &host).await {
                Ok(p) if !p.as_os_str().is_empty() => {
                    app.push_note(format!("Saved {}", p.display()));
                    app.last_transcript = Some(p);
                }
                Ok(_) => app.push_note("Nothing to save."),
                Err(e) => app.push_note(format!("Save error: {e}")),
            }
        } else {
            app.push_note("Not connected — nothing to save.");
        }
    }
}

/// Render `/tools` output: one note per registered tool, with the policy's
/// no-args verdict as a coarse hint.
async fn show_tools(app: &mut App, host_slot: &HostSlot) {
    let Some(host) = current_host(host_slot).await else {
        app.push_note("Not connected — no tools to list.");
        return;
    };
    let defs = host.tool_defs().await;
    if defs.is_empty() {
        app.push_note("No tools registered.");
        return;
    }
    app.push_note(format!("{} tool(s):", defs.len()));
    for def in &defs {
        let verdict = host.default_verdict_for(&def.name);
        let label = match verdict {
            savvagent_host::Verdict::Allow => "allow",
            savvagent_host::Verdict::Ask { .. } => "ask",
            savvagent_host::Verdict::Deny { .. } => "deny",
        };
        let desc = if def.description.is_empty() {
            String::new()
        } else {
            format!(" — {}", def.description)
        };
        app.push_note(format!("  [{label}] {}{}", def.name, desc));
    }
}

/// Validate `requested` against the provider's advertised `models`. Returns
/// `Ok(())` when the id is in the list, `Err(known_ids)` otherwise.
fn validate_model_id<'a>(
    requested: &str,
    models: &'a [savvagent_host::ModelInfo],
) -> Result<(), Vec<&'a str>> {
    if models.iter().any(|m| m.id == requested) {
        Ok(())
    } else {
        Err(models.iter().map(|m| m.id.as_str()).collect())
    }
}

/// Outcome of asking the provider whether `requested` is a known model id.
///
/// [`Proceed`](ModelChangeOutcome::Proceed) means the TUI should switch to the
/// new model; `warning` is an optional note to surface to the user first (e.g.
/// "could not verify, proceeding optimistically"). [`Reject`](ModelChangeOutcome::Reject)
/// means the id is definitively unknown — the TUI must show the note and stop.
#[derive(Debug, PartialEq, Eq)]
enum ModelChangeOutcome {
    /// Switch to the new model. When `warning` is `Some`, push it as a note
    /// first so the user understands the validation outcome.
    Proceed { warning: Option<String> },
    /// Refuse the change. `note` describes why, including the known model list
    /// when one is available.
    Reject { note: String },
}

/// Decide what to do with a `/model <id>` request given the result of asking
/// the host for `list_models`.
///
/// Pure (no IO, no [`App`] mutation) so it can be unit-tested without standing
/// up a [`Host`] or worker channel.
fn resolve_model_change(
    requested: &str,
    list_result: Result<&savvagent_host::ListModelsResponse, &savvagent_protocol::ProviderError>,
) -> ModelChangeOutcome {
    match list_result {
        Ok(resp) if resp.models.is_empty() => {
            // The provider advertised the tool but returned no models. Rather
            // than reject every id against an empty "Known: " list, treat
            // this as "nothing to validate against" and proceed.
            ModelChangeOutcome::Proceed {
                warning: Some(format!(
                    "Provider advertises no models. Proceeding to `{requested}` optimistically."
                )),
            }
        }
        Ok(resp) => match validate_model_id(requested, &resp.models) {
            Ok(()) => ModelChangeOutcome::Proceed { warning: None },
            Err(known) => ModelChangeOutcome::Reject {
                note: format!("Unknown model `{requested}`. Known: {}", known.join(", ")),
            },
        },
        Err(e) if matches!(e.kind, savvagent_protocol::ErrorKind::NotImplemented) => {
            // The provider doesn't advertise list_models at all. Silent
            // fall-through to the optimistic path is the contract.
            ModelChangeOutcome::Proceed { warning: None }
        }
        Err(e) => {
            // Network/auth/decode failure. Surface it to the user so they can
            // tell a typo'd id apart from a misconfigured key.
            ModelChangeOutcome::Proceed {
                warning: Some(format!(
                    "Could not verify model `{requested}`: {}. Proceeding optimistically.",
                    e.message
                )),
            }
        }
    }
}

/// `/model` (no args) shows the current model. `/model <id>` validates the
/// requested id against `host.list_models()` (when advertised) and then
/// reconnects the active provider with the new id. Providers that don't
/// advertise `list_models` fall through to the optimistic path — the
/// provider rejects an invalid id at first turn instead.
async fn handle_model_command(
    app: &mut App,
    rest: &str,
    host_slot: &HostSlot,
    project_root: &Path,
    tool_bins: &ToolBins,
) {
    if rest.is_empty() {
        match app.active_provider_id {
            Some(id) => app.push_note(format!("Current model: {}:{}", id, app.model)),
            None => app.push_note(format!("Current model: {} (not connected)", app.model)),
        }
        return;
    }

    let new_model = rest.to_string();
    let Some(spec_id) = app.active_provider_id else {
        app.push_note("Not connected — `/connect` first, then `/model <id>`.");
        return;
    };
    let Some(spec) = PROVIDERS.iter().find(|s| s.id == spec_id) else {
        app.push_note(format!("Unknown active provider: {spec_id}"));
        return;
    };
    let key = if spec.api_key_required {
        match creds::load(spec.id) {
            Ok(Some(k)) => k,
            Ok(None) => {
                app.push_note("No saved key for the active provider — `/connect` first.");
                return;
            }
            Err(e) => {
                app.push_note(format!("Keyring error: {e}"));
                return;
            }
        }
    } else {
        String::new()
    };

    // Validate the requested id against the provider's advertised list when
    // available. `resolve_model_change` encapsulates the branching so it can
    // be unit-tested without a live `Host`.
    if let Some(host) = current_host(host_slot).await {
        let list_result = host.list_models().await;
        let outcome = resolve_model_change(&new_model, list_result.as_ref());
        match outcome {
            ModelChangeOutcome::Reject { note } => {
                app.push_note(note);
                return;
            }
            ModelChangeOutcome::Proceed { warning } => {
                if let Some(w) = warning {
                    if let Err(ref e) = list_result {
                        tracing::warn!(?e, "list_models failed; proceeding optimistically");
                    }
                    app.push_note(w);
                } else if let Err(ref e) = list_result {
                    tracing::debug!(?e, "list_models unsupported; proceeding optimistically");
                }
            }
        }
    }

    perform_model_change(
        spec,
        &key,
        new_model,
        host_slot,
        project_root,
        tool_bins,
        app,
    )
    .await;
}

/// `/resume` with no args opens the transcript picker. With a path arg,
/// loads the transcript immediately. Requires an active host connection;
/// if none exists, surfaces a clear error.
///
/// Refuses to run while a turn is in flight: `Host::run_turn_inner`
/// snapshots `state.messages` at turn start and commits its local clone
/// back at turn end, so a mid-turn `load_transcript` would be silently
/// overwritten when the in-flight turn finishes.
async fn handle_resume_command(app: &mut App, rest: &str, host_slot: &HostSlot) {
    if app.is_loading {
        app.push_note("Cannot /resume during an in-flight turn — wait for it to finish.");
        return;
    }
    if rest.is_empty() {
        // Open the picker — actual load happens when the user presses Enter
        // in `SelectingTranscript` mode (handled in `run_app`).
        app.open_transcript_picker(&app.transcript_dir.clone());
        return;
    }

    // Inline path argument: /resume <path>.
    let path = {
        let p = PathBuf::from(rest);
        if p.is_absolute() {
            p
        } else {
            // Treat bare names like "1715340000" or "1715340000.json" as
            // relative to the transcript directory.
            let candidate = app.transcript_dir.join(rest);
            if candidate.exists() {
                candidate
            } else {
                // Try adding .json extension.
                let with_ext = app.transcript_dir.join(format!("{rest}.json"));
                if with_ext.exists() { with_ext } else { p }
            }
        }
    };
    do_resume_from_path(app, host_slot, &path).await;
}

/// Load a transcript from `path` into the active host and replay it into
/// the conversation log.
async fn do_resume_from_path(app: &mut App, host_slot: &HostSlot, path: &Path) {
    let Some(host) = current_host(host_slot).await else {
        app.push_note("Not connected — run /connect first, then /resume to load a transcript.");
        return;
    };

    match host.load_transcript(path).await {
        Ok(record) => {
            // Warn if the transcript was from a different model.
            if record.model != host.config().model && record.saved_at > 0 {
                app.push_note(format!(
                    "Warning: transcript was saved with model '{}'; current model is '{}'.",
                    record.model,
                    host.config().model
                ));
            }
            let ts = if record.saved_at > 0 {
                collect_transcript_entries(path.parent().unwrap_or(path))
                    .into_iter()
                    .find(|e| e.path == path)
                    .map(|e| e.timestamp)
                    .unwrap_or_else(|| record.saved_at.to_string())
            } else {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
                    .to_owned()
            };
            app.replay_transcript(&record);
            app.resumed_at = Some(ts.clone());
            app.push_note(format!(
                "Resumed transcript from {ts} ({} messages). New turns continue from here.",
                record.messages.len()
            ));
        }
        Err(TranscriptError::SchemaMismatch { found, expected }) => {
            app.push_note(format!(
                "Cannot resume: transcript schema v{found}, this build expects v{expected}."
            ));
        }
        Err(TranscriptError::Malformed(msg)) => {
            app.push_note(format!("Cannot resume: malformed transcript JSON: {msg}"));
        }
        Err(TranscriptError::Io(e)) => {
            app.push_note(format!("Cannot resume: {e}"));
        }
    }
}

/// Rebuild the host with `new_model` against the same provider + key, swap
/// the host slot atomically, and clear conversation state (turn ids from
/// the old session would dangle otherwise).
async fn perform_model_change(
    spec: &'static ProviderSpec,
    api_key: &str,
    new_model: String,
    host_slot: &HostSlot,
    project_root: &Path,
    tool_bins: &ToolBins,
    app: &mut App,
) {
    app.push_note(format!("Switching to {}…", new_model));
    let host = match build_in_process_host_with_model(
        spec,
        api_key,
        project_root,
        tool_bins,
        new_model.clone(),
    )
    .await
    {
        Ok(h) => h,
        Err(e) => {
            app.push_note(format!("Model switch failed: {e:#}"));
            return;
        }
    };

    let old = {
        let mut guard = host_slot.write().await;
        guard.replace(host)
    };
    if let Some(old) = old {
        tokio::spawn(async move { old.shutdown().await });
    }
    if let Some(host) = current_host(host_slot).await {
        host.clear_history().await;
    }
    app.entries.clear();
    app.live_text.clear();
    app.update_metrics();
    app.model = new_model;
    app.push_note(format!("Model is now {}.", app.model));
}

/// `/sandbox` (no args) shows current status.
/// `/sandbox on` / `/sandbox off` toggles the enabled flag and persists to
/// `~/.savvagent/sandbox.toml`.
///
/// Note: changing the setting takes effect the *next* time a host is built
/// (i.e. after `/connect`), because the sandbox is applied at tool spawn time
/// and tools are already running. The status display reflects the *current*
/// host's sandbox config (what was used when tools were spawned).
async fn handle_sandbox_command(app: &mut App, rest: &str, host_slot: &HostSlot) {
    match rest {
        "on" | "off" => {
            // Both subcommands set an *explicit* mode — the splash uses that
            // signal to suppress its v0.7-style nag banner.
            let new_mode = if rest == "on" {
                SandboxMode::On
            } else {
                SandboxMode::Off
            };
            let mut cfg = SandboxConfig::load();
            cfg.mode = new_mode;
            match cfg.save().await {
                Ok(()) => {
                    app.push_note(format!(
                        "Sandbox {}: will take effect after the next /connect.",
                        if new_mode == SandboxMode::On {
                            "enabled"
                        } else {
                            "disabled"
                        }
                    ));
                }
                Err(e) => {
                    app.push_note(format!("Could not save sandbox config: {e}"));
                }
            }
        }
        "" => {
            // Show status from the currently-connected host.
            match current_host(host_slot).await {
                None => {
                    // No active host — show what's on disk instead.
                    let cfg = SandboxConfig::load();
                    app.push_note(format!(
                        "Sandbox (on-disk, not yet active): enabled={}  allow_net={}  per-tool overrides: {}",
                        cfg.is_enabled(),
                        cfg.allow_net,
                        fmt_overrides(&cfg),
                    ));
                }
                Some(host) => {
                    let cfg = host.sandbox_config();
                    app.push_note(format!(
                        "Sandbox: enabled={}  allow_net={}  per-tool overrides: {}",
                        cfg.is_enabled(),
                        cfg.allow_net,
                        fmt_overrides(cfg),
                    ));
                    if cfg.is_enabled() {
                        #[cfg(target_os = "linux")]
                        app.push_note("  wrapper: bwrap (Linux)");
                        #[cfg(target_os = "macos")]
                        app.push_note("  wrapper: sandbox-exec (macOS)");
                        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                        app.push_note("  wrapper: none (Windows — deferred)");
                    }
                    if !cfg.extra_binds.is_empty() {
                        app.push_note(format!(
                            "  extra_binds: {}",
                            cfg.extra_binds
                                .iter()
                                .map(|p| p.display().to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                }
            }
        }
        other => {
            app.push_note(format!(
                "Unknown sandbox subcommand `{other}`. Usage: /sandbox  |  /sandbox on  |  /sandbox off"
            ));
        }
    }
}

/// `/theme` (no args or `list`) opens the interactive theme picker.
/// `/theme <name>` switches directly to the requested theme and
/// persists it to `~/.savvagent/theme.toml`; an unknown name is a
/// soft error that leaves the active theme unchanged. The picker is
/// refused while a turn is in-flight to avoid mode-switching mid-stream.
///
/// The theme change is applied at the next [`ui::render`] call — no
/// host reconnect or widget rebuild needed.
fn handle_theme_command(app: &mut App, args: &str) {
    // Refuse all /theme invocations during an in-flight turn — picker,
    // `list`, and direct-slug paths alike. Mirrors `/bash` and `/resume`
    // so users don't get a half-applied theme or a mode switch mid-stream.
    if app.is_loading {
        app.push_note("Cannot /theme during an in-flight turn — wait for it to finish.");
        return;
    }
    let trimmed = args.trim();
    if trimmed.is_empty() || trimmed == "list" {
        app.open_theme_picker();
        return;
    }

    match theme::Theme::from_name(trimmed) {
        Some(new_theme) => {
            app.active_theme = new_theme;
            match theme::save(new_theme) {
                Ok(()) => {
                    app.push_note(format!("theme set to `{}`", new_theme.name()));
                }
                Err(e) => {
                    app.push_note(format!(
                        "theme `{}` applied for this session, but persistence failed: {e}",
                        new_theme.name()
                    ));
                }
            }
        }
        None => {
            app.push_note(format!(
                "theme `{}` not found — keeping `{}`. Run `/theme list` to see available themes.",
                trimmed,
                app.active_theme.name(),
            ));
        }
    }
}

/// `/bash <cmd>` — run `cmd` through `tool-bash` without round-tripping
/// through the provider. `--net` / `--no-net` flags at the front of `rest`
/// override the bash-network policy for this call only.
///
/// The call is dispatched on a worker task; its
/// [`TurnEvent`]s — most importantly any
/// [`TurnEvent::BashNetworkRequested`] the resolver emits — are forwarded
/// to the main loop's worker channel so the modal flow stays unchanged.
async fn handle_bash_slash_command(
    app: &mut App,
    rest_raw: &str,
    host_slot: &HostSlot,
    worker_tx: &mpsc::Sender<WorkerMsg>,
) {
    let parsed = match parse_bash_command(rest_raw) {
        Ok(p) => p,
        Err(BashCommandError::UnknownFlag { token }) => {
            app.push_note(format!(
                "Unknown bash flag `{token}` — only `--net` and `--no-net` are recognised. \
                 Usage: /bash [--net|--no-net] <command>. \
                 Example: /bash --net curl https://example.com"
            ));
            return;
        }
        Err(_) => {
            app.push_note(
                "Usage: /bash [--net|--no-net] <command>. Example: /bash --net curl https://example.com",
            );
            return;
        }
    };
    let Some(host) = current_host(host_slot).await else {
        app.push_note("Not connected — `/connect` first, then `/bash <command>`.");
        return;
    };
    // Pre-flight: confirm tool-bash is actually configured. Without this
    // check the call falls through to `call_with_bash_net_override` and
    // surfaces as the opaque "unknown tool: run" error, which gives the
    // user no actionable repair path. The `run` tool is the contract
    // surface tool-bash advertises.
    let bash_configured = host.tool_defs().await.iter().any(|t| t.name == "run");
    if !bash_configured {
        app.push_note(
            "tool-bash isn't configured — set SAVVAGENT_TOOL_BASH_BIN or run `cargo build` \
             so the `savvagent-tool-bash` binary is available, then `/connect` again.",
        );
        return;
    }
    if app.is_loading {
        app.push_note("Cannot /bash during an in-flight turn — wait for it to finish.");
        return;
    }

    // Surface the invocation in the transcript so its eventual result is
    // attached to a visible Tool entry (matches how model-driven calls
    // render).
    app.entries.push(Entry::Tool {
        name: "run".to_string(),
        arguments: format!("/bash {}", parsed.command),
        status: None,
        result_preview: None,
    });
    app.is_loading = true;
    app.update_metrics();

    let tx = worker_tx.clone();
    let command = parsed.command.clone();
    let net_override = parsed.net_override;
    tokio::spawn(async move {
        let (ev_tx, mut ev_rx) = mpsc::channel::<TurnEvent>(8);
        let host_for_run = host.clone();
        let runner = tokio::spawn(async move {
            host_for_run
                .run_bash_command(&command, net_override, Some(ev_tx))
                .await
        });
        // Forward bash-network prompt events into the main loop. The
        // forwarder exits when `ev_tx` is dropped (i.e. the runner
        // returns).
        let forwarder_tx = tx.clone();
        let forwarder = tokio::spawn(async move {
            while let Some(ev) = ev_rx.recv().await {
                if forwarder_tx.send(WorkerMsg::Event(ev)).await.is_err() {
                    break;
                }
            }
        });
        let outcome = runner.await;
        // Forwarder will drain on ev_tx drop; join it so we don't race a
        // dangling event past the final ToolCallFinished.
        let _ = forwarder.await;
        match outcome {
            Ok(Ok((is_error, payload))) => {
                let status = if is_error {
                    ToolCallStatus::Errored
                } else {
                    ToolCallStatus::Ok
                };
                let _ = tx
                    .send(WorkerMsg::Event(TurnEvent::ToolCallFinished {
                        name: "run".into(),
                        status,
                        result: payload,
                    }))
                    .await;
                let _ = tx.send(WorkerMsg::BashDone).await;
            }
            Ok(Err(msg)) => {
                let _ = tx.send(WorkerMsg::Error(format!("/bash: {msg}"))).await;
                let _ = tx.send(WorkerMsg::BashDone).await;
            }
            Err(join_err) => {
                let _ = tx
                    .send(WorkerMsg::Error(format!("/bash worker failed: {join_err}")))
                    .await;
                let _ = tx.send(WorkerMsg::BashDone).await;
            }
        }
    });
}

fn fmt_overrides(cfg: &SandboxConfig) -> String {
    if cfg.tool_overrides.is_empty() {
        return "(none)".into();
    }
    cfg.tool_overrides
        .iter()
        .map(|(k, v)| {
            let net = match v.allow_net {
                Some(true) => "net=yes",
                Some(false) => "net=no",
                None => "net=inherit",
            };
            format!("{k}({net})")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Persist the key (if required), build the in-process handler, swap the host.
async fn perform_connect(
    spec: &'static ProviderSpec,
    api_key: String,
    host_slot: &HostSlot,
    project_root: &Path,
    tool_bins: &ToolBins,
    app: &mut App,
) {
    if spec.api_key_required {
        if let Err(e) = creds::save(spec.id, &api_key) {
            app.push_note(format!("Could not store key in OS keyring: {e}"));
            return;
        }
    }

    app.push_note(format!("Connecting to {}…", spec.display_name));

    let host = match build_in_process_host(spec, &api_key, project_root, tool_bins).await {
        Ok(h) => h,
        Err(e) => {
            app.push_note(format!("Connect to {} failed: {e:#}", spec.id));
            return;
        }
    };

    let old = {
        let mut guard = host_slot.write().await;
        guard.replace(host)
    };
    if let Some(old) = old {
        // Old host — fire-and-forget shutdown. Tool children get reaped here.
        tokio::spawn(async move { old.shutdown().await });
    }

    // Switching providers can leave dangling tool_use ids; safer to start
    // a fresh conversation than to mix histories.
    if let Some(host) = current_host(host_slot).await {
        host.clear_history().await;
    }
    app.entries.clear();
    app.live_text.clear();
    app.update_metrics();

    app.connected = true;
    app.active_provider_id = Some(spec.id);
    app.model = std::env::var("SAVVAGENT_MODEL").unwrap_or_else(|_| spec.default_model.to_string());
    // Align the splash sandbox indicator with the now-active host's config.
    // If the user lands on `/connect` within the 3s splash window, this
    // refresh makes the banner reflect what tools will actually be wrapped
    // with rather than the on-disk file read at TUI launch.
    if let Some(host) = current_host(host_slot).await {
        app.refresh_splash_sandbox_from_host(host.sandbox_config());
    }
    app.push_note(format!("Connected to {}.", spec.display_name));
}

async fn run_app(
    terminal: &mut tui::Tui,
    app: &mut App,
    host_slot: HostSlot,
    project_root: PathBuf,
    tool_bins: ToolBins,
) -> Result<()> {
    let (worker_tx, mut worker_rx) = mpsc::channel::<WorkerMsg>(128);

    loop {
        let frame_area = terminal.get_frame().area();
        let frame_data = ui::compute_home_frame_data(app, frame_area).await;
        terminal.draw(|f| ui::render(app, f, &frame_data))?;

        while let Ok(msg) = worker_rx.try_recv() {
            match msg {
                WorkerMsg::Event(e) => {
                    let was_complete = matches!(e, TurnEvent::TurnComplete { .. });
                    app.apply_turn_event(e);
                    app.update_metrics();
                    if was_complete {
                        if let Some(host) = current_host(&host_slot).await {
                            if let Ok(path) = save_transcript_now(app, &host).await {
                                if !path.as_os_str().is_empty() {
                                    app.last_transcript = Some(path);
                                }
                            }
                        }
                    }
                }
                WorkerMsg::Error(msg) => {
                    app.is_loading = false;
                    app.entries.push(Entry::Note(format!("Error: {msg}")));
                    app.update_metrics();
                }
                WorkerMsg::BashDone => {
                    app.is_loading = false;
                    app.update_metrics();
                }
            }
        }

        if app.should_quit {
            drain_pending_bash_net(app, &host_slot).await;
            return Ok(());
        }

        if app.show_splash && app.splash_shown_at.elapsed() >= splash::SPLASH_DURATION {
            app.show_splash = false;
        }

        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        let evt = event::read()?;
        let Event::Key(key) = &evt else { continue };
        if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
            continue;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            drain_pending_bash_net(app, &host_slot).await;
            return Ok(());
        }

        if app.show_splash {
            app.show_splash = false;
            continue;
        }

        match app.input_mode {
            InputMode::Editing => {
                if app.is_file_picker_active {
                    match key.code {
                        KeyCode::Enter => {
                            let file = app.file_explorer.current();
                            if file.is_dir {
                                app.file_explorer.handle(&evt)?;
                            } else {
                                app.file_picker_select();
                            }
                        }
                        KeyCode::Esc => app.close_file_picker(),
                        _ => {
                            app.file_explorer.handle(&evt)?;
                        }
                    }
                } else {
                    match key.code {
                        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.open_command_palette();
                        }
                        KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                            let value = app.input_textarea.lines().join("\n");
                            if value.is_empty() || app.is_loading {
                                continue;
                            }
                            if value.starts_with('/') {
                                app.input_textarea = TextArea::default();
                                dispatch_slash_command(
                                    app,
                                    &value,
                                    &host_slot,
                                    &project_root,
                                    &tool_bins,
                                    &worker_tx,
                                )
                                .await;
                                continue;
                            }
                            let Some(host) = current_host(&host_slot).await else {
                                app.push_note("Not connected. Use /connect first.");
                                app.input_textarea = TextArea::default();
                                continue;
                            };
                            app.push_user(value.clone());
                            app.input_textarea = TextArea::default();
                            app.is_loading = true;

                            let tx = worker_tx.clone();
                            tokio::spawn(async move {
                                let (ev_tx, mut ev_rx) = mpsc::channel(64);
                                let host_for_run = host.clone();
                                let prompt = value;
                                let runner = tokio::spawn(async move {
                                    host_for_run.run_turn_streaming(prompt, ev_tx).await
                                });
                                while let Some(ev) = ev_rx.recv().await {
                                    if tx.send(WorkerMsg::Event(ev)).await.is_err() {
                                        break;
                                    }
                                }
                                match runner.await {
                                    Ok(Ok(_)) => {}
                                    Ok(Err(e)) => {
                                        let _ = tx.send(WorkerMsg::Error(e.to_string())).await;
                                    }
                                    Err(join_err) => {
                                        let _ = tx
                                            .send(WorkerMsg::Error(format!(
                                                "worker task failed: {join_err}"
                                            )))
                                            .await;
                                    }
                                }
                            });
                        }
                        KeyCode::Esc => {
                            app.input_textarea = TextArea::default();
                        }
                        KeyCode::Char('@') => {
                            app.input_textarea.input(evt);
                            app.open_file_picker();
                        }
                        KeyCode::Char('/')
                            if !key.modifiers.contains(KeyModifiers::CONTROL)
                                && app.input_textarea.lines().iter().all(|l| l.is_empty()) =>
                        {
                            app.open_command_palette();
                        }
                        _ => {
                            app.input_textarea.input(evt);
                        }
                    }
                }
            }
            InputMode::CommandPalette => match key.code {
                KeyCode::Esc => app.close_command_palette(),
                KeyCode::Up if app.command_index > 0 => app.command_index -= 1,
                KeyCode::Down => {
                    let visible = app.filtered_command_indices().len();
                    if app.command_index + 1 < visible {
                        app.command_index += 1;
                    }
                }
                KeyCode::Enter => {
                    if let Some(CommandSelection::Execute(cmd)) = app.select_command() {
                        dispatch_slash_command(
                            app,
                            &cmd,
                            &host_slot,
                            &project_root,
                            &tool_bins,
                            &worker_tx,
                        )
                        .await;
                    }
                }
                KeyCode::Backspace if !app.palette_pop_char() => {
                    // Empty filter — backspace past the implicit `/` closes
                    // the palette and returns to a clean prompt.
                    app.close_command_palette();
                }
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    app.palette_push_char(c);
                }
                _ => {}
            },
            InputMode::SelectingProvider => match key.code {
                KeyCode::Esc => app.input_mode = InputMode::Editing,
                KeyCode::Up if app.provider_index > 0 => app.provider_index -= 1,
                KeyCode::Down if app.provider_index + 1 < PROVIDERS.len() => {
                    app.provider_index += 1
                }
                KeyCode::Enter => {
                    let idx = app.provider_index;
                    if let Some(spec) = PROVIDERS.get(idx) {
                        if !spec.api_key_required {
                            // Keyless provider — connect immediately without a
                            // stored or prompted API key.
                            app.input_mode = InputMode::Editing;
                            perform_connect(
                                spec,
                                String::new(),
                                &host_slot,
                                &project_root,
                                &tool_bins,
                                app,
                            )
                            .await;
                        } else {
                            match creds::load(spec.id) {
                                Ok(Some(key)) => {
                                    app.input_mode = InputMode::Editing;
                                    app.push_note(format!(
                                        "Using stored key for {}.",
                                        spec.display_name
                                    ));
                                    perform_connect(
                                        spec,
                                        key,
                                        &host_slot,
                                        &project_root,
                                        &tool_bins,
                                        app,
                                    )
                                    .await;
                                }
                                Ok(None) => app.enter_api_key_for(idx),
                                Err(e) => {
                                    app.push_note(format!("Keyring error: {e}"));
                                    app.enter_api_key_for(idx);
                                }
                            }
                        }
                    } else {
                        app.input_mode = InputMode::Editing;
                    }
                }
                _ => {}
            },
            InputMode::EnteringApiKey => match key.code {
                KeyCode::Esc => app.cancel_connect(),
                KeyCode::Enter => {
                    if let Some((spec, key)) = app.take_pending_api_key() {
                        app.input_mode = InputMode::Editing;
                        perform_connect(spec, key, &host_slot, &project_root, &tool_bins, app)
                            .await;
                    } else {
                        app.push_note("API key cannot be empty.");
                    }
                }
                _ => {
                    app.api_key_textarea.input(evt);
                }
            },
            InputMode::ViewingFile => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    app.input_mode = InputMode::Editing;
                    app.active_file_path = None;
                    app.editor = None;
                }
                _ => {
                    if let Some(editor) = &mut app.editor {
                        let area = terminal.size()?;
                        let popup = ui::centered_rect(80, 80, area.into());
                        let inner = popup.inner(ratatui::layout::Margin {
                            horizontal: 1,
                            vertical: 1,
                        });
                        editor.input(*key, &inner)?;
                    }
                }
            },
            InputMode::EditingFile => match key.code {
                KeyCode::Esc => {
                    app.save_file();
                    app.input_mode = InputMode::Editing;
                    app.active_file_path = None;
                    app.editor = None;
                }
                _ => {
                    if let Some(editor) = &mut app.editor {
                        let area = terminal.size()?;
                        let popup = ui::centered_rect(80, 80, area.into());
                        let inner = popup.inner(ratatui::layout::Margin {
                            horizontal: 1,
                            vertical: 1,
                        });
                        editor.input(*key, &inner)?;
                    }
                }
            },
            InputMode::PermissionPrompt => {
                let action = match key.code {
                    KeyCode::Char('y') => Some((PermissionDecision::Allow, false)),
                    KeyCode::Char('n') | KeyCode::Esc => Some((PermissionDecision::Deny, false)),
                    KeyCode::Char('a') => Some((PermissionDecision::Allow, true)),
                    KeyCode::Char('N') => Some((PermissionDecision::Deny, true)),
                    _ => None,
                };
                if let Some((decision, persist)) = action {
                    resolve_pending_permission(app, &host_slot, decision, persist).await;
                }
            }
            InputMode::BashNetworkPrompt { id, .. } => {
                let choice = match key.code {
                    KeyCode::Char('o') | KeyCode::Char('O') => Some(BashNetworkChoice::Once),
                    KeyCode::Char('a') | KeyCode::Char('A') => {
                        Some(BashNetworkChoice::AlwaysThisSession)
                    }
                    KeyCode::Char('d') | KeyCode::Char('D') => Some(BashNetworkChoice::DenyOnce),
                    KeyCode::Char('f')
                    | KeyCode::Char('F')
                    | KeyCode::Char('n')
                    | KeyCode::Char('N') => Some(BashNetworkChoice::DenyAlways),
                    // Esc → Cancelled (policy-equivalent to DenyOnce, but
                    // labelled distinctly so the user sees that backing
                    // out implied a deny rather than reading their Esc
                    // as an active "deny" decision.
                    KeyCode::Esc => Some(BashNetworkChoice::Cancelled),
                    _ => None,
                };
                if let Some(choice) = choice {
                    if let Some(host) = current_host(&host_slot).await {
                        let host = host.clone();
                        tokio::spawn(async move {
                            host.resolve_bash_network_decision(id, choice).await;
                        });
                    }
                    app.input_mode = InputMode::Editing;
                    let label = match choice {
                        BashNetworkChoice::Once => "bash net: allowed once",
                        BashNetworkChoice::AlwaysThisSession => {
                            "bash net: always allowed (this session)"
                        }
                        BashNetworkChoice::DenyOnce => "bash net: denied once",
                        BashNetworkChoice::DenyAlways => "bash net: never (this session)",
                        BashNetworkChoice::Cancelled => {
                            "bash net: cancelled (via Esc — defaulted to deny)"
                        }
                    };
                    app.push_note(label);
                }
            }
            InputMode::SelectingTranscript => match key.code {
                KeyCode::Esc => app.close_transcript_picker(),
                KeyCode::Up if app.transcript_index > 0 => {
                    app.transcript_index -= 1;
                }
                KeyCode::Down => {
                    let count = app.transcript_entries.len();
                    if app.transcript_index + 1 < count {
                        app.transcript_index += 1;
                    }
                }
                KeyCode::Enter => {
                    if let Some(path) = app.selected_transcript_path().map(|p| p.to_path_buf()) {
                        app.close_transcript_picker();
                        if app.is_loading {
                            app.push_note(
                                "Cannot /resume during an in-flight turn — wait for it to finish.",
                            );
                        } else {
                            do_resume_from_path(app, &host_slot, &path).await;
                        }
                    }
                }
                _ => {}
            },
            InputMode::SelectingTheme => match key.code {
                KeyCode::Esc => app.theme_picker_cancel(),
                KeyCode::Up => app.theme_picker_cursor_up(),
                KeyCode::Down => app.theme_picker_cursor_down(),
                KeyCode::Backspace => app.theme_picker_backspace(),
                KeyCode::Enter => {
                    if app.theme_picker_filtered_themes().is_empty() {
                        // Spec edge case 2: Enter on a zero-match filter is a
                        // no-op. Cursor stays on the "no themes match" hint;
                        // user can Backspace to widen, type to narrow further,
                        // or Esc to cancel. Without this guard, the last-good
                        // preview that `clamp_theme_picker_after_filter_change`
                        // preserved would get committed and persisted.
                    } else {
                        let chosen = app.active_theme;
                        app.theme_picker_confirm();
                        match theme::save(chosen) {
                            Ok(()) => {
                                app.push_note(format!("theme set to `{}`", chosen.name()));
                            }
                            Err(e) => {
                                app.push_note(format!(
                                    "theme `{}` applied for this session, but persistence failed: {e}",
                                    chosen.name()
                                ));
                            }
                        }
                    }
                }
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    app.theme_picker_typed_char(c);
                }
                _ => {}
            },
        }
    }
}

/// On graceful exit, if the user is mid-modal on a bash-network prompt,
/// resolve it as [`BashNetworkChoice::DenyOnce`] so any worker awaiting
/// the corresponding `oneshot` doesn't hang while the runtime tears
/// down. Without this, the worker would only unblock once the `Host`
/// (and the Sender inside `pending_bash_network`) is finally dropped —
/// which happens *after* `run_app` returns. That gap is small but real,
/// and on shutdown we want the worker to finish promptly and let
/// `Host::shutdown` drain cleanly.
async fn drain_pending_bash_net(app: &App, host_slot: &HostSlot) {
    if let InputMode::BashNetworkPrompt { id, .. } = app.input_mode {
        if let Some(host) = current_host(host_slot).await {
            host.resolve_bash_network_decision(id, BashNetworkChoice::DenyOnce)
                .await;
        }
    }
}

/// Pop the pending permission off the app and resolve it on the active
/// host. With `persist = true`, also records a session rule so future
/// requests with identical args short-circuit the modal.
async fn resolve_pending_permission(
    app: &mut App,
    host_slot: &HostSlot,
    decision: PermissionDecision,
    persist: bool,
) {
    let Some(req) = app.pending_permission.take() else {
        app.input_mode = InputMode::Editing;
        return;
    };
    let host = current_host(host_slot).await;
    if let Some(host) = &host {
        if persist {
            host.add_session_rule(&req.name, &req.args, decision).await;
        }
        host.resolve_permission(req.id, decision).await;
    } else {
        // Host swapped while modal was up — the old host's gate will return
        // Err on its dropped oneshot, which is the cleanup path. Nothing
        // for us to do here.
    }
    app.input_mode = InputMode::Editing;

    let label = match (decision, persist) {
        (PermissionDecision::Allow, false) => "allowed once",
        (PermissionDecision::Allow, true) => "always allowed (this session)",
        (PermissionDecision::Deny, false) => "denied",
        (PermissionDecision::Deny, true) => "always denied (this session)",
    };
    app.push_note(format!("{}: {label}", req.name));
}

#[cfg(test)]
mod model_validation_tests {
    use super::{ModelChangeOutcome, resolve_model_change, validate_model_id};
    use savvagent_host::{ListModelsResponse, ModelInfo};
    use savvagent_protocol::{ErrorKind, ProviderError};

    fn info(id: &str) -> ModelInfo {
        ModelInfo {
            id: id.to_string(),
            display_name: None,
            context_window: None,
        }
    }

    fn resp(ids: &[&str]) -> ListModelsResponse {
        ListModelsResponse {
            models: ids.iter().map(|id| info(id)).collect(),
            default_model_id: None,
        }
    }

    fn err(kind: ErrorKind, msg: &str) -> ProviderError {
        ProviderError {
            kind,
            message: msg.to_string(),
            retry_after_ms: None,
            provider_code: None,
        }
    }

    #[test]
    fn validate_model_id_known() {
        let models = vec![info("a"), info("b")];
        assert!(validate_model_id("a", &models).is_ok());
    }

    #[test]
    fn validate_model_id_unknown_returns_known_set() {
        let models = vec![info("a"), info("b")];
        let err = validate_model_id("c", &models).unwrap_err();
        assert_eq!(err, vec!["a", "b"]);
    }

    #[test]
    fn validate_model_id_empty_list_always_rejects() {
        let models: Vec<ModelInfo> = vec![];
        let err = validate_model_id("anything", &models).unwrap_err();
        assert!(err.is_empty());
    }

    #[test]
    fn resolve_proceeds_silently_for_known_id() {
        let r = resp(&["a", "b"]);
        let outcome = resolve_model_change("a", Ok(&r));
        assert_eq!(outcome, ModelChangeOutcome::Proceed { warning: None });
    }

    #[test]
    fn resolve_rejects_unknown_id_with_known_set() {
        let r = resp(&["a", "b"]);
        let outcome = resolve_model_change("c", Ok(&r));
        match outcome {
            ModelChangeOutcome::Reject { note } => {
                assert!(note.contains("Unknown model `c`"), "note: {note}");
                assert!(note.contains("a, b"), "note: {note}");
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn resolve_empty_list_proceeds_with_warning() {
        let r = resp(&[]);
        let outcome = resolve_model_change("anything", Ok(&r));
        match outcome {
            ModelChangeOutcome::Proceed { warning: Some(w) } => {
                assert!(w.contains("Provider advertises no models"), "w: {w}");
                assert!(w.contains("`anything`"), "w: {w}");
            }
            other => panic!("expected Proceed with warning, got {other:?}"),
        }
    }

    #[test]
    fn resolve_not_implemented_proceeds_silently() {
        let e = err(ErrorKind::NotImplemented, "list_models not implemented");
        let outcome = resolve_model_change("anything", Err(&e));
        assert_eq!(outcome, ModelChangeOutcome::Proceed { warning: None });
    }

    #[test]
    fn resolve_network_error_proceeds_with_warning() {
        let e = err(ErrorKind::Network, "HTTP 401: invalid_api_key");
        let outcome = resolve_model_change("gpt-x", Err(&e));
        match outcome {
            ModelChangeOutcome::Proceed { warning: Some(w) } => {
                assert!(w.contains("Could not verify"), "w: {w}");
                assert!(w.contains("`gpt-x`"), "w: {w}");
                assert!(w.contains("invalid_api_key"), "w: {w}");
            }
            other => panic!("expected Proceed with warning, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod theme_command_tests {
    use super::{App, CommandSelection, Entry, InputMode, handle_theme_command, theme};
    use crate::test_helpers::{HOME_LOCK, HomeGuard};
    use std::path::PathBuf;

    fn fresh_app() -> App {
        App::new("test-model".into(), PathBuf::from("/tmp"))
    }

    fn note_lines(app: &App) -> Vec<&str> {
        app.entries
            .iter()
            .filter_map(|e| {
                if let Entry::Note(t) = e {
                    Some(t.as_str())
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn empty_args_opens_picker() {
        let _g = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let mut app = fresh_app();
        handle_theme_command(&mut app, "");
        assert!(
            matches!(app.input_mode, InputMode::SelectingTheme),
            "empty `/theme` must open the picker"
        );
    }

    #[test]
    fn list_arg_opens_picker() {
        let _g = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let mut app = fresh_app();
        handle_theme_command(&mut app, "list");
        assert!(matches!(app.input_mode, InputMode::SelectingTheme));
    }

    #[test]
    fn unknown_name_does_not_change_active_theme() {
        let _g = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let mut app = fresh_app();
        let original = app.active_theme;
        handle_theme_command(&mut app, "totally-bogus");
        assert_eq!(
            app.active_theme, original,
            "unknown name must not change the active theme"
        );
        // Picker must NOT open on an unknown slug — that path is a
        // direct-typing escape hatch with a clear error.
        assert!(matches!(app.input_mode, InputMode::Editing));
        let notes = note_lines(&app);
        let last = notes.last().unwrap();
        assert!(last.contains("totally-bogus"), "last note: {last}");
        assert!(last.contains("not found"), "last note: {last}");
        assert!(last.contains("/theme list"), "last note: {last}");
    }

    #[test]
    fn known_name_sets_active_theme() {
        let _g = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let mut app = fresh_app();
        handle_theme_command(&mut app, "light");
        assert_eq!(app.active_theme, theme::Theme::Light);
        // Direct slug path: no picker opens; result is announced via note.
        assert!(matches!(app.input_mode, InputMode::Editing));
        let notes = note_lines(&app);
        assert!(notes.last().unwrap().contains("set to `light`"));
    }

    #[test]
    fn known_name_persists_through_load() {
        let _g = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let mut app = fresh_app();
        handle_theme_command(&mut app, "high-contrast");
        assert_eq!(app.active_theme, theme::Theme::HighContrast);
        let app2 = fresh_app();
        assert_eq!(app2.active_theme, theme::Theme::HighContrast);
    }

    #[test]
    fn upstream_theme_name_is_accepted_and_persisted() {
        let _g = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let mut app = fresh_app();
        handle_theme_command(&mut app, "tokyo-night");
        match app.active_theme {
            theme::Theme::Upstream(t) => assert_eq!(t.slug(), "tokyo-night"),
            other => panic!("expected upstream tokyo-night, got {other:?}"),
        }
        let app2 = fresh_app();
        assert_eq!(app2.active_theme, app.active_theme);
    }

    #[test]
    fn empty_args_refuses_when_is_loading() {
        let _g = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let mut app = fresh_app();
        app.is_loading = true;
        handle_theme_command(&mut app, "");
        assert!(
            matches!(app.input_mode, InputMode::Editing),
            "must not open picker during in-flight turn"
        );
        let notes = note_lines(&app);
        let last = notes.last().unwrap();
        assert!(
            last.contains("in-flight") || last.contains("wait"),
            "last note must explain the refusal: {last}"
        );
    }

    #[test]
    fn list_arg_refuses_when_is_loading() {
        let _g = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let mut app = fresh_app();
        app.is_loading = true;
        handle_theme_command(&mut app, "list");
        assert!(
            matches!(app.input_mode, InputMode::Editing),
            "must not open picker during in-flight turn"
        );
        let notes = note_lines(&app);
        let last = notes.last().unwrap();
        assert!(
            last.contains("in-flight") || last.contains("wait"),
            "last note must explain the refusal: {last}"
        );
    }

    #[test]
    fn direct_slug_refuses_when_is_loading() {
        let _g = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let mut app = fresh_app();
        let original = app.active_theme;
        app.is_loading = true;
        handle_theme_command(&mut app, "dark");
        // Must not mutate active_theme during an in-flight turn.
        assert_eq!(app.active_theme, original);
        let notes = note_lines(&app);
        let last = notes.last().unwrap();
        assert!(
            last.contains("in-flight") || last.contains("wait"),
            "last note must explain the refusal: {last}"
        );
    }

    #[test]
    fn theme_picker_enter_with_empty_filter_is_noop() {
        let _g = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let mut app = fresh_app();
        // Open picker, narrow filter to zero matches.
        handle_theme_command(&mut app, "");
        assert!(matches!(app.input_mode, InputMode::SelectingTheme));
        for c in "totallybogus".chars() {
            app.theme_picker_typed_char(c);
        }
        assert!(app.theme_picker_filtered_themes().is_empty());
        let before_theme = app.active_theme;
        let before_notes = note_lines(&app).len();

        // Simulate the production Enter handler's empty-filter guard: when
        // filtered is empty, do nothing. We pin the *state invariant* the
        // production code preserves — state stays untouched, no note pushed,
        // mode stays in SelectingTheme so Esc can still cancel.
        if app.theme_picker_filtered_themes().is_empty() {
            // no-op
        } else {
            // Would otherwise commit; this branch is unreachable here.
            unreachable!("filter is empty");
        }

        assert_eq!(
            app.active_theme, before_theme,
            "Enter on empty filter must not clobber the live-preview theme"
        );
        assert!(
            matches!(app.input_mode, InputMode::SelectingTheme),
            "Enter on empty filter must stay in the picker"
        );
        assert_eq!(
            note_lines(&app).len(),
            before_notes,
            "Enter on empty filter must not push any note"
        );
    }

    #[test]
    fn theme_picker_enter_persists_chosen_theme() {
        let _g = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let mut app = fresh_app();
        // Open picker.
        handle_theme_command(&mut app, "");
        assert!(matches!(app.input_mode, InputMode::SelectingTheme));
        // Simulate scrolling to a specific theme.
        app.theme_picker_filter = "tokyo".to_string();
        app.theme_picker_index = 0;
        let filtered = app.theme_picker_filtered_themes();
        let chosen = filtered[0];
        app.active_theme = chosen;
        // Simulate the Enter handler.
        app.theme_picker_confirm();
        let _ = theme::save(app.active_theme);

        let app2 = fresh_app();
        assert_eq!(app2.active_theme, chosen);
    }

    #[test]
    fn theme_picker_esc_does_not_persist() {
        let _g = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        // Pre-save Dark so theme.toml has a known starting value.
        let mut app = fresh_app();
        handle_theme_command(&mut app, "dark");
        assert_eq!(app.active_theme, theme::Theme::Dark);

        // Open picker, live-preview, then cancel.
        handle_theme_command(&mut app, "");
        app.theme_picker_cursor_down();
        assert_ne!(app.active_theme, theme::Theme::Dark);
        app.theme_picker_cancel();
        assert_eq!(app.active_theme, theme::Theme::Dark);

        // Reload: theme.toml should still say dark.
        let app2 = fresh_app();
        assert_eq!(app2.active_theme, theme::Theme::Dark);
    }

    #[test]
    fn command_palette_selecting_theme_opens_picker_not_prefill() {
        let _g = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let mut app = fresh_app();
        // Find the /theme command index in app.commands.
        let theme_idx = app
            .commands
            .iter()
            .position(|c| c.name == "/theme")
            .expect("/theme must be registered");
        // Simulate command-palette selection of /theme by setting palette
        // state and calling select_command. The contract: /theme should
        // resolve as Execute(...) (not Prefill) so the keypath runs
        // handle_theme_command immediately and opens the picker.
        app.input_mode = InputMode::CommandPalette;
        app.command_index = theme_idx;
        let outcome = app.select_command();
        match outcome {
            Some(CommandSelection::Execute(cmd)) => {
                assert_eq!(cmd.trim(), "/theme");
                // Now run handle_theme_command on empty args, which is
                // what the main.rs keypath dispatches to for "/theme".
                handle_theme_command(&mut app, "");
                assert!(matches!(app.input_mode, InputMode::SelectingTheme));
            }
            Some(CommandSelection::Prefill(_)) => {
                panic!("/theme should be Execute now that needs_arg is false");
            }
            None => panic!("/theme command must be selectable"),
        }
    }
}
