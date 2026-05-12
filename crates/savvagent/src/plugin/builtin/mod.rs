//! Built-in plugin implementations. Each subdirectory hosts one plugin.

/// Filterable slash-command picker; opened via `/` from the home view.
pub mod command_palette;

/// Footer widget: sandbox state + turn state + working dir + key reminder.
pub mod home_footer;

/// Tips widget: one-line hint above the prompt; switches text after Connect.
pub mod home_tips;

/// Startup HUD screen with connect status; responds to HostStarting + Connect.
pub mod splash;

/// Fullscreen read-only file viewer; opened via `/view <path>`.
pub mod view_file;
