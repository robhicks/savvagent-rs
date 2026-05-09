//! Tool-call permission policy.
//!
//! Sits between the host's turn loop and [`crate::tools::ToolRegistry::call`].
//! Each tool use the model proposes is evaluated against a
//! [`PermissionPolicy`], which yields a [`Verdict`]:
//!
//! - [`Verdict::Allow`] — the call proceeds.
//! - [`Verdict::Deny`] — the call is replaced with a synthesized error
//!   `tool_result` that's fed back to the model so it can adjust.
//! - [`Verdict::Ask`] — the host emits
//!   [`crate::TurnEvent::PermissionRequested`] and waits for the embedder to
//!   call [`crate::Host::resolve_permission`] with a [`PermissionDecision`].
//!
//! PR 1 (M9) ships built-in defaults only. The layered config sources
//! (`SAVVAGENT.md` front-matter, `~/.savvagent/permissions.toml`,
//! `HostConfig::with_permission_overrides`) land in M9 PR 4.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Outcome of evaluating a tool call against the policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Run the tool without prompting.
    Allow,
    /// Pause the turn and ask the user.
    Ask {
        /// Short, human-readable description for the modal.
        summary: String,
    },
    /// Refuse the call. `reason` flows back to the model in the synthesized
    /// `tool_result` so it can re-plan.
    Deny {
        /// Why the policy denied the call.
        reason: String,
    },
}

/// Resolution coming back from the embedder for a [`Verdict::Ask`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    /// Run the tool this once.
    Allow,
    /// Refuse this call.
    Deny,
}

/// Built-in permission policy. Stateless across calls; cheap to clone.
#[derive(Debug, Clone)]
pub struct PermissionPolicy {
    project_root: PathBuf,
}

impl PermissionPolicy {
    /// Built-in defaults rooted at `project_root`.
    ///
    /// - `read_file`, `list_dir`, `glob`: `Allow`
    /// - `write_file`: `Allow` inside `project_root`, `Ask` outside
    /// - `run` (bash): `Ask`
    /// - any tool with a `path` arg pointing at `.env*` or `.ssh/`: `Deny`
    /// - unknown tools: `Ask`
    pub fn default_for(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
        }
    }

    /// Resolve a verdict for a tool call.
    pub fn evaluate(&self, tool_name: &str, args: &Value) -> Verdict {
        // Sensitive paths are denied regardless of the tool. Tools that don't
        // take a `path` arg sail past this check.
        if let Some(p) = path_arg(args) {
            if is_sensitive_path(&p) {
                return Verdict::Deny {
                    reason: format!("path `{p}` is policy-protected (.env / .ssh)"),
                };
            }
        }

        match tool_name {
            "read_file" | "list_dir" | "glob" => Verdict::Allow,
            "write_file" => match path_arg(args) {
                Some(p) if is_under(&p, &self.project_root) => Verdict::Allow,
                Some(p) => Verdict::Ask {
                    summary: format!("write_file outside project root: {p}"),
                },
                None => Verdict::Ask {
                    summary: "write_file with no path".into(),
                },
            },
            "run" => Verdict::Ask {
                summary: command_summary(args).unwrap_or_else(|| "run".into()),
            },
            other => Verdict::Ask {
                summary: format!("{other}({})", short_args(args)),
            },
        }
    }
}

fn path_arg(args: &Value) -> Option<String> {
    args.get("path")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

fn command_summary(args: &Value) -> Option<String> {
    let cmd = args.get("command").and_then(|v| v.as_str())?;
    let trimmed: String = cmd.chars().take(80).collect();
    Some(format!("run: {trimmed}"))
}

fn short_args(args: &Value) -> String {
    let s = serde_json::to_string(args).unwrap_or_else(|_| "?".into());
    if s.len() <= 80 {
        s
    } else {
        format!("{}...", &s[..80])
    }
}

fn is_sensitive_path(p: &str) -> bool {
    let s = p.replace('\\', "/");
    let last = s.rsplit('/').next().unwrap_or("");
    if last == ".env" || last.starts_with(".env.") {
        return true;
    }
    s == ".ssh" || s.starts_with(".ssh/") || s.contains("/.ssh/") || s.ends_with("/.ssh")
}

fn is_under(p: &str, root: &Path) -> bool {
    let path = Path::new(p);
    if path.is_absolute() {
        return path.starts_with(root);
    }
    // Relative paths resolve against the project root, so they're inside
    // unless they `..` past it. Component-walk and refuse anything weirder
    // than Normal/CurDir/ParentDir.
    let mut depth: i32 = 0;
    for c in path.components() {
        use std::path::Component::*;
        match c {
            Normal(_) => depth += 1,
            CurDir => {}
            ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn p() -> PermissionPolicy {
        PermissionPolicy::default_for("/home/me/proj")
    }

    #[test]
    fn read_list_glob_allowed() {
        for tool in ["read_file", "list_dir", "glob"] {
            assert_eq!(
                p().evaluate(tool, &json!({"path": "src/lib.rs"})),
                Verdict::Allow,
                "{tool}",
            );
        }
    }

    #[test]
    fn write_file_inside_project_allowed() {
        assert_eq!(
            p().evaluate("write_file", &json!({"path": "src/lib.rs"})),
            Verdict::Allow
        );
        assert_eq!(
            p().evaluate(
                "write_file",
                &json!({"path": "/home/me/proj/src/lib.rs"})
            ),
            Verdict::Allow
        );
        assert_eq!(
            p().evaluate("write_file", &json!({"path": "./Cargo.toml"})),
            Verdict::Allow
        );
    }

    #[test]
    fn write_file_outside_project_asks() {
        assert!(matches!(
            p().evaluate("write_file", &json!({"path": "/etc/hosts"})),
            Verdict::Ask { .. }
        ));
        assert!(matches!(
            p().evaluate("write_file", &json!({"path": "../../oops"})),
            Verdict::Ask { .. }
        ));
    }

    #[test]
    fn dotenv_denied_for_any_tool() {
        for path in [".env", "src/.env", ".env.local", "a/b/.env.production"] {
            assert!(
                matches!(
                    p().evaluate("read_file", &json!({"path": path})),
                    Verdict::Deny { .. }
                ),
                "read_file {path}"
            );
            assert!(
                matches!(
                    p().evaluate("write_file", &json!({"path": path})),
                    Verdict::Deny { .. }
                ),
                "write_file {path}"
            );
        }
    }

    #[test]
    fn ssh_dir_denied() {
        for path in [
            "/home/me/.ssh/id_rsa",
            ".ssh/known_hosts",
            "some/path/.ssh/key",
        ] {
            assert!(
                matches!(
                    p().evaluate("read_file", &json!({"path": path})),
                    Verdict::Deny { .. }
                ),
                "{path}"
            );
        }
    }

    #[test]
    fn bash_run_asks_with_command_summary() {
        match p().evaluate("run", &json!({"command": "ls -la"})) {
            Verdict::Ask { summary } => assert!(summary.contains("ls -la"), "{summary}"),
            other => panic!("expected Ask, got {other:?}"),
        }
    }

    #[test]
    fn unknown_tool_asks() {
        assert!(matches!(
            p().evaluate("mystery_tool", &json!({"x": 1})),
            Verdict::Ask { .. }
        ));
    }
}
