//! Inline-canvas painting + GPU texture cache for the egui front-end.
//!
//! Renderer ownership stays on `App::canvas_registry` (shared with the TUI).
//! This module owns only the GUI-side texture handles. See
//! `docs/superpowers/specs/2026-05-28-v0.19.0-egui-canvas-design.md`.

use std::collections::HashMap;

use savvagent_plugin::{ContentBlockId, Frame, PixelFormat, PixelSize};

use crate::app::App;
use crate::palette::Palette;

/// One cached texture for an `Entry::Canvas`. The pair `(width_px, height_px)`
/// records the size the texture was built at so a width change invalidates
/// the cache without re-querying the handle.
pub(super) struct GuiTexEntry {
    pub(super) width_px: u32,
    pub(super) height_px: u32,
    pub(super) handle: egui::TextureHandle,
}

/// Texture cache keyed by `ContentBlockId`. Entries are dropped when their
/// width no longer matches the desired width, when dispatch reports
/// `dirty=true`, or when `clear()` is called.
#[derive(Default)]
pub struct GuiCanvasCache {
    textures: HashMap<ContentBlockId, GuiTexEntry>,
}

impl std::fmt::Debug for GuiCanvasCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuiCanvasCache")
            .field("entries", &self.textures.len())
            .finish()
    }
}

impl GuiCanvasCache {
    #[allow(dead_code)] // Wired into App in Task 8.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop every cached texture handle. Call alongside
    /// `App::canvas_registry.clear()` (today only in `App::replay_transcript`).
    #[allow(dead_code)] // Wired into App in Task 12.
    pub fn clear(&mut self) {
        self.textures.clear();
    }

    /// Drop the cached texture for `id` if present.
    #[allow(dead_code)] // Wired into dispatch in Task 9.
    pub fn invalidate(&mut self, id: ContentBlockId) {
        self.textures.remove(&id);
    }

    /// Internal: get the current entry for `id` whose `width_px` matches.
    pub(super) fn get_if_fits(
        &self,
        id: ContentBlockId,
        width_px: u32,
    ) -> Option<&GuiTexEntry> {
        let entry = self.textures.get(&id)?;
        (entry.width_px == width_px).then_some(entry)
    }

    /// Internal: insert (or replace) the entry for `id`.
    pub(super) fn insert(
        &mut self,
        id: ContentBlockId,
        width_px: u32,
        height_px: u32,
        handle: egui::TextureHandle,
    ) {
        self.textures.insert(
            id,
            GuiTexEntry {
                width_px,
                height_px,
                handle,
            },
        );
    }
}

/// Translate a plugin-emitted `Frame` into an `egui::ColorImage`.
/// Accepts both RGBA8 (canonical) and BGRA8 (byte-swapped) frames.
/// Returns `None` for zero-sized frames or when the byte length does not
/// match `width * height * 4`.
pub(super) fn frame_to_color_image(frame: &Frame) -> Option<egui::ColorImage> {
    if frame.width == 0 || frame.height == 0 {
        return None;
    }
    let expected = (frame.width as usize)
        .checked_mul(frame.height as usize)?
        .checked_mul(4)?;
    if frame.bytes.len() != expected {
        return None;
    }
    let mut rgba = frame.bytes.clone();
    if matches!(frame.format, PixelFormat::Bgra8) {
        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
    }
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [frame.width as usize, frame.height as usize],
        &rgba,
    ))
}

/// Paint one `Entry::Canvas` into the current `ui`.
///
/// Renders (or reuses a cached texture for) the canvas, then drains pointer
/// events for the painted rect and forwards them to
/// [`crate::canvas_input::handle_canvas_mouse`]. A `dirty=true` outcome
/// invalidates the texture cache so the next frame re-renders.
#[allow(clippy::too_many_arguments)] // Distinct structural inputs; the signature is shared with Tasks 8-11.
pub fn paint(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    app: &mut App,
    cache: &mut GuiCanvasCache,
    host_slot: &crate::HostSlot,
    rt: &tokio::runtime::Handle,
    id: ContentBlockId,
    source: &str,
    source_preview: Option<&str>,
    _palette: &Palette,
) {
    // Streaming preview: monospace text, no Blitz call.
    if let Some(preview) = source_preview {
        ui.label(egui::RichText::new("Rendering HTML canvas…").weak());
        for line in preview.split('\n') {
            ui.label(egui::RichText::new(line).monospace());
        }
        return;
    }
    if source.is_empty() {
        ui.weak("[empty canvas]");
        return;
    }

    let ppp = ctx.pixels_per_point();
    let width_pts = ui.available_width().max(1.0);
    let width_px = (width_pts * ppp).floor().max(1.0) as u32;

    // Two paths build a Response; both must surface it for input handling.
    let resp = if let Some(entry) = cache.get_if_fits(id, width_px) {
        let display = egui::vec2(
            entry.width_px as f32 / ppp,
            entry.height_px as f32 / ppp,
        );
        ui.add(
            egui::Image::new(egui::load::SizedTexture::new(entry.handle.id(), display))
                .sense(egui::Sense::click_and_drag()),
        )
    } else {
        let frame = match app.canvas_registry.get_mut(id) {
            Some(r) => r.render(PixelSize {
                width: width_px,
                height: 0,
            }),
            None => {
                tracing::warn!(?id, "no renderer for canvas — skipping paint");
                ui.weak("[canvas renderer missing]");
                return;
            }
        };
        let Some(img) = frame_to_color_image(&frame) else {
            tracing::warn!(?id, w = frame.width, h = frame.height, "bad canvas frame");
            ui.weak("[canvas render failed]");
            return;
        };
        let handle = ctx.load_texture(
            format!("canvas-{}", id.0),
            img,
            egui::TextureOptions::LINEAR,
        );
        let display = egui::vec2(frame.width as f32 / ppp, frame.height as f32 / ppp);
        let resp = ui.add(
            egui::Image::new(egui::load::SizedTexture::new(handle.id(), display))
                .sense(egui::Sense::click_and_drag()),
        );
        cache.insert(id, frame.width, frame.height, handle);
        resp
    };

    // ---- Mouse dispatch ------------------------------------------------
    let rect = resp.rect;
    let events: Vec<egui::Event> = ctx.input(|i| i.events.clone());
    for ev in events {
        if let Some(mouse) = mouse_event_to_portable(&ev, rect, ppp) {
            // Enter the tokio runtime so any task spawned during dispatch
            // lands on the right scheduler. Drop the guard before the next
            // iteration so we re-enter freshly each dispatch.
            let _guard = rt.enter();
            let host_slot = host_slot.clone();
            let dirty = futures::executor::block_on(
                crate::canvas_input::handle_canvas_mouse(app, &host_slot, id, mouse),
            );
            if dirty {
                cache.invalidate(id);
            }
        }
    }
}

/// Translate an `egui::Event` into a frame-pixel
/// [`savvagent_plugin::MouseEventPortable`] for the painted rect.
///
/// Returns `None` for events outside the rect, for unsupported button kinds,
/// or for events without a meaningful pointer position.
fn mouse_event_to_portable(
    ev: &egui::Event,
    rect: egui::Rect,
    ppp: f32,
) -> Option<savvagent_plugin::MouseEventPortable> {
    use savvagent_plugin::{KeyMods, MouseButton, MouseEventKind, MouseEventPortable};

    let (kind, button, pos, modifiers) = match ev {
        egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers,
        } => {
            let btn = match button {
                egui::PointerButton::Primary => Some(MouseButton::Left),
                egui::PointerButton::Secondary => Some(MouseButton::Right),
                egui::PointerButton::Middle => Some(MouseButton::Middle),
                _ => None,
            };
            (
                if *pressed {
                    MouseEventKind::Press
                } else {
                    MouseEventKind::Release
                },
                btn,
                *pos,
                modifiers_to_portable(modifiers),
            )
        }
        egui::Event::PointerMoved(pos) => {
            (MouseEventKind::Move, None, *pos, KeyMods::default())
        }
        egui::Event::MouseWheel {
            delta, modifiers, ..
        } => {
            // Wheel events don't carry a pointer position; fall back to the
            // rect center so the rect-contains check below always passes for
            // wheel events that arrive while hovering the canvas.
            let pos = egui::pos2(rect.center().x, rect.center().y);
            let kind = if delta.y > 0.0 {
                MouseEventKind::ScrollUp
            } else if delta.y < 0.0 {
                MouseEventKind::ScrollDown
            } else {
                return None;
            };
            (kind, None, pos, modifiers_to_portable(modifiers))
        }
        _ => return None,
    };

    if !rect.contains(pos) {
        return None;
    }
    let x_pixel = ((pos.x - rect.min.x) * ppp).max(0.0) as u32;
    let y_pixel = ((pos.y - rect.min.y) * ppp).max(0.0) as u32;
    Some(MouseEventPortable {
        kind,
        button,
        x_pixel,
        y_pixel,
        modifiers,
    })
}

/// Map `egui::Modifiers` to the plugin-portable [`savvagent_plugin::KeyMods`].
///
/// `egui::Modifiers::command` is `ctrl` on Linux/Windows and `⌘` on macOS,
/// which matches the semantics of `KeyMods::meta` (Super / Windows / Command).
fn modifiers_to_portable(m: &egui::Modifiers) -> savvagent_plugin::KeyMods {
    savvagent_plugin::KeyMods {
        ctrl: m.ctrl,
        shift: m.shift,
        alt: m.alt,
        meta: m.command,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: u32, h: u32, format: PixelFormat, fill: [u8; 4]) -> Frame {
        let bytes = (0..(w * h)).flat_map(|_| fill).collect();
        Frame {
            width: w,
            height: h,
            format,
            bytes,
        }
    }

    #[test]
    fn rgba_round_trips() {
        // Alpha is 255 so `Color32::from_rgba_unmultiplied` short-circuits
        // to `from_rgb` and skips premultiplication — keeping the test
        // about RGB channel ordering, not the linear-space alpha math.
        let f = frame(2, 1, PixelFormat::Rgba8, [10, 20, 30, 255]);
        let img = frame_to_color_image(&f).unwrap();
        assert_eq!(img.size, [2, 1]);
        assert_eq!(img.pixels[0].r(), 10);
        assert_eq!(img.pixels[0].g(), 20);
        assert_eq!(img.pixels[0].b(), 30);
        assert_eq!(img.pixels[0].a(), 255);
    }

    #[test]
    fn bgra_is_byte_swapped() {
        // Input is BGRA = (B=10, G=20, R=30, A=255); output must be R=30,
        // G=20, B=10, A=255 — first and third channels swapped. Alpha 255
        // keeps premultiplication a no-op so we can assert raw RGB values.
        let f = frame(1, 1, PixelFormat::Bgra8, [10, 20, 30, 255]);
        let img = frame_to_color_image(&f).unwrap();
        assert_eq!(img.pixels[0].r(), 30);
        assert_eq!(img.pixels[0].g(), 20);
        assert_eq!(img.pixels[0].b(), 10);
        assert_eq!(img.pixels[0].a(), 255);
    }

    #[test]
    fn zero_size_returns_none() {
        assert!(frame_to_color_image(&frame(0, 1, PixelFormat::Rgba8, [0; 4])).is_none());
        assert!(frame_to_color_image(&frame(1, 0, PixelFormat::Rgba8, [0; 4])).is_none());
    }

    #[test]
    fn mismatched_byte_length_returns_none() {
        let bad = Frame {
            width: 2,
            height: 2,
            format: PixelFormat::Rgba8,
            bytes: vec![0; 7], // not 2*2*4
        };
        assert!(frame_to_color_image(&bad).is_none());
    }

    #[test]
    fn cache_get_if_fits_matches_width() {
        let mut cache = GuiCanvasCache::new();
        let ctx = egui::Context::default();
        let img = egui::ColorImage::filled([1, 1], egui::Color32::RED);
        let h = ctx.load_texture("t", img, egui::TextureOptions::LINEAR);
        cache.insert(ContentBlockId(0), 100, 50, h);
        assert!(cache.get_if_fits(ContentBlockId(0), 100).is_some());
        assert!(cache.get_if_fits(ContentBlockId(0), 200).is_none());
    }

    #[test]
    fn invalidate_drops_only_one_entry() {
        let mut cache = GuiCanvasCache::new();
        let ctx = egui::Context::default();
        let img = || egui::ColorImage::filled([1, 1], egui::Color32::RED);
        cache.insert(
            ContentBlockId(0),
            10,
            10,
            ctx.load_texture("a", img(), egui::TextureOptions::LINEAR),
        );
        cache.insert(
            ContentBlockId(1),
            10,
            10,
            ctx.load_texture("b", img(), egui::TextureOptions::LINEAR),
        );
        cache.invalidate(ContentBlockId(0));
        assert!(cache.get_if_fits(ContentBlockId(0), 10).is_none());
        assert!(cache.get_if_fits(ContentBlockId(1), 10).is_some());
    }

    #[test]
    fn clear_drops_everything() {
        let mut cache = GuiCanvasCache::new();
        let ctx = egui::Context::default();
        let img = egui::ColorImage::filled([1, 1], egui::Color32::RED);
        cache.insert(
            ContentBlockId(0),
            10,
            10,
            ctx.load_texture("a", img, egui::TextureOptions::LINEAR),
        );
        cache.clear();
        assert!(cache.get_if_fits(ContentBlockId(0), 10).is_none());
    }

    #[test]
    fn mouse_translates_pointer_button_inside_rect() {
        use savvagent_plugin::{MouseButton, MouseEventKind};

        let rect = egui::Rect::from_min_size(
            egui::pos2(10.0, 20.0),
            egui::vec2(100.0, 50.0),
        );
        let ev = egui::Event::PointerButton {
            pos: egui::pos2(30.0, 40.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        };
        let m = mouse_event_to_portable(&ev, rect, 2.0).expect("inside rect");
        assert_eq!(m.kind, MouseEventKind::Press);
        assert_eq!(m.button, Some(MouseButton::Left));
        // (30 - 10) * 2.0 = 40px ; (40 - 20) * 2.0 = 40px.
        assert_eq!(m.x_pixel, 40);
        assert_eq!(m.y_pixel, 40);
    }

    #[test]
    fn mouse_outside_rect_returns_none() {
        let rect = egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(10.0, 10.0),
        );
        let ev = egui::Event::PointerMoved(egui::pos2(100.0, 100.0));
        assert!(mouse_event_to_portable(&ev, rect, 1.0).is_none());
    }
}
