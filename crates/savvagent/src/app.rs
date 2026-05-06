//! TUI state. The app holds a shared [`Host`] and a render-friendly
//! conversation log built incrementally from streaming [`TurnEvent`]s.

use std::path::PathBuf;

use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Borders};
use ratatui_code_editor::editor::Editor;
use ratatui_explorer::{FileExplorer, FileExplorerBuilder, Theme};
use savvagent_host::{ToolCallStatus, TurnEvent};
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
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            )
            .with_highlight_dir_style(
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
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
            connected: false,
            active_provider_id: None,
            provider_index: 0,
            api_key_textarea: TextArea::default(),
            pending_provider: None,
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
            TurnEvent::ToolCallFinished { name: _, status, result } => {
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
                Entry::Tool { arguments, result_preview, .. } => {
                    arguments.len() + result_preview.as_deref().map(str::len).unwrap_or(0)
                }
            })
            .sum::<usize>()
            + self.live_text.len();
        self.context_size = chars / 4;
    }

    /// Slash commands surfaced in Ctrl-P.
    pub fn refresh_commands(&mut self) {
        self.commands = vec![
            Command { name: "/connect".into(), description: "Choose a provider and set its API key".into() },
            Command { name: "/clear".into(), description: "Reset conversation history".into() },
            Command { name: "/save".into(), description: "Save transcript now".into() },
            Command { name: "/view".into(), description: "View a file".into() },
            Command { name: "/edit".into(), description: "Edit a file".into() },
            Command { name: "/quit".into(), description: "Quit".into() },
        ];
    }

    /// Open the command palette.
    pub fn open_command_palette(&mut self) {
        self.refresh_commands();
        self.input_mode = InputMode::CommandPalette;
        self.command_index = 0;
    }

    /// Pre-fill the input textarea with the selected command name.
    pub fn select_command(&mut self) {
        let cmd = self.commands[self.command_index].name.clone();
        self.input_textarea = TextArea::from(vec![format!("{cmd} ")]);
        self.input_mode = InputMode::Editing;
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
        let col = self.input_textarea.lines().last().map(|l| l.len()).unwrap_or(0) as u16;
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
            Ok(content) => match Editor::new(lang, &content, ratatui_code_editor::theme::vesper()) {
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
            },
            Err(e) => self.push_note(format!("Read error: {e}")),
        }
    }

    /// Persist the open editor's buffer to disk.
    pub fn save_file(&mut self) {
        let Some(path) = self.active_file_path.clone() else { return };
        let Some(editor) = &self.editor else { return };
        let content = editor.get_content();
        match std::fs::write(&path, content) {
            Ok(_) => self.push_note(format!("Saved {}", path.display())),
            Err(e) => self.push_note(format!("Write error: {e}")),
        }
    }

    /// Dispatch a `/...` command. Returns `true` if it was a slash command.
    pub fn handle_command(&mut self, command: &str) -> bool {
        let parts: Vec<&str> = command.split_whitespace().collect();
        let Some(head) = parts.first() else { return false };
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
