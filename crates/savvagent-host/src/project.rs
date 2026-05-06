//! Project-level context.

use std::path::Path;

/// File the host looks for at the project root for project-specific
/// instructions (analogous to OpenCode's `AGENTS.md`).
pub const PROJECT_CONTEXT_FILE: &str = "SAVVAGENT.md";

/// Build a system prompt by combining an optional override with the project
/// context file (`SAVVAGENT.md`) at `project_root`, if present.
pub fn system_prompt(project_root: &Path, override_prompt: Option<&str>) -> Option<String> {
    let context = std::fs::read_to_string(project_root.join(PROJECT_CONTEXT_FILE)).ok();
    match (override_prompt, context) {
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
}
