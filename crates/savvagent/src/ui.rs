//! Render pass: paint the current [`App`] state into the frame.

use crate::app::{App, Entry, InputMode, TranscriptEntry};
use crate::palette::Palette;
use crate::providers::PROVIDERS;
use crate::splash;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, FrameExt, List, ListItem, Paragraph, Wrap},
};
use savvagent_host::ToolCallStatus;

/// Pre-computed plugin slot output for one render frame. Built async from
/// `compute_home_frame_data` before `terminal.draw` runs so the draw closure
/// stays synchronous and never touches plugin mutexes.
pub struct HomeFrameData {
    pub tips: Vec<savvagent_plugin::StyledLine>,
    pub footer_left: Vec<savvagent_plugin::StyledLine>,
    pub footer_center: Vec<savvagent_plugin::StyledLine>,
    pub footer_right: Vec<savvagent_plugin::StyledLine>,
}

impl HomeFrameData {
    /// Empty fallback used when plugins are not installed yet.
    pub fn empty() -> Self {
        Self {
            tips: vec![],
            footer_left: vec![],
            footer_center: vec![],
            footer_right: vec![],
        }
    }
}

/// Resolve every slot's lines for the current frame. Locks plugin mutexes
/// briefly per contributor.
pub async fn compute_home_frame_data(app: &crate::app::App, area: Rect) -> HomeFrameData {
    use crate::plugin::convert::rect_to_region;
    use crate::plugin::slots::SlotRouter;

    let Some(reg) = app.plugin_registry.as_ref().cloned() else {
        return HomeFrameData::empty();
    };
    let Some(idx) = app.plugin_indexes.as_ref().cloned() else {
        return HomeFrameData::empty();
    };
    let reg_guard = reg.read().await;
    let idx_guard = idx.read().await;
    let router = SlotRouter::new(&idx_guard, &reg_guard);

    // Give each footer slot ~1/3 of the terminal width for budgeting.
    let footer_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(33),
            Constraint::Percentage(34),
            Constraint::Percentage(33),
        ])
        .split(Rect::new(area.x, area.y, area.width, 1));

    let tips = router
        .render(
            "home.tips",
            rect_to_region(Rect::new(area.x, area.y, area.width, 1)),
        )
        .await;
    let footer_left = router
        .render("home.footer.left", rect_to_region(footer_cols[0]))
        .await;
    let footer_center = router
        .render("home.footer.center", rect_to_region(footer_cols[1]))
        .await;
    let footer_right = router
        .render("home.footer.right", rect_to_region(footer_cols[2]))
        .await;

    HomeFrameData {
        tips,
        footer_left,
        footer_center,
        footer_right,
    }
}

pub fn render(app: &mut App, frame: &mut Frame, frame_data: &HomeFrameData) {
    let area = frame.area();

    if app.show_splash {
        splash::render(frame, area, &app.splash_sandbox);
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
            Constraint::Length(1), // tips (plugin slot: home.tips)
            Constraint::Length(3), // input
            Constraint::Length(1), // footer (plugin slots: home.footer.*)
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

    // Tips row — one-line hints above the prompt, rendered from plugin slot.
    let tips_lines: Vec<Line<'static>> = frame_data
        .tips
        .iter()
        .cloned()
        .map(crate::plugin::convert::styled_line_to_ratatui)
        .collect();
    let tips_para = Paragraph::new(tips_lines).style(palette.base_style());
    frame.render_widget(tips_para, chunks[2]);

    let mut textarea = app.input_textarea.clone();
    textarea.set_block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette.border).bg(palette.bg)),
    );
    textarea.set_style(palette.base_style());
    frame.render_widget(&textarea, chunks[3]);

    // Footer row — three horizontal segments from plugin slots.
    let footer_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(33),
            Constraint::Percentage(34),
            Constraint::Percentage(33),
        ])
        .split(chunks[4]);

    let footer_left_lines: Vec<Line<'static>> = frame_data
        .footer_left
        .iter()
        .cloned()
        .map(crate::plugin::convert::styled_line_to_ratatui)
        .collect();
    frame.render_widget(
        Paragraph::new(footer_left_lines).style(palette.base_style()),
        footer_cols[0],
    );

    let footer_center_lines: Vec<Line<'static>> = frame_data
        .footer_center
        .iter()
        .cloned()
        .map(crate::plugin::convert::styled_line_to_ratatui)
        .collect();
    frame.render_widget(
        Paragraph::new(footer_center_lines)
            .style(palette.base_style())
            .centered(),
        footer_cols[1],
    );

    let footer_right_lines: Vec<Line<'static>> = frame_data
        .footer_right
        .iter()
        .cloned()
        .map(crate::plugin::convert::styled_line_to_ratatui)
        .collect();
    frame.render_widget(
        Paragraph::new(footer_right_lines)
            .style(palette.base_style())
            .right_aligned(),
        footer_cols[2],
    );

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
                    Span::styled(&cmd.description, palette.base_style().fg(palette.muted)),
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

    if matches!(app.input_mode, InputMode::SelectingTheme) {
        render_theme_picker(app, frame, area, palette);
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

fn render_theme_picker(app: &App, frame: &mut Frame, area: Rect, palette: Palette) {
    let popup = centered_rect(60, 50, area);
    frame.render_widget(Clear, popup);

    let filtered = app.theme_picker_filtered_themes();
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(filtered.len() + 4);

    if filtered.is_empty() {
        lines.push(Line::from(""));
        lines.push(
            Line::from(Span::styled(
                format!("no themes match `{}`", app.theme_picker_filter),
                palette.base_style().fg(palette.muted),
            ))
            .centered(),
        );
    } else {
        let builtins: Vec<&crate::theme::Theme> =
            filtered.iter().filter(|t| t.is_builtin()).collect();
        let catalog: Vec<&crate::theme::Theme> =
            filtered.iter().filter(|t| !t.is_builtin()).collect();

        if !builtins.is_empty() {
            lines.push(Line::from(Span::styled(
                "  built-in:".to_string(),
                palette.base_style().fg(palette.muted),
            )));
            for t in &builtins {
                let filtered_idx = filtered.iter().position(|x| x == *t).unwrap();
                lines.push(render_theme_picker_row(app, palette, t, filtered_idx));
            }
        }
        if !catalog.is_empty() {
            lines.push(Line::from(Span::styled(
                "  catalog (ratatui-themes):".to_string(),
                palette.base_style().fg(palette.muted),
            )));
            for t in &catalog {
                let filtered_idx = filtered.iter().position(|x| x == *t).unwrap();
                lines.push(render_theme_picker_row(app, palette, t, filtered_idx));
            }
        }
    }

    let title = if app.theme_picker_filter.is_empty() {
        " Pick a theme ".to_string()
    } else {
        format!(" Pick a theme · /{} ", app.theme_picker_filter)
    };
    let body = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .style(palette.base_style())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(palette.border).bg(palette.bg))
                .title(title)
                .title_bottom(
                    Line::from(" [↑/↓] move  [type to filter]  [Enter] select  [Esc] cancel ")
                        .right_aligned(),
                ),
        );
    frame.render_widget(body, popup);
}

fn render_theme_picker_row(
    app: &App,
    palette: Palette,
    theme: &crate::theme::Theme,
    filtered_index: usize,
) -> Line<'static> {
    let is_cursor = filtered_index == app.theme_picker_index;
    let is_active = *theme == app.theme_picker_pre_theme;
    let prefix = if is_cursor { "    > " } else { "      " };
    let active_marker = if is_active { "  (active)" } else { "" };
    let style = if is_cursor {
        palette
            .base_style()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        palette.base_style()
    };
    Line::from(vec![Span::styled(
        format!("{prefix}{}{active_marker}", theme.name()),
        style,
    )])
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
