//! Pure helpers for painting/operating the plugin screen stack in egui:
//! modal geometry (egui Rect + the logical `Region` to hand `Screen::render`)
//! and de-duplication of egui's paired Key/Text events into a single stream of
//! `KeyEventPortable` suitable for `Screen::on_key`.

use egui::Rect;
use savvagent_plugin::KeyCodePortable;
use savvagent_plugin::manifest::ScreenLayout;
use savvagent_plugin::types::Region;

use crate::egui_app::convert::egui_event_to_portable;

/// Where to paint a screen and the logical `Region` to hand its `render`.
pub struct ModalGeometry {
    /// The egui rect the overlay (border + content) occupies.
    pub outer: Rect,
    /// The inner area, in logical monospace columns/rows, after chrome.
    pub region: Region,
}

/// Compute the overlay geometry for a `ScreenLayout` given the available rect
/// (the central area) and the monospace glyph advance/row size in points.
/// Mirrors the ratatui `paint_screen` sizing: percentage-of-area for
/// CenteredModal (clamped to >= 20 cols), Margin{h:2,v:1} + 1-cell border for
/// the inner region; Fullscreen = whole area; BottomSheet = bottom `height`
/// rows.
pub fn modal_geometry(avail: Rect, layout: &ScreenLayout, glyph_w: f32, glyph_h: f32) -> ModalGeometry {
    let cols = |w: f32| (w / glyph_w).floor() as u16;
    let rows = |h: f32| (h / glyph_h).floor() as u16;
    match *layout {
        // Fullscreen, plus the forward-compat fallback for any future layout
        // variant (`ScreenLayout` is `#[non_exhaustive]`): fill the area.
        ScreenLayout::Fullscreen { .. } => ModalGeometry {
            outer: avail,
            region: Region { x: 0, y: 0, width: cols(avail.width()), height: rows(avail.height()) },
        },
        ScreenLayout::CenteredModal { width_pct, height_pct, .. } => {
            let min_w = 20.0 * glyph_w;
            let w = ((avail.width() * width_pct as f32 / 100.0).max(min_w)).min(avail.width());
            let h = (avail.height() * height_pct as f32 / 100.0).min(avail.height());
            let outer = Rect::from_center_size(avail.center(), egui::vec2(w, h));
            // chrome: 1-col/row border + Margin{h:2,v:1} on each side.
            let inner_cols = cols(w).saturating_sub(2 * (1 + 2));
            let inner_rows = rows(h).saturating_sub(2 * (1 + 1));
            ModalGeometry { outer, region: Region { x: 0, y: 0, width: inner_cols, height: inner_rows } }
        }
        ScreenLayout::BottomSheet { height } => {
            let h = (height as f32 * glyph_h).min(avail.height());
            let outer = Rect::from_min_size(
                egui::pos2(avail.min.x, avail.max.y - h),
                egui::vec2(avail.width(), h),
            );
            ModalGeometry {
                outer,
                region: Region { x: 0, y: 0, width: cols(avail.width()), height: rows(h) },
            }
        }
        _ => ModalGeometry {
            outer: avail,
            region: Region { x: 0, y: 0, width: cols(avail.width()), height: rows(avail.height()) },
        },
    }
}

/// Collapse egui's per-frame event list into the `KeyEventPortable`s a
/// `Screen::on_key` should see, WITHOUT the Key/Text double-count: a printable
/// `Event::Text` becomes one `Char`; an `Event::Key` is forwarded only when it
/// is NOT a plain unmodified printable char (i.e. navigation/control keys, or
/// any key carrying ctrl/alt/meta — accelerators egui does not echo as Text).
// Consumed by screen-stack input routing in Task 3; allow until then.
#[allow(dead_code)]
pub fn portable_keys_from_events(events: &[egui::Event]) -> Vec<savvagent_plugin::KeyEventPortable> {
    let mut out = Vec::new();
    for ev in events {
        let Some(k) = egui_event_to_portable(ev) else { continue };
        let is_text = matches!(ev, egui::Event::Text(_));
        let is_plain_char = matches!(k.code, KeyCodePortable::Char(_))
            && !k.modifiers.ctrl && !k.modifiers.alt && !k.modifiers.meta;
        // From a Key event, drop plain printable chars (the paired Text event
        // carries them); keep everything from Text, and keep modified/non-char
        // keys from Key events.
        if !is_text && is_plain_char {
            continue;
        }
        out.push(k);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::manifest::ScreenLayout;
    use savvagent_plugin::{KeyCodePortable, KeyMods};

    // Glyph metrics used throughout: 8pt advance, 16pt row.
    const GW: f32 = 8.0;
    const GH: f32 = 16.0;

    #[test]
    fn centered_modal_is_centered_and_percentage_sized() {
        let avail = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 800.0));
        let layout = ScreenLayout::CenteredModal { width_pct: 60, height_pct: 50, title: None };
        let g = modal_geometry(avail, &layout, GW, GH);
        // 60% of 1000 = 600 wide, 50% of 800 = 400 tall, centered.
        assert!((g.outer.width() - 600.0).abs() < 1.0);
        assert!((g.outer.height() - 400.0).abs() < 1.0);
        assert!((g.outer.center().x - 500.0).abs() < 1.0);
        assert!((g.outer.center().y - 400.0).abs() < 1.0);
    }

    #[test]
    fn centered_modal_inner_region_subtracts_chrome_margin() {
        let avail = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 800.0));
        let layout = ScreenLayout::CenteredModal { width_pct: 60, height_pct: 50, title: None };
        let g = modal_geometry(avail, &layout, GW, GH);
        // Inner = outer minus border(1) + Margin{h:2,v:1} on each side in *cols/rows*.
        // outer 600x400 pts -> 75x25 cols/rows; minus 2*(1 border + 2 h margin)=6 cols,
        // 2*(1 border + 1 v margin)=4 rows -> 69 cols, 21 rows.
        assert_eq!(g.region.width, 69);
        assert_eq!(g.region.height, 21);
    }

    #[test]
    fn fullscreen_region_is_whole_area() {
        let avail = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 800.0));
        let layout = ScreenLayout::Fullscreen { hide_chrome: false };
        let g = modal_geometry(avail, &layout, GW, GH);
        assert_eq!(g.outer, avail);
        assert_eq!(g.region.width, 100); // 800/8
        assert_eq!(g.region.height, 50); // 800/16
    }

    #[test]
    fn text_event_yields_one_char_and_suppresses_paired_key() {
        // egui emits Key{A} + Text("a") for one press; we want a single Char('a').
        let events = vec![
            egui::Event::Key {
                key: egui::Key::A, physical_key: None, pressed: true,
                repeat: false, modifiers: egui::Modifiers::NONE,
            },
            egui::Event::Text("a".into()),
        ];
        let keys = portable_keys_from_events(&events);
        assert_eq!(keys.len(), 1);
        assert!(matches!(keys[0].code, KeyCodePortable::Char('a')));
    }

    #[test]
    fn non_text_key_is_forwarded() {
        let events = vec![egui::Event::Key {
            key: egui::Key::Enter, physical_key: None, pressed: true,
            repeat: false, modifiers: egui::Modifiers::NONE,
        }];
        let keys = portable_keys_from_events(&events);
        assert_eq!(keys.len(), 1);
        assert!(matches!(keys[0].code, KeyCodePortable::Enter));
    }

    #[test]
    fn ctrl_accelerator_key_is_forwarded_even_though_char() {
        // Ctrl+S produces no Text event, so the Key path must forward it.
        let events = vec![egui::Event::Key {
            key: egui::Key::S, physical_key: None, pressed: true,
            repeat: false, modifiers: egui::Modifiers::CTRL,
        }];
        let keys = portable_keys_from_events(&events);
        assert_eq!(keys.len(), 1);
        assert!(keys[0].modifiers.ctrl);
        assert!(matches!(keys[0].code, KeyCodePortable::Char('s')));
        let _ = KeyMods::default();
    }
}
