//! Native egui front-end (v0.19.0 migration, Plan 1). Built alongside the
//! ratatui TUI and launched via the `savvagent gui` subcommand — see
//! `docs/superpowers/specs/2026-05-26-v0.19.0-egui-frontend-design.md`.
//!
//! Pinned eframe/egui: 0.32. Trait method confirmed: `fn update(&mut self,
//! ctx: &egui::Context, frame: &mut eframe::Frame)`. eframe default features
//! (glow + x11 + wayland + default_fonts) are kept so the window opens on
//! Linux; submodules are added by later foundation tasks.

/// The eframe application. Fields are filled in by later tasks (host slot,
/// worker channel, render-model cache, prompt buffer).
pub struct SavvagentApp {
    // Populated in Task 6.
}

impl SavvagentApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {}
    }
}

impl eframe::App for SavvagentApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label("savvagent egui front-end — foundation");
        });
    }
}

/// Launch the native window. Called from `main()` on the `gui` subcommand.
pub fn run() -> eframe::Result {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 800.0])
            .with_title("savvagent"),
        ..Default::default()
    };
    eframe::run_native(
        "savvagent",
        native_options,
        Box::new(|cc| Ok(Box::new(SavvagentApp::new(cc)))),
    )
}
