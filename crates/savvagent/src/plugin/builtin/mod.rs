//! Built-in plugin implementations. Each subdirectory hosts one plugin.

/// Clears the conversation log; registered as `/clear`.
pub mod clear;

/// Filterable slash-command picker; opened via `/` from the home view.
pub mod command_palette;

/// Provider connection picker; opened via `/connect` with no args.
pub mod connect;

/// Basic in-TUI file editor; opened via `/edit <path>`.
pub mod edit_file;

/// Footer widget: sandbox state + turn state + working dir + key reminder.
pub mod home_footer;

/// Tips widget: one-line hint above the prompt; switches text after Connect.
pub mod home_tips;

/// Cycles to the next model on the active provider; registered as `/model`.
pub mod model;

/// Transcript picker; opened via `/resume` with an in-memory cache backed
/// by the [`HookKind::TranscriptSaved`] hook.
pub mod resume;

/// Saves the active transcript to disk; registered as `/save [path]`.
pub mod save;

/// Startup HUD screen with connect status; responds to HostStarting + Connect.
pub mod splash;

/// Fullscreen read-only file viewer; opened via `/view <path>`.
pub mod view_file;
