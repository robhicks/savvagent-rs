//! Language catalog (static data + lookups).
//!
//! Mirrors `crates/savvagent/src/plugin/builtin/themes/catalog.rs`.
//! Persistence and env detection land in later tasks.

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
