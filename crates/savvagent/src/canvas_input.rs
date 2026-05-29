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

use crate::app::{App, Entry, InputMode, make_input_textarea};
use crate::HostSlot;

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

/// Apply the effects a canvas renderer emitted in response to an input
/// event. Phase 2.0 wires the two `OpenUrl` targets:
///
/// * `SystemBrowser` shells out to the OS opener (`xdg-open` / `open` /
///   `start`); failures are warn-only so a missing opener never crashes the
///   TUI. (Task 24 will consolidate this into an `open_in_browser` helper.)
/// * `ContinueConversation` stages the URL into the prompt editor and notes
///   it, leaving the user to review and submit — programmatic prompt
///   submission (`Effect::PromptSend`) is still a stub, so we don't fabricate
///   a turn here.
///
/// `Effect::Stack` is flattened recursively. Every other effect is logged and
/// ignored for Phase 2.0 (canvases only emit `OpenUrl` today).
pub(crate) async fn apply_canvas_effects(
    app: &mut App,
    _host_slot: &HostSlot,
    effects: Vec<savvagent_plugin::Effect>,
) {
    for effect in effects {
        match effect {
            savvagent_plugin::Effect::OpenUrl { url, target } => match target {
                savvagent_plugin::UrlTarget::SystemBrowser => {
                    let opener = if cfg!(target_os = "macos") {
                        "open"
                    } else if cfg!(target_os = "windows") {
                        "start"
                    } else {
                        "xdg-open"
                    };
                    match tokio::process::Command::new(opener).arg(&url).spawn() {
                        Ok(_) => {
                            app.push_note(format!("Opening {url} in browser"));
                        }
                        Err(err) => {
                            tracing::warn!(error = %err, %url, "failed to open URL in browser");
                            app.push_note(format!("Failed to open {url}: {err}"));
                        }
                    }
                }
                savvagent_plugin::UrlTarget::ContinueConversation => {
                    app.input_textarea = make_input_textarea(std::iter::once(url.clone()));
                    app.input_mode = InputMode::Editing;
                    app.push_note(format!(
                        "Staged \"{url}\" in the prompt — press Enter to send"
                    ));
                }
            },
            savvagent_plugin::Effect::Stack(inner) => {
                Box::pin(apply_canvas_effects(app, _host_slot, inner)).await;
            }
            other => {
                tracing::debug!(effect = ?other, "ignoring canvas effect (unhandled in Phase 2.0)");
            }
        }
    }
}
