//! Render pass: paint the current [`App`] state into the frame.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, FrameExt, List, ListItem, Paragraph, Wrap},
};
use savvagent_host::ToolCallStatus;

use crate::app::{App, Entry, InputMode, TranscriptEntry};
use crate::palette::Palette;
use crate::providers::PROVIDERS;
use crate::splash;

pub fn render(app: &mut App, frame: &mut Frame) {
    let area = frame.area();

    if app.show_splash {
        splash::render(frame, area);
        return;
    }

    let palette = Palette::for_theme(app.active_theme);

    // Paint the active theme's base style across the whole frame so any
    // widget that doesn't set its own bg picks up the theme background.
    frame.buffer_mut().set_style(area, palette.base_style());

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

    let resumed_label = app
        .resumed_at
        .as_deref()
        .map(|ts| format!(" · resumed: {ts}"))
        .unwrap_or_default();
    let header_text = if app.connected {
        format!(
            "Savvagent — {} · {}{}",
            app.active_provider_id.unwrap_or("?"),
            app.model,
            resumed_label,
        )
    } else {
        "Savvagent — disconnected · type /connect".to_string()
    };
    let header_color = if app.connected {
        palette.accent
    } else {
        palette.warning
    };
    let header = Paragraph::new(header_text)
        .style(
            palette
                .base_style()
                .fg(header_color)
                .add_modifier(Modifier::BOLD),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(palette.border).bg(palette.bg)),
        );
    frame.render_widget(header, chunks[0]);

    render_log(app, frame, chunks[1], palette);

    let status = if app.is_loading {
        Paragraph::new(" ● thinking…").style(
            palette
                .base_style()
                .fg(palette.warning)
                .add_modifier(Modifier::ITALIC),
        )
    } else {
        Paragraph::new(" ○ ready").style(palette.base_style().fg(palette.accent))
    };
    frame.render_widget(
        status.block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(palette.border).bg(palette.bg)),
        ),
        chunks[2],
    );

    let mut textarea = app.input_textarea.clone();
    textarea.set_block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette.border).bg(palette.bg)),
    );
    textarea.set_style(palette.base_style());
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
    .style(palette.base_style().fg(palette.accent));
    frame.render_widget(metrics, metrics_chunks[0]);
    let version = Paragraph::new(Line::from(version_text).right_aligned())
        .style(palette.base_style().fg(palette.accent));
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
                    palette
                        .base_style()
                        .fg(palette.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    palette.base_style()
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{:<10} ", cmd.name), style),
                    Span::styled(
                        &cmd.description,
                        palette.base_style().fg(palette.muted),
                    ),
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
                    .border_style(Style::default().fg(palette.border).bg(palette.bg))
                    .title(title)
                    .title_bottom(
                        Line::from(" [↑/↓] move  [Enter] select  [Esc] cancel ").right_aligned(),
                    ),
            )
            .style(palette.base_style())
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
                    palette
                        .base_style()
                        .fg(palette.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    palette.base_style()
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
                        palette.base_style().fg(palette.muted),
                    ),
                ]))
            })
            .collect();
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(palette.border).bg(palette.bg))
                    .title(" Connect to provider ")
                    .title_bottom(
                        Line::from(" [↑/↓] move  [Enter] select  [Esc] cancel ").right_aligned(),
                    ),
            )
            .style(palette.base_style())
            .highlight_symbol("> ");
        frame.render_widget(list, popup);
    }

    if matches!(app.input_mode, InputMode::PermissionPrompt) {
        if let Some(req) = &app.pending_permission {
            let popup = centered_rect(60, 40, area);
            frame.render_widget(Clear, popup);

            let args_pretty =
                serde_json::to_string_pretty(&req.args).unwrap_or_else(|_| req.args.to_string());
            let mut lines: Vec<Line<'static>> = Vec::new();
            lines.push(Line::from(Span::styled(
                format!("Tool: {}", req.name),
                palette
                    .base_style()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                req.summary.clone(),
                palette.base_style().fg(palette.fg),
            )));
            lines.push(Line::from(""));
            for line in args_pretty.lines() {
                lines.push(Line::from(Span::styled(
                    line.to_string(),
                    palette.base_style().fg(palette.muted),
                )));
            }

            let body = Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .style(palette.base_style())
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(palette.border).bg(palette.bg))
                        .title(" Permission requested ")
                        .title_bottom(
                            Line::from(" [y] allow  [n] deny  [a] always  [N] never  [Esc] deny ")
                                .right_aligned(),
                        ),
                );
            frame.render_widget(body, popup);
        }
    }

    if let InputMode::BashNetworkPrompt { summary, .. } = &app.input_mode {
        let popup = centered_rect(60, 35, area);
        frame.render_widget(Clear, popup);

        let lines: Vec<Line<'static>> = vec![
            Line::from(Span::styled(
                "Bash needs network access".to_string(),
                palette
                    .base_style()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                summary.clone(),
                palette.base_style().fg(palette.fg),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  [O]nce              allow this invocation only".to_string(),
                palette.base_style().fg(palette.success),
            )),
            Line::from(Span::styled(
                "  [A]lways            allow for the rest of this session".to_string(),
                palette.base_style().fg(palette.success),
            )),
            Line::from(Span::styled(
                "  [D]eny once         deny this invocation only".to_string(),
                palette.base_style().fg(palette.error),
            )),
            Line::from(Span::styled(
                "  [F]orever (Never)   deny for the rest of this session".to_string(),
                palette.base_style().fg(palette.error),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Per-call override: re-run with `/bash --net <cmd>` or `/bash --no-net <cmd>`"
                    .to_string(),
                palette.base_style().fg(palette.muted),
            )),
        ];

        let body = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .style(palette.base_style())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(palette.border).bg(palette.bg))
                    .title(" Bash network access? ")
                    .title_bottom(
                        Line::from(" [O]nce  [A]lways  [D]eny  [F]orever  [Esc] deny ")
                            .right_aligned(),
                    ),
            );
        frame.render_widget(body, popup);
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
            .border_style(Style::default().fg(palette.border).bg(palette.bg))
            .style(palette.base_style())
            .title(title)
            .title_bottom(Line::from(" [Enter] connect  [Esc] cancel ").right_aligned());
        let inner = popup.inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        frame.render_widget(block, popup);
        let mut ta = app.api_key_textarea.clone();
        ta.set_block(Block::default());
        ta.set_style(palette.base_style());
        frame.render_widget(&ta, inner);
    }

    if matches!(app.input_mode, InputMode::SelectingTranscript) {
        render_transcript_picker(app, frame, area, palette);
    }
}

fn render_transcript_picker(app: &App, frame: &mut Frame, area: Rect, palette: Palette) {
    let popup = centered_rect(70, 50, area);
    frame.render_widget(Clear, popup);

    if app.transcript_entries.is_empty() {
        let body = Paragraph::new("No transcripts found in ~/.savvagent/transcripts/")
            .style(palette.base_style())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(palette.border).bg(palette.bg))
                    .title(" Resume transcript ")
                    .title_bottom(Line::from(" [Esc] cancel ").right_aligned()),
            );
        frame.render_widget(body, popup);
        return;
    }

    let items: Vec<ListItem> = app
        .transcript_entries
        .iter()
        .enumerate()
        .map(|(i, entry)| render_transcript_item(entry, i == app.transcript_index, palette))
        .collect();

    let list = List::new(items).style(palette.base_style()).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette.border).bg(palette.bg))
            .title(" Resume transcript ")
            .title_bottom(Line::from(" [↑/↓] move  [Enter] resume  [Esc] cancel ").right_aligned()),
    );
    frame.render_widget(list, popup);
}

fn render_transcript_item(
    entry: &TranscriptEntry,
    selected: bool,
    palette: Palette,
) -> ListItem<'static> {
    let style = if selected {
        palette
            .base_style()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        palette.base_style()
    };
    let meta_style = palette.base_style().fg(palette.muted);
    let line = Line::from(vec![
        Span::styled(format!("{:<22}", entry.timestamp), style),
        Span::styled(format!(" {:>3} msgs  ", entry.message_count), meta_style),
        Span::styled(entry.preview.clone(), palette.base_style().fg(palette.fg)),
    ]);
    ListItem::new(line)
}

fn render_log(app: &App, frame: &mut Frame, area: Rect, palette: Palette) {
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(app.entries.len() * 2 + 1);
    for entry in &app.entries {
        match entry {
            Entry::User(text) => {
                lines.push(line_block("You: ", text, palette.success, palette));
            }
            Entry::Assistant(text) => {
                lines.push(line_block("Agent: ", text, palette.secondary, palette));
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
                    None => palette.warning,
                    Some(ToolCallStatus::Ok) => palette.success,
                    Some(ToolCallStatus::Errored) => palette.error,
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("  {badge} "), palette.base_style().fg(color)),
                    Span::styled(
                        format!("{name}({arguments})"),
                        palette
                            .base_style()
                            .fg(palette.warning)
                            .add_modifier(Modifier::DIM),
                    ),
                ]));
                if let Some(preview) = result_preview {
                    lines.push(Line::from(Span::styled(
                        format!("    → {preview}"),
                        palette.base_style().fg(palette.muted),
                    )));
                }
            }
            Entry::Note(text) => {
                lines.push(Line::from(Span::styled(
                    format!("· {text}"),
                    palette
                        .base_style()
                        .fg(palette.muted)
                        .add_modifier(Modifier::ITALIC),
                )));
            }
        }
    }

    if !app.live_text.is_empty() {
        lines.push(line_block(
            "Agent: ",
            &app.live_text,
            palette.secondary,
            palette,
        ));
    }

    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .style(palette.base_style())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(palette.border).bg(palette.bg))
                .title(" Conversation "),
        );
    frame.render_widget(para, area);
}

fn line_block(prefix: &str, text: &str, color: Color, palette: Palette) -> Line<'static> {
    let style = palette.base_style().fg(color);
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
