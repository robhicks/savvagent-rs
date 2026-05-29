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

use savvagent_plugin::{ContentBlockId, InputEvent, KeyCodePortable, KeyEventPortable, MouseEventPortable};

use crate::HostSlot;
use crate::app::{App, Entry, InputMode, make_input_textarea};

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

/// Dispatch a frame-pixel mouse event to the renderer for `id` and apply any
/// returned effects. Returns `true` if the renderer reported `dirty=true`,
/// so the caller can invalidate any cached texture for that canvas.
pub async fn handle_canvas_mouse(
    app: &mut App,
    host_slot: &HostSlot,
    id: ContentBlockId,
    mouse: MouseEventPortable,
) -> bool {
    let outcome = match app.canvas_registry.get_mut(id) {
        Some(renderer) => match renderer.dispatch(InputEvent::Mouse(mouse)).await {
            Ok(outcome) => outcome,
            Err(err) => {
                tracing::warn!(error = %err, "canvas mouse dispatch failed");
                return false;
            }
        },
        None => return false,
    };
    let dirty = outcome.dirty;
    apply_canvas_effects(app, host_slot, outcome.effects).await;
    dirty
}

/// Handle a key event delivered while a canvas holds focus.
///
/// Precedence:
/// 1. Built-in keys (`Esc`, `Tab`, `BackTab`, `Ctrl-J`, `Ctrl-K`, `Ctrl-O`).
/// 2. Plugin `KeyScope::OnFocusedCanvas` bindings.
/// 3. Raw key dispatch to the focused renderer.
///
/// Mirrors the TUI's previous inline handler in `main.rs` — the move
/// preserves behaviour byte-for-byte; only the entry shape changes.
pub async fn handle_focused_canvas_key(
    app: &mut App,
    host_slot: &HostSlot,
    id: ContentBlockId,
    element_idx: Option<u32>,
    key: KeyEventPortable,
) {
    let ctrl = key.modifiers.ctrl;

    // --- 1. Built-in keys (always win) ---
    match key.code {
        KeyCodePortable::Esc => {
            app.unfocus_canvas();
            return;
        }
        KeyCodePortable::Tab => {
            let len = app
                .canvas_registry
                .get_mut(id)
                .map(|r| r.focusable_elements().len())
                .unwrap_or(0);
            let next = cycle_index(element_idx, len, 1);
            if let Some(r) = app.canvas_registry.get_mut(id) {
                r.set_focus(next);
            }
            app.set_canvas_element(next);
            return;
        }
        KeyCodePortable::BackTab => {
            let len = app
                .canvas_registry
                .get_mut(id)
                .map(|r| r.focusable_elements().len())
                .unwrap_or(0);
            let next = cycle_index(element_idx, len, -1);
            if let Some(r) = app.canvas_registry.get_mut(id) {
                r.set_focus(next);
            }
            app.set_canvas_element(next);
            return;
        }
        KeyCodePortable::Char('j') if ctrl => {
            if let Some(next) = adjacent_canvas(&app.entries, id, CANVAS_NEXT) {
                app.focus_canvas(next, None);
            }
            return;
        }
        KeyCodePortable::Char('k') if ctrl => {
            if let Some(prev) = adjacent_canvas(&app.entries, id, CANVAS_PREV) {
                app.focus_canvas(prev, None);
            }
            return;
        }
        KeyCodePortable::Char('o') if ctrl => {
            // Open the focused canvas's final source in the system browser.
            let source = app.entries.iter().find_map(|e| match e {
                Entry::Canvas {
                    id: eid, source, ..
                } if *eid == id => Some(source.clone()),
                _ => None,
            });
            match source {
                Some(source) => {
                    use crate::plugin::builtin::html_canvas::open_in_browser;
                    match open_in_browser::write_temp_html(id, &source) {
                        Ok(path) => match open_in_browser::shell_open(&path) {
                            Ok(()) => app.push_note(format!(
                                "Opening canvas in browser ({})",
                                path.display()
                            )),
                            Err(err) => {
                                tracing::warn!(error = %err, "failed to open canvas in browser");
                                app.push_note(format!("Failed to open canvas: {err}"));
                            }
                        },
                        Err(err) => {
                            tracing::warn!(error = %err, "failed to write canvas temp file");
                            app.push_note(format!("Failed to write canvas file: {err}"));
                        }
                    }
                }
                None => app.push_note("No source available for this canvas yet".to_string()),
            }
            return;
        }
        _ => {}
    }

    // --- 2. Plugin OnFocusedCanvas bindings (built-in keys already missed) ---
    if let (Some(_reg), Some(idx)) = (&app.plugin_registry, &app.plugin_indexes) {
        let action = {
            let idx_guard = idx.read().await;
            let router = crate::plugin::keybindings::KeybindingRouter::new(&idx_guard);
            router.route_canvas(&key)
        };
        if let Some(action) = action {
            crate::dispatch_bound_action(app, action).await;
            return;
        }
    }

    // --- 3. Raw key dispatch to the renderer ---
    // Borrow the renderer mutably only for the dispatch await; `effects`
    // is owned afterwards so no borrow of `app` is held across
    // `apply_canvas_effects` (mirrors the mouse handler).
    let effects = if let Some(renderer) = app.canvas_registry.get_mut(id) {
        match renderer
            .dispatch(savvagent_plugin::InputEvent::Key(key))
            .await
        {
            Ok(outcome) => Some(outcome.effects),
            Err(err) => {
                tracing::warn!(error = %err, "canvas key dispatch failed");
                None
            }
        }
    } else {
        None
    };
    if let Some(effects) = effects {
        apply_canvas_effects(app, host_slot, effects).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, InputMode};
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    fn build_app() -> App {
        App::new("test-model".into(), PathBuf::from("/tmp"), "en".to_string())
    }

    fn empty_host_slot() -> crate::HostSlot {
        Arc::new(RwLock::new(None))
    }

    fn key(code: KeyCodePortable) -> KeyEventPortable {
        KeyEventPortable {
            code,
            modifiers: savvagent_plugin::KeyMods::default(),
        }
    }

    #[tokio::test]
    async fn esc_unfocuses_canvas() {
        let mut app = build_app();
        let id = ContentBlockId(0);
        // Manually seed focus state — no renderer needs to exist for the
        // built-in Esc branch.
        app.input_mode = InputMode::Canvas { id, element_idx: None };
        let hs = empty_host_slot();
        handle_focused_canvas_key(&mut app, &hs, id, None, key(KeyCodePortable::Esc)).await;
        assert!(matches!(app.input_mode, InputMode::Editing));
    }
}
