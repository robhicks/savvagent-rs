//! Render pass: paint the current [`App`] state into the frame.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, FrameExt, List, ListItem, Paragraph, Wrap},
};
use savvagent_host::ToolCallStatus;

use crate::app::{App, Entry, InputMode};
use crate::providers::PROVIDERS;
use crate::splash;

pub fn render(app: &mut App, frame: &mut Frame) {
    let area = frame.area();

    if app.show_splash {
        splash::render(frame, area);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Min(1),    // log
            Constraint::Length(1), // status
            Constraint::Length(3), // input
            Constraint::Length(1), // metrics
        ])
        .split(area);

    let header_text = if app.connected {
        format!(
            "Savvagent — {} · {}",
            app.active_provider_id.unwrap_or("?"),
            app.model
        )
    } else {
        "Savvagent — disconnected · type /connect".to_string()
    };
    let header_color = if app.connected {
        Color::Blue
    } else {
        Color::Magenta
    };
    let header = Paragraph::new(header_text)
        .style(
            Style::default()
                .fg(header_color)
                .add_modifier(Modifier::BOLD),
        )
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(header, chunks[0]);

    render_log(app, frame, chunks[1]);

    let status = if app.is_loading {
        Paragraph::new(" ● thinking…").style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::ITALIC),
        )
    } else {
        Paragraph::new(" ○ ready").style(Style::default().fg(Color::Blue))
    };
    frame.render_widget(
        status.block(Block::default().borders(Borders::BOTTOM)),
        chunks[2],
    );

    let mut textarea = app.input_textarea.clone();
    textarea.set_block(Block::default().borders(Borders::ALL));
    frame.render_widget(&textarea, chunks[3]);

    let transcript_label = match &app.last_transcript {
        Some(p) => format!(" · transcript: {}", p.display()),
        None => String::new(),
    };
    let version_text = format!("v{} ", env!("CARGO_PKG_VERSION"));
    let metrics_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(version_text.len() as u16),
        ])
        .split(chunks[4]);
    let metrics = Paragraph::new(format!(
        " ctx≈{} tokens · entries: {}{}",
        app.context_size,
        app.entries.len(),
        transcript_label
    ))
    .style(Style::default().fg(Color::Blue));
    frame.render_widget(metrics, metrics_chunks[0]);
    let version = Paragraph::new(Line::from(version_text).right_aligned())
        .style(Style::default().fg(Color::Blue));
    frame.render_widget(version, metrics_chunks[1]);

    if app.is_file_picker_active {
        let popup = centered_rect(60, 40, area);
        frame.render_widget(Clear, popup);
        frame.render_widget_ref(app.file_explorer.widget(), popup);
    }

    if matches!(
        app.input_mode,
        InputMode::ViewingFile | InputMode::EditingFile
    ) {
        if let Some(editor) = &app.editor {
            let popup = centered_rect(80, 80, area);
            frame.render_widget(Clear, popup);

            let path_str = app
                .active_file_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let (title, hint) = if matches!(app.input_mode, InputMode::EditingFile) {
                (
                    format!(" Editing: {path_str} "),
                    " [Esc] Save & Close | [Enter] New Line ",
                )
            } else {
                (
                    format!(" Viewing: {path_str} "),
                    " [Esc] Close | [j/k] Scroll ",
                )
            };

            let block = Block::default()
                .borders(Borders::ALL)
                .title(title)
                .title_bottom(Line::from(hint).right_aligned());
            let inner = popup.inner(Margin {
                horizontal: 1,
                vertical: 1,
            });
            frame.render_widget(block, popup);
            frame.render_widget(editor, inner);
        }
    }

    if matches!(app.input_mode, InputMode::CommandPalette) {
        let popup = centered_rect(50, 30, area);
        frame.render_widget(Clear, popup);
        let filtered = app.filtered_command_indices();
        let items: Vec<ListItem> = filtered
            .iter()
            .enumerate()
            .map(|(visible_idx, &cmd_idx)| {
                let cmd = &app.commands[cmd_idx];
                let style = if visible_idx == app.command_index {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{:<10} ", cmd.name), style),
                    Span::styled(&cmd.description, Style::default().fg(Color::DarkGray)),
                ]))
            })
            .collect();
        let title = if app.palette_filter.is_empty() {
            " Commands ".to_string()
        } else {
            format!(" Commands · /{} ", app.palette_filter)
        };
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .title_bottom(
                        Line::from(" [↑/↓] move  [Enter] select  [Esc] cancel ")
                            .right_aligned(),
                    ),
            )
            .highlight_symbol("> ");
        frame.render_widget(list, popup);
    }

    if matches!(app.input_mode, InputMode::EditingFile) {
        if let Some(editor) = &app.editor {
            let popup = centered_rect(80, 80, area);
            let inner = popup.inner(Margin {
                horizontal: 1,
                vertical: 1,
            });
            if let Some((x, y)) = editor.get_visible_cursor(&inner) {
                frame.set_cursor_position((x, y));
            }
        }
    }

    if matches!(app.input_mode, InputMode::SelectingProvider) {
        let popup = centered_rect(60, 40, area);
        frame.render_widget(Clear, popup);
        let items: Vec<ListItem> = PROVIDERS
            .iter()
            .enumerate()
            .map(|(i, spec)| {
                let style = if i == app.provider_index {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                let active_marker = if Some(spec.id) == app.active_provider_id {
                    " (active)"
                } else {
                    ""
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{:<22}", spec.display_name), style),
                    Span::styled(
                        format!(" {}{}", spec.id, active_marker),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]))
            })
            .collect();
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Connect to provider ")
                    .title_bottom(
                        Line::from(" [↑/↓] move  [Enter] select  [Esc] cancel ").right_aligned(),
                    ),
            )
            .highlight_symbol("> ");
        frame.render_widget(list, popup);
    }

    if matches!(app.input_mode, InputMode::PermissionPrompt) {
        if let Some(req) = &app.pending_permission {
            let popup = centered_rect(60, 40, area);
            frame.render_widget(Clear, popup);

            let args_pretty = serde_json::to_string_pretty(&req.args)
                .unwrap_or_else(|_| req.args.to_string());
            let mut lines: Vec<Line<'static>> = Vec::new();
            lines.push(Line::from(Span::styled(
                format!("Tool: {}", req.name),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                req.summary.clone(),
                Style::default().fg(Color::White),
            )));
            lines.push(Line::from(""));
            for line in args_pretty.lines() {
                lines.push(Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(Color::DarkGray),
                )));
            }

            let body = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Permission requested ")
                    .title_bottom(
                        Line::from(
                            " [y] allow  [n] deny  [a] always  [N] never  [Esc] deny ",
                        )
                        .right_aligned(),
                    ),
            );
            frame.render_widget(body, popup);
        }
    }

    if matches!(app.input_mode, InputMode::EnteringApiKey) {
        let popup = centered_rect(60, 20, area);
        frame.render_widget(Clear, popup);
        let title = match app.pending_provider {
            Some(spec) => format!(" {} API key ", spec.display_name),
            None => " API key ".to_string(),
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .title_bottom(Line::from(" [Enter] connect  [Esc] cancel ").right_aligned());
        let inner = popup.inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        frame.render_widget(block, popup);
        let mut ta = app.api_key_textarea.clone();
        ta.set_block(Block::default());
        frame.render_widget(&ta, inner);
    }
}

fn render_log(app: &App, frame: &mut Frame, area: Rect) {
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(app.entries.len() * 2 + 1);
    for entry in &app.entries {
        match entry {
            Entry::User(text) => {
                lines.push(line_block("You: ", text, Color::Green));
            }
            Entry::Assistant(text) => {
                lines.push(line_block("Agent: ", text, Color::Cyan));
            }
            Entry::Tool {
                name,
                arguments,
                status,
                result_preview,
            } => {
                let badge = match status {
                    None => "…",
                    Some(ToolCallStatus::Ok) => "✓",
                    Some(ToolCallStatus::Errored) => "✗",
                };
                let color = match status {
                    None => Color::Yellow,
                    Some(ToolCallStatus::Ok) => Color::Green,
                    Some(ToolCallStatus::Errored) => Color::Red,
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("  {badge} "), Style::default().fg(color)),
                    Span::styled(
                        format!("{name}({arguments})"),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::DIM),
                    ),
                ]));
                if let Some(preview) = result_preview {
                    lines.push(Line::from(Span::styled(
                        format!("    → {preview}"),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
            Entry::Note(text) => {
                lines.push(Line::from(Span::styled(
                    format!("· {text}"),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                )));
            }
        }
    }

    if !app.live_text.is_empty() {
        lines.push(line_block("Agent: ", &app.live_text, Color::Cyan));
    }

    let para = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Conversation "),
    );
    frame.render_widget(para, area);
}

fn line_block(prefix: &str, text: &str, color: Color) -> Line<'static> {
    let style = Style::default().fg(color);
    Line::from(vec![
        Span::styled(prefix.to_string(), style.add_modifier(Modifier::BOLD)),
        Span::styled(text.to_string(), style),
    ])
}

pub fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
