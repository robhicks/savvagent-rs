use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, FrameExt, List, ListItem, Paragraph},
    Frame,
};
use crate::app::{App, InputMode};
use crate::agent::Role;
use rust_i18n::t;

pub fn render(app: &mut App, frame: &mut Frame) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(3),
        ])
        .split(area);

    // Header
    let header = Paragraph::new(t!("header", agent = app.current_agent, llm = format!("{:?}", app.current_provider)))
        .style(Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(header, chunks[0]);

    // Messages
    let mut list_items = Vec::new();
    for m in &app.agent.conversation {
        let (prefix, style) = match m.role {
            Role::User => (t!("user_prefix"), Style::default().fg(Color::Green)),
            Role::Assistant => (t!("agent_prefix"), Style::default().fg(Color::Cyan)),
            Role::Tool => (t!("output_prefix"), Style::default().fg(Color::DarkGray)),
        };

        if !m.content.is_empty() {
            list_items.push(ListItem::new(Line::from(vec![
                Span::styled(prefix, style.add_modifier(Modifier::BOLD)),
                Span::styled(&m.content, style),
            ])));
        }

        if let Some(ref tool_calls) = m.tool_calls {
            for tc in tool_calls {
                list_items.push(ListItem::new(Line::from(vec![
                    Span::styled("> ", Style::default().fg(Color::Yellow).add_modifier(Modifier::DIM)),
                    Span::styled(t!("calling_tool", name = tc.name, args = tc.arguments), Style::default().fg(Color::Yellow).add_modifier(Modifier::DIM)),
                ])));
            }
        }
    }

    let messages_list = List::new(list_items)
        .block(Block::default().borders(Borders::ALL).title(t!("activity_log")));
    frame.render_widget(messages_list, chunks[1]);

    // Status Bar
    let mode_str = match app.input_mode {
        InputMode::Normal => "NORMAL",
        InputMode::Editing => "INSERT",
        InputMode::ViewingFile => "VIEW",
        InputMode::EditingFile => "EDIT",
    };

    let status = if app.is_loading {
        Paragraph::new(format!(" ● {} [{}]", t!("thinking"), mode_str))
            .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::ITALIC))
    } else {
        Paragraph::new(format!(" ○ {} [{}]", t!("ready"), mode_str))
            .style(Style::default().fg(Color::Blue))
    };
    frame.render_widget(status.block(Block::default().borders(Borders::BOTTOM)), chunks[2]);

    // Input
    let width = chunks[3].width.max(3) - 3; // buffer for borders
    let scroll = app.input.visual_scroll(width as usize);
    let input = Paragraph::new(app.input.value())
        .style(match app.input_mode {
            InputMode::Normal | InputMode::ViewingFile | InputMode::EditingFile => Style::default(),
            InputMode::Editing => Style::default().fg(Color::Yellow),
        })
        .scroll((0, scroll as u16))
        .block(Block::default().borders(Borders::ALL).title(t!("input_hint")));
    frame.render_widget(input, chunks[3]);

    if app.is_file_picker_active {
        let popup_area = centered_rect(60, 40, area);
        frame.render_widget(Clear, popup_area);
        frame.render_widget_ref(app.file_explorer.widget(), popup_area);
    }

    if let InputMode::ViewingFile | InputMode::EditingFile = app.input_mode {
        if let Some(editor) = &app.editor {
            let popup_area = centered_rect(80, 80, area);
            frame.render_widget(Clear, popup_area);

            let path_str = app.active_file_path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
            let (title, hint) = if let InputMode::EditingFile = app.input_mode {
                (t!("edit_title", path = path_str), t!("edit_hint"))
            } else {
                (t!("view_title", path = path_str), t!("view_hint"))
            };

            let block = Block::default()
                .borders(Borders::ALL)
                .title(title)
                .title_bottom(Line::from(hint).right_aligned());

            let inner_area = popup_area.inner(ratatui::layout::Margin { horizontal: 1, vertical: 1 });
            frame.render_widget(block, popup_area);
            frame.render_widget(editor, inner_area);
        }
    }

    match app.input_mode {
        InputMode::Normal => {}
        InputMode::Editing => {
            frame.set_cursor_position((
                chunks[3].x + ((app.input.visual_cursor()).max(scroll) - scroll) as u16 + 1,
                chunks[3].y + 1,
            ));
        }
        InputMode::EditingFile => {
            if let Some(editor) = &app.editor {
                let popup_area = centered_rect(80, 80, area);
                let inner_area = popup_area.inner(ratatui::layout::Margin { horizontal: 1, vertical: 1 });
                if let Some((x, y)) = editor.get_visible_cursor(&inner_area) {
                    frame.set_cursor_position((x, y));
                }
            }
        }
        _ => {}
    }
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
