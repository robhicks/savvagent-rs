//! `HtmlCanvas` — the static-rendering implementation of
//! `ContentRenderer` for SPP `ContentBlock::Html`.

use std::fmt;

use async_trait::async_trait;
use savvagent_plugin::{ContentBlockId, ContentRenderer, Frame, PixelFormat, PixelSize};

/// Static HTML canvas renderer. Phase 1: render-only; Phase 2 adds
/// event dispatch + focus + freeze/thaw.
pub struct HtmlCanvas {
    id: ContentBlockId,
    source: String,
}

impl fmt::Debug for HtmlCanvas {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HtmlCanvas")
            .field("id", &self.id)
            .field("source_len", &self.source.len())
            .finish()
    }
}

impl HtmlCanvas {
    /// Construct a canvas from HTML source.
    pub fn new(id: ContentBlockId, source: &str) -> Self {
        crate::subset::validate(source);
        Self {
            id,
            source: source.to_string(),
        }
    }

    /// Return the HTML source this canvas was constructed from.
    pub fn source(&self) -> &str {
        &self.source
    }
}

#[async_trait]
impl ContentRenderer for HtmlCanvas {
    fn id(&self) -> ContentBlockId {
        self.id
    }

    fn render(&mut self, size: PixelSize) -> Frame {
        render_html_to_rgba(&self.source, size.width)
    }
}

/// Headless Blitz pipeline: parse `source` → resolve at the requested
/// width → measure natural height → repaint at exact natural height →
/// return an Rgba8 [`Frame`].
///
/// The implementation follows the Phase 0 spike notes
/// (`docs/superpowers/notes/2026-05-21-blitz-spike.md` §"Static
/// rendering" / §"Pixel-buffer access" / §"Natural height").
fn render_html_to_rgba(source: &str, width: u32) -> Frame {
    use anyrender::{ImageRenderer as _, PaintScene as _};
    use anyrender_vello_cpu::VelloCpuImageRenderer;
    use blitz_dom::{BaseDocument, DocumentConfig, StyleThreading};
    use blitz_html::HtmlDocument;
    use blitz_paint::paint_scene;
    use blitz_traits::shell::{ColorScheme, Viewport};
    use peniko::{
        Color, Fill,
        kurbo::{Affine, Rect},
    };

    // `size.height` is ignored by design: we always return the natural height
    // for the requested width per the trait's contract (PixelSize::height is
    // a hint; Frame::height is authoritative).

    // Width must be > 0 — guard against accidental 0 by clamping to 1px.
    // (The trait contract says `size.width > 0`; we never want a panic.)
    let width = width.max(1);
    // Initial measure pass uses a generous viewport height; we replace it
    // with the document's natural height before the final paint.
    let measure_height: u32 = 100_000;
    let scale: f32 = 1.0;

    // ---- Measure pass: parse + resolve at requested width to get natural height.
    let mut document = HtmlDocument::from_html(
        source,
        DocumentConfig {
            base_url: None,
            net_provider: None,
            // Sequential: Blitz's default Parallel threading panics with
            // `already mutably borrowed` when two HtmlCanvas instances resolve
            // concurrently against Stylo's global thread pool (blitz #430).
            style_threading: StyleThreading::Sequential,
            viewport: Some(Viewport::new(
                width,
                measure_height,
                scale,
                ColorScheme::Light,
            )),
            ..Default::default()
        },
    );
    {
        let base: &mut BaseDocument = document.as_mut();
        base.resolve(0.0);
    }

    let natural_height: u32 = {
        let base: &BaseDocument = document.as_ref();
        let root = base.root_element();
        // Root element's `final_layout.size.height` is f32 pixels.
        // Round up so we don't clip the bottom row of content.
        root.final_layout.size.height.ceil().max(1.0) as u32
    };

    // ---- Final paint at the natural height. Set the viewport to the
    // measured dimensions and re-resolve so layout matches the paint.
    {
        let base: &mut BaseDocument = document.as_mut();
        base.set_viewport(Viewport::new(
            width,
            natural_height,
            scale,
            ColorScheme::Light,
        ));
        base.resolve(0.0);
    }

    // VelloCpuImageRenderer truncates to u16 internally; warn + clamp so
    // pathological canvases fail visibly rather than producing a buffer
    // whose size doesn't match the renderer's expectations.
    const MAX_DIM: u32 = u16::MAX as u32;
    let width = if width > MAX_DIM {
        tracing::warn!(width, max = MAX_DIM, "canvas width truncated to u16 max");
        MAX_DIM
    } else {
        width
    };
    let natural_height = if natural_height > MAX_DIM {
        tracing::warn!(natural_height, max = MAX_DIM, "canvas height truncated to u16 max");
        MAX_DIM
    } else {
        natural_height
    };

    let buffer = {
        let base: &mut BaseDocument = document.as_mut();
        // VelloCpuImageRenderer renders a single frame to a Vec<u8>.
        // The renderer hands us a scene; we paint a white background
        // first (so transparent or unstyled regions don't end up with
        // garbage from an uninitialized buffer in some backends) and
        // then delegate the document paint to `blitz_paint::paint_scene`.
        let mut renderer = VelloCpuImageRenderer::new(width, natural_height);
        let mut out: Vec<u8> = Vec::new();
        renderer.render_to_vec(
            |scene| {
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    Color::WHITE,
                    None,
                    &Rect::new(0.0, 0.0, width as f64, natural_height as f64),
                );
                paint_scene(scene, base, scale as f64, width, natural_height, 0, 0);
            },
            &mut out,
        );
        out
    };

    // Sanity-check the buffer length matches the trait contract.
    // anyrender_vello_cpu produces RGBA8 row-major top-down, which is
    // exactly what `PixelFormat::Rgba8` is defined to be.
    debug_assert_eq!(
        buffer.len() as u32,
        width * natural_height * 4,
        "Blitz produced unexpected pixel buffer size: {} bytes, expected {}",
        buffer.len(),
        width * natural_height * 4,
    );

    Frame {
        width,
        height: natural_height,
        format: PixelFormat::Rgba8,
        bytes: buffer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TINY_HTML: &str = "<!doctype html><body style='margin:0'>\
                             <div style='width:32px;height:16px;\
                             background:#ff0000'></div></body>";

    #[test]
    fn canvas_renders_at_requested_width() {
        let mut c = HtmlCanvas::new(ContentBlockId(7), TINY_HTML);
        let frame = c.render(PixelSize {
            width: 64,
            height: 0, // 0 means "natural height"
        });
        assert_eq!(frame.format, PixelFormat::Rgba8);
        assert_eq!(frame.width, 64);
        assert!(frame.height > 0);
        assert_eq!(
            frame.bytes.len() as u32,
            frame.width * frame.height * 4,
            "Rgba8 byte count must match width*height*4",
        );
    }

    #[test]
    fn canvas_id_round_trips() {
        let c = HtmlCanvas::new(ContentBlockId(42), TINY_HTML);
        assert_eq!(c.id(), ContentBlockId(42));
    }
}
