mod app;
mod tui;
mod ui;
mod agent;
mod tools;
mod llm;

#[macro_use]
extern crate rust_i18n;

i18n!("locales", fallback = "en");

use anyhow::Result;
use app::{App, InputMode};
use agent::{Role, Message};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::time::Duration;
use tui_input::InputRequest;
use tokio::sync::mpsc;
// use ratatui::layout::Rect;

fn key_to_input_request(key: &KeyEvent) -> Option<InputRequest> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char(c) if !ctrl => Some(InputRequest::InsertChar(c)),
        KeyCode::Backspace if ctrl => Some(InputRequest::DeletePrevWord),
        KeyCode::Backspace => Some(InputRequest::DeletePrevChar),
        KeyCode::Delete if ctrl => Some(InputRequest::DeleteNextWord),
        KeyCode::Delete => Some(InputRequest::DeleteNextChar),
        KeyCode::Left if ctrl => Some(InputRequest::GoToPrevWord),
        KeyCode::Left => Some(InputRequest::GoToPrevChar),
        KeyCode::Right if ctrl => Some(InputRequest::GoToNextWord),
        KeyCode::Right => Some(InputRequest::GoToNextChar),
        KeyCode::Home => Some(InputRequest::GoToStart),
        KeyCode::End => Some(InputRequest::GoToEnd),
        _ => None,
    }
}

enum AgentMessage {
    Update(Message),
    Done,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let mut terminal = tui::init()?;
    let mut app = App::new();
    let (tx, rx) = mpsc::channel(100);

    // Use a panic hook to ensure terminal is restored even on crash
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = tui::restore();
        original_hook(panic_info);
    }));

    let res = run_app(&mut terminal, &mut app, tx, rx).await;

    tui::restore()?;

    if let Err(err) = res {
        println!("{err:?}");
    }

    Ok(())
}

async fn run_app(
    terminal: &mut tui::Tui,
    app: &mut App,
    tx: mpsc::Sender<AgentMessage>,
    mut rx: mpsc::Receiver<AgentMessage>,
) -> Result<()> {
    loop {
        terminal.draw(|f| ui::render(app, f))?;

        // Check for agent messages
        while let Ok(msg) = rx.try_recv() {
            match msg {
                AgentMessage::Update(m) => {
                    app.agent.add_message(m);
                }
                AgentMessage::Done => {
                    app.is_loading = false;
                }
            }
        }

        if event::poll(Duration::from_millis(50))? {
            let event = event::read()?;
            if let Event::Key(key) = &event {
                if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat {
                    match app.input_mode {
                        InputMode::Normal => match key.code {
                            KeyCode::Char('i') => {
                                app.input_mode = InputMode::Editing;
                            }
                            KeyCode::Char('/') | KeyCode::Char('@') => {
                                app.input_mode = InputMode::Editing;
                                if let Some(req) = key_to_input_request(key) {
                                    app.input.handle(req);
                                }
                                if key.code == KeyCode::Char('@') {
                                    app.open_file_picker();
                                }
                            }
                            KeyCode::Char('q') | KeyCode::Esc => {
                                return Ok(());
                            }
                            _ => {}
                        },
                        InputMode::Editing => {
                            if app.is_file_picker_active {
                                match key.code {
                                    KeyCode::Enter => {
                                        let file = app.file_explorer.current();
                                        if file.is_dir {
                                            app.file_explorer.handle(&event)?;
                                        } else {
                                            app.file_picker_select();
                                        }
                                    }
                                    KeyCode::Esc => {
                                        app.close_file_picker();
                                    }
                                    _ => {
                                        app.file_explorer.handle(&event)?;
                                    }
                                }
                            } else {
                                match key.code {
                                    KeyCode::Enter => {
                                        let value = app.input.value().to_string();
                                        if !value.is_empty() && !app.is_loading {
                                            if value.starts_with('/') {
                                                app.handle_command(&value);
                                                app.input.reset();
                                            } else {
                                                app.push_message(Role::User, value.clone());
                                                app.input.reset();
                                                app.is_loading = true;
                                                
                                                let conversation = app.agent.conversation.clone();
                                                let provider = app.current_provider;
                                                
                                                let task_tx = tx.clone();
                                                tokio::spawn(async move {
                                                    let client = llm::get_client(provider);
                                                    let mut agent = agent::Agent::new();
                                                    agent.conversation = conversation;

                                                    loop {
                                                        match agent.step(client.as_ref()).await {
                                                            Ok(Some(final_msg)) => {
                                                                if task_tx.send(AgentMessage::Update(final_msg)).await.is_err() { break; }
                                                                break;
                                                            }
                                                            Ok(None) => {
                                                                let len = agent.conversation.len();
                                                                if len >= 2 {
                                                                    if task_tx.send(AgentMessage::Update(agent.conversation[len-2].clone())).await.is_err() { break; }
                                                                    if task_tx.send(AgentMessage::Update(agent.conversation[len-1].clone())).await.is_err() { break; }
                                                                }
                                                            }
                                                            Err(e) => {
                                                                let _ = task_tx.send(AgentMessage::Update(Message {
                                                                    role: Role::Assistant,
                                                                    content: format!("Error: {}", e),
                                                                    tool_calls: None,
                                                                    tool_call_id: None,
                                                                })).await;
                                                                break;
                                                            }
                                                        }
                                                    }
                                                    let _ = task_tx.send(AgentMessage::Done).await;
                                                });
                                            }
                                        }
                                    }
                                    KeyCode::Esc => {
                                        app.input_mode = InputMode::Normal;
                                    }
                                    KeyCode::Char('@') => {
                                        if let Some(req) = key_to_input_request(key) {
                                            app.input.handle(req);
                                        }
                                        app.open_file_picker();
                                    }
                                    _ => {
                                        if let Some(req) = key_to_input_request(key) {
                                            app.input.handle(req);
                                        }
                                    }
                                }
                            }
                        }
                        InputMode::ViewingFile => match key.code {
                            KeyCode::Esc | KeyCode::Char('q') => {
                                app.input_mode = InputMode::Normal;
                                app.active_file_path = None;
                                app.editor = None;
                            }
                            _ => {
                                if let Some(editor) = &mut app.editor {
                                    let area = terminal.size()?;
                                    let popup_area = ui::centered_rect(80, 80, area.into());
                                    let inner_area = popup_area.inner(ratatui::layout::Margin { horizontal: 1, vertical: 1 });
                                    editor.input(*key, &inner_area)?;
                                }
                            }
                        },
                        InputMode::EditingFile => match key.code {
                            KeyCode::Esc => {
                                app.save_file();
                                app.input_mode = InputMode::Normal;
                                app.active_file_path = None;
                                app.editor = None;
                            }
                            _ => {
                                if let Some(editor) = &mut app.editor {
                                    let area = terminal.size()?;
                                    let popup_area = ui::centered_rect(80, 80, area.into());
                                    let inner_area = popup_area.inner(ratatui::layout::Margin { horizontal: 1, vertical: 1 });
                                    editor.input(*key, &inner_area)?;
                                }
                            }
                        }
                    }
                }
            }
        }

        if app.should_quit {
            return Ok(());
        }
    }
}
