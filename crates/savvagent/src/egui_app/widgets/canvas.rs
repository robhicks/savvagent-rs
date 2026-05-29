//! Inline-canvas painting + GPU texture cache for the egui front-end.
//!
//! Renderer ownership stays on `App::canvas_registry` (shared with the TUI).
//! This module owns only the GUI-side texture handles. See
//! `docs/superpowers/specs/2026-05-28-v0.19.0-egui-canvas-design.md`.

use std::collections::HashMap;

use savvagent_plugin::{ContentBlockId, Frame, PixelFormat};

/// One cached texture for an `Entry::Canvas`. The pair `(width_px, height_px)`
/// records the size the texture was built at so a width change invalidates
/// the cache without re-querying the handle.
pub(super) struct GuiTexEntry {
    pub(super) width_px: u32,
    #[allow(dead_code)] // Used by Task 7 (paint()) to size the sampled rect.
    pub(super) height_px: u32,
    #[allow(dead_code)] // Used by Task 7 (paint()) to draw the texture.
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
    #[allow(dead_code)] // Used by Task 7 (paint()).
    pub(super) fn get_if_fits(
        &self,
        id: ContentBlockId,
        width_px: u32,
    ) -> Option<&GuiTexEntry> {
        let entry = self.textures.get(&id)?;
        (entry.width_px == width_px).then_some(entry)
    }

    /// Internal: insert (or replace) the entry for `id`.
    #[allow(dead_code)] // Used by Task 7 (paint()).
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
#[allow(dead_code)] // Used by Task 7 (paint()).
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
}
