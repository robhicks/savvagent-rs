use crate::agent::{Agent, Message, Role};
use crate::llm::LlmProvider;
use ratatui_code_editor::editor::Editor;
use ratatui_explorer::{FileExplorer, Theme, FileExplorerBuilder};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, BorderType};
use std::fs;
use std::path::PathBuf;
use tui_input::Input;

pub enum InputMode {
    Normal,
    Editing,
    ViewingFile,
    EditingFile,
}

pub struct App {
    pub input: Input,
    pub input_mode: InputMode,
    pub agent: Agent,
    pub should_quit: bool,
    pub is_loading: bool,
    pub available_agents: Vec<String>,
    pub current_agent: String,
    pub current_provider: LlmProvider,
    pub current_locale: String,
    pub is_file_picker_active: bool,
    pub file_explorer: FileExplorer,
    pub editor: Option<Editor>,
    pub active_file_path: Option<PathBuf>,
}

impl App {
    pub fn new() -> Self {
        let available_agents = vec![
            "explore".to_string(),
            "general".to_string(),
            "test-specialist".to_string(),
        ];
        let current_agent = available_agents[1].clone();
        let current_provider = LlmProvider::Mock;
        let current_locale = "en".to_string();
        rust_i18n::set_locale(&current_locale);

        let theme = Theme::default()
            .add_default_title()
            .with_block(Block::default().borders(Borders::ALL).border_type(BorderType::Rounded))
            .with_highlight_item_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
            .with_highlight_dir_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
            .with_highlight_symbol("> ".into());

        let file_explorer = FileExplorerBuilder::build_with_theme(theme)
            .expect("failed to initialize file explorer");

        Self {
            input: Input::default(),
            input_mode: InputMode::Normal,
            agent: Agent::new(),
            should_quit: false,
            is_loading: false,
            available_agents,
            current_agent,
            current_provider,
            current_locale,
            is_file_picker_active: false,
            file_explorer,
            editor: None,
            active_file_path: None,
        }
    }

    pub fn open_file_picker(&mut self) {
        self.is_file_picker_active = true;
    }

    pub fn close_file_picker(&mut self) {
        self.is_file_picker_active = false;
    }

    pub fn file_picker_select(&mut self) {
        let file = self.file_explorer.current();
        if file.is_dir {
            return;
        }
        let path = file.path.clone();

        let mut current_val = self.input.value().to_string();
        if let Some(last_at) = current_val.rfind('@') {
            current_val.truncate(last_at + 1);
            current_val.push_str(&path.to_string_lossy());
        } else {
            if !current_val.is_empty() && !current_val.ends_with(' ') {
                current_val.push(' ');
            }
            current_val.push('@');
            current_val.push_str(&path.to_string_lossy());
        }
        self.input = Input::new(current_val);
        self.close_file_picker();
    }

    pub fn open_file(&mut self, path: PathBuf, edit: bool) {
        if !path.exists() {
            self.push_message(
                Role::Assistant,
                t!("file_not_found", path = path.display().to_string()).into(),
            );
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

        match fs::read_to_string(&path) {
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
                    Err(e) => {
                        self.push_message(
                            Role::Assistant,
                            format!("Error creating editor: {}", e),
                        );
                    }
                }
            }
            Err(e) => {
                self.push_message(
                    Role::Assistant,
                    t!("read_error", error = e.to_string()).into(),
                );
            }
        }
    }

    pub fn save_file(&mut self) {
        let Some(path) = self.active_file_path.clone() else { return };
        let Some(editor) = &self.editor else { return };
        let content = editor.get_content();
        match fs::write(&path, content) {
            Ok(_) => self.push_message(
                Role::Assistant,
                t!("file_saved", path = path.display().to_string()).into(),
            ),
            Err(e) => self.push_message(
                Role::Assistant,
                t!("write_error", error = e.to_string()).into(),
            ),
        }
    }

    pub fn push_message(&mut self, role: Role, content: String) {
        self.agent.add_message(Message {
            role,
            content,
            tool_calls: None,
            tool_call_id: None,
        });
    }

    pub fn handle_command(&mut self, command: &str) {
        let parts: Vec<&str> = command.split_whitespace().collect();
        if parts.is_empty() {
            return;
        }

        match parts[0] {
            "/agents" => {
                if parts.len() > 1 {
                    let agent_name = parts[1];
                    if self.available_agents.contains(&agent_name.to_string()) {
                        self.current_agent = agent_name.to_string();
                        self.push_message(
                            Role::Assistant,
                            t!("switched_agent", name = agent_name).into(),
                        );
                    } else {
                        self.push_message(
                            Role::Assistant,
                            t!(
                                "unknown_agent",
                                name = agent_name,
                                available = self.available_agents.join(", ")
                            )
                            .into(),
                        );
                    }
                } else {
                    self.push_message(
                        Role::Assistant,
                        t!(
                            "available_agents",
                            available = self.available_agents.join(", "),
                            current = &self.current_agent
                        )
                        .into(),
                    );
                }
            }
            "/llm" => {
                if parts.len() > 1 {
                    match parts[1] {
                        "openai" => {
                            self.current_provider = LlmProvider::OpenAI;
                            self.push_message(
                                Role::Assistant,
                                t!("switched_llm", provider = "OpenAI").into(),
                            );
                        }
                        "anthropic" => {
                            self.current_provider = LlmProvider::Anthropic;
                            self.push_message(
                                Role::Assistant,
                                t!("switched_llm", provider = "Anthropic").into(),
                            );
                        }
                        "gemini" => {
                            self.current_provider = LlmProvider::Gemini;
                            self.push_message(
                                Role::Assistant,
                                t!("switched_llm", provider = "Gemini").into(),
                            );
                        }
                        "mock" => {
                            self.current_provider = LlmProvider::Mock;
                            self.push_message(
                                Role::Assistant,
                                t!("switched_llm", provider = "Mock").into(),
                            );
                        }
                        _ => {
                            self.push_message(
                                Role::Assistant,
                                t!("unknown_llm", name = parts[1]).into(),
                            );
                        }
                    }
                } else {
                    self.push_message(
                        Role::Assistant,
                        t!("current_llm", provider = format!("{:?}", self.current_provider))
                            .into(),
                    );
                }
            }
            "/locale" => {
                if parts.len() > 1 {
                    let locale = parts[1];
                    match locale {
                        "en" | "es" => {
                            self.current_locale = locale.to_string();
                            rust_i18n::set_locale(&self.current_locale);
                            self.push_message(
                                Role::Assistant,
                                t!("switched_locale", locale = locale).into(),
                            );
                        }
                        _ => {
                            self.push_message(
                                Role::Assistant,
                                t!("unknown_locale", locale = locale).into(),
                            );
                        }
                    }
                } else {
                    self.push_message(
                        Role::Assistant,
                        t!("current_locale", locale = &self.current_locale).into(),
                    );
                }
            }
            "/view" => {
                if parts.len() > 1 {
                    let mut path_str = parts[1];
                    if path_str.starts_with('@') {
                        path_str = &path_str[1..];
                    }
                    self.open_file(PathBuf::from(path_str), false);
                }
            }
            "/edit" => {
                if parts.len() > 1 {
                    let mut path_str = parts[1];
                    if path_str.starts_with('@') {
                        path_str = &path_str[1..];
                    }
                    self.open_file(PathBuf::from(path_str), true);
                }
            }
            _ => {
                self.push_message(
                    Role::Assistant,
                    t!("unknown_command", command = parts[0]).into(),
                );
            }
        }
    }
}
