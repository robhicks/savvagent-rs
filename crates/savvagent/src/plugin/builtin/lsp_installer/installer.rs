//! Per-entry installer: binary download/verify/extract or npm i -g.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::AsyncWriteExt;

use super::catalog::{ArchiveKind, CatalogEntry, InstallMethod, Target};

/// Streaming progress emitted by the install path via its `notify`
/// callback. Each variant maps roughly to one stage of the install
/// pipeline; the wrapping plugin pumps these into the conversation log
/// as styled notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallProgress {
    /// Install task started for `entry_id`. Fired once per entry.
    Started {
        /// Catalog id (matches `CatalogEntry::id`).
        entry_id: String,
    },
    /// HTTP download in flight. `total` is `None` if the server didn't
    /// send `Content-Length`. v1 emits this with the final byte count
    /// only (no streaming chunk progress); the field is shaped for a
    /// follow-up streaming implementation.
    Downloading {
        /// Catalog id.
        entry_id: String,
        /// Bytes received so far.
        bytes_so_far: u64,
        /// Total size in bytes, if the server reported it.
        total: Option<u64>,
    },
    /// SHA256 verification in progress. Absence of a subsequent
    /// `Failed` means it passed.
    Verifying {
        /// Catalog id.
        entry_id: String,
    },
    /// Archive extraction in progress.
    Extracting {
        /// Catalog id.
        entry_id: String,
    },
    /// `npm i -g` running; `line` is one line of npm's combined
    /// stdout/stderr.
    RunningNpm {
        /// Catalog id.
        entry_id: String,
        /// One line of npm's output.
        line: String,
    },
    /// Install succeeded. `installed_at` is the absolute path to the
    /// binary that should be referenced in `lsp.toml`.
    Done {
        /// Catalog id.
        entry_id: String,
        /// Absolute path to the installed binary.
        installed_at: PathBuf,
    },
    /// Install failed (terminal for this entry; the batch continues).
    Failed {
        /// Catalog id.
        entry_id: String,
        /// Human-readable reason.
        reason: String,
    },
}

/// Returned by the install path on success — carries the data
/// [`super::config_writer`] needs to upsert the entry into
/// `~/.savvagent/lsp.toml`.
#[derive(Debug, Clone)]
pub struct InstallOutcome {
    /// Catalog id (matches `CatalogEntry::id`).
    pub entry_id: String,
    /// Absolute path to the installed binary; used as `command` in the
    /// `lsp.toml` entry when the catalog template's `command` is
    /// `"{{BIN}}"`.
    pub installed_at: PathBuf,
}

/// Reasons the install path can return `Err`.
#[derive(Debug, Error)]
pub enum InstallError {
    /// The host's `(OS, arch)` doesn't match any supported `Target`.
    #[error("unsupported host target — {0}")]
    UnsupportedTarget(String),
    /// A required external tool (e.g. `npm`) isn't on `$PATH`.
    #[error("required tool not found: {tool} (install it and re-run /lsp)")]
    ToolNotFound {
        /// The missing tool's executable name.
        tool: String,
    },
    /// HTTP download failed.
    #[error("download failed: {0}")]
    Download(String),
    /// SHA256 of the downloaded payload didn't match the catalog's
    /// pinned value.
    #[error("checksum mismatch for {entry_id}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        /// Catalog id.
        entry_id: String,
        /// Pinned SHA256 from the catalog (hex).
        expected: String,
        /// Computed SHA256 from the download (hex).
        actual: String,
    },
    /// Archive extraction failed.
    #[error("extract failed for {entry_id}: {reason}")]
    Extract {
        /// Catalog id.
        entry_id: String,
        /// Reason from the extractor.
        reason: String,
    },
    /// `npm i -g` exited non-zero or couldn't be invoked.
    #[error("npm install failed for {entry_id}: {reason}")]
    Npm {
        /// Catalog id.
        entry_id: String,
        /// Reason (npm's exit message or our own framing).
        reason: String,
    },
    /// Filesystem error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Thin abstraction over an HTTP client so tests can substitute a
/// fixture without spinning up a real server.
#[async_trait::async_trait]
pub trait Downloader: Send + Sync {
    /// Fetch `url` and return its body bytes.
    async fn fetch(&self, url: &str) -> Result<bytes::Bytes, InstallError>;
}

/// Production [`Downloader`] backed by `reqwest`. Sets the
/// `User-Agent: savvagent/<version>` header so GitHub's asset CDN logs
/// us as a known client.
pub struct ReqwestDownloader {
    /// The underlying client. Reused across fetches.
    pub client: reqwest::Client,
}

impl ReqwestDownloader {
    /// Build a default client. Returns `None` if `reqwest::Client::builder().build()`
    /// fails (network stack misconfigured); callers fall back to a
    /// PushNote error.
    pub fn new() -> Option<Self> {
        reqwest::Client::builder()
            .build()
            .ok()
            .map(|client| Self { client })
    }
}

#[async_trait::async_trait]
impl Downloader for ReqwestDownloader {
    async fn fetch(&self, url: &str) -> Result<bytes::Bytes, InstallError> {
        let resp = self
            .client
            .get(url)
            .header(
                reqwest::header::USER_AGENT,
                concat!("savvagent/", env!("CARGO_PKG_VERSION")),
            )
            .send()
            .await
            .map_err(|e| InstallError::Download(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(InstallError::Download(format!(
                "HTTP {}: {}",
                resp.status(),
                url
            )));
        }
        resp.bytes()
            .await
            .map_err(|e| InstallError::Download(e.to_string()))
    }
}

/// Install a single `BinaryDownload` catalog entry: download from the
/// pinned URL, verify SHA256, extract into
/// `<lsp_bin_root>/<entry.id>/`, set the executable bit on Unix.
///
/// `notify` receives one `InstallProgress` per stage; the wrapping
/// plugin pumps these into the conversation log.
pub async fn install_binary_entry(
    entry: &CatalogEntry,
    target: Target,
    lsp_bin_root: &Path,
    downloader: &dyn Downloader,
    notify: impl Fn(InstallProgress) + Send + Sync,
) -> Result<InstallOutcome, InstallError> {
    let InstallMethod::BinaryDownload {
        urls,
        archive: _,
        binary_path,
    } = entry.method
    else {
        return Err(InstallError::Download(format!(
            "{}: install_binary_entry called on a non-Binary entry",
            entry.id
        )));
    };

    let (_, url, expected_sha) = urls
        .iter()
        .find(|(t, _, _)| *t == target)
        .ok_or_else(|| InstallError::UnsupportedTarget(format!("{target:?}")))?;

    notify(InstallProgress::Started {
        entry_id: entry.id.into(),
    });

    notify(InstallProgress::Downloading {
        entry_id: entry.id.into(),
        bytes_so_far: 0,
        total: None,
    });
    let bytes = downloader.fetch(url).await?;
    notify(InstallProgress::Downloading {
        entry_id: entry.id.into(),
        bytes_so_far: bytes.len() as u64,
        total: Some(bytes.len() as u64),
    });

    notify(InstallProgress::Verifying {
        entry_id: entry.id.into(),
    });
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let actual = hex::encode(hasher.finalize());
    if actual != *expected_sha {
        return Err(InstallError::ChecksumMismatch {
            entry_id: entry.id.into(),
            expected: (*expected_sha).into(),
            actual,
        });
    }

    notify(InstallProgress::Extracting {
        entry_id: entry.id.into(),
    });
    let install_dir = lsp_bin_root.join(entry.id);
    if install_dir.exists() {
        tokio::fs::remove_dir_all(&install_dir).await?;
    }
    tokio::fs::create_dir_all(&install_dir).await?;
    let installed_at = extract_one(&bytes, url, binary_path, &install_dir, entry.id).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&installed_at)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&installed_at, perms)?;
    }

    notify(InstallProgress::Done {
        entry_id: entry.id.into(),
        installed_at: installed_at.clone(),
    });
    Ok(InstallOutcome {
        entry_id: entry.id.into(),
        installed_at,
    })
}

/// Extract `bytes` into `install_dir`, choosing the extractor by `url`'s
/// suffix (`.gz` / `.tar.gz` / `.zip`). Returns the absolute path to
/// the binary inside the install dir (with `.exe` appended on Windows
/// if the catalog template omitted it).
async fn extract_one(
    bytes: &bytes::Bytes,
    url: &str,
    binary_path: &str,
    install_dir: &Path,
    entry_id: &str,
) -> Result<PathBuf, InstallError> {
    let extract_kind = if url.ends_with(".tar.gz") || url.ends_with(".tgz") {
        ArchiveKind::TarGz
    } else if url.ends_with(".zip") {
        ArchiveKind::Zip
    } else if url.ends_with(".gz") {
        ArchiveKind::GzipOnly
    } else {
        return Err(InstallError::Extract {
            entry_id: entry_id.into(),
            reason: format!("unrecognised archive suffix in {url}"),
        });
    };

    match extract_kind {
        ArchiveKind::TarGz => {
            let dec = flate2::read::GzDecoder::new(&bytes[..]);
            let mut ar = tar::Archive::new(dec);
            ar.unpack(install_dir).map_err(|e| InstallError::Extract {
                entry_id: entry_id.into(),
                reason: e.to_string(),
            })?;
        }
        ArchiveKind::Zip => {
            let reader = std::io::Cursor::new(&bytes[..]);
            let mut zip = zip::ZipArchive::new(reader).map_err(|e| InstallError::Extract {
                entry_id: entry_id.into(),
                reason: e.to_string(),
            })?;
            zip.extract(install_dir)
                .map_err(|e| InstallError::Extract {
                    entry_id: entry_id.into(),
                    reason: e.to_string(),
                })?;
        }
        ArchiveKind::GzipOnly => {
            let bin_in_dir = install_dir.join(resolve_binary_path(binary_path));
            let mut dec = flate2::read::GzDecoder::new(&bytes[..]);
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut dec, &mut buf).map_err(InstallError::Io)?;
            let mut out = tokio::fs::File::create(&bin_in_dir).await?;
            out.write_all(&buf).await?;
            out.flush().await?;
        }
    }

    let bin_in_dir = install_dir.join(resolve_binary_path(binary_path));
    if !bin_in_dir.exists() {
        return Err(InstallError::Extract {
            entry_id: entry_id.into(),
            reason: format!("binary not found at {} after extract", bin_in_dir.display()),
        });
    }
    Ok(bin_in_dir)
}

/// Append `.exe` to `binary_path` on Windows when the catalog template
/// omitted it. The catalog deliberately keeps `binary_path` Unix-style
/// so a single literal works across targets.
fn resolve_binary_path(binary_path: &str) -> PathBuf {
    #[cfg(windows)]
    {
        if !binary_path.ends_with(".exe") {
            return PathBuf::from(format!("{binary_path}.exe"));
        }
    }
    PathBuf::from(binary_path)
}

/// Thin abstraction over the `npm` subprocess so tests can stub it.
#[async_trait::async_trait]
pub trait NpmRunner: Send + Sync {
    /// Run `npm i -g <package>@<version>`. Each line of npm's combined
    /// stdout/stderr is forwarded via `on_line`. Returns `Ok` on a
    /// zero exit code, `Err(message)` otherwise.
    async fn install_global(
        &self,
        package: &str,
        version: &str,
        on_line: &(dyn Fn(String) + Send + Sync),
    ) -> Result<(), String>;
    /// Return `npm root -g` — the directory npm installs globals into.
    async fn root_global(&self) -> Result<PathBuf, String>;
}

/// Production [`NpmRunner`] backed by `tokio::process::Command`.
pub struct SystemNpmRunner;

#[async_trait::async_trait]
impl NpmRunner for SystemNpmRunner {
    async fn install_global(
        &self,
        package: &str,
        version: &str,
        on_line: &(dyn Fn(String) + Send + Sync),
    ) -> Result<(), String> {
        use std::collections::VecDeque;
        use tokio::io::{AsyncBufReadExt, BufReader};
        use tokio::process::Command;

        let mut child = Command::new("npm")
            .args(["i", "-g", &format!("{package}@{version}")])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn npm: {e}"))?;
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let mut out_lines = BufReader::new(stdout).lines();
        let mut err_lines = BufReader::new(stderr).lines();

        // Drain both streams independently; one of them closing first
        // must not abandon the other (npm's stderr typically carries the
        // failure summary AFTER stdout has already EOF'd). Also keep a
        // small rolling tail so a non-zero exit can surface the actual
        // diagnostic rather than just "exit status 1".
        const TAIL_CAP: usize = 20;
        let mut tail: VecDeque<String> = VecDeque::with_capacity(TAIL_CAP);
        let mut out_done = false;
        let mut err_done = false;
        loop {
            tokio::select! {
                line = out_lines.next_line(), if !out_done => match line {
                    Ok(Some(l)) => {
                        if tail.len() == TAIL_CAP { tail.pop_front(); }
                        tail.push_back(l.clone());
                        on_line(l);
                    }
                    _ => out_done = true,
                },
                line = err_lines.next_line(), if !err_done => match line {
                    Ok(Some(l)) => {
                        if tail.len() == TAIL_CAP { tail.pop_front(); }
                        tail.push_back(l.clone());
                        on_line(l);
                    }
                    _ => err_done = true,
                },
                else => break,
            }
            if out_done && err_done {
                break;
            }
        }

        let status = child.wait().await.map_err(|e| format!("wait npm: {e}"))?;
        if !status.success() {
            let suffix = if tail.is_empty() {
                String::new()
            } else {
                format!(" — last output:\n{}", tail.into_iter().collect::<Vec<_>>().join("\n"))
            };
            return Err(format!("npm exited with status {status}{suffix}"));
        }
        Ok(())
    }

    async fn root_global(&self) -> Result<PathBuf, String> {
        let out = tokio::process::Command::new("npm")
            .args(["root", "-g"])
            .output()
            .await
            .map_err(|e| format!("spawn `npm root -g`: {e}"))?;
        if !out.status.success() {
            return Err(format!("npm root -g failed: status {}", out.status));
        }
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok(PathBuf::from(path))
    }
}

/// `Some(path)` if `npm` is on `$PATH`, `None` otherwise. The wrapping
/// plugin uses this to skip npm-based entries with a clear note rather
/// than blowing up mid-install.
pub fn detect_npm() -> Option<PathBuf> {
    which::which("npm").ok()
}

/// Install a single `NpmGlobal` catalog entry by shelling out to the
/// host's `npm`. Returns the absolute path npm placed the binary at;
/// the wrapping plugin writes that path into `lsp.toml` only when the
/// catalog's `lsp_entry.command` is `"{{BIN}}"` — usually npm entries
/// pin a literal `command` and the installed path is informational.
pub async fn install_npm_entry(
    entry: &CatalogEntry,
    runner: &dyn NpmRunner,
    notify: impl Fn(InstallProgress) + Send + Sync,
) -> Result<InstallOutcome, InstallError> {
    let InstallMethod::NpmGlobal { package, binary } = entry.method else {
        return Err(InstallError::Npm {
            entry_id: entry.id.into(),
            reason: "install_npm_entry called on a non-Npm entry".into(),
        });
    };

    notify(InstallProgress::Started {
        entry_id: entry.id.into(),
    });

    let entry_id_for_notify = entry.id.to_string();
    let notify_for_npm = &notify;
    runner
        .install_global(package, entry.version, &move |line| {
            notify_for_npm(InstallProgress::RunningNpm {
                entry_id: entry_id_for_notify.clone(),
                line,
            });
        })
        .await
        .map_err(|reason| InstallError::Npm {
            entry_id: entry.id.into(),
            reason,
        })?;

    let root = runner
        .root_global()
        .await
        .map_err(|reason| InstallError::Npm {
            entry_id: entry.id.into(),
            reason,
        })?;
    // `npm root -g` returns `<prefix>/lib/node_modules`. Bins live at
    // `<prefix>/bin/<binary>` on Unix, `<prefix>\<binary>.cmd` on
    // Windows. v1 supports Unix layout; Windows users typically have
    // npm putting bins on `$PATH` directly so the literal `command`
    // in the lsp.toml entry resolves regardless of this path.
    let installed_at = root
        .parent()
        .and_then(|p| p.parent())
        .map(|prefix| prefix.join("bin").join(binary))
        .ok_or_else(|| InstallError::Npm {
            entry_id: entry.id.into(),
            reason: format!("could not derive bin path from npm root {}", root.display()),
        })?;

    notify(InstallProgress::Done {
        entry_id: entry.id.into(),
        installed_at: installed_at.clone(),
    });
    Ok(InstallOutcome {
        entry_id: entry.id.into(),
        installed_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::builtin::lsp_installer::catalog::{
        ArchiveKind, Category, InstallMethod, LspEntryTemplate,
    };

    fn fake_entry(urls: &'static [(Target, &'static str, &'static str)]) -> CatalogEntry {
        CatalogEntry {
            id: "fakelsp",
            display_name: "fakelsp",
            language_label: "fake",
            version: "0.0.0",
            category: Category::Binary,
            method: InstallMethod::BinaryDownload {
                urls,
                archive: ArchiveKind::GzipOnly,
                binary_path: "fakelsp",
            },
            lsp_entry: LspEntryTemplate {
                id: "fake",
                extensions: &["fake"],
                root_markers: &["fake.toml"],
                command: "{{BIN}}",
                args: &[],
            },
        }
    }

    struct StubDownloader {
        payload: bytes::Bytes,
    }

    #[async_trait::async_trait]
    impl Downloader for StubDownloader {
        async fn fetch(&self, _url: &str) -> Result<bytes::Bytes, InstallError> {
            Ok(self.payload.clone())
        }
    }

    fn gzipped(plain: &[u8]) -> Vec<u8> {
        use flate2::{Compression, write::GzEncoder};
        use std::io::Write;
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(plain).unwrap();
        enc.finish().unwrap()
    }

    #[tokio::test]
    async fn binary_download_happy_path_writes_executable() {
        let plain = b"#!/bin/sh\necho fakelsp\n";
        let archive = gzipped(plain);
        let sha = hex::encode(Sha256::digest(&archive));
        let url_static: &'static str = Box::leak(
            "https://example.test/fakelsp.gz"
                .to_string()
                .into_boxed_str(),
        );
        let sha_static: &'static str = Box::leak(sha.into_boxed_str());
        let urls: &'static [(Target, &'static str, &'static str)] =
            Box::leak(Box::new([(Target::LinuxX86_64Gnu, url_static, sha_static)]));

        let entry = fake_entry(urls);
        let tmp = tempfile::tempdir().unwrap();
        let dl = StubDownloader {
            payload: bytes::Bytes::from(archive),
        };
        let outcome = install_binary_entry(&entry, Target::LinuxX86_64Gnu, tmp.path(), &dl, |_| {})
            .await
            .unwrap();
        assert!(outcome.installed_at.exists(), "binary must exist on disk");
        let written = std::fs::read(&outcome.installed_at).unwrap();
        assert_eq!(written, plain, "binary contents must match");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&outcome.installed_at)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o111,
                0o111,
                "binary must be executable, got {mode:o}"
            );
        }
    }

    #[tokio::test]
    async fn checksum_mismatch_returns_error() {
        let archive = gzipped(b"not-the-payload-we-expected");
        let urls: &'static [(Target, &'static str, &'static str)] = &[(
            Target::LinuxX86_64Gnu,
            "https://example.test/fakelsp.gz",
            "0000000000000000000000000000000000000000000000000000000000000000",
        )];
        let entry = fake_entry(urls);
        let tmp = tempfile::tempdir().unwrap();
        let dl = StubDownloader {
            payload: bytes::Bytes::from(archive),
        };
        let err = install_binary_entry(&entry, Target::LinuxX86_64Gnu, tmp.path(), &dl, |_| {})
            .await
            .unwrap_err();
        match err {
            InstallError::ChecksumMismatch { .. } => (),
            other => panic!("expected ChecksumMismatch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unsupported_target_returns_error() {
        let urls: &'static [(Target, &'static str, &'static str)] = &[(
            Target::LinuxX86_64Gnu,
            "https://example.test/fakelsp.gz",
            "0",
        )];
        let entry = fake_entry(urls);
        let tmp = tempfile::tempdir().unwrap();
        let dl = StubDownloader {
            payload: bytes::Bytes::new(),
        };
        let err = install_binary_entry(&entry, Target::MacosAarch64, tmp.path(), &dl, |_| {})
            .await
            .unwrap_err();
        assert!(matches!(err, InstallError::UnsupportedTarget(_)));
    }

    struct StubNpm {
        install_result: Result<(), String>,
        root: PathBuf,
    }

    #[async_trait::async_trait]
    impl NpmRunner for StubNpm {
        async fn install_global(
            &self,
            _package: &str,
            _version: &str,
            on_line: &(dyn Fn(String) + Send + Sync),
        ) -> Result<(), String> {
            on_line("added 1 package".into());
            self.install_result.clone()
        }
        async fn root_global(&self) -> Result<PathBuf, String> {
            Ok(self.root.clone())
        }
    }

    fn npm_entry() -> CatalogEntry {
        CatalogEntry {
            id: "fake-npm-lsp",
            display_name: "fake-npm-lsp",
            language_label: "fake",
            version: "1.2.3",
            category: Category::Npm,
            method: InstallMethod::NpmGlobal {
                package: "fake-npm-lsp",
                binary: "fake-npm-lsp",
            },
            lsp_entry: LspEntryTemplate {
                id: "fake",
                extensions: &["fake"],
                root_markers: &["fake.toml"],
                command: "fake-npm-lsp",
                args: &[],
            },
        }
    }

    #[tokio::test]
    async fn npm_happy_path_derives_bin_from_root() {
        let tmp = tempfile::tempdir().unwrap();
        let prefix = tmp.path();
        let root = prefix.join("lib").join("node_modules");
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        std::fs::write(prefix.join("bin").join("fake-npm-lsp"), b"#!/bin/sh\n").unwrap();
        let runner = StubNpm {
            install_result: Ok(()),
            root,
        };
        let outcome = install_npm_entry(&npm_entry(), &runner, |_| {})
            .await
            .unwrap();
        assert_eq!(
            outcome.installed_at,
            prefix.join("bin").join("fake-npm-lsp")
        );
    }

    #[tokio::test]
    async fn npm_install_failure_returns_npm_error() {
        let runner = StubNpm {
            install_result: Err("network down".into()),
            root: PathBuf::from("/tmp/unused"),
        };
        let err = install_npm_entry(&npm_entry(), &runner, |_| {})
            .await
            .unwrap_err();
        match err {
            InstallError::Npm { reason, .. } => assert!(reason.contains("network down")),
            other => panic!("expected InstallError::Npm, got {other:?}"),
        }
    }

    #[test]
    fn tool_not_found_display_mentions_tool_and_action() {
        let e = InstallError::ToolNotFound { tool: "npm".into() };
        let msg = format!("{e}");
        assert!(msg.contains("npm"));
        assert!(msg.contains("install it"));
    }

    #[test]
    fn checksum_mismatch_display_includes_both_hashes() {
        let e = InstallError::ChecksumMismatch {
            entry_id: "rust-analyzer".into(),
            expected: "abc".into(),
            actual: "def".into(),
        };
        let msg = format!("{e}");
        assert!(msg.contains("rust-analyzer"));
        assert!(msg.contains("abc"));
        assert!(msg.contains("def"));
    }

    /// End-to-end smoke test: serve a gzipped fixture over loopback,
    /// run `install_binary_entry` against the production
    /// [`ReqwestDownloader`], assert the binary was extracted with the
    /// executable bit set. Verifies the wiring between download → SHA
    /// verify → gzip extract that the stubbed-Downloader tests can't
    /// exercise.
    #[tokio::test]
    async fn smoke_local_http_install() {
        use std::sync::Arc;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::TcpListener;

        let payload = b"#!/bin/sh\necho hello-from-fakelsp\n";
        let archive = gzipped(payload);
        let sha = hex::encode(Sha256::digest(&archive));
        let archive = Arc::new(archive);

        // Single-shot HTTP/1.1 server. Listens on 127.0.0.1 with an
        // OS-assigned port, serves the gzipped archive, then exits.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}/fakelsp.gz");

        let archive_for_server = Arc::clone(&archive);
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let (read, mut write) = sock.split();
            let mut reader = BufReader::new(read);
            // Read + discard the request headers until an empty line.
            let mut buf = String::new();
            loop {
                buf.clear();
                let n = reader.read_line(&mut buf).await.unwrap_or(0);
                if n == 0 || buf == "\r\n" || buf == "\n" {
                    break;
                }
            }
            let body = &*archive_for_server;
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\n\r\n",
                body.len()
            );
            write.write_all(header.as_bytes()).await.unwrap();
            write.write_all(body).await.unwrap();
            write.flush().await.unwrap();
        });

        // Leak the URL + SHA + url-array into 'static so they can sit
        // in CatalogEntry's static-only fields. Acceptable in a test.
        let url_static: &'static str = Box::leak(url.into_boxed_str());
        let sha_static: &'static str = Box::leak(sha.into_boxed_str());
        let urls: &'static [(Target, &'static str, &'static str)] = Box::leak(Box::new([(
            Target::current().expect("supported host target"),
            url_static,
            sha_static,
        )]));

        let entry = CatalogEntry {
            id: "fakelsp-smoke",
            display_name: "fakelsp-smoke",
            language_label: "fake",
            version: "0.0.0",
            category: Category::Binary,
            method: InstallMethod::BinaryDownload {
                urls,
                archive: ArchiveKind::GzipOnly,
                binary_path: "fakelsp-smoke",
            },
            lsp_entry: LspEntryTemplate {
                id: "fake",
                extensions: &["fake"],
                root_markers: &["fake.toml"],
                command: "{{BIN}}",
                args: &[],
            },
        };

        let tmp = tempfile::tempdir().unwrap();
        let dl = ReqwestDownloader::new().expect("reqwest builds");
        let outcome =
            install_binary_entry(&entry, Target::current().unwrap(), tmp.path(), &dl, |_| {})
                .await
                .expect("install must succeed end-to-end");

        assert!(outcome.installed_at.exists());
        let written = std::fs::read(&outcome.installed_at).unwrap();
        assert_eq!(written, payload, "binary contents must match the fixture");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&outcome.installed_at)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "binary must be executable");
        }

        let _ = server.await;
    }
}
