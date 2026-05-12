//! Conversion helpers between `savvagent-plugin` types and ratatui types.
//!
//! These are free functions rather than trait impls to keep the plugin crate
//! free of any ratatui dependency.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use savvagent_plugin::{Region, StyledLine, StyledSpan, TextMods, ThemeColor};

/// Convert a `savvagent_plugin::Region` into a `ratatui::layout::Rect`.
#[allow(dead_code)] // used by screen-stack dispatch in PR 3
pub fn region_to_rect(r: Region) -> ratatui::layout::Rect {
    ratatui::layout::Rect {
        x: r.x,
        y: r.y,
        width: r.width,
        height: r.height,
    }
}

/// Convert a `ratatui::layout::Rect` into a `savvagent_plugin::Region`.
pub fn rect_to_region(r: ratatui::layout::Rect) -> Region {
    Region {
        x: r.x,
        y: r.y,
        width: r.width,
        height: r.height,
    }
}

/// Map a `ThemeColor` to the corresponding `ratatui::style::Color`.
pub fn theme_color_to_ratatui(c: ThemeColor) -> Color {
    match c {
        ThemeColor::Default => Color::Reset,
        ThemeColor::Black => Color::Black,
        ThemeColor::Red => Color::Red,
        ThemeColor::Green => Color::Green,
        ThemeColor::Yellow => Color::Yellow,
        ThemeColor::Blue => Color::Blue,
        ThemeColor::Magenta => Color::Magenta,
        ThemeColor::Cyan => Color::Cyan,
        ThemeColor::White => Color::White,
        ThemeColor::DarkGray => Color::DarkGray,
        ThemeColor::LightRed => Color::LightRed,
        ThemeColor::LightGreen => Color::LightGreen,
        ThemeColor::LightYellow => Color::LightYellow,
        ThemeColor::LightBlue => Color::LightBlue,
        ThemeColor::LightMagenta => Color::LightMagenta,
        ThemeColor::LightCyan => Color::LightCyan,
        ThemeColor::Gray => Color::Gray,
        ThemeColor::Indexed(i) => Color::Indexed(i),
        ThemeColor::Rgb { r, g, b } => Color::Rgb(r, g, b),
    }
}

/// Map `TextMods` to a ratatui `Modifier` bitfield.
pub fn text_mods_to_modifier(m: TextMods) -> Modifier {
    let mut out = Modifier::empty();
    if m.bold {
        out |= Modifier::BOLD;
    }
    if m.italic {
        out |= Modifier::ITALIC;
    }
    if m.underline {
        out |= Modifier::UNDERLINED;
    }
    if m.reverse {
        out |= Modifier::REVERSED;
    }
    if m.dim {
        out |= Modifier::DIM;
    }
    out
}

/// Convert a `StyledSpan` to a ratatui `Span<'static>`.
pub fn styled_span_to_ratatui(span: StyledSpan) -> Span<'static> {
    let mut style = Style::default();
    if let Some(fg) = span.fg {
        style = style.fg(theme_color_to_ratatui(fg));
    }
    if let Some(bg) = span.bg {
        style = style.bg(theme_color_to_ratatui(bg));
    }
    let mods = text_mods_to_modifier(span.modifiers);
    if !mods.is_empty() {
        style = style.add_modifier(mods);
    }
    Span::styled(span.text, style)
}

/// Convert a `StyledLine` to a ratatui `Line<'static>`.
pub fn styled_line_to_ratatui(line: StyledLine) -> Line<'static> {
    Line::from(
        line.spans
            .into_iter()
            .map(styled_span_to_ratatui)
            .collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::TextMods;

    #[test]
    fn region_rect_roundtrip() {
        let r = Region {
            x: 1,
            y: 2,
            width: 80,
            height: 24,
        };
        let rect = region_to_rect(r);
        let back = rect_to_region(rect);
        assert_eq!(back, r);
    }

    #[test]
    fn theme_color_cyan_maps_correctly() {
        assert_eq!(theme_color_to_ratatui(ThemeColor::Cyan), Color::Cyan);
    }

    #[test]
    fn text_mods_bold_italic() {
        let m = TextMods {
            bold: true,
            italic: true,
            ..Default::default()
        };
        let mods = text_mods_to_modifier(m);
        assert!(mods.contains(Modifier::BOLD));
        assert!(mods.contains(Modifier::ITALIC));
        assert!(!mods.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn styled_line_to_ratatui_produces_spans() {
        use savvagent_plugin::{StyledSpan, TextMods};
        let line = StyledLine {
            spans: vec![StyledSpan {
                text: "hello".into(),
                fg: Some(ThemeColor::Green),
                bg: None,
                modifiers: TextMods::default(),
            }],
        };
        let rline = styled_line_to_ratatui(line);
        assert_eq!(rline.spans.len(), 1);
        assert_eq!(rline.spans[0].content, "hello");
    }
}
