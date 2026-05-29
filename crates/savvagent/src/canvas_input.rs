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
