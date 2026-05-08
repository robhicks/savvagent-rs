//! Filesystem tools as a Model Context Protocol stdio server.
//!
//! Exposes four tools that an agent host can call over MCP:
//!
//! - [`read_file`](FsTools::read_file) — read a UTF-8 file with a size cap.
//! - [`write_file`](FsTools::write_file) — write/overwrite a UTF-8 file.
//! - [`list_dir`](FsTools::list_dir) — list directory entries (optionally recursively).
//! - [`glob`](FsTools::glob) — expand a glob pattern relative to a root.
//!
//! The binary `savvagent-tool-fs` wraps [`FsTools`] in an `rmcp` stdio
//! transport; see `src/main.rs`.
//!
//! v0.1 ships **without sandboxing**. Tools run with the full privileges of the
//! invoking user. Hosts are expected to confirm destructive calls before
//! routing them.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::path::{Path, PathBuf};

use rmcp::{
    ErrorData, ServerHandler,
    handler::server::{
        router::tool::ToolRouter,
        wrapper::{Json, Parameters},
    },
    model::{Implementation, ProtocolVersion, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};

/// Default upper bound on bytes returned by `read_file`. Override per call via
/// [`ReadFileInput::max_bytes`].
pub const DEFAULT_MAX_READ_BYTES: u64 = 1 << 20;

/// Default upper bound on entries returned by `list_dir`. Override per call via
/// [`ListDirInput::max_entries`].
pub const DEFAULT_MAX_LIST_ENTRIES: u32 = 1024;

/// Default upper bound on matches returned by `glob`. Override per call via
/// [`GlobInput::max_matches`].
pub const DEFAULT_MAX_GLOB_MATCHES: u32 = 1024;

// ---------------------------------------------------------------------------
// Tool input/output types
// ---------------------------------------------------------------------------

/// Arguments to `read_file`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ReadFileInput {
    /// Path to the file. Relative paths resolve against the server's CWD.
    pub path: String,
    /// Reject files larger than this many bytes. Default: 1 MiB.
    #[serde(default)]
    pub max_bytes: Option<u64>,
}

/// Result of `read_file`.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReadFileOutput {
    /// The path that was read, as supplied by the caller.
    pub path: String,
    /// Size of the file in bytes.
    pub bytes: u64,
    /// File contents, decoded as UTF-8.
    pub content: String,
}

/// Arguments to `write_file`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct WriteFileInput {
    /// Destination path. Relative paths resolve against the server's CWD.
    pub path: String,
    /// UTF-8 contents to write. The file is fully overwritten.
    pub content: String,
    /// Create missing parent directories if true. Default: false.
    #[serde(default)]
    pub create_dirs: bool,
}

/// Result of `write_file`.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WriteFileOutput {
    /// The path that was written.
    pub path: String,
    /// Number of bytes written.
    pub bytes_written: u64,
}

/// Arguments to `list_dir`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ListDirInput {
    /// Directory path. Relative paths resolve against the server's CWD.
    pub path: String,
    /// Walk subdirectories if true. Default: false.
    #[serde(default)]
    pub recursive: bool,
    /// Cap on the number of entries returned. Default: 1024.
    #[serde(default)]
    pub max_entries: Option<u32>,
}

/// One row returned by `list_dir`.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DirEntry {
    /// File or directory name (final path component).
    pub name: String,
    /// Full path as returned by the underlying filesystem walk.
    pub path: String,
    /// True if the entry is a directory.
    pub is_dir: bool,
    /// File size in bytes. Zero for directories.
    pub size_bytes: u64,
}

/// Result of `list_dir`.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ListDirOutput {
    /// The directory that was listed.
    pub path: String,
    /// Entries discovered, in unspecified order.
    pub entries: Vec<DirEntry>,
    /// True if the result was capped by `max_entries`.
    pub truncated: bool,
}

/// Arguments to `glob`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct GlobInput {
    /// Glob pattern (e.g. `**/*.rs`). Standard `glob` crate syntax.
    pub pattern: String,
    /// Directory the pattern is rooted at. Default: server CWD (`.`).
    #[serde(default)]
    pub root: Option<String>,
    /// Cap on the number of matches returned. Default: 1024.
    #[serde(default)]
    pub max_matches: Option<u32>,
}

/// Result of `glob`.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GlobOutput {
    /// Echo of the input pattern.
    pub pattern: String,
    /// Echo of the resolved root.
    pub root: String,
    /// Matched paths, relative to `root` when possible.
    pub matches: Vec<String>,
    /// True if the result was capped by `max_matches`.
    pub truncated: bool,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors a tool handler can surface to the caller.
#[derive(Debug, thiserror::Error)]
pub enum FsToolError {
    /// Underlying filesystem I/O failed.
    #[error("{op}: {source}")]
    Io {
        /// Short label describing the operation that failed.
        op: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// File exceeded the byte cap requested by the caller.
    #[error("file too large: {bytes} bytes (limit {limit})")]
    FileTooLarge {
        /// Actual file size.
        bytes: u64,
        /// Cap that was applied.
        limit: u64,
    },
    /// File contents were not valid UTF-8.
    #[error("file is not valid UTF-8: {0}")]
    NotUtf8(String),
    /// Caller passed an invalid argument (bad pattern, empty path, …).
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    /// Glob pattern failed to compile.
    #[error("invalid glob pattern: {0}")]
    Glob(String),
}

impl From<FsToolError> for ErrorData {
    fn from(err: FsToolError) -> Self {
        match err {
            FsToolError::InvalidArgument(_) | FsToolError::Glob(_) => {
                ErrorData::invalid_params(err.to_string(), None)
            }
            FsToolError::FileTooLarge { .. } | FsToolError::NotUtf8(_) => {
                ErrorData::invalid_request(err.to_string(), None)
            }
            FsToolError::Io { .. } => ErrorData::internal_error(err.to_string(), None),
        }
    }
}

fn io_err(op: &str, source: std::io::Error) -> FsToolError {
    FsToolError::Io {
        op: op.to_string(),
        source,
    }
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// MCP server exposing the filesystem tools.
#[derive(Debug, Clone)]
pub struct FsTools {
    #[allow(dead_code)] // Read by the `#[tool_handler]` macro expansion.
    tool_router: ToolRouter<Self>,
}

impl Default for FsTools {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router]
impl FsTools {
    /// Construct a new server instance with the default tool router.
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    /// Read a UTF-8 text file from disk, capped at `max_bytes`.
    #[tool(
        name = "read_file",
        description = "Read a UTF-8 text file. Errors if the file exceeds max_bytes (default 1 MiB) or is not valid UTF-8."
    )]
    pub async fn read_file(
        &self,
        Parameters(input): Parameters<ReadFileInput>,
    ) -> Result<Json<ReadFileOutput>, ErrorData> {
        if input.path.is_empty() {
            return Err(FsToolError::InvalidArgument("path is empty".into()).into());
        }
        let limit = input.max_bytes.unwrap_or(DEFAULT_MAX_READ_BYTES);
        let path = PathBuf::from(&input.path);

        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|e| io_err("stat", e))?;
        if !metadata.is_file() {
            return Err(FsToolError::InvalidArgument(format!(
                "{} is not a regular file",
                input.path
            ))
            .into());
        }
        let bytes = metadata.len();
        if bytes > limit {
            return Err(FsToolError::FileTooLarge { bytes, limit }.into());
        }

        let raw = tokio::fs::read(&path)
            .await
            .map_err(|e| io_err("read", e))?;
        let content = String::from_utf8(raw).map_err(|e| FsToolError::NotUtf8(e.to_string()))?;

        Ok(Json(ReadFileOutput {
            path: input.path,
            bytes,
            content,
        }))
    }

    /// Write `content` to `path`, fully overwriting any existing file.
    #[tool(
        name = "write_file",
        description = "Overwrite (or create) a UTF-8 text file. Set create_dirs=true to create missing parent directories."
    )]
    pub async fn write_file(
        &self,
        Parameters(input): Parameters<WriteFileInput>,
    ) -> Result<Json<WriteFileOutput>, ErrorData> {
        if input.path.is_empty() {
            return Err(FsToolError::InvalidArgument("path is empty".into()).into());
        }
        let path = PathBuf::from(&input.path);

        if input.create_dirs {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| io_err("create_dir_all", e))?;
            }
        }

        let bytes_written = input.content.len() as u64;
        tokio::fs::write(&path, input.content.as_bytes())
            .await
            .map_err(|e| io_err("write", e))?;

        Ok(Json(WriteFileOutput {
            path: input.path,
            bytes_written,
        }))
    }

    /// List the entries of a directory.
    #[tool(
        name = "list_dir",
        description = "List entries in a directory. Set recursive=true to walk subdirectories."
    )]
    pub async fn list_dir(
        &self,
        Parameters(input): Parameters<ListDirInput>,
    ) -> Result<Json<ListDirOutput>, ErrorData> {
        if input.path.is_empty() {
            return Err(FsToolError::InvalidArgument("path is empty".into()).into());
        }
        let limit = input.max_entries.unwrap_or(DEFAULT_MAX_LIST_ENTRIES) as usize;
        let root = PathBuf::from(&input.path);

        let metadata = tokio::fs::metadata(&root)
            .await
            .map_err(|e| io_err("stat", e))?;
        if !metadata.is_dir() {
            return Err(
                FsToolError::InvalidArgument(format!("{} is not a directory", input.path)).into(),
            );
        }

        let walk_root = root.clone();
        let recursive = input.recursive;
        let entries =
            tokio::task::spawn_blocking(move || -> Result<(Vec<DirEntry>, bool), FsToolError> {
                walk_dir(&walk_root, recursive, limit)
            })
            .await
            .map_err(|e| FsToolError::InvalidArgument(format!("walk task panicked: {e}")))??;

        Ok(Json(ListDirOutput {
            path: input.path,
            entries: entries.0,
            truncated: entries.1,
        }))
    }

    /// Expand a glob pattern relative to `root`.
    #[tool(
        name = "glob",
        description = "Expand a glob pattern (e.g. **/*.rs) under a root directory."
    )]
    pub async fn glob(
        &self,
        Parameters(input): Parameters<GlobInput>,
    ) -> Result<Json<GlobOutput>, ErrorData> {
        if input.pattern.is_empty() {
            return Err(FsToolError::InvalidArgument("pattern is empty".into()).into());
        }
        let limit = input.max_matches.unwrap_or(DEFAULT_MAX_GLOB_MATCHES) as usize;
        let root_str = input.root.clone().unwrap_or_else(|| ".".into());
        let pattern = input.pattern.clone();
        let root_for_thread = root_str.clone();

        let (matches, truncated) =
            tokio::task::spawn_blocking(move || -> Result<(Vec<String>, bool), FsToolError> {
                let joined = if root_for_thread == "." {
                    pattern.clone()
                } else {
                    format!("{}/{}", root_for_thread.trim_end_matches('/'), pattern)
                };
                let mut out = Vec::new();
                let mut truncated = false;
                let iter = glob::glob(&joined).map_err(|e| FsToolError::Glob(e.to_string()))?;
                for entry in iter {
                    if out.len() >= limit {
                        truncated = true;
                        break;
                    }
                    let p = entry.map_err(|e| FsToolError::Glob(e.to_string()))?;
                    let rel = p
                        .strip_prefix(Path::new(&root_for_thread))
                        .unwrap_or(&p)
                        .to_string_lossy()
                        .into_owned();
                    out.push(rel);
                }
                Ok((out, truncated))
            })
            .await
            .map_err(|e| FsToolError::InvalidArgument(format!("glob task panicked: {e}")))??;

        Ok(Json(GlobOutput {
            pattern: input.pattern,
            root: root_str,
            matches,
            truncated,
        }))
    }
}

#[tool_handler]
impl ServerHandler for FsTools {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::default())
            .with_server_info(
                Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
                    .with_description(
                        "Savvagent filesystem tools (read_file, write_file, list_dir, glob)",
                    ),
            )
            .with_instructions(
                "Filesystem tool server for Savvagent. v0.1 has no sandbox; the host \
                 is expected to confirm destructive calls before routing them.",
            )
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn walk_dir(
    root: &Path,
    recursive: bool,
    limit: usize,
) -> Result<(Vec<DirEntry>, bool), FsToolError> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut truncated = false;

    while let Some(dir) = stack.pop() {
        let read = std::fs::read_dir(&dir).map_err(|e| io_err("read_dir", e))?;
        for entry in read {
            if out.len() >= limit {
                truncated = true;
                return Ok((out, truncated));
            }
            let entry = entry.map_err(|e| io_err("read_dir entry", e))?;
            let meta = entry.metadata().map_err(|e| io_err("entry metadata", e))?;
            let is_dir = meta.is_dir();
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            out.push(DirEntry {
                name,
                path: path.to_string_lossy().into_owned(),
                is_dir,
                size_bytes: if is_dir { 0 } else { meta.len() },
            });
            if recursive && is_dir {
                stack.push(path);
            }
        }
    }
    Ok((out, truncated))
}

// ---------------------------------------------------------------------------
// Binary entry point
// ---------------------------------------------------------------------------

/// Serve [`FsTools`] over a stdio MCP transport. Shared between the
/// `savvagent-tool-fs` binary in this crate and the bundled shim in the
/// `savvagent` crate's release archive.
pub async fn run() -> anyhow::Result<()> {
    use rmcp::{ServiceExt, transport::stdio};

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!(
        "savvagent-tool-fs {} starting on stdio",
        env!("CARGO_PKG_VERSION")
    );

    let service = FsTools::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn read_file_round_trips_utf8() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        tokio::fs::write(&path, b"hello world").await.unwrap();

        let tools = FsTools::new();
        let out = tools
            .read_file(Parameters(ReadFileInput {
                path: path.to_string_lossy().into_owned(),
                max_bytes: None,
            }))
            .await
            .unwrap();
        assert_eq!(out.0.bytes, 11);
        assert_eq!(out.0.content, "hello world");
    }

    #[tokio::test]
    async fn read_file_rejects_oversize() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("big.txt");
        tokio::fs::write(&path, vec![b'x'; 32]).await.unwrap();

        let tools = FsTools::new();
        let err = tools
            .read_file(Parameters(ReadFileInput {
                path: path.to_string_lossy().into_owned(),
                max_bytes: Some(8),
            }))
            .await
            .err()
            .expect("expected error");
        assert!(err.message.contains("too large"), "{}", err.message);
    }

    #[tokio::test]
    async fn read_file_rejects_non_utf8() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bin");
        tokio::fs::write(&path, [0xff, 0xfe, 0xfd]).await.unwrap();

        let tools = FsTools::new();
        let err = tools
            .read_file(Parameters(ReadFileInput {
                path: path.to_string_lossy().into_owned(),
                max_bytes: None,
            }))
            .await
            .err()
            .expect("expected error");
        assert!(err.message.contains("UTF-8"), "{}", err.message);
    }

    #[tokio::test]
    async fn write_file_creates_parents_when_requested() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a/b/c.txt");

        let tools = FsTools::new();
        let out = tools
            .write_file(Parameters(WriteFileInput {
                path: path.to_string_lossy().into_owned(),
                content: "hi".into(),
                create_dirs: true,
            }))
            .await
            .unwrap();
        assert_eq!(out.0.bytes_written, 2);
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "hi");
    }

    #[tokio::test]
    async fn write_file_overwrites() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("note.txt");
        tokio::fs::write(&path, b"old").await.unwrap();

        let tools = FsTools::new();
        tools
            .write_file(Parameters(WriteFileInput {
                path: path.to_string_lossy().into_owned(),
                content: "new!".into(),
                create_dirs: false,
            }))
            .await
            .unwrap();
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "new!");
    }

    #[tokio::test]
    async fn list_dir_flat() {
        let dir = tempdir().unwrap();
        tokio::fs::write(dir.path().join("a.txt"), b"a")
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("b.txt"), b"bb")
            .await
            .unwrap();
        tokio::fs::create_dir(dir.path().join("nested"))
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("nested/c.txt"), b"ccc")
            .await
            .unwrap();

        let tools = FsTools::new();
        let out = tools
            .list_dir(Parameters(ListDirInput {
                path: dir.path().to_string_lossy().into_owned(),
                recursive: false,
                max_entries: None,
            }))
            .await
            .unwrap();
        let names: Vec<_> = out.0.entries.iter().map(|e| e.name.clone()).collect();
        assert_eq!(names.len(), 3, "{:?}", names);
        assert!(names.contains(&"a.txt".to_string()));
        assert!(names.contains(&"nested".to_string()));
        assert!(out.0.entries.iter().any(|e| e.name == "nested" && e.is_dir));
    }

    #[tokio::test]
    async fn list_dir_recursive() {
        let dir = tempdir().unwrap();
        tokio::fs::create_dir(dir.path().join("sub")).await.unwrap();
        tokio::fs::write(dir.path().join("sub/x.txt"), b"x")
            .await
            .unwrap();

        let tools = FsTools::new();
        let out = tools
            .list_dir(Parameters(ListDirInput {
                path: dir.path().to_string_lossy().into_owned(),
                recursive: true,
                max_entries: None,
            }))
            .await
            .unwrap();
        assert!(out.0.entries.iter().any(|e| e.name == "x.txt"));
    }

    #[tokio::test]
    async fn list_dir_truncates() {
        let dir = tempdir().unwrap();
        for i in 0..5 {
            tokio::fs::write(dir.path().join(format!("f{i}.txt")), b"x")
                .await
                .unwrap();
        }

        let tools = FsTools::new();
        let out = tools
            .list_dir(Parameters(ListDirInput {
                path: dir.path().to_string_lossy().into_owned(),
                recursive: false,
                max_entries: Some(3),
            }))
            .await
            .unwrap();
        assert_eq!(out.0.entries.len(), 3);
        assert!(out.0.truncated);
    }

    #[tokio::test]
    async fn glob_matches_under_root() {
        let dir = tempdir().unwrap();
        tokio::fs::write(dir.path().join("keep.rs"), b"")
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("skip.txt"), b"")
            .await
            .unwrap();
        tokio::fs::create_dir(dir.path().join("deep"))
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("deep/also.rs"), b"")
            .await
            .unwrap();

        let tools = FsTools::new();
        let out = tools
            .glob(Parameters(GlobInput {
                pattern: "**/*.rs".into(),
                root: Some(dir.path().to_string_lossy().into_owned()),
                max_matches: None,
            }))
            .await
            .unwrap();
        assert!(out.0.matches.iter().any(|m| m.ends_with("keep.rs")));
        assert!(out.0.matches.iter().any(|m| m.ends_with("also.rs")));
        assert!(!out.0.matches.iter().any(|m| m.ends_with("skip.txt")));
    }

    #[tokio::test]
    async fn rejects_empty_path() {
        let tools = FsTools::new();
        assert!(
            tools
                .read_file(Parameters(ReadFileInput {
                    path: String::new(),
                    max_bytes: None
                }))
                .await
                .is_err()
        );
        assert!(
            tools
                .write_file(Parameters(WriteFileInput {
                    path: String::new(),
                    content: String::new(),
                    create_dirs: false
                }))
                .await
                .is_err()
        );
        assert!(
            tools
                .list_dir(Parameters(ListDirInput {
                    path: String::new(),
                    recursive: false,
                    max_entries: None,
                }))
                .await
                .is_err()
        );
        assert!(
            tools
                .glob(Parameters(GlobInput {
                    pattern: String::new(),
                    root: None,
                    max_matches: None,
                }))
                .await
                .is_err()
        );
    }
}
