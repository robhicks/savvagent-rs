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
//! - `SAVVAGENT_MODEL`        (overrides the per-provider default)
//! - `SAVVAGENT_TOOL_FS_BIN`  (default `savvagent-tool-fs` on $PATH)

mod app;
mod creds;
mod providers;
mod splash;
mod tui;
mod ui;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use app::{App, CommandSelection, Entry, InputMode};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use providers::{PROVIDERS, ProviderSpec};
use savvagent_host::{
    Host, HostConfig, PermissionDecision, ProviderEndpoint, ToolEndpoint, TurnEvent,
};
use savvagent_mcp::{InProcessProviderClient, ProviderClient};
use tokio::sync::{RwLock, mpsc};
use tui_textarea::TextArea;

/// Worker → main-loop messages.
enum WorkerMsg {
    Event(TurnEvent),
    /// Sent if `run_turn_streaming` returned an error.
    Error(String),
}

type HostSlot = Arc<RwLock<Option<Arc<Host>>>>;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    init_tracing();

    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let tool_bin = locate_tool_fs();

    let initial = bootstrap_host(&project_root, tool_bin.as_deref()).await;
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
    app.connected = host_slot.read().await.is_some();
    app.active_provider_id = initial_provider;
    if !app.connected {
        app.push_note(
            "Not connected. Type / (or press Ctrl-P) and pick /connect to set up a provider.",
        );
    }
    if tool_bin.is_none() {
        app.push_note(
            "Note: savvagent-tool-fs not found — tools disabled. Run `cargo build` or set SAVVAGENT_TOOL_FS_BIN.",
        );
    }
    let res = run_app(
        &mut terminal,
        &mut app,
        host_slot.clone(),
        project_root,
        tool_bin,
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
    tool_bin: Option<&Path>,
) -> Option<(Arc<Host>, String, Option<&'static str>)> {
    if let Ok(url) = std::env::var("SAVVAGENT_PROVIDER_URL") {
        let model =
            std::env::var("SAVVAGENT_MODEL").unwrap_or_else(|_| "claude-haiku-4-5".to_string());
        match start_host_remote(
            url,
            model.clone(),
            project_root.to_path_buf(),
            tool_bin.map(Path::to_path_buf),
        )
        .await
        {
            Ok(host) => return Some((host, model, None)),
            Err(e) => {
                eprintln!("warning: SAVVAGENT_PROVIDER_URL set but connect failed: {e:#}");
            }
        }
    }

    for spec in PROVIDERS {
        let Ok(Some(key)) = creds::load(spec.id) else {
            continue;
        };
        match build_in_process_host(spec, &key, project_root, tool_bin).await {
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
    tool_bin: Option<&Path>,
) -> Result<Arc<Host>> {
    let model = std::env::var("SAVVAGENT_MODEL").unwrap_or_else(|_| spec.default_model.to_string());
    build_in_process_host_with_model(spec, api_key, project_root, tool_bin, model).await
}

/// Same as [`build_in_process_host`] but with an explicit `model` id —
/// used by `/model <id>` to reconnect against the same provider with a
/// different model.
async fn build_in_process_host_with_model(
    spec: &'static ProviderSpec,
    api_key: &str,
    project_root: &Path,
    tool_bin: Option<&Path>,
    model: String,
) -> Result<Arc<Host>> {
    let handler = (spec.build)(api_key).with_context(|| format!("building {} handler", spec.id))?;
    let client: Box<dyn ProviderClient + Send + Sync> =
        Box::new(InProcessProviderClient::new(handler));
    // The endpoint variant is a placeholder when we hand the host a
    // pre-built ProviderClient via `with_components`; pick a recognizable
    // dummy URL so a stray log line says where it came from.
    let mut config = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: format!("inproc://{}", spec.id),
        },
        model,
    )
    .with_project_root(project_root.to_path_buf());
    if let Some(bin) = tool_bin {
        config = config.with_tool(ToolEndpoint::Stdio {
            command: bin.to_path_buf(),
            args: vec![],
        });
    }
    let host = Host::with_components(config, client)
        .await
        .context("Host::with_components")?;
    Ok(Arc::new(host))
}

async fn start_host_remote(
    url: String,
    model: String,
    project_root: PathBuf,
    tool_bin: Option<PathBuf>,
) -> Result<Arc<Host>> {
    let mut config = HostConfig::new(ProviderEndpoint::StreamableHttp { url }, model)
        .with_project_root(project_root);
    if let Some(bin) = tool_bin {
        config = config.with_tool(ToolEndpoint::Stdio {
            command: bin,
            args: vec![],
        });
    }
    let host = Host::start(config).await.context("failed to start host")?;
    Ok(Arc::new(host))
}

/// Resolve the `savvagent-tool-fs` binary. Tries (in order):
///
/// 1. `SAVVAGENT_TOOL_FS_BIN` env override (must point at an existing file).
/// 2. A sibling of the running TUI executable — i.e. `target/<profile>/`
///    when launched via `cargo run`, or the install dir when installed.
/// 3. Bare `savvagent-tool-fs` resolved via `PATH`.
///
/// Returns `None` if none of the candidates exists. The caller surfaces a
/// note so the user knows tools are disabled.
fn locate_tool_fs() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SAVVAGENT_TOOL_FS_BIN") {
        let path = PathBuf::from(p);
        return path.exists().then_some(path);
    }
    let bin_name = if cfg!(windows) {
        "savvagent-tool-fs.exe"
    } else {
        "savvagent-tool-fs"
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(bin_name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(bin_name);
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
    tool_bin: Option<&Path>,
) {
    let trimmed = cmd.trim_start();
    let (head, rest) = match trimmed.split_once(char::is_whitespace) {
        Some((h, r)) => (h, r.trim()),
        None => (trimmed, ""),
    };

    match head {
        "/tools" => {
            show_tools(app, host_slot).await;
            return;
        }
        "/model" => {
            handle_model_command(app, rest, host_slot, project_root, tool_bin).await;
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

/// `/model` (no args) shows the current model. `/model <id>` reconnects
/// the active provider with the new id (optimistic — provider rejects at
/// first turn if invalid).
async fn handle_model_command(
    app: &mut App,
    rest: &str,
    host_slot: &HostSlot,
    project_root: &Path,
    tool_bin: Option<&Path>,
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
    let key = match creds::load(spec.id) {
        Ok(Some(k)) => k,
        Ok(None) => {
            app.push_note("No saved key for the active provider — `/connect` first.");
            return;
        }
        Err(e) => {
            app.push_note(format!("Keyring error: {e}"));
            return;
        }
    };

    perform_model_change(spec, &key, new_model, host_slot, project_root, tool_bin, app).await;
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
    tool_bin: Option<&Path>,
    app: &mut App,
) {
    app.push_note(format!("Switching to {}…", new_model));
    let host = match build_in_process_host_with_model(
        spec,
        api_key,
        project_root,
        tool_bin,
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

/// Persist the key, build the in-process handler, swap the host.
async fn perform_connect(
    spec: &'static ProviderSpec,
    api_key: String,
    host_slot: &HostSlot,
    project_root: &Path,
    tool_bin: Option<&Path>,
    app: &mut App,
) {
    if let Err(e) = creds::save(spec.id, &api_key) {
        app.push_note(format!("Could not store key in OS keyring: {e}"));
        return;
    }

    app.push_note(format!("Connecting to {}…", spec.display_name));

    let host = match build_in_process_host(spec, &api_key, project_root, tool_bin).await {
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
    app.push_note(format!("Connected to {}.", spec.display_name));
}

async fn run_app(
    terminal: &mut tui::Tui,
    app: &mut App,
    host_slot: HostSlot,
    project_root: PathBuf,
    tool_bin: Option<PathBuf>,
) -> Result<()> {
    let (worker_tx, mut worker_rx) = mpsc::channel::<WorkerMsg>(128);

    loop {
        terminal.draw(|f| ui::render(app, f))?;

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
            }
        }

        if app.should_quit {
            return Ok(());
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
                                    tool_bin.as_deref(),
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
                                && app
                                    .input_textarea
                                    .lines()
                                    .iter()
                                    .all(|l| l.is_empty()) =>
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
                            tool_bin.as_deref(),
                        )
                        .await;
                    }
                }
                KeyCode::Backspace => {
                    if !app.palette_pop_char() {
                        // Empty filter — backspace past the implicit `/` closes
                        // the palette and returns to a clean prompt.
                        app.close_command_palette();
                    }
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
                                    tool_bin.as_deref(),
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
                        perform_connect(
                            spec,
                            key,
                            &host_slot,
                            &project_root,
                            tool_bin.as_deref(),
                            app,
                        )
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
