//! Streaming parser that extracts `savvagent-canvas` HTML fences from
//! model text output and emits a sequence of [`FenceChunk::Text`] and
//! [`FenceChunk::Html`] chunks.
//!
//! Fence syntax: a line beginning with ```` ```html-canvas ```` opens
//! an HTML block; a line that is exactly ` ``` ` (three backticks)
//! closes it. Anything between the open and close (inclusive of
//! whitespace) becomes a single `Html` chunk. Other code fences
//! (e.g. ```` ```rust ````, ```` ```html ```` without the
//! `-canvas` suffix) are passed through as text.
//!
//! The parser is push-based so it works on streaming token deltas: feed
//! each text fragment with [`FenceParser::push`], get back a `Vec` of
//! chunks. Call [`FenceParser::finish`] at end-of-stream to flush any
//! buffered text or to surface an unclosed-fence warning.

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]
#![warn(missing_debug_implementations)]
#![warn(missing_docs)]

/// One unit of parsed output from [`FenceParser::push`] / `finish`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FenceChunk {
    /// Plain text outside any html-canvas fence.
    Text(String),
    /// HTML content from inside a `html-canvas` fence (fence lines
    /// themselves are stripped).
    Html(String),
}

/// Outcome of [`FenceParser::finish`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishResult {
    /// Flushed chunks (any buffered text or unclosed-fence content).
    pub chunks: Vec<FenceChunk>,
    /// `true` if a fence was opened but never closed before EOF; the
    /// open content was flushed as `Html` regardless.
    pub unclosed_fence: bool,
}

/// Push-based fence parser.
#[derive(Debug, Default)]
pub struct FenceParser {
    /// Carry-over bytes from the previous push that didn't form a
    /// complete line yet.
    buf: String,
    /// True iff we are currently inside an open html-canvas fence.
    inside_canvas: bool,
}

impl FenceParser {
    /// Construct an empty parser.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of text; returns any complete chunks parsable so far.
    pub fn push(&mut self, fragment: &str) -> Vec<FenceChunk> {
        let mut out = Vec::new();
        self.buf.push_str(fragment);

        // Walk line-by-line, but keep any trailing incomplete line in `buf`.
        loop {
            match self.buf.find('\n') {
                Some(nl) => {
                    let line: String = self.buf.drain(..=nl).collect();
                    self.consume_line(&line, &mut out);
                }
                None => break,
            }
        }
        out
    }

    /// End of stream — flush remaining buffered content.
    pub fn finish(mut self) -> FinishResult {
        let mut chunks = Vec::new();
        let unclosed = self.inside_canvas;
        if !self.buf.is_empty() {
            // Flush trailing partial line.
            let line = std::mem::take(&mut self.buf);
            self.consume_line(&line, &mut chunks);
        }
        FinishResult {
            chunks,
            unclosed_fence: unclosed,
        }
    }

    fn consume_line(&mut self, line: &str, out: &mut Vec<FenceChunk>) {
        let trimmed = line.trim_end_matches(|c: char| c == '\n' || c == '\r');
        if !self.inside_canvas {
            if trimmed.trim_start() == "```html-canvas" {
                self.inside_canvas = true;
                return; // fence line consumed; no emission
            }
            append_text(out, line);
        } else {
            if trimmed.trim_start() == "```" {
                self.inside_canvas = false;
                return; // closing fence consumed; no emission
            }
            append_html(out, line);
        }
    }
}

fn append_text(out: &mut Vec<FenceChunk>, s: &str) {
    if let Some(FenceChunk::Text(t)) = out.last_mut() {
        t.push_str(s);
    } else {
        out.push(FenceChunk::Text(s.to_string()));
    }
}

fn append_html(out: &mut Vec<FenceChunk>, s: &str) {
    if let Some(FenceChunk::Html(t)) = out.last_mut() {
        t.push_str(s);
    } else {
        out.push(FenceChunk::Html(s.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_fence_is_pure_text() {
        let mut p = FenceParser::new();
        let chunks = p.push("hello\nworld\n");
        assert_eq!(
            chunks,
            vec![FenceChunk::Text("hello\nworld\n".into())]
        );
        let fin = p.finish();
        assert!(fin.chunks.is_empty());
        assert!(!fin.unclosed_fence);
    }

    #[test]
    fn complete_fence_extracts_html() {
        let mut p = FenceParser::new();
        let mut chunks = p.push("Here:\n");
        chunks.extend(p.push("```html-canvas\n"));
        chunks.extend(p.push("<!doctype html><body>x</body>\n"));
        chunks.extend(p.push("```\n"));
        chunks.extend(p.push("trailing\n"));

        assert_eq!(
            chunks,
            vec![
                FenceChunk::Text("Here:\n".into()),
                FenceChunk::Html("<!doctype html><body>x</body>\n".into()),
                FenceChunk::Text("trailing\n".into()),
            ],
        );
        let fin = p.finish();
        assert!(fin.chunks.is_empty());
        assert!(!fin.unclosed_fence);
    }

    #[test]
    fn other_code_fences_pass_through_as_text() {
        let mut p = FenceParser::new();
        let chunks = p.push("```rust\nfn x() {}\n```\n");
        assert_eq!(
            chunks,
            vec![FenceChunk::Text("```rust\nfn x() {}\n```\n".into())]
        );
    }

    #[test]
    fn plain_html_fence_is_not_canvas() {
        // ```html (no -canvas) must be treated as a code sample.
        let mut p = FenceParser::new();
        let chunks = p.push("```html\n<p>x</p>\n```\n");
        assert_eq!(
            chunks,
            vec![FenceChunk::Text("```html\n<p>x</p>\n```\n".into())]
        );
    }

    #[test]
    fn split_across_pushes() {
        let mut p = FenceParser::new();
        let mut chunks = p.push("```ht");
        chunks.extend(p.push("ml-canvas\n<b>"));
        chunks.extend(p.push("hi</b>\n```\n"));
        assert_eq!(chunks, vec![FenceChunk::Html("<b>hi</b>\n".into())]);
    }

    #[test]
    fn unclosed_fence_flushed_at_finish() {
        let mut p = FenceParser::new();
        let chunks = p.push("```html-canvas\n<body>");
        assert!(chunks.is_empty());
        let fin = p.finish();
        assert_eq!(fin.chunks, vec![FenceChunk::Html("<body>".into())]);
        assert!(fin.unclosed_fence);
    }
}
