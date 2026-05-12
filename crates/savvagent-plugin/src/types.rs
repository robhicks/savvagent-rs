//! ID newtypes and small structural types crossing plugin boundaries.

/// Stable identifier for a registered plugin (transparent newtype over `String`).
///
/// Built-ins use the prefix `internal:` (for example, `internal:themes`,
/// `internal:provider-anthropic`). Third-party plugins are expected to use a
/// vendor-scoped prefix (e.g. `acme:my-plugin`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PluginId(
    /// The plugin's identifier string.
    pub String,
);

/// Stable identifier for an LLM provider (transparent newtype over `String`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProviderId(
    /// The provider's id string (e.g. `"anthropic"`).
    pub String,
);

/// Transparent newtype identifying a live terminal screen instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScreenInstanceId(
    /// Numeric handle assigned by the TUI runtime.
    pub u32,
);

/// Axis-aligned rectangle within the terminal grid (columns × rows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    /// X coordinate (columns from the terminal left edge).
    pub x: u16,
    /// Y coordinate (rows from the terminal top edge).
    pub y: u16,
    /// Width in columns.
    pub width: u16,
    /// Height in rows.
    pub height: u16,
}

/// Wall-clock instant with sub-second precision, WIT-portable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timestamp {
    /// Seconds since the Unix epoch (may be negative for pre-epoch instants).
    pub secs: i64,
    /// Sub-second component in nanoseconds. Caller must keep this in `0..1_000_000_000`;
    /// values outside that range have unspecified meaning when converted to wall-clock
    /// times in plugin runtimes.
    pub nanos: u32,
}

/// A keyboard event in a runtime-independent shape (no `crossterm` dep).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyEventPortable {
    /// The key code pressed.
    pub code: KeyCodePortable,
    /// Modifier keys held at the time of the event.
    pub modifiers: KeyMods,
}

/// Runtime-independent representation of a key code.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KeyCodePortable {
    /// A printable character. The `char` is the Unicode scalar value as decoded by the terminal.
    Char(char),
    /// The Backspace key.
    Backspace,
    /// The Enter (Return) key.
    Enter,
    /// The Escape key.
    Esc,
    /// The Tab key (forward).
    Tab,
    /// Shift+Tab (reverse tab).
    BackTab,
    /// The Insert key.
    Insert,
    /// The Delete (forward-delete) key.
    Delete,
    /// The Up arrow key.
    Up,
    /// The Down arrow key.
    Down,
    /// The Left arrow key.
    Left,
    /// The Right arrow key.
    Right,
    /// The Home key.
    Home,
    /// The End key.
    End,
    /// The Page Up key.
    PageUp,
    /// The Page Down key.
    PageDown,
    /// A function key. The `u8` is the function-key number (1–12 common; higher if supported).
    F(u8),
    /// A key code that the terminal reported but that is not otherwise mapped.
    Null,
}

/// Modifier keys held during a key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KeyMods {
    /// Whether the Control modifier was held.
    pub ctrl: bool,
    /// Whether the Alt (Option on macOS) modifier was held.
    pub alt: bool,
    /// Whether the Shift modifier was held.
    pub shift: bool,
    /// Whether the Meta (Super / Windows / Command) modifier was held.
    pub meta: bool,
}

/// A single-key chord. Reserved for future multi-key chord extension.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChordPortable {
    /// The key event that forms this chord.
    pub key: KeyEventPortable,
}

use crate::styled::ThemeColor;

/// A theme catalog entry exposed by the `internal:themes` plugin (and any
/// third-party theme plugin).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeEntry {
    /// Machine-readable identifier for this theme (e.g. `"light"`, `"dracula"`).
    pub slug: String,
    /// Human-readable display name shown in the theme picker UI.
    pub label: String,
    /// Whether this theme is considered a dark-background theme.
    pub dark: bool,
    /// Color palette associated with this theme.
    pub palette: ThemePalette,
}

/// Minimal palette in PR 1. PR 6 (`internal:themes` extraction) ports the
/// full field set from `crates/savvagent/src/theme.rs` into this struct.
/// Extensions are additive — `non_exhaustive` reserves the ability to grow
/// the palette without breaking trait-surface clients.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ThemePalette {
    /// Primary background color of the theme.
    pub bg: ThemeColor,
    /// Primary foreground (text) color of the theme.
    pub fg: ThemeColor,
    /// Accent color used for highlights and active elements.
    pub accent: ThemeColor,
    /// Muted color used for less prominent text and borders.
    pub muted: ThemeColor,
}

impl ThemePalette {
    /// Constructor that prevents external code from depending on field order.
    /// Required by `#[non_exhaustive]`.
    pub fn new(bg: ThemeColor, fg: ThemeColor, accent: ThemeColor, muted: ThemeColor) -> Self {
        Self { bg, fg, accent, muted }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_string_newtypes() {
        let p = PluginId("internal:themes".to_string());
        let q = ProviderId("anthropic".to_string());
        assert_eq!(p.0, "internal:themes");
        assert_eq!(q.0, "anthropic");
    }

    #[test]
    fn region_fields_are_u16() {
        let r = Region { x: 0, y: 0, width: 80, height: 24 };
        let area: u32 = r.width as u32 * r.height as u32;
        assert_eq!(area, 1920);
    }

    #[test]
    fn timestamp_is_i64_secs_plus_u32_nanos() {
        let t = Timestamp { secs: 1_700_000_000, nanos: 500_000_000 };
        assert_eq!(t.secs, 1_700_000_000);
        assert_eq!(t.nanos, 500_000_000);
    }

    #[test]
    fn screen_instance_id_is_u32() {
        let s = ScreenInstanceId(42);
        assert_eq!(s.0, 42u32);
    }

    #[test]
    fn key_event_portable_is_constructible() {
        let k = KeyEventPortable {
            code: KeyCodePortable::Char('a'),
            modifiers: KeyMods { ctrl: true, alt: false, shift: false, meta: false },
        };
        assert!(k.modifiers.ctrl);
        match k.code {
            KeyCodePortable::Char(c) => assert_eq!(c, 'a'),
            _ => panic!("unexpected variant"),
        }
    }

    #[test]
    fn chord_portable_wraps_one_key() {
        let c = ChordPortable {
            key: KeyEventPortable {
                code: KeyCodePortable::Char('s'),
                modifiers: KeyMods { ctrl: true, ..Default::default() },
            },
        };
        match c.key.code {
            KeyCodePortable::Char(c) => assert_eq!(c, 's'),
            _ => panic!(),
        }
    }

    #[test]
    fn theme_entry_is_constructible() {
        let entry = ThemeEntry {
            slug: "light".to_string(),
            label: "Light".to_string(),
            dark: false,
            palette: ThemePalette {
                bg: crate::styled::ThemeColor::White,
                fg: crate::styled::ThemeColor::Black,
                accent: crate::styled::ThemeColor::Blue,
                muted: crate::styled::ThemeColor::Gray,
            },
        };
        assert_eq!(entry.slug, "light");
        assert!(!entry.dark);
    }
}
