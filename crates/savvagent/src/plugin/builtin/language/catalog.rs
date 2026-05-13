//! Language catalog (static data + lookups).
//!
//! Mirrors `crates/savvagent/src/plugin/builtin/themes/catalog.rs`.
//! Persistence and env detection land in later tasks.

use std::path::PathBuf;
use serde::{Deserialize, Serialize};

/// Shipped language entry. Static; the catalog is a const slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Language {
    /// ISO 639-1 code used in `t!()` lookups and `language.toml`.
    pub code: &'static str,
    /// English display name (e.g. "Spanish").
    pub english_name: &'static str,
    /// Native display name (e.g. "Español", "हिन्दी").
    pub native_name: &'static str,
}

/// The shipped languages, in display order (English first).
const LANGUAGES: &[Language] = &[
    Language { code: "en", english_name: "English", native_name: "English" },
    Language { code: "es", english_name: "Spanish", native_name: "Español" },
    Language { code: "pt", english_name: "Portuguese", native_name: "Português" },
    Language { code: "hi", english_name: "Hindi", native_name: "हिन्दी" },
];

/// Returns the shipped catalog, in display order.
pub fn supported() -> &'static [Language] {
    LANGUAGES
}

/// Returns `true` if `code` is in the shipped catalog.
pub fn is_supported(code: &str) -> bool {
    LANGUAGES.iter().any(|l| l.code == code)
}

/// Looks up a language by code. Returns `None` for unsupported codes.
pub fn lookup(code: &str) -> Option<&'static Language> {
    LANGUAGES.iter().find(|l| l.code == code)
}

/// Normalize a POSIX-style locale env var value to a language code.
///
/// Strips the encoding suffix (`.UTF-8`), region suffix (`_RU`, `-US`),
/// and modifier suffix (`@euro`). Returns `None` for the C/POSIX
/// pseudo-locales and for empty/whitespace input.
fn normalize_env_locale(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || matches!(trimmed, "C" | "POSIX") {
        return None;
    }
    let head = trimmed
        .split(|c: char| c == '_' || c == '-' || c == '.' || c == '@')
        .next()?;
    if head.is_empty() {
        return None;
    }
    Some(head.to_ascii_lowercase())
}

/// On-disk shape of `~/.savvagent/language.toml`. Single key.
#[derive(Debug, Serialize, Deserialize)]
struct LanguageConfig {
    language: String,
}

/// Compute `~/.savvagent/language.toml`. Returns `None` if `$HOME` is
/// unset or empty (matches the convention in
/// `themes::catalog::config_path` and `sandbox.rs::sandbox_toml_path`).
pub(crate) fn config_path() -> Option<PathBuf> {
    let raw = std::env::var("HOME").ok()?;
    if raw.is_empty() {
        return None;
    }
    Some(PathBuf::from(raw).join(".savvagent").join("language.toml"))
}

/// Load the saved language code from `~/.savvagent/language.toml`.
///
/// Returns `None` if the file is missing, fails to parse, or its
/// `language` field is not in the shipped catalog. Logs a one-line
/// warning to stderr on parse failure or unsupported value.
pub fn load() -> Option<String> {
    let path = config_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    match toml::from_str::<LanguageConfig>(&text) {
        Ok(cfg) if is_supported(&cfg.language) => Some(cfg.language),
        Ok(cfg) => {
            eprintln!(
                "language.toml at {} contains unsupported language `{}`; falling back to detection.",
                path.display(),
                cfg.language
            );
            None
        }
        Err(e) => {
            eprintln!(
                "language.toml at {} failed to parse: {e}; falling back to detection.",
                path.display()
            );
            None
        }
    }
}

/// Persist `code` to `~/.savvagent/language.toml`. Silent no-op if
/// `$HOME` is unset (matches `themes::catalog::save`).
pub fn save(code: &str) -> std::io::Result<()> {
    let Some(path) = config_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let cfg = LanguageConfig { language: code.to_string() };
    let text = toml::to_string(&cfg).expect("LanguageConfig serialization is infallible");
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{HomeGuard, HOME_LOCK};
    use std::io::Write;

    #[test]
    fn catalog_includes_all_four_shipped_locales() {
        let codes: Vec<&str> = supported().iter().map(|l| l.code).collect();
        assert_eq!(codes, vec!["en", "es", "pt", "hi"]);
    }

    #[test]
    fn english_is_supported() {
        assert!(is_supported("en"));
    }

    #[test]
    fn klingon_is_not_supported() {
        assert!(!is_supported("klingon"));
    }

    #[test]
    fn lookup_returns_native_name() {
        assert_eq!(lookup("hi").map(|l| l.native_name), Some("हिन्दी"));
        assert_eq!(lookup("xx"), None);
    }

    #[test]
    fn normalize_env_locale_cases() {
        let cases: &[(&str, Option<&str>)] = &[
            ("es_ES.UTF-8", Some("es")),
            ("pt_BR@euro",  Some("pt")),
            ("en-US",       Some("en")),
            ("hi",          Some("hi")),
            ("C",           None),
            ("POSIX",       None),
            ("",            None),
            ("   ",         None),
        ];
        for (input, expected) in cases {
            let got = normalize_env_locale(input);
            let want = expected.map(String::from);
            assert_eq!(got, want, "normalize_env_locale({input:?}) -> {got:?}, expected {want:?}");
        }
    }

    #[test]
    fn save_then_load_round_trips() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        save("es").expect("save should succeed");
        assert_eq!(load(), Some("es".to_string()));
    }

    #[test]
    fn load_missing_file_returns_none() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        assert_eq!(load(), None);
    }

    #[test]
    fn load_malformed_toml_returns_none() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let path = config_path().expect("HOME set in HomeGuard");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"language =\n").unwrap();
        assert_eq!(load(), None);
    }

    #[test]
    fn load_unsupported_code_in_file_returns_none() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let path = config_path().expect("HOME set in HomeGuard");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"language = \"klingon\"\n").unwrap();
        assert_eq!(load(), None);
    }
}
