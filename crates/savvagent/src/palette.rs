//! Render-path color palette derived from the active [`Theme`].
//!
//! The TUI does not restructure widgets when the theme changes — it only
//! swaps the [`Color`] values used by [`Style`]s on existing widgets.
//! [`Palette::for_theme`] returns the slot-to-color map for one of the
//! three built-in themes; render-path code reads slots (`fg`, `accent`,
//! `error`, …) instead of hard-coded colors.
//!
//! Slots are deliberately minimal: only the ones the UI actually paints
//! today. Add new slots as new widgets need them; default to the closest
//! existing slot when extending.

use crate::theme::Theme;
use ratatui::style::{Color, Style};

/// Render-path color palette derived from an active [`Theme`].
///
/// Each field is one semantic "slot" the UI paints with. Values are
/// concrete [`Color`]s so widgets can build [`Style`]s with no extra
/// indirection.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// Default text color.
    pub fg: Color,
    /// Default background color.
    pub bg: Color,
    /// Border/chrome color for blocks and dividers.
    pub border: Color,
    /// Accent color for highlights, active rows, header chrome.
    pub accent: Color,
    /// Muted/dim color for notes, metadata, hints.
    pub muted: Color,
    /// Error / denial color.
    pub error: Color,
    /// Success / "on" color.
    pub success: Color,
    /// Warning / "pending" color.
    pub warning: Color,
    /// Secondary accent (e.g. user vs. assistant differentiation).
    pub secondary: Color,
}

impl Palette {
    /// Resolve the palette for a built-in [`Theme`].
    ///
    /// Mappings are hand-tuned to keep the existing color story intact
    /// (cyan/green/yellow for the three log roles, red for errors,
    /// blue for header chrome) while making the light/high-contrast
    /// themes legible on their respective backgrounds.
    #[must_use]
    pub fn for_theme(theme: Theme) -> Self {
        match theme {
            Theme::Dark => Self {
                fg: Color::White,
                bg: Color::Reset,
                border: Color::DarkGray,
                accent: Color::Blue,
                muted: Color::DarkGray,
                error: Color::Red,
                success: Color::Green,
                warning: Color::Yellow,
                secondary: Color::Cyan,
            },
            Theme::Light => Self {
                fg: Color::Black,
                bg: Color::White,
                border: Color::Gray,
                accent: Color::Blue,
                muted: Color::DarkGray,
                error: Color::Red,
                success: Color::Green,
                warning: Color::Yellow,
                secondary: Color::Magenta,
            },
            Theme::HighContrast => Self {
                fg: Color::White,
                bg: Color::Black,
                border: Color::White,
                accent: Color::Yellow,
                muted: Color::Gray,
                error: Color::LightRed,
                success: Color::LightGreen,
                warning: Color::LightYellow,
                secondary: Color::LightCyan,
            },
        }
    }

    /// Base `fg`-on-`bg` style for the frame's default background.
    #[must_use]
    pub fn base_style(self) -> Style {
        Style::default().fg(self.fg).bg(self.bg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_theme_returns_distinct_palettes() {
        let dark = Palette::for_theme(Theme::Dark);
        let light = Palette::for_theme(Theme::Light);
        let hc = Palette::for_theme(Theme::HighContrast);
        // Backgrounds differ across all three themes.
        assert_ne!(dark.bg, light.bg);
        assert_ne!(dark.bg, hc.bg);
        assert_ne!(light.bg, hc.bg);
    }

    #[test]
    fn base_style_uses_palette_fg_and_bg() {
        let p = Palette::for_theme(Theme::Light);
        let s = p.base_style();
        assert_eq!(s.fg, Some(Color::Black));
        assert_eq!(s.bg, Some(Color::White));
    }
}
