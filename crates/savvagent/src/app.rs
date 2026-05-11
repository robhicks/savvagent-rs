//! TUI state. The app holds a shared [`Host`] and a render-friendly
//! conversation log built incrementally from streaming [`TurnEvent`]s.

use std::path::PathBuf;
use std::time::Instant;

use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Borders};
use ratatui_code_editor::editor::Editor;
use ratatui_explorer::{FileExplorer, FileExplorerBuilder, Theme};
use savvagent_host::{ToolCallStatus, TranscriptFile, TurnEvent};
use serde_json::Value;
use tui_textarea::TextArea;

use crate::providers::{PROVIDERS, ProviderSpec};

/// Input mode — which sub-widget consumes the next key.
pub enum InputMode {
    /// Editing the prompt textarea.
    Editing,
    /// Browsing a read-only file in the popup editor.
    ViewingFile,
    /// Editing a file in the popup editor.
    EditingFile,
    /// Command palette open.
    CommandPalette,
    /// Provider selection list — first step of `/connect`.
    SelectingProvider,
    /// API-key input — second step of `/connect`. Masked.
    EnteringApiKey,
    /// Tool-permission modal up; the turn loop is paused on a `oneshot`.
    PermissionPrompt,
    /// Bash-network prompt modal up; the lazy bash spawn is paused on
    /// a `oneshot` keyed by `id`. The user picks Once /
    /// AlwaysThisSession / DenyOnce / DenyAlways via a single-key
    /// hotkey; the choice is forwarded to
    /// [`savvagent_host::Host::resolve_bash_network_decision`].
    BashNetworkPrompt {
        /// Opaque host-side request id; pass back when resolving.
        id: u64,
        /// Human-readable summary from the policy.
        summary: String,
    },
    /// Transcript picker open — selecting a file for `/resume`.
    SelectingTranscript,
}

/// Snapshot of a pending [`TurnEvent::PermissionRequested`] used to render
/// the modal and resolve the host's outstanding `oneshot`.
#[derive(Debug, Clone)]
pub struct PendingPermission {
    /// Opaque host-side request id; pass back to `Host::resolve_permission`.
    pub id: u64,
    /// Tool the model wants to invoke.
    pub name: String,
    /// Short human-readable summary from the policy.
    pub summary: String,
    /// Full argument JSON, rendered (truncated) below the summary.
    pub args: Value,
}

/// One row in the transcript picker list.
#[derive(Debug, Clone)]
pub struct TranscriptEntry {
    /// Full path to the `.json` file.
    pub path: PathBuf,
    /// Human-readable timestamp label (e.g. `2026-05-10 14:32:01`).
    pub timestamp: String,
    /// First user message text, truncated for preview.
    pub preview: String,
    /// Total number of messages in the transcript.
    pub message_count: usize,
}

/// One row in the conversation log.
#[derive(Debug, Clone)]
pub enum Entry {
    /// Submitted user prompt.
    User(String),
    /// Finalized assistant text.
    Assistant(String),
    /// Tool the model is calling (or just called). `status = None` means in-flight.
    Tool {
        /// Tool name.
        name: String,
        /// One-line summary of the JSON arguments.
        arguments: String,
        /// Outcome (None while running).
        status: Option<ToolCallStatus>,
        /// Truncated payload (only set after completion).
        result_preview: Option<String>,
    },
    /// Local notice — file ops, errors, transcript notifications.
    Note(String),
}

/// Slash command shown in the palette.
pub struct Command {
    /// Including the leading slash.
    pub name: String,
    /// One-liner shown in the palette.
    pub description: String,
    /// `true` for commands that take an argument (e.g. `/view <path>`). When
    /// the user picks one of these from the palette we prefill the prompt
    /// instead of executing it; commands without args run on Enter.
    pub needs_arg: bool,
}

/// Parsed `/bash` slash-command suffix. The TUI uses this to thread an
/// optional per-call network override down to
/// [`savvagent_host::Host::run_bash_command`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashCommand {
    /// Per-call override of `tool-bash`'s network access. `None` means
    /// "use the resolved permission decision".
    pub net_override: Option<bool>,
    /// The shell command itself, stripped of recognised flags.
    pub command: String,
}

/// Error returned by [`parse_bash_command`].
#[derive(Debug, PartialEq, Eq)]
pub enum BashCommandError {
    /// The user typed `/bash` (or `/bash --net`) with nothing after.
    EmptyCommand,
    /// The user typed a dashed token at the start of the command that
    /// wasn't `--net` or `--no-net`. We surface these as errors so a
    /// typo can't silently fall through to being treated as a literal
    /// shell command — important for a security-relevant opt-in flag.
    UnknownFlag {
        /// The exact token we couldn't recognise (e.g. `-net`, `--Net`,
        /// `--net=true`, `--quiet`).
        token: String,
    },
}

impl std::fmt::Display for BashCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BashCommandError::EmptyCommand => write!(f, "bash command is empty"),
            BashCommandError::UnknownFlag { token } => write!(
                f,
                "unknown bash flag `{token}` — only `--net` and `--no-net` are recognised"
            ),
        }
    }
}

impl std::error::Error for BashCommandError {}

/// Parse the suffix of a `/bash` slash command. Recognises a leading
/// `--net` / `--no-net` flag and returns the rest verbatim as `command`.
///
/// The flag must appear *first* — `echo --net hi` is a literal command,
/// not a flag-prefixed invocation. This keeps quoting simple: anything
/// after the (optional) leading flag is forwarded as-is to `bash -c`.
///
/// Strict-flag rule: when the input starts with `-`, the first
/// whitespace-separated token MUST be exactly `--net` or `--no-net`.
/// Anything else (`-net`, `--Net`, `--net=true`, `--quiet`, …) is
/// returned as [`BashCommandError::UnknownFlag`] so a typo on this
/// security-relevant opt-in flag can never silently degrade into "run
/// the typo as a literal command".
pub fn parse_bash_command(input: &str) -> Result<BashCommand, BashCommandError> {
    let trimmed = input.trim_start();
    if trimmed.is_empty() {
        return Err(BashCommandError::EmptyCommand);
    }

    // If the input starts with `-`, the first token must be exactly
    // `--net` or `--no-net`. Any other dashed token is a typo we want
    // to surface rather than silently treat as a shell command.
    if trimmed.starts_with('-') {
        let (token, rest) = match trimmed.split_once(char::is_whitespace) {
            Some((t, r)) => (t, r.trim_start()),
            None => (trimmed, ""),
        };
        let net_override = match token {
            "--net" => Some(true),
            "--no-net" => Some(false),
            other => {
                return Err(BashCommandError::UnknownFlag {
                    token: other.to_string(),
                });
            }
        };
        if rest.is_empty() {
            return Err(BashCommandError::EmptyCommand);
        }
        return Ok(BashCommand {
            net_override,
            command: rest.to_string(),
        });
    }

    Ok(BashCommand {
        net_override: None,
        command: trimmed.to_string(),
    })
}

/// Outcome of [`App::select_command`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandSelection {
    /// The command takes no argument — caller should run it now.
    Execute(String),
    /// The command takes an argument — the prompt has been prefilled with
    /// `"<name> "` and we're back in editing mode for the user to type it.
    Prefill(String),
}

/// TUI app state.
pub struct App {
    pub input_textarea: TextArea<'static>,
    pub input_mode: InputMode,
    pub model: String,
    pub transcript_dir: PathBuf,

    /// Finalized + in-progress conversation entries.
    pub entries: Vec<Entry>,
    /// Live token buffer for the assistant turn currently streaming.
    pub live_text: String,
    /// True while a turn is in flight.
    pub is_loading: bool,
    /// Set by `/quit` or Ctrl-C to break the event loop.
    pub should_quit: bool,
    /// Approximate context size (chars / 4) — naive token estimate.
    pub context_size: usize,
    /// Most recent transcript path written this session.
    pub last_transcript: Option<PathBuf>,

    pub is_file_picker_active: bool,
    pub file_explorer: FileExplorer,
    pub editor: Option<Editor>,
    pub active_file_path: Option<PathBuf>,

    pub commands: Vec<Command>,
    pub command_index: usize,
    /// Live filter typed after `/` while the command palette is open. Without
    /// the leading slash. Empty when the palette was opened via Ctrl-P.
    pub palette_filter: String,

    /// True once `/connect` has linked the TUI to a running provider.
    pub connected: bool,
    /// Provider id currently in use (`anthropic`, `gemini`, …).
    pub active_provider_id: Option<&'static str>,
    /// Cursor in the provider selector.
    pub provider_index: usize,
    /// Masked input for the API key (only populated during `EnteringApiKey`).
    pub api_key_textarea: TextArea<'static>,
    /// Provider chosen in the selector and being keyed in now.
    pub pending_provider: Option<&'static ProviderSpec>,

    /// Whether the startup splash banner is still being shown. Cleared on the
    /// first key press the main loop sees (any key, including modifiers), or
    /// after [`SPLASH_DURATION`] elapses since [`splash_shown_at`].
    pub show_splash: bool,
    /// When the splash was first painted. Used by the main loop to auto-dismiss
    /// after [`SPLASH_DURATION`] when the user doesn't press a key.
    pub splash_shown_at: Instant,

    /// Active permission request, if the host is paused on a `oneshot`. Set
    /// when `TurnEvent::PermissionRequested` arrives, cleared when the user
    /// answers the modal.
    pub pending_permission: Option<PendingPermission>,

    // --- /resume transcript picker ---
    /// Transcript files available for resumption, sorted newest-first.
    pub transcript_entries: Vec<TranscriptEntry>,
    /// Highlighted row in the transcript picker.
    pub transcript_index: usize,

    /// When the current session was resumed from a saved transcript, this
    /// holds a human-readable timestamp string shown in the header.
    pub resumed_at: Option<String>,

    /// Theme applied to the render path. Loaded from
    /// `~/.savvagent/theme.toml` at startup, mutated by `/theme <name>`,
    /// and persisted on every successful set.
    pub active_theme: crate::theme::Theme,
}

impl App {
    /// Build TUI state. The host runs out-of-band; the app only carries the
    /// model name (for the header), the directory transcripts get written
    /// into, and the conversation log it builds from streaming events.
    pub fn new(model: String, transcript_dir: PathBuf) -> Self {
        let theme = Theme::default()
            .add_default_title()
            .with_block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded),
            )
            .with_highlight_item_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .with_highlight_dir_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .with_highlight_symbol("> ");

        let file_explorer = FileExplorerBuilder::build_with_theme(theme)
            .expect("failed to initialize file explorer");

        let mut app = Self {
            input_textarea: TextArea::default(),
            input_mode: InputMode::Editing,
            model,
            transcript_dir,
            entries: Vec::new(),
            live_text: String::new(),
            is_loading: false,
            should_quit: false,
            context_size: 0,
            last_transcript: None,
            is_file_picker_active: false,
            file_explorer,
            editor: None,
            active_file_path: None,
            commands: Vec::new(),
            command_index: 0,
            palette_filter: String::new(),
            connected: false,
            active_provider_id: None,
            provider_index: 0,
            api_key_textarea: TextArea::default(),
            pending_provider: None,
            show_splash: true,
            splash_shown_at: Instant::now(),
            pending_permission: None,
            transcript_entries: Vec::new(),
            transcript_index: 0,
            resumed_at: None,
            active_theme: crate::theme::load(),
        };
        app.refresh_commands();
        app
    }

    /// Apply one streaming event from the host into the conversation log.
    pub fn apply_turn_event(&mut self, event: TurnEvent) {
        match event {
            TurnEvent::IterationStarted { .. } => {}
            TurnEvent::TextDelta { text } => {
                self.live_text.push_str(&text);
            }
            TurnEvent::ToolCallStarted { name, arguments } => {
                // If we have buffered streaming text from this iteration,
                // commit it as a finalized assistant entry first.
                self.flush_live_text();
                self.entries.push(Entry::Tool {
                    name,
                    arguments: summarize_args(&arguments),
                    status: None,
                    result_preview: None,
                });
            }
            TurnEvent::ToolCallFinished {
                name: _,
                status,
                result,
            } => {
                if let Some(Entry::Tool {
                    status: s,
                    result_preview: p,
                    ..
                }) = self
                    .entries
                    .iter_mut()
                    .rev()
                    .find(|e| matches!(e, Entry::Tool { status: None, .. }))
                {
                    *s = Some(status);
                    *p = Some(truncate(&result, 240));
                }
            }
            TurnEvent::PermissionRequested {
                id,
                name,
                summary,
                args,
            } => {
                self.flush_live_text();
                self.pending_permission = Some(PendingPermission {
                    id,
                    name,
                    summary,
                    args,
                });
                self.input_mode = InputMode::PermissionPrompt;
            }
            TurnEvent::BashNetworkRequested { id, summary } => {
                self.flush_live_text();
                self.entries.push(Entry::Note(format!(
                    "bash network access requested — see modal ({summary})"
                )));
                self.input_mode = InputMode::BashNetworkPrompt { id, summary };
            }
            TurnEvent::ToolCallDenied { name, reason } => {
                self.flush_live_text();
                self.entries
                    .push(Entry::Note(format!("denied {name}: {reason}")));
            }
            TurnEvent::TurnComplete { outcome } => {
                // If streaming delivered text deltas, flush them. Otherwise
                // fall back to the authoritative final text on the outcome —
                // a non-streaming provider, or a dropped delta, would
                // otherwise leave the user with no visible reply.
                if !self.live_text.is_empty() {
                    self.flush_live_text();
                } else if !outcome.text.is_empty() {
                    self.entries.push(Entry::Assistant(outcome.text));
                } else {
                    self.entries.push(Entry::Note(format!(
                        "(turn ended with no text · iterations={} · tool_calls={})",
                        outcome.iterations,
                        outcome.tool_calls.len()
                    )));
                }
                self.is_loading = false;
            }
        }
    }

    /// Move any buffered streaming text into a finalized assistant entry.
    fn flush_live_text(&mut self) {
        if self.live_text.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.live_text);
        self.entries.push(Entry::Assistant(text));
    }

    /// Convenience: append a user-visible note (file ops, errors, system messages).
    pub fn push_note(&mut self, msg: impl Into<String>) {
        self.entries.push(Entry::Note(msg.into()));
        self.update_metrics();
    }

    /// Append a user prompt to the log (call before spawning the streaming task).
    pub fn push_user(&mut self, text: String) {
        self.entries.push(Entry::User(text));
        self.update_metrics();
    }

    /// Recompute the rough context-size estimate.
    pub fn update_metrics(&mut self) {
        let chars: usize = self
            .entries
            .iter()
            .map(|e| match e {
                Entry::User(t) | Entry::Assistant(t) | Entry::Note(t) => t.len(),
                Entry::Tool {
                    arguments,
                    result_preview,
                    ..
                } => arguments.len() + result_preview.as_deref().map(str::len).unwrap_or(0),
            })
            .sum::<usize>()
            + self.live_text.len();
        self.context_size = chars / 4;
    }

    /// Slash commands surfaced in the palette.
    pub fn refresh_commands(&mut self) {
        self.commands = vec![
            Command {
                name: "/connect".into(),
                description: "Switch provider (uses stored key, or prompts if missing)".into(),
                needs_arg: false,
            },
            Command {
                name: "/clear".into(),
                description: "Reset conversation history".into(),
                needs_arg: false,
            },
            Command {
                name: "/save".into(),
                description: "Save transcript now".into(),
                needs_arg: false,
            },
            Command {
                name: "/view".into(),
                description: "View a file".into(),
                needs_arg: true,
            },
            Command {
                name: "/edit".into(),
                description: "Edit a file".into(),
                needs_arg: true,
            },
            Command {
                name: "/tools".into(),
                description: "List registered tools and their default permission verdict".into(),
                needs_arg: false,
            },
            Command {
                name: "/model".into(),
                description: "Show the current model (or `/model <id>` to switch)".into(),
                needs_arg: false,
            },
            Command {
                name: "/resume".into(),
                description: "Resume a saved transcript (opens picker, or /resume <path>)".into(),
                needs_arg: false,
            },
            Command {
                name: "/sandbox".into(),
                description: "Show sandbox status (`/sandbox on` or `/sandbox off` to toggle)"
                    .into(),
                needs_arg: false,
            },
            Command {
                name: "/theme".into(),
                description: "List themes or set one (`/theme list` or `/theme <name>`)".into(),
                needs_arg: false,
            },
            Command {
                name: "/bash".into(),
                description: "Run a shell command (use `--net` / `--no-net` to override network)"
                    .into(),
                needs_arg: true,
            },
            Command {
                name: "/quit".into(),
                description: "Quit".into(),
                needs_arg: false,
            },
        ];
    }

    /// Open the command palette.
    pub fn open_command_palette(&mut self) {
        self.refresh_commands();
        self.input_mode = InputMode::CommandPalette;
        self.command_index = 0;
        self.palette_filter.clear();
    }

    /// Indices into `self.commands` that match the current filter. If the
    /// filter is empty, returns every index.
    pub fn filtered_command_indices(&self) -> Vec<usize> {
        if self.palette_filter.is_empty() {
            return (0..self.commands.len()).collect();
        }
        let needle = self.palette_filter.to_lowercase();
        self.commands
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                let name = c.name.strip_prefix('/').unwrap_or(&c.name).to_lowercase();
                name.starts_with(&needle)
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Append a char to the palette filter and reset the cursor.
    pub fn palette_push_char(&mut self, c: char) {
        self.palette_filter.push(c);
        self.command_index = 0;
    }

    /// Pop one char from the palette filter. Returns `false` if it was already
    /// empty (the caller can use this to close the palette on Backspace past
    /// the leading `/`).
    pub fn palette_pop_char(&mut self) -> bool {
        let popped = self.palette_filter.pop().is_some();
        self.command_index = 0;
        popped
    }

    /// Close the palette without selecting anything.
    pub fn close_command_palette(&mut self) {
        self.input_mode = InputMode::Editing;
        self.palette_filter.clear();
        self.command_index = 0;
    }

    /// Resolve the highlighted palette item. Operates on the filtered view —
    /// `command_index` is the position within the visible list, not within
    /// `self.commands`. Closes the palette either way; returns whether the
    /// caller should execute the command now or just leave the prefilled
    /// prompt for the user to finish typing arguments.
    pub fn select_command(&mut self) -> Option<CommandSelection> {
        let filtered = self.filtered_command_indices();
        let real_idx = match filtered.get(self.command_index).copied() {
            Some(i) => i,
            None => {
                self.close_command_palette();
                return None;
            }
        };
        let cmd = &self.commands[real_idx];
        let name = cmd.name.clone();
        let needs_arg = cmd.needs_arg;

        self.input_mode = InputMode::Editing;
        self.palette_filter.clear();
        self.command_index = 0;

        if needs_arg {
            self.input_textarea = TextArea::from(vec![format!("{name} ")]);
            Some(CommandSelection::Prefill(name))
        } else {
            self.input_textarea = TextArea::default();
            Some(CommandSelection::Execute(name))
        }
    }

    /// Insert the currently-highlighted file as `@path` in the textarea.
    pub fn file_picker_select(&mut self) {
        let file = self.file_explorer.current();
        if file.is_dir {
            return;
        }
        let path = file.path.clone();

        let mut current = self.input_textarea.lines().join("\n");
        if let Some(last_at) = current.rfind('@') {
            current.truncate(last_at + 1);
            current.push_str(&path.to_string_lossy());
        } else {
            if !current.is_empty() && !current.ends_with(' ') {
                current.push(' ');
            }
            current.push('@');
            current.push_str(&path.to_string_lossy());
        }
        self.input_textarea = TextArea::from(current.lines().map(|s| s.to_string()));
        let row = self.input_textarea.lines().len().saturating_sub(1) as u16;
        let col = self
            .input_textarea
            .lines()
            .last()
            .map(|l| l.len())
            .unwrap_or(0) as u16;
        self.input_textarea
            .move_cursor(tui_textarea::CursorMove::Jump(row, col));
        self.close_file_picker();
    }

    /// Show the file-picker popup.
    pub fn open_file_picker(&mut self) {
        self.is_file_picker_active = true;
    }

    /// Hide the file-picker popup.
    pub fn close_file_picker(&mut self) {
        self.is_file_picker_active = false;
    }

    /// Open `path` in the popup editor (read-only or read-write per `edit`).
    pub fn open_file(&mut self, path: PathBuf, edit: bool) {
        if !path.exists() {
            self.push_note(format!("File not found: {}", path.display()));
            return;
        }
        let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("txt");
        let lang = match extension {
            "rs" => "rust",
            "py" => "python",
            "js" => "javascript",
            "ts" => "typescript",
            "json" => "json",
            "toml" => "toml",
            "yml" | "yaml" => "yaml",
            "md" => "markdown",
            _ => "text",
        };
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                match Editor::new(lang, &content, ratatui_code_editor::theme::vesper()) {
                    Ok(editor) => {
                        self.editor = Some(editor);
                        self.active_file_path = Some(path);
                        self.input_mode = if edit {
                            InputMode::EditingFile
                        } else {
                            InputMode::ViewingFile
                        };
                    }
                    Err(e) => self.push_note(format!("Editor error: {e}")),
                }
            }
            Err(e) => self.push_note(format!("Read error: {e}")),
        }
    }

    /// Persist the open editor's buffer to disk.
    pub fn save_file(&mut self) {
        let Some(path) = self.active_file_path.clone() else {
            return;
        };
        let Some(editor) = &self.editor else { return };
        let content = editor.get_content();
        match std::fs::write(&path, content) {
            Ok(_) => self.push_note(format!("Saved {}", path.display())),
            Err(e) => self.push_note(format!("Write error: {e}")),
        }
    }

    /// Populate `transcript_entries` from `dir` and enter the picker mode.
    ///
    /// Entries are sorted newest-first by the Unix timestamp embedded in the
    /// filename (`<unix>.json`). Files that cannot be parsed as JSON are
    /// silently skipped so a single corrupt file doesn't break the whole
    /// picker.
    pub fn open_transcript_picker(&mut self, dir: &std::path::Path) {
        self.transcript_entries = collect_transcript_entries(dir);
        self.transcript_index = 0;
        self.input_mode = InputMode::SelectingTranscript;
    }

    /// Close the transcript picker without selecting anything.
    pub fn close_transcript_picker(&mut self) {
        self.transcript_entries.clear();
        self.transcript_index = 0;
        self.input_mode = InputMode::Editing;
    }

    /// Return the path of the currently-highlighted transcript entry, if any.
    pub fn selected_transcript_path(&self) -> Option<&std::path::Path> {
        self.transcript_entries
            .get(self.transcript_index)
            .map(|e| e.path.as_path())
    }

    /// Replay a loaded transcript into the visible conversation log as
    /// "history" entries. Tool-use blocks are rendered with `[history]` status
    /// so they look distinct from live calls. Called after `load_transcript`
    /// succeeds so the user can see prior context.
    pub fn replay_transcript(&mut self, record: &TranscriptFile) {
        use savvagent_protocol::{ContentBlock, Role};

        self.entries.clear();
        self.live_text.clear();

        for msg in &record.messages {
            match msg.role {
                Role::User => {
                    // Collect text blocks; skip tool_result blocks (they're
                    // the host's synthetic responses — not user prose).
                    let text: String = msg
                        .content
                        .iter()
                        .filter_map(|b| {
                            if let ContentBlock::Text { text } = b {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !text.is_empty() {
                        self.entries.push(Entry::User(text));
                    }
                }
                Role::Assistant => {
                    for block in &msg.content {
                        match block {
                            ContentBlock::Text { text } if !text.is_empty() => {
                                self.entries.push(Entry::Assistant(text.clone()));
                            }
                            ContentBlock::ToolUse { name, input, .. } => {
                                self.entries.push(Entry::Tool {
                                    name: name.clone(),
                                    arguments: summarize_args(input),
                                    status: Some(ToolCallStatus::Ok),
                                    result_preview: Some("[history]".into()),
                                });
                            }
                            ContentBlock::Thinking { .. } => {
                                // Signal a thinking block occurred without
                                // dumping the raw chain-of-thought into the
                                // visible log. Rendered dimmed via Note.
                                self.entries.push(Entry::Note("[thinking]".into()));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        self.update_metrics();
    }

    /// Dispatch a `/...` command. Returns `true` if it was a slash command.
    pub fn handle_command(&mut self, command: &str) -> bool {
        let parts: Vec<&str> = command.split_whitespace().collect();
        let Some(head) = parts.first() else {
            return false;
        };
        match *head {
            "/connect" => {
                self.open_provider_selector();
                true
            }
            "/clear" => {
                self.entries.clear();
                self.live_text.clear();
                self.update_metrics();
                self.push_note("History cleared.");
                true
            }
            "/save" => {
                self.push_note("Saving transcript…");
                true
            }
            "/view" => {
                if let Some(p) = parts.get(1) {
                    let s = p.strip_prefix('@').unwrap_or(p);
                    self.open_file(PathBuf::from(s), false);
                }
                true
            }
            "/edit" => {
                if let Some(p) = parts.get(1) {
                    let s = p.strip_prefix('@').unwrap_or(p);
                    self.open_file(PathBuf::from(s), true);
                }
                true
            }
            "/quit" => {
                self.should_quit = true;
                true
            }
            _ if head.starts_with('/') => {
                self.push_note(format!("Unknown command: {head}"));
                true
            }
            _ => false,
        }
    }

    /// Open the `/connect` provider selector.
    pub fn open_provider_selector(&mut self) {
        self.provider_index = self
            .active_provider_id
            .and_then(|id| PROVIDERS.iter().position(|p| p.id == id))
            .unwrap_or(0);
        self.input_mode = InputMode::SelectingProvider;
    }

    /// Advance from provider selection to API-key entry, or cancel if `idx` is OOB.
    pub fn enter_api_key_for(&mut self, idx: usize) {
        let Some(spec) = PROVIDERS.get(idx) else {
            self.input_mode = InputMode::Editing;
            return;
        };
        self.pending_provider = Some(spec);
        let mut ta = TextArea::default();
        ta.set_mask_char('●');
        ta.set_placeholder_text(format!("Paste your {} key", spec.api_key_env));
        self.api_key_textarea = ta;
        self.input_mode = InputMode::EnteringApiKey;
    }

    /// Read the masked input, then clear it.
    pub fn take_pending_api_key(&mut self) -> Option<(&'static ProviderSpec, String)> {
        let spec = self.pending_provider.take()?;
        let key = self.api_key_textarea.lines().join("");
        self.api_key_textarea = TextArea::default();
        if key.is_empty() {
            return None;
        }
        Some((spec, key))
    }

    /// Abort the `/connect` flow and return to the prompt.
    pub fn cancel_connect(&mut self) {
        self.pending_provider = None;
        self.api_key_textarea = TextArea::default();
        self.input_mode = InputMode::Editing;
    }
}

fn summarize_args(value: &Value) -> String {
    let s = serde_json::to_string(value).unwrap_or_default();
    truncate(&s, 80)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// Scan `dir` for `*.json` transcript files and return picker rows
/// sorted newest-first.
///
/// Uses two strategies for ordering:
/// 1. The `saved_at` timestamp inside the file (versioned format).
/// 2. The numeric stem of the filename (`<unix>.json`) for legacy files.
///
/// Files that cannot be read or parsed as JSON are silently skipped.
pub fn collect_transcript_entries(dir: &std::path::Path) -> Vec<TranscriptEntry> {
    use savvagent_protocol::ContentBlock;

    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut entries: Vec<(u64, TranscriptEntry)> = Vec::new();

    for item in read_dir.flatten() {
        let path = item.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }

        // Try to parse for metadata. On any failure, skip.
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(root) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };

        let (saved_at, messages) = match &root {
            serde_json::Value::Object(map) if map.contains_key("schema_version") => {
                // `Host::load_transcript` requires the full `TranscriptFile`
                // (with non-Option `messages`) to deserialize, so a row whose
                // `messages` field is missing or unparseable would always
                // produce a `Malformed` error on selection. Skip those —
                // consistent with the docstring contract above.
                let Some(msgs_val) = map.get("messages") else {
                    continue;
                };
                let Ok(msgs) =
                    serde_json::from_value::<Vec<savvagent_protocol::Message>>(msgs_val.clone())
                else {
                    continue;
                };
                let sa = map.get("saved_at").and_then(|v| v.as_u64()).unwrap_or(0);
                (sa, msgs)
            }
            serde_json::Value::Array(_) => {
                let Ok(msgs) =
                    serde_json::from_value::<Vec<savvagent_protocol::Message>>(root.clone())
                else {
                    continue;
                };
                // Fall back to stem-as-timestamp for legacy files.
                let sa = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                (sa, msgs)
            }
            _ => continue,
        };

        // Sort key: prefer saved_at, fall back to stem.
        let sort_key = if saved_at > 0 {
            saved_at
        } else {
            path.file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0)
        };

        let timestamp = if saved_at > 0 {
            format_unix_ts(saved_at)
        } else {
            // Legacy: stem is already the unix ts.
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
            stem.parse::<u64>()
                .map(format_unix_ts)
                .unwrap_or_else(|_| stem.to_owned())
        };

        // First user message text as preview.
        let preview = messages
            .iter()
            .find(|m| m.role == savvagent_protocol::Role::User)
            .and_then(|m| {
                m.content.iter().find_map(|b| {
                    if let ContentBlock::Text { text } = b {
                        Some(truncate(text, 60))
                    } else {
                        None
                    }
                })
            })
            .unwrap_or_else(|| "(empty)".into());

        entries.push((
            sort_key,
            TranscriptEntry {
                path,
                timestamp,
                preview,
                message_count: messages.len(),
            },
        ));
    }

    // Newest first.
    entries.sort_by_key(|e| std::cmp::Reverse(e.0));
    entries.into_iter().map(|(_, e)| e).collect()
}

/// Format a Unix timestamp as a local-time-like string.
/// Uses naive UTC formatting since we don't pull in a chrono dep.
fn format_unix_ts(secs: u64) -> String {
    // Simple: express as YYYY-MM-DD HH:MM:SS UTC.
    let s = secs;
    let sec = s % 60;
    let min = (s / 60) % 60;
    let hour = (s / 3600) % 24;
    let days = s / 86400;
    // Days since Unix epoch → Gregorian calendar (Proleptic).
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{min:02}:{sec:02}")
}

/// Minimal Gregorian calendar conversion for Unix-epoch day count.
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from https://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fresh_app() -> App {
        App::new("test-model".into(), PathBuf::from("/tmp"))
    }

    #[test]
    fn empty_filter_lists_every_command() {
        let app = fresh_app();
        let filtered = app.filtered_command_indices();
        assert_eq!(filtered.len(), app.commands.len());
    }

    #[test]
    fn filter_narrows_by_prefix_case_insensitive() {
        let mut app = fresh_app();
        app.palette_filter = "co".into();
        let names: Vec<&str> = app
            .filtered_command_indices()
            .into_iter()
            .map(|i| app.commands[i].name.as_str())
            .collect();
        assert_eq!(names, vec!["/connect"]);

        app.palette_filter = "C".into();
        let names: Vec<&str> = app
            .filtered_command_indices()
            .into_iter()
            .map(|i| app.commands[i].name.as_str())
            .collect();
        assert!(names.contains(&"/connect"));
        assert!(names.contains(&"/clear"));
    }

    #[test]
    fn filter_with_no_matches_returns_empty_list() {
        let mut app = fresh_app();
        app.palette_filter = "xyz".into();
        assert!(app.filtered_command_indices().is_empty());
    }

    #[test]
    fn select_no_arg_command_returns_execute_with_empty_input() {
        let mut app = fresh_app();
        app.input_mode = InputMode::CommandPalette;
        app.palette_filter = "c".into();
        // Two visible commands at this point: /connect (0) and /clear (1).
        app.command_index = 1;
        let outcome = app.select_command();
        assert_eq!(outcome, Some(CommandSelection::Execute("/clear".into())));
        assert!(matches!(app.input_mode, InputMode::Editing));
        assert_eq!(app.input_textarea.lines(), &[String::new()]);
        assert!(app.palette_filter.is_empty());
    }

    #[test]
    fn select_arg_command_returns_prefill_with_seeded_input() {
        let mut app = fresh_app();
        app.input_mode = InputMode::CommandPalette;
        app.palette_filter = "vi".into();
        app.command_index = 0;
        let outcome = app.select_command();
        assert_eq!(outcome, Some(CommandSelection::Prefill("/view".into())));
        assert_eq!(app.input_textarea.lines(), &["/view ".to_string()]);
    }

    #[test]
    fn select_with_no_match_closes_palette() {
        let mut app = fresh_app();
        app.input_mode = InputMode::CommandPalette;
        app.palette_filter = "zzz".into();
        let outcome = app.select_command();
        assert!(outcome.is_none());
        assert!(matches!(app.input_mode, InputMode::Editing));
    }

    #[test]
    fn pop_past_empty_signals_close() {
        let mut app = fresh_app();
        app.palette_push_char('c');
        assert!(app.palette_pop_char());
        assert!(!app.palette_pop_char());
    }

    #[test]
    fn permission_request_enters_prompt_mode() {
        let mut app = fresh_app();
        app.apply_turn_event(TurnEvent::PermissionRequested {
            id: 42,
            name: "run".into(),
            summary: "run: ls".into(),
            args: serde_json::json!({"command": "ls"}),
        });
        assert!(matches!(app.input_mode, InputMode::PermissionPrompt));
        let req = app.pending_permission.expect("pending should be set");
        assert_eq!(req.id, 42);
        assert_eq!(req.name, "run");
    }

    #[test]
    fn bash_command_parses_net_flag() {
        let p = parse_bash_command("--net curl https://example.com").unwrap();
        assert_eq!(p.net_override, Some(true));
        assert_eq!(p.command, "curl https://example.com");
    }

    #[test]
    fn bash_command_parses_no_net_flag() {
        let p = parse_bash_command("--no-net ls /tmp").unwrap();
        assert_eq!(p.net_override, Some(false));
        assert_eq!(p.command, "ls /tmp");
    }

    #[test]
    fn bash_command_without_flag_has_no_override() {
        let p = parse_bash_command("ls /tmp").unwrap();
        assert_eq!(p.net_override, None);
        assert_eq!(p.command, "ls /tmp");
    }

    #[test]
    fn bash_command_flag_only_recognised_at_start() {
        // A --net mid-command is part of the command body.
        let p = parse_bash_command("echo --net hi").unwrap();
        assert_eq!(p.net_override, None);
        assert_eq!(p.command, "echo --net hi");
    }

    #[test]
    fn bash_command_empty_after_flag_is_an_error() {
        assert!(matches!(
            parse_bash_command("--net   ").unwrap_err(),
            BashCommandError::EmptyCommand
        ));
        assert!(matches!(
            parse_bash_command("").unwrap_err(),
            BashCommandError::EmptyCommand
        ));
    }

    #[test]
    fn bash_command_leading_whitespace_trimmed() {
        let p = parse_bash_command("   --net  echo hi").unwrap();
        assert_eq!(p.net_override, Some(true));
        assert_eq!(p.command, "echo hi");
    }

    #[test]
    fn bash_command_rejects_single_dash_typo() {
        let err = parse_bash_command("-net curl foo").unwrap_err();
        assert!(matches!(err, BashCommandError::UnknownFlag { .. }));
    }

    #[test]
    fn bash_command_rejects_capitalised_flag() {
        assert!(matches!(
            parse_bash_command("--Net curl foo").unwrap_err(),
            BashCommandError::UnknownFlag { .. }
        ));
    }

    #[test]
    fn bash_command_rejects_net_with_equals() {
        assert!(matches!(
            parse_bash_command("--net=true curl foo").unwrap_err(),
            BashCommandError::UnknownFlag { .. }
        ));
    }

    #[test]
    fn bash_command_rejects_unknown_dash_token() {
        assert!(matches!(
            parse_bash_command("--quiet ls").unwrap_err(),
            BashCommandError::UnknownFlag { .. }
        ));
    }

    #[test]
    fn bash_command_net_alone_without_command_is_an_error() {
        // `--net` followed by only whitespace — must error EmptyCommand,
        // not UnknownFlag.
        assert!(matches!(
            parse_bash_command("--net").unwrap_err(),
            BashCommandError::EmptyCommand
        ));
    }

    #[test]
    fn bash_network_request_enters_modal_with_id_and_summary() {
        let mut app = fresh_app();
        app.apply_turn_event(TurnEvent::BashNetworkRequested {
            id: 7,
            summary: "tool-bash spawn requests network access".into(),
        });
        match &app.input_mode {
            InputMode::BashNetworkPrompt { id, summary } => {
                assert_eq!(*id, 7);
                assert!(summary.contains("tool-bash"), "summary: {summary}");
            }
            other => panic!(
                "expected BashNetworkPrompt, got {:?}",
                input_mode_label(other)
            ),
        }
    }

    fn input_mode_label(m: &InputMode) -> &'static str {
        match m {
            InputMode::Editing => "Editing",
            InputMode::ViewingFile => "ViewingFile",
            InputMode::EditingFile => "EditingFile",
            InputMode::CommandPalette => "CommandPalette",
            InputMode::SelectingProvider => "SelectingProvider",
            InputMode::EnteringApiKey => "EnteringApiKey",
            InputMode::PermissionPrompt => "PermissionPrompt",
            InputMode::BashNetworkPrompt { .. } => "BashNetworkPrompt",
            InputMode::SelectingTranscript => "SelectingTranscript",
        }
    }
}
