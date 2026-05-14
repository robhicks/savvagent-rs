//! Build the default system prompt from real session data.
//!
//! The prompt has five sections rendered in this order:
//! identity → behavior expectations → tool affordances → environment →
//! conventions. Static sections live in `const` strings; dynamic
//! sections are rendered from [`PromptEnv`] and a `&[ToolDef]` slice.
//!
//! Security: tool-server-supplied text never enters the rendered
//! prompt. The affordances section lists tool NAMES only — descriptions
//! are delivered to the model via the request's typed `tools` field,
//! not promoted into the system message. See the spec at
//! `docs/superpowers/specs/2026-05-14-default-system-prompt-design.md`
//! §5.3.

use std::path::Path;

use savvagent_protocol::ToolDef;

/// App-version label source. See [`crate::HostConfig::with_app_version`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppVersion<'a> {
    /// Embedder-supplied label, e.g. the TUI binary's `CARGO_PKG_VERSION`.
    /// Rendered as `Savvagent version: <version>`.
    App(&'a str),
    /// No embedder version provided. Falls back to the `savvagent-host`
    /// crate version. Rendered as `Savvagent host crate version:
    /// <version>` to flag the distinction for library callers.
    HostCrateFallback(&'static str),
}

/// Snapshot of the host environment used to render the default prompt.
/// Cheap to construct; pure to read.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Consumed by Task 8 (Host::start wiring); allow removed then.
pub struct PromptEnv<'a> {
    /// Root directory of the user's project.
    pub project_root: &'a Path,
    /// Operating system name (e.g. `"linux"`, `"macos"`).
    pub os: &'static str,
    /// CPU architecture (e.g. `"x86_64"`, `"aarch64"`).
    pub arch: &'static str,
    /// True iff git is present at the project root.
    pub git_present: bool,
    /// Trusted shell-availability flag. True iff the host wired a
    /// `tool-bash`-marker endpoint. Sourced from
    /// `ToolRegistry::bash_available()`, NOT from name matching.
    pub bash_available: bool,
    /// App version label source.
    pub app_version: AppVersion<'a>,
}

const IDENTITY: &str = "\
You are Savvagent — an open-source terminal coding agent. You run \
locally as a Rust binary and talk to the user through a TUI in their \
terminal. You orchestrate tool calls and provider completions over MCP.";

const BEHAVIOR: &str = "\
## Behavior expectations

- Use the tools available to you proactively. If you're not sure \
whether a tool will work for a task, try it before assuming a \
limitation.
- Never claim you \"cannot access\" something without first checking \
whether a tool you have can reach it (e.g. the shell can run `gh`, \
`curl`, `git`, `rg`, language toolchains, build systems, and any \
other CLI installed on the user's machine).
- Prefer concrete, verifiable actions over disclaimers. When the \
user asks about external state (an issue, a file, a process), look \
it up.
- Keep responses tight. The user is reading them in a terminal.";

const CONVENTIONS: &str = "\
## Conventions

- File paths in user-visible output use the form `path/to/file.rs:42` \
so the user can click to navigate in supported terminals.
- When you edit files, show the user a brief summary of what changed, \
not the full diff (their TUI already renders diffs).";

/// Render the default prompt. Pure over `(env, tools)`. The builder
/// reads `tool.name` only — descriptions are NOT included verbatim.
#[allow(dead_code)] // Consumed by Task 8 (Host::start wiring); allow removed then.
pub fn build(env: &PromptEnv<'_>, tools: &[ToolDef]) -> String {
    let _ = (env, tools); // referenced in later tasks
    let mut out = String::new();
    out.push_str(IDENTITY);
    out.push_str("\n\n");
    out.push_str(BEHAVIOR);
    out.push_str("\n\n");
    out.push_str(CONVENTIONS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> PromptEnv<'static> {
        PromptEnv {
            project_root: Path::new("/tmp/proj"),
            os: "linux",
            arch: "x86_64",
            git_present: false,
            bash_available: false,
            app_version: AppVersion::App("0.14.0"),
        }
    }

    #[test]
    fn build_contains_identity_savvagent_name() {
        let s = build(&env(), &[]);
        assert!(s.contains("You are Savvagent"), "{s}");
    }

    #[test]
    fn build_contains_proactive_tool_use_guidance() {
        let s = build(&env(), &[]);
        assert!(s.contains("Use the tools available to you proactively"));
    }

    #[test]
    fn build_contains_no_cannot_access_disclaimer_warning() {
        let s = build(&env(), &[]);
        assert!(s.contains("Never claim you \"cannot access\""));
    }

    #[test]
    fn build_contains_path_line_number_convention() {
        let s = build(&env(), &[]);
        assert!(s.contains("path/to/file.rs:42"));
    }
}
