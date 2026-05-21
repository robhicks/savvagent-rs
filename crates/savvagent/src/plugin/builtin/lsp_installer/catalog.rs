//! Pinned LSP catalog (server id, version, download URLs, SHA256s).
//!
//! Versions + per-target SHA256s are pinned at publication time.
//! Refresh by editing this file when an upstream release lands.

/// One of the target triples we ship installers for. Matches the
/// cargo-dist `targets` list in `Cargo.toml`'s `[workspace.metadata.dist]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    /// `x86_64-unknown-linux-gnu`.
    LinuxX86_64Gnu,
    /// `aarch64-unknown-linux-gnu`.
    LinuxAarch64Gnu,
    /// `x86_64-apple-darwin`.
    MacosX86_64,
    /// `aarch64-apple-darwin`.
    MacosAarch64,
    /// `x86_64-pc-windows-msvc`.
    WindowsX86_64,
}

impl Target {
    /// Resolve the current host's target triple, or `None` if savvagent
    /// has no installer assets for this combination.
    pub fn current() -> Option<Self> {
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("linux", "x86_64") => Some(Self::LinuxX86_64Gnu),
            ("linux", "aarch64") => Some(Self::LinuxAarch64Gnu),
            ("macos", "x86_64") => Some(Self::MacosX86_64),
            ("macos", "aarch64") => Some(Self::MacosAarch64),
            ("windows", "x86_64") => Some(Self::WindowsX86_64),
            _ => None,
        }
    }
}

/// How a binary asset is packaged. The installer picks the right
/// extractor by the URL's actual suffix at install time; this enum is
/// the *predominant* archive kind for catalog browsing/documentation
/// (rust-analyzer's Windows asset is `.zip` while the rest are `.gz`,
/// for example).
#[derive(Debug, Clone, Copy)]
pub enum ArchiveKind {
    /// Single gzipped binary (`.gz`) — extracted as the binary itself.
    GzipOnly,
    /// Gzipped tarball (`.tar.gz` / `.tgz`).
    TarGz,
    /// Zip archive (`.zip`).
    Zip,
}

/// High-level grouping shown in the picker. `Binary` entries are
/// downloaded directly; `Npm` entries require `npm` on `$PATH`.
#[derive(Debug, Clone, Copy)]
pub enum Category {
    /// Direct binary download from a pinned URL.
    Binary,
    /// `npm i -g <package>` install.
    Npm,
}

/// The `[[language]]` entry to merge into `~/.savvagent/lsp.toml` after
/// a successful install. `command` may contain the literal token
/// `"{{BIN}}"` — the installer replaces it with the absolute path to
/// the installed binary.
#[derive(Debug, Clone, Copy)]
pub struct LspEntryTemplate {
    /// Stable id, matches `tool_lsp::config::LanguageEntry::id`.
    pub id: &'static str,
    /// File extensions (no leading dot).
    pub extensions: &'static [&'static str],
    /// Root marker filenames.
    pub root_markers: &'static [&'static str],
    /// Executable command to write. `"{{BIN}}"` is substituted with the
    /// installed binary's absolute path at write time.
    pub command: &'static str,
    /// Arguments passed to `command`.
    pub args: &'static [&'static str],
}

/// How [`super::installer`] should install a particular catalog entry.
#[derive(Debug, Clone, Copy)]
pub enum InstallMethod {
    /// Download from a templated URL, verify SHA256, extract.
    BinaryDownload {
        /// One URL + checksum per supported `Target`.
        urls: &'static [(Target, &'static str, &'static str)],
        /// Predominant archive kind (the installer inspects each URL's
        /// suffix to pick the actual extractor).
        archive: ArchiveKind,
        /// Relative path inside the extracted archive to the binary
        /// we'll point `lsp.toml` at, e.g. `"bin/lua-language-server"`.
        /// On Windows the installer appends `.exe` if missing.
        binary_path: &'static str,
    },
    /// `npm i -g <package>@<version>` (uses the host's npm).
    NpmGlobal {
        /// npm package name.
        package: &'static str,
        /// Binary that npm exposes after install — usually the same as
        /// `package` but sometimes different (e.g. pyright →
        /// `pyright-langserver`).
        binary: &'static str,
    },
}

/// A single catalog entry. `static CATALOG: &[CatalogEntry]` below
/// holds every server we ship installer support for.
#[derive(Debug, Clone, Copy)]
pub struct CatalogEntry {
    /// Stable id, used in `/lsp __install <id>` and as the install-dir
    /// name under `~/.savvagent/lsp-bin/<id>/`.
    pub id: &'static str,
    /// Human-readable name shown in the picker.
    pub display_name: &'static str,
    /// Language label shown in the picker (`"rust"`, `"typescript"`, …).
    pub language_label: &'static str,
    /// Pinned upstream version.
    pub version: &'static str,
    /// Picker grouping.
    pub category: Category,
    /// How to install.
    pub method: InstallMethod,
    /// What to write into `lsp.toml` after a successful install.
    pub lsp_entry: LspEntryTemplate,
}

/// Pinned v1 catalog. Versions and checksums are refreshed at catalog
/// publication time; see `docs/superpowers/specs/2026-05-20-lsp-installer-design.md`
/// for the update workflow.
pub static CATALOG: &[CatalogEntry] = &[
    // Populated by Task 12.
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_current_returns_some_on_supported_host() {
        assert!(
            Target::current().is_some(),
            "expected supported host target on CI runners"
        );
    }

    #[test]
    fn catalog_ids_are_unique() {
        let mut ids: Vec<&str> = CATALOG.iter().map(|e| e.id).collect();
        ids.sort();
        let len_before = ids.len();
        ids.dedup();
        assert_eq!(
            ids.len(),
            len_before,
            "duplicate ids in CATALOG: {:?}",
            ids
        );
    }

    #[test]
    fn binary_entries_cover_every_target() {
        for entry in CATALOG {
            if let InstallMethod::BinaryDownload { urls, .. } = entry.method {
                let mut covered: Vec<String> = urls
                    .iter()
                    .map(|(t, _, _)| format!("{t:?}"))
                    .collect();
                covered.sort();
                covered.dedup();
                assert_eq!(
                    covered.len(),
                    5,
                    "{}: must list one URL per Target variant (got {:?})",
                    entry.id,
                    covered
                );
            }
        }
    }
}
