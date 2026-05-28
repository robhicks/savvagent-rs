//! GUI editor state + paint pass for the `view-file` / `edit-file`
//! marker screens. The buffer lives on `SavvagentApp` (not on `App`,
//! which still owns the ratatui editor for the TUI path); it is
//! lazy-loaded from `App::active_file_path` the first frame after a
//! marker screen opens and cleared when the screen pops.
