//! First-launch migration: when config.toml is absent or `v1_done=false`,
//! scan the keyring for existing provider keys and decide the initial
//! `startup_providers` list. If more than one key exists, the TUI opens
//! a picker (UI side, Task 9); if exactly one or zero, write a deterministic
//! default. Either way, set `v1_done = true` so the picker never reopens.

use crate::config_file::ConfigFile;

/// Output of `decide_migration` — what to write or prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationOutcome {
    /// Multiple keys exist; TUI should open the picker.
    Picker { detected: Vec<String> },
    /// Zero or one keys; write this list directly.
    Direct { startup_providers: Vec<String> },
    /// Migration was already done.
    AlreadyDone,
}

/// Known provider ids to scan the keyring for. Order matters: deterministic
/// fallback when the user dismisses the picker.
const KNOWN_PROVIDERS: &[&str] = &["anthropic", "gemini", "openai", "local"];

pub fn decide_migration(cfg: &ConfigFile) -> MigrationOutcome {
    if cfg.migration.v1_done {
        return MigrationOutcome::AlreadyDone;
    }
    let detected: Vec<String> = KNOWN_PROVIDERS
        .iter()
        .filter(|id| {
            crate::creds::load(id)
                .map(|opt| opt.is_some())
                .unwrap_or(false)
        })
        .map(|s| s.to_string())
        .collect();
    match detected.len() {
        0 => MigrationOutcome::Direct {
            startup_providers: Vec::new(),
        },
        1 => MigrationOutcome::Direct {
            startup_providers: detected,
        },
        _ => MigrationOutcome::Picker { detected },
    }
}

/// Fallback when the user dismisses the picker without confirming.
/// Anthropic if present in `detected`; else first alphabetically.
pub fn dismissed_fallback(detected: &[String]) -> Vec<String> {
    if detected.iter().any(|s| s == "anthropic") {
        return vec!["anthropic".into()];
    }
    let mut sorted = detected.to_vec();
    sorted.sort();
    sorted
        .into_iter()
        .next()
        .map(|s| vec![s])
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_file::ConfigFile;

    #[test]
    fn already_done_short_circuits() {
        let mut cfg = ConfigFile::default();
        cfg.migration.v1_done = true;
        assert_eq!(decide_migration(&cfg), MigrationOutcome::AlreadyDone);
    }

    #[test]
    fn dismissed_fallback_prefers_anthropic() {
        let detected = vec!["gemini".into(), "anthropic".into(), "openai".into()];
        assert_eq!(dismissed_fallback(&detected), vec!["anthropic".to_string()]);
    }

    #[test]
    fn dismissed_fallback_alphabetical_when_no_anthropic() {
        let detected = vec!["openai".into(), "gemini".into()];
        assert_eq!(dismissed_fallback(&detected), vec!["gemini".to_string()]);
    }

    #[test]
    fn dismissed_fallback_empty_when_no_keys() {
        let detected: Vec<String> = vec![];
        assert!(dismissed_fallback(&detected).is_empty());
    }
}
