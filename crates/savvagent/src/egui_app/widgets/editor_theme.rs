//! Adapter: a `Palette` (Plan 1's `Color32`-backed semantic-slot model) →
//! an `egui_code_editor::ColorTheme` whose per-token colors track the
//! active TUI theme. Mirrors the slot correspondences in
//! `plugin/builtin/themes/editor_theme.rs::build_editor_theme`.
//!
//! Confirmed `egui_code_editor::ColorTheme` field names (against 0.2.17):
//! - `name: &'static str` — display name (e.g. "GRUVBOX")
//! - `dark: bool` — whether the theme is a dark variant
//! - `bg: &'static str` — background hex (e.g. "#1D2021")
//! - `cursor: &'static str` — cursor color hex
//! - `selection: &'static str` — selection background hex
//! - `comments: &'static str` — comment-token color hex
//! - `functions: &'static str` — function-name color hex
//! - `keywords: &'static str` — keyword color hex
//! - `literals: &'static str` — literal color hex (booleans, nil, etc.)
//! - `numerics: &'static str` — numeric-literal color hex
//! - `punctuation: &'static str` — operator/punctuation color hex
//! - `strs: &'static str` — string-literal color hex
//! - `types: &'static str` — type-name color hex
//! - `special: &'static str` — special-token color hex (errors, attributes)
//!
//! All color fields are `&'static str` (hex strings), NOT `egui::Color32`.
//! The plan's `Box::leak(format!("#{:02X}{:02X}{:02X}", r, g, b))` pattern
//! is the canonical way to materialize per-frame `Palette` colors as the
//! `'static` strings `ColorTheme` requires. Construct via struct literal
//! (all 14 fields are `pub`); `Default` is also implemented, and the crate
//! ships nine pre-defined `const` themes (e.g. `ColorTheme::GRUVBOX`,
//! `ColorTheme::GITHUB_DARK`). A single `monocolor()` constructor exists
//! for monochromatic palettes.
