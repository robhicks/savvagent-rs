//! Conversion helpers between `savvagent-plugin` types and ratatui types.
//!
//! These are free functions rather than trait impls to keep the plugin crate
//! free of any ratatui dependency.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use savvagent_plugin::{
    KeyCodePortable, KeyEventPortable, KeyMods, Region, StyledLine, StyledSpan, TextMods,
    ThemeColor,
};

/// Convert a crossterm `KeyEvent` into the WIT-portable shape used by
/// the plugin runtime. Unknown key codes map to `KeyCodePortable::Unknown`.
pub fn key_event_to_portable(e: KeyEvent) -> KeyEventPortable {
    KeyEventPortable {
        code: match e.code {
            KeyCode::Char(c) => KeyCodePortable::Char(c),
            KeyCode::Backspace => KeyCodePortable::Backspace,
            KeyCode::Enter => KeyCodePortable::Enter,
            KeyCode::Esc => KeyCodePortable::Esc,
            KeyCode::Tab => KeyCodePortable::Tab,
            KeyCode::BackTab => KeyCodePortable::BackTab,
            KeyCode::Insert => KeyCodePortable::Insert,
            KeyCode::Delete => KeyCodePortable::Delete,
            KeyCode::Up => KeyCodePortable::Up,
            KeyCode::Down => KeyCodePortable::Down,
            KeyCode::Left => KeyCodePortable::Left,
            KeyCode::Right => KeyCodePortable::Right,
            KeyCode::Home => KeyCodePortable::Home,
            KeyCode::End => KeyCodePortable::End,
            KeyCode::PageUp => KeyCodePortable::PageUp,
            KeyCode::PageDown => KeyCodePortable::PageDown,
            KeyCode::F(n) => KeyCodePortable::F(n),
            KeyCode::Null => KeyCodePortable::Unknown,
            _ => KeyCodePortable::Unknown,
        },
        modifiers: KeyMods {
            ctrl: e.modifiers.contains(KeyModifiers::CONTROL),
            alt: e.modifiers.contains(KeyModifiers::ALT),
            shift: e.modifiers.contains(KeyModifiers::SHIFT),
            meta: e.modifiers.contains(KeyModifiers::SUPER),
        },
    }
}

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

    #[test]
    fn key_event_to_portable_char_ctrl() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let evt = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        let p = super::key_event_to_portable(evt);
        assert!(matches!(
            p.code,
            savvagent_plugin::KeyCodePortable::Char('s')
        ));
        assert!(p.modifiers.ctrl);
        assert!(!p.modifiers.alt);
    }

    #[test]
    fn key_event_to_portable_null_maps_to_unknown() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let evt = KeyEvent::new(KeyCode::Null, KeyModifiers::NONE);
        let p = super::key_event_to_portable(evt);
        assert!(matches!(p.code, savvagent_plugin::KeyCodePortable::Unknown));
    }
}
