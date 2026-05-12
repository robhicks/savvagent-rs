//! Owned styled-text types crossing plugin boundaries.
//!
//! Plugins return `Vec<StyledLine>` from `render_slot` and `Screen::render`.
//! The runtime converts these into ratatui `Span` / `Line` at the boundary.

/// A line of styled text, owned, with no ratatui dep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledLine {
    /// Ordered sequence of styled spans that make up this line.
    pub spans: Vec<StyledSpan>,
}

/// A single styled run of text within a [`StyledLine`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledSpan {
    /// The text content of this span.
    pub text: String,
    /// Optional foreground color; `None` means inherit from the terminal theme.
    pub fg: Option<ThemeColor>,
    /// Optional background color; `None` means inherit from the terminal theme.
    pub bg: Option<ThemeColor>,
    /// Text attribute modifiers applied to this span.
    pub modifiers: TextMods,
}

/// A terminal color that can be used as a foreground or background.
///
/// Variants cover the 16 ANSI named colors, the 256-color indexed palette,
/// and direct RGB. The runtime maps these to ratatui `Color` at the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThemeColor {
    /// Terminal default color (inherits from the current theme).
    Default,
    /// ANSI color: black.
    Black,
    /// ANSI color: red.
    Red,
    /// ANSI color: green.
    Green,
    /// ANSI color: yellow.
    Yellow,
    /// ANSI color: blue.
    Blue,
    /// ANSI color: magenta.
    Magenta,
    /// ANSI color: cyan.
    Cyan,
    /// ANSI color: white.
    White,
    /// ANSI color: dark gray (bright black).
    DarkGray,
    /// ANSI color: light red (bright red).
    LightRed,
    /// ANSI color: light green (bright green).
    LightGreen,
    /// ANSI color: light yellow (bright yellow).
    LightYellow,
    /// ANSI color: light blue (bright blue).
    LightBlue,
    /// ANSI color: light magenta (bright magenta).
    LightMagenta,
    /// ANSI color: light cyan (bright cyan).
    LightCyan,
    /// ANSI color: gray (bright white).
    Gray,
    /// 256-color terminal palette index (0..=255).
    Indexed(u8),
    /// Direct RGB color; each component is in 0..=255.
    Rgb {
        /// Red component (0..=255).
        r: u8,
        /// Green component (0..=255).
        g: u8,
        /// Blue component (0..=255).
        b: u8,
    },
}

/// Plain-bool text attribute flags for WIT portability (rule 9.7).
///
/// Using individual booleans instead of ratatui's `Modifier` bitflags
/// keeps this type serialisable over WIT without a ratatui dependency.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct TextMods {
    /// Bold text.
    pub bold: bool,
    /// Italic text.
    pub italic: bool,
    /// Underlined text.
    pub underline: bool,
    /// Reverse video (swap fg/bg).
    pub reverse: bool,
    /// Dim (faint) text.
    pub dim: bool,
}

impl StyledLine {
    /// Create a plain (unstyled) line containing a single span with the given text.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            spans: vec![StyledSpan {
                text: text.into(),
                fg: None,
                bg: None,
                modifiers: TextMods::default(),
            }],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styled_line_holds_owned_spans() {
        let line = StyledLine {
            spans: vec![StyledSpan {
                text: "hello".to_string(),
                fg: Some(ThemeColor::Green),
                bg: None,
                modifiers: TextMods { bold: true, ..Default::default() },
            }],
        };
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].text, "hello");
    }

    #[test]
    fn theme_color_supports_named_indexed_and_rgb() {
        let _named = ThemeColor::Red;
        let _idx = ThemeColor::Indexed(208);
        let _rgb = ThemeColor::Rgb { r: 255, g: 128, b: 64 };
    }

    #[test]
    fn text_mods_default_is_all_false() {
        let m = TextMods::default();
        assert!(!m.bold);
        assert!(!m.italic);
        assert!(!m.underline);
        assert!(!m.reverse);
        assert!(!m.dim);
    }
}
