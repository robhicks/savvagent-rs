//! Startup splash. Painted as a full-frame overlay until any key dismisses it.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
};

const LOGO: &[&str] = &[
    "███████╗ █████╗ ██╗   ██╗██╗   ██╗ █████╗  ██████╗ ███████╗███╗   ██╗████████╗",
    "██╔════╝██╔══██╗██║   ██║██║   ██║██╔══██╗██╔════╝ ██╔════╝████╗  ██║╚══██╔══╝",
    "███████╗███████║██║   ██║██║   ██║███████║██║  ███╗█████╗  ██╔██╗ ██║   ██║   ",
    "╚════██║██╔══██║╚██╗ ██╔╝╚██╗ ██╔╝██╔══██║██║   ██║██╔══╝  ██║╚██╗██║   ██║   ",
    "███████║██║  ██║ ╚████╔╝  ╚████╔╝ ██║  ██║╚██████╔╝███████╗██║ ╚████║   ██║   ",
    "╚══════╝╚═╝  ╚═╝  ╚═══╝    ╚═══╝  ╚═╝  ╚═╝ ╚═════╝ ╚══════╝╚═╝  ╚═══╝   ╚═╝   ",
];

const TAGLINE: &str = "the savvy MCP-native terminal coding agent";
const HINT: &str = "press any key to continue";

const LOGO_WIDTH: u16 = 78;

pub fn render(frame: &mut Frame, area: Rect) {
    frame.render_widget(Clear, area);

    let logo_style = Style::default()
        .fg(Color::LightBlue)
        .add_modifier(Modifier::BOLD);
    let tagline_style = Style::default().fg(Color::LightBlue);
    let hint_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::ITALIC);

    let mut lines: Vec<Line<'static>> = LOGO
        .iter()
        .map(|row| Line::from(Span::styled(row.to_string(), logo_style)))
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(TAGLINE, tagline_style)).centered());
    lines.push(Line::from(""));
    lines.push(
        Line::from(Span::styled(
            format!("{HINT} · v{}", env!("CARGO_PKG_VERSION")),
            hint_style,
        ))
        .centered(),
    );

    let total_h = lines.len() as u16;
    let rect = center_rect(LOGO_WIDTH, total_h, area);
    frame.render_widget(Paragraph::new(lines), rect);
}

fn center_rect(w: u16, h: u16, area: Rect) -> Rect {
    let width = w.min(area.width);
    let height = h.min(area.height);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect {
        x,
        y,
        width,
        height,
    }
}
