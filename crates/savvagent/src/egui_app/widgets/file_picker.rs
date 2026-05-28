//! `Ctrl+O` file picker for the GUI prompt. Wraps an
//! `egui_file_dialog::FileDialog` and exposes `open()`, `update(ctx)`,
//! and `take_picked() -> Option<PathBuf>`, plus the pure helper
//! `splice_at_reference` that appends `@<path>` to a prompt buffer.
//!
//! Confirmed `egui_file_dialog::FileDialog` API names (against 0.11.0):
//! - `FileDialog::new() -> Self` — creates a new dialog instance with
//!   default values; owned by the caller.
//! - `pick_file(&mut self)` — shortcut to open the dialog in pick-file
//!   mode (NOT `select_file()` — the upstream rename to `pick_*` is the
//!   surface name on 0.11.0).
//! - `update(&mut self, ctx: &egui::Context) -> &Self` — per-frame
//!   update; must be called every frame while the dialog is visible.
//! - `take_picked(&mut self) -> Option<PathBuf>` — returns the picked
//!   path once after the user confirms, then transitions the dialog to
//!   `DialogState::Closed`. (A non-consuming `picked() -> Option<&Path>`
//!   companion also exists if a borrow is preferable.)
