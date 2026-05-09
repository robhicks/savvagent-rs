//! Project-level context.
//!
//! `SAVVAGENT.md` carries optional YAML front-matter delimited by `---`
//! lines. The body (whatever follows the closing delimiter, or the whole
//! file if no front-matter is present) gets injected into the system
//! prompt. The front-matter feeds [`crate::permissions::PermissionPolicy`]
//! its project-pinned rules.

use std::path::Path;

use serde::Deserialize;

use crate::permissions::FrontMatterPermissions;

/// File the host looks for at the project root for project-specific
/// instructions (analogous to OpenCode's `AGENTS.md`).
pub const PROJECT_CONTEXT_FILE: &str = "SAVVAGENT.md";

/// Result of [`parse_savvagent_md`]: a possibly-empty body plus whatever
/// permissions section was parsed from the front-matter. Both fields are
/// safely defaulted on missing file or malformed front-matter — callers
/// don't need to distinguish "no front-matter" from "front-matter parse
/// error" since the policy treats both as "no overrides."
#[derive(Debug, Default, Clone)]
pub struct ParsedProjectContext {
    /// Body text below the front-matter (or the full file if there's none).
    /// Already trimmed of leading/trailing whitespace.
    pub body: Option<String>,
    /// Permissions parsed from the front-matter, or empty defaults.
    pub permissions: FrontMatterPermissions,
}

/// Read `SAVVAGENT.md` (if present) and split into body + front-matter.
/// Falls back silently to defaults on missing file, malformed
/// front-matter, or unknown YAML schema.
pub fn parse_savvagent_md(project_root: &Path) -> ParsedProjectContext {
    let Ok(text) = std::fs::read_to_string(project_root.join(PROJECT_CONTEXT_FILE)) else {
        return ParsedProjectContext::default();
    };
    parse_text(&text)
}

#[derive(Debug, Default, Clone, Deserialize)]
struct FrontMatterDoc {
    #[serde(default)]
    permissions: FrontMatterPermissions,
}

/// Strip an optional `---`-delimited YAML block from the head of `text`,
/// parse its `permissions` section, and return the body + permissions.
fn parse_text(text: &str) -> ParsedProjectContext {
    if let Some((yaml, body)) = split_front_matter(text) {
        let permissions = serde_yaml_ng::from_str::<FrontMatterDoc>(yaml)
            .map(|d| d.permissions)
            .unwrap_or_default();
        let body = if body.trim().is_empty() {
            None
        } else {
            Some(body.trim().to_string())
        };
        ParsedProjectContext { body, permissions }
    } else {
        let body = if text.trim().is_empty() {
            None
        } else {
            Some(text.trim().to_string())
        };
        ParsedProjectContext {
            body,
            permissions: FrontMatterPermissions::default(),
        }
    }
}

/// Match `^---\n<yaml>\n---(\n|$)<body>` and return `(yaml, body)`. Returns
/// `None` if `text` doesn't open with a `---` line or has no closing
/// delimiter on its own line.
fn split_front_matter(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix("---\n")?;
    let close_idx = rest.find("\n---\n").or_else(|| {
        // Allow a trailing `---` with no newline after it (file ends there).
        if rest.ends_with("\n---") {
            Some(rest.len() - 4)
        } else {
            None
        }
    })?;
    let yaml = &rest[..close_idx];
    // Skip the closing delimiter and one optional newline.
    let after = &rest[close_idx + 4..];
    let body = after.strip_prefix('\n').unwrap_or(after);
    Some((yaml, body))
}

/// Build a system prompt by combining an optional override with the
/// project context body parsed from `SAVVAGENT.md`. Front-matter (if any)
/// is consumed by [`crate::permissions::PermissionPolicy`] and *not*
/// included in the prompt.
pub fn system_prompt(project_root: &Path, override_prompt: Option<&str>) -> Option<String> {
    let parsed = parse_savvagent_md(project_root);
    match (override_prompt, parsed.body) {
        (Some(p), Some(c)) => Some(format!(
            "{p}\n\n# Project context (from {PROJECT_CONTEXT_FILE})\n\n{c}"
        )),
        (Some(p), None) => Some(p.to_string()),
        (None, Some(c)) => Some(format!(
            "# Project context (from {PROJECT_CONTEXT_FILE})\n\n{c}"
        )),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn no_file_no_override_yields_none() {
        let d = tempdir().unwrap();
        assert!(system_prompt(d.path(), None).is_none());
    }

    #[test]
    fn override_only() {
        let d = tempdir().unwrap();
        let s = system_prompt(d.path(), Some("base prompt")).unwrap();
        assert_eq!(s, "base prompt");
    }

    #[test]
    fn merges_context_and_override() {
        let d = tempdir().unwrap();
        std::fs::write(d.path().join(PROJECT_CONTEXT_FILE), "use snake_case.").unwrap();
        let s = system_prompt(d.path(), Some("base")).unwrap();
        assert!(s.starts_with("base"));
        assert!(s.contains("use snake_case."));
    }

    #[test]
    fn context_only() {
        let d = tempdir().unwrap();
        std::fs::write(d.path().join(PROJECT_CONTEXT_FILE), "ctx").unwrap();
        let s = system_prompt(d.path(), None).unwrap();
        assert!(s.contains("ctx"));
    }

    #[test]
    fn front_matter_is_stripped_from_system_prompt() {
        let d = tempdir().unwrap();
        std::fs::write(
            d.path().join(PROJECT_CONTEXT_FILE),
            "---\npermissions:\n  allow: []\n---\nuse snake_case.",
        )
        .unwrap();
        let s = system_prompt(d.path(), None).unwrap();
        assert!(s.contains("use snake_case."));
        assert!(!s.contains("permissions"), "{}", s);
        assert!(!s.contains("---"), "{}", s);
    }

    #[test]
    fn front_matter_permissions_round_trip() {
        let parsed = parse_text(
            "---\n\
             permissions:\n  \
               allow:\n    \
                 - tool: run\n      \
                   command: cargo\n  \
               deny:\n    \
                 - tool: read_file\n      \
                   path: secret.txt\n\
             ---\n\
             body text\n",
        );
        assert_eq!(parsed.body.as_deref(), Some("body text"));
        assert_eq!(parsed.permissions.allow.len(), 1);
        assert_eq!(parsed.permissions.allow[0].tool, "run");
        assert_eq!(parsed.permissions.allow[0].command.as_deref(), Some("cargo"));
        assert_eq!(parsed.permissions.deny.len(), 1);
        assert_eq!(parsed.permissions.deny[0].path.as_deref(), Some("secret.txt"));
    }

    #[test]
    fn malformed_front_matter_falls_back_silently() {
        // Garbage YAML between the delimiters → permissions stays empty,
        // body still surfaces. The host should never refuse to start.
        let parsed = parse_text("---\n: : not yaml :\n---\nbody\n");
        assert_eq!(parsed.body.as_deref(), Some("body"));
        assert!(parsed.permissions.allow.is_empty());
        assert!(parsed.permissions.deny.is_empty());
    }

    #[test]
    fn no_front_matter_treats_whole_file_as_body() {
        let parsed = parse_text("just a body, no delimiters\n");
        assert_eq!(parsed.body.as_deref(), Some("just a body, no delimiters"));
        assert!(parsed.permissions.allow.is_empty());
    }

    #[test]
    fn front_matter_only_no_body() {
        let parsed = parse_text("---\npermissions:\n  allow: []\n---\n");
        assert!(parsed.body.is_none());
    }
}
