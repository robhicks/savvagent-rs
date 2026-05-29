//! Shell-agnostic key + mouse dispatch for focused canvases.
//!
//! Both the ratatui TUI (`main.rs`) and the egui GUI
//! (`egui_app/widgets/canvas.rs`) translate their native events to
//! `KeyEventPortable` / `MouseEventPortable` and call the helpers here.
//! The bodies are the same logic the TUI used to keep inline; the move
//! lets the GUI reuse them without duplicating built-in shortcuts
//! (Esc / Tab / BackTab / Ctrl-J / Ctrl-K / Ctrl-O) or plugin
//! `OnFocusedCanvas` keybinding dispatch.

#![allow(dead_code)] // items are wired up over the next few tasks

use savvagent_plugin::ContentBlockId;

use crate::app::Entry;

/// Direction of canvas-to-canvas traversal (`Ctrl-J` / `Ctrl-K`).
pub(crate) const CANVAS_NEXT: i32 = 1;
pub(crate) const CANVAS_PREV: i32 = -1;

/// Return the id of the canvas adjacent to `current` in `entries` order,
/// stepping by `delta` (`+1` next, `-1` previous) with wrap-around.
/// Returns `None` when there are no canvases, and `Some(current)` when it
/// is the only canvas. Non-canvas entries are skipped.
pub(crate) fn adjacent_canvas(
    entries: &[Entry],
    current: ContentBlockId,
    delta: i32,
) -> Option<ContentBlockId> {
    let ids: Vec<ContentBlockId> = entries
        .iter()
        .filter_map(|e| match e {
            Entry::Canvas { id, .. } => Some(*id),
            _ => None,
        })
        .collect();
    if ids.is_empty() {
        return None;
    }
    let pos = ids.iter().position(|x| *x == current)?;
    let len = ids.len() as i32;
    let next = (pos as i32 + delta).rem_euclid(len) as usize;
    Some(ids[next])
}

/// Compute the next focusable-element index after stepping `current` by
/// `delta` over `len` elements, wrapping. `None` (nothing focused) steps
/// to the first (`delta >= 0`) or last (`delta < 0`) element. Returns
/// `None` when there are no focusable elements.
pub(crate) fn cycle_index(current: Option<u32>, len: usize, delta: i32) -> Option<u32> {
    if len == 0 {
        return None;
    }
    let len_i = len as i32;
    let next = match current {
        Some(c) => (c as i32 + delta).rem_euclid(len_i),
        None if delta >= 0 => 0,
        None => len_i - 1,
    };
    Some(next as u32)
}
