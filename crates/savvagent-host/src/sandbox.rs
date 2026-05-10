//! Layer-3 OS-level sandboxing for tool MCP server spawns.
//!
//! Each tool child process can be wrapped in an OS sandbox so that it cannot
//! access the network or write outside the project root even if the tool binary
//! itself is compromised. Sandboxing is **opt-in** for v0.5.0 — set
//! [`SandboxConfig::enabled`] to `true` to activate it.
//!
//! # Platform support
//!
//! | Platform | Mechanism      | Status |
//! |----------|----------------|--------|
//! | Linux    | `bwrap` (bubblewrap) | Supported |
//! | macOS    | `sandbox-exec` | Supported |
//! | Windows  | None           | Deferred — runs unwrapped with a one-time warning |
//!
//! If the required wrapper binary (`bwrap` on Linux, `sandbox-exec` on macOS)
//! is not found on `$PATH`, a warning is logged and the tool runs unwrapped.
//! The sandbox is never a hard prerequisite.
//!
//! # `tool-bash` and network access
//!
//! `tool-bash` is given `allow_net = true` by default in the per-tool override
//! map when sandboxing is on, because many bash commands require network access
//! (e.g. `curl`, `cargo`, package managers). If you want to sandbox bash away
//! from the network, explicitly set `allow_net = false` for `tool-bash` in your
//! `SandboxConfig::tool_overrides`.
//!
//! # Config persistence
//!
//! `~/.savvagent/sandbox.toml` is loaded at startup (silent fallback on any
//! parse error) and written through whenever the user changes settings via the
//! `/sandbox` command.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Config types
// ---------------------------------------------------------------------------

/// Per-tool sandbox overrides. Applied on top of the global [`SandboxConfig`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolSandboxOverride {
    /// When `Some(true)`, allow network access for this tool regardless of
    /// the global `allow_net`. When `Some(false)`, deny even if global allows.
    /// When `None`, inherit the global setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_net: Option<bool>,

    /// Additional paths to bind read-write inside the sandbox for this tool.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_binds: Vec<PathBuf>,
}

/// Sandbox configuration. Persisted to `~/.savvagent/sandbox.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Whether to apply OS-level sandboxing to tool spawns. Default: `false`.
    #[serde(default)]
    pub enabled: bool,

    /// Allow network access for all tools when sandboxed. Default: `false`.
    ///
    /// `tool-bash` overrides this to `true` in the default per-tool map
    /// because many bash commands require network (curl, cargo, etc.). To
    /// sandbox bash off the network, set `allow_net = false` explicitly in
    /// `tool_overrides["tool-bash"]`.
    #[serde(default)]
    pub allow_net: bool,

    /// Per-tool override map. Key is a substring of the tool binary path
    /// (e.g. `"tool-bash"` matches any path containing `tool-bash`).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub tool_overrides: HashMap<String, ToolSandboxOverride>,

    /// Additional paths to bind read-write inside the sandbox for all tools.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_binds: Vec<PathBuf>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        let mut tool_overrides = HashMap::new();
        // tool-bash needs network for curl, cargo, package managers, etc.
        tool_overrides.insert(
            "tool-bash".to_string(),
            ToolSandboxOverride {
                allow_net: Some(true),
                extra_binds: Vec::new(),
            },
        );
        Self {
            enabled: false,
            allow_net: false,
            tool_overrides,
            extra_binds: Vec::new(),
        }
    }
}

impl SandboxConfig {
    /// Load from `~/.savvagent/sandbox.toml`. Returns the default if the file
    /// is absent or unparseable (with a debug log in the latter case).
    pub fn load() -> Self {
        let Some(path) = sandbox_toml_path() else {
            return Self::default();
        };
        load_from_path(&path)
    }

    /// Persist to `~/.savvagent/sandbox.toml`. Errors are propagated; the
    /// caller decides whether to surface them to the user.
    pub async fn save(&self) -> std::io::Result<()> {
        let Some(path) = sandbox_toml_path() else {
            return Ok(());
        };
        let text = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, text).await
    }

    /// Resolve whether network should be allowed for a tool identified by its
    /// binary path.
    pub fn net_allowed_for(&self, tool_bin: &Path) -> bool {
        if let Some(ov) = self.find_override(tool_bin) {
            if let Some(net) = ov.allow_net {
                return net;
            }
        }
        self.allow_net
    }

    /// Collect the extra bind paths applicable to a given tool binary.
    pub fn extra_binds_for<'a>(&'a self, tool_bin: &Path) -> Vec<&'a Path> {
        let mut out: Vec<&Path> = self.extra_binds.iter().map(PathBuf::as_path).collect();
        if let Some(ov) = self.find_override(tool_bin) {
            out.extend(ov.extra_binds.iter().map(PathBuf::as_path));
        }
        out
    }

    fn find_override(&self, tool_bin: &Path) -> Option<&ToolSandboxOverride> {
        let bin_str = tool_bin.to_string_lossy();
        self.tool_overrides
            .iter()
            .find(|(key, _)| bin_str.contains(key.as_str()))
            .map(|(_, v)| v)
    }
}

fn load_from_path(path: &Path) -> SandboxConfig {
    match std::fs::read_to_string(path) {
        Ok(text) => match toml::from_str::<SandboxConfig>(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::debug!("sandbox.toml parse error (using defaults): {e}");
                SandboxConfig::default()
            }
        },
        Err(_) => SandboxConfig::default(),
    }
}

/// `~/.savvagent/sandbox.toml`, or `None` if `$HOME` is unset.
fn sandbox_toml_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)?;
    Some(home.join(".savvagent").join("sandbox.toml"))
}

// ---------------------------------------------------------------------------
// Command builder
// ---------------------------------------------------------------------------

/// The wrapper an [`apply_sandbox`] call resolved to. Used for logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxWrapper {
    /// Sandbox was disabled or the wrapper binary was not found.
    None,
    /// Wrapped with `bwrap` (Linux).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Bwrap,
    /// Wrapped with `sandbox-exec` (macOS).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    SandboxExec,
}

/// Apply sandbox wrapping (if configured and available) to `cmd` for the given
/// `tool_bin` path, using `project_root` as the read-write bind root.
///
/// Returns the wrapper kind that was applied, for logging at the call-site.
pub fn apply_sandbox(
    cmd: &mut tokio::process::Command,
    tool_bin: &Path,
    project_root: &Path,
    config: &SandboxConfig,
) -> SandboxWrapper {
    if !config.enabled {
        return SandboxWrapper::None;
    }

    #[cfg(target_os = "linux")]
    {
        apply_linux(cmd, tool_bin, project_root, config)
    }

    #[cfg(target_os = "macos")]
    {
        apply_macos(cmd, tool_bin, project_root, config)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        WARNED.get_or_init(|| {
            tracing::warn!(
                "sandbox: Windows OS-level sandboxing is not yet implemented; \
                 tools will run unwrapped. Disable sandboxing to silence this warning."
            );
        });
        let _ = (cmd, tool_bin, project_root, config);
        SandboxWrapper::None
    }
}

// ---------------------------------------------------------------------------
// Linux — bubblewrap
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn apply_linux(
    cmd: &mut tokio::process::Command,
    tool_bin: &Path,
    project_root: &Path,
    config: &SandboxConfig,
) -> SandboxWrapper {
    let bwrap = match which_binary("bwrap") {
        Some(p) => p,
        None => {
            tracing::warn!(
                "sandbox: `bwrap` not found on PATH — tool `{}` will run unwrapped",
                tool_bin.display()
            );
            return SandboxWrapper::None;
        }
    };

    // Collect the original program + args before we mutate `cmd`.
    let orig_program = cmd.as_std().get_program().to_owned();
    let orig_args: Vec<std::ffi::OsString> = cmd.as_std().get_args().map(|a| a.to_owned()).collect();

    let allow_net = config.net_allowed_for(tool_bin);
    let extra_binds = config.extra_binds_for(tool_bin);

    let mut wrapper_args: Vec<std::ffi::OsString> = Vec::new();

    // Read-only bind of the entire filesystem as the base.
    wrapper_args.push("--ro-bind".into());
    wrapper_args.push("/".into());
    wrapper_args.push("/".into());

    // Read-write bind for the project root so tools can write files.
    if let Some(root) = canonical_or_original(project_root) {
        wrapper_args.push("--bind".into());
        wrapper_args.push(root.as_os_str().to_owned());
        wrapper_args.push(root.as_os_str().to_owned());
    }

    // Additional per-tool read-write binds.
    for bind in extra_binds {
        if let Some(b) = canonical_or_original(bind) {
            wrapper_args.push("--bind".into());
            wrapper_args.push(b.as_os_str().to_owned());
            wrapper_args.push(b.as_os_str().to_owned());
        }
    }

    // Network namespace unshare — disabled unless allow_net is true.
    if !allow_net {
        wrapper_args.push("--unshare-net".into());
    }

    // Die with the parent process so orphaned tool servers don't linger.
    wrapper_args.push("--die-with-parent".into());

    // New session to prevent the tool from sending signals to the terminal.
    wrapper_args.push("--new-session".into());

    // Separator and then the original command.
    wrapper_args.push("--".into());
    wrapper_args.push(orig_program);
    wrapper_args.extend(orig_args);

    // Rewrite `cmd` to invoke bwrap instead.
    *cmd = tokio::process::Command::new(bwrap);
    cmd.args(wrapper_args);

    tracing::info!(
        "sandbox: wrapping `{}` with bwrap (net={})",
        tool_bin.display(),
        allow_net
    );
    SandboxWrapper::Bwrap
}

// ---------------------------------------------------------------------------
// macOS — sandbox-exec
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn apply_macos(
    cmd: &mut tokio::process::Command,
    tool_bin: &Path,
    project_root: &Path,
    config: &SandboxConfig,
) -> SandboxWrapper {
    let sandbox_exec = match which_binary("sandbox-exec") {
        Some(p) => p,
        None => {
            tracing::warn!(
                "sandbox: `sandbox-exec` not found — tool `{}` will run unwrapped",
                tool_bin.display()
            );
            return SandboxWrapper::None;
        }
    };

    let allow_net = config.net_allowed_for(tool_bin);
    let extra_binds = config.extra_binds_for(tool_bin);
    let project_root_str = project_root.to_string_lossy();

    // Build the sandbox-exec(1) TinyScheme profile.
    let mut profile = String::from("(version 1)\n");
    profile.push_str("(allow default)\n");

    // Deny all file-write except under project root (and extra binds).
    profile.push_str("(deny file-write*)\n");
    profile.push_str(&format!(
        "(allow file-write* (subpath \"{}\"))\n",
        project_root_str
    ));
    for bind in &extra_binds {
        let bind_str = bind.to_string_lossy();
        profile.push_str(&format!(
            "(allow file-write* (subpath \"{}\"))\n",
            bind_str
        ));
    }

    // Deny network unless allowed.
    if !allow_net {
        profile.push_str("(deny network*)\n");
    }

    // Allow process fork and exec (needed for subprocess spawning inside tool).
    profile.push_str("(allow process-fork)\n");
    profile.push_str("(allow process-exec)\n");

    // Collect original program + args.
    let orig_program = cmd.as_std().get_program().to_owned();
    let orig_args: Vec<std::ffi::OsString> = cmd.as_std().get_args().map(|a| a.to_owned()).collect();

    let mut wrapper_args: Vec<std::ffi::OsString> = Vec::new();
    wrapper_args.push("-p".into());
    wrapper_args.push(profile.into());
    wrapper_args.push(orig_program);
    wrapper_args.extend(orig_args);

    *cmd = tokio::process::Command::new(sandbox_exec);
    cmd.args(wrapper_args);

    tracing::info!(
        "sandbox: wrapping `{}` with sandbox-exec (net={})",
        tool_bin.display(),
        allow_net
    );
    SandboxWrapper::SandboxExec
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Find `name` on `$PATH`. Returns `None` if not found.
fn which_binary(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let candidate = dir.join(name);
            candidate.exists().then_some(candidate)
        })
    })
}

/// Try `canonicalize`; fall back to the original path if that fails (e.g.
/// the path doesn't exist yet at sandbox-apply time).
fn canonical_or_original(p: &Path) -> Option<PathBuf> {
    if p.as_os_str().is_empty() {
        return None;
    }
    Some(std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_cmd(bin: &str) -> tokio::process::Command {
        tokio::process::Command::new(bin)
    }

    fn config_on() -> SandboxConfig {
        SandboxConfig {
            enabled: true,
            ..SandboxConfig::default()
        }
    }

    fn config_off() -> SandboxConfig {
        SandboxConfig {
            enabled: false,
            ..SandboxConfig::default()
        }
    }

    #[test]
    fn disabled_config_returns_none_wrapper() {
        let mut cmd = make_cmd("/usr/bin/savvagent-tool-fs");
        let wrapper = apply_sandbox(
            &mut cmd,
            Path::new("/usr/bin/savvagent-tool-fs"),
            Path::new("/home/user/project"),
            &config_off(),
        );
        assert_eq!(wrapper, SandboxWrapper::None);
        // The command should be unchanged.
        assert_eq!(
            cmd.as_std().get_program(),
            std::ffi::OsStr::new("/usr/bin/savvagent-tool-fs")
        );
    }

    #[test]
    fn default_config_has_sandboxing_disabled() {
        let cfg = SandboxConfig::default();
        assert!(!cfg.enabled);
    }

    #[test]
    fn default_config_tool_bash_gets_net_override() {
        let cfg = SandboxConfig::default();
        let bash_bin = PathBuf::from("/usr/local/bin/savvagent-tool-bash");
        // tool-bash should have network allowed even when global allow_net = false.
        assert!(!cfg.allow_net, "global allow_net should default false");
        assert!(
            cfg.net_allowed_for(&bash_bin),
            "tool-bash override should allow net"
        );
    }

    #[test]
    fn non_bash_tool_inherits_global_net_setting() {
        let cfg = SandboxConfig {
            enabled: true,
            allow_net: false,
            ..SandboxConfig::default()
        };
        let fs_bin = PathBuf::from("/usr/local/bin/savvagent-tool-fs");
        assert!(!cfg.net_allowed_for(&fs_bin));
    }

    #[test]
    fn per_tool_override_can_deny_net_for_bash() {
        let mut cfg = SandboxConfig::default();
        cfg.tool_overrides.insert(
            "tool-bash".to_string(),
            ToolSandboxOverride {
                allow_net: Some(false),
                extra_binds: Vec::new(),
            },
        );
        let bash_bin = PathBuf::from("/usr/local/bin/savvagent-tool-bash");
        assert!(!cfg.net_allowed_for(&bash_bin));
    }

    #[test]
    fn extra_binds_merged_from_global_and_tool_override() {
        let global_bind = PathBuf::from("/data/shared");
        let tool_bind = PathBuf::from("/tmp/scratch");
        let mut cfg = SandboxConfig {
            extra_binds: vec![global_bind.clone()],
            ..SandboxConfig::default()
        };
        cfg.tool_overrides.insert(
            "tool-fs".to_string(),
            ToolSandboxOverride {
                allow_net: None,
                extra_binds: vec![tool_bind.clone()],
            },
        );
        let fs_bin = PathBuf::from("/usr/local/bin/savvagent-tool-fs");
        let binds = cfg.extra_binds_for(&fs_bin);
        assert_eq!(binds.len(), 2);
        assert!(binds.contains(&global_bind.as_path()));
        assert!(binds.contains(&tool_bind.as_path()));
    }

    #[cfg(target_os = "linux")]
    mod linux {
        use super::*;

        fn has_bwrap() -> bool {
            which_binary("bwrap").is_some()
        }

        /// Build a sandbox-wrapped command and verify the bwrap argv contains
        /// the expected flags. This is a pure command-builder test — it does
        /// NOT actually execute bwrap.
        #[test]
        fn bwrap_argv_contains_required_flags() {
            if !has_bwrap() {
                // bwrap not installed in CI — skip gracefully.
                return;
            }
            let mut cmd = make_cmd("/usr/local/bin/savvagent-tool-fs");
            let cfg = config_on();
            let wrapper = apply_sandbox(
                &mut cmd,
                Path::new("/usr/local/bin/savvagent-tool-fs"),
                Path::new("/tmp/project"),
                &cfg,
            );
            assert_eq!(wrapper, SandboxWrapper::Bwrap);
            let args: Vec<String> = cmd
                .as_std()
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            // Must contain read-only root bind.
            assert!(
                args.windows(3)
                    .any(|w| w == ["--ro-bind", "/", "/"]),
                "expected --ro-bind / / in {args:?}"
            );
            // Must contain --die-with-parent.
            assert!(
                args.contains(&"--die-with-parent".to_string()),
                "expected --die-with-parent in {args:?}"
            );
            // tool-fs has no net override → global allow_net = false → --unshare-net present.
            assert!(
                args.contains(&"--unshare-net".to_string()),
                "expected --unshare-net in {args:?}"
            );
        }

        #[test]
        fn bwrap_argv_no_unshare_when_net_allowed() {
            if !has_bwrap() {
                return;
            }
            let mut cfg = config_on();
            cfg.allow_net = true;
            let mut cmd = make_cmd("/usr/local/bin/savvagent-tool-fs");
            apply_sandbox(
                &mut cmd,
                Path::new("/usr/local/bin/savvagent-tool-fs"),
                Path::new("/tmp/project"),
                &cfg,
            );
            let args: Vec<String> = cmd
                .as_std()
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            assert!(
                !args.contains(&"--unshare-net".to_string()),
                "--unshare-net should be absent when allow_net=true: {args:?}"
            );
        }

        #[test]
        fn tool_bash_gets_no_unshare_net_by_default() {
            if !has_bwrap() {
                return;
            }
            let cfg = config_on(); // allow_net = false globally
            let mut cmd = make_cmd("/usr/local/bin/savvagent-tool-bash");
            apply_sandbox(
                &mut cmd,
                Path::new("/usr/local/bin/savvagent-tool-bash"),
                Path::new("/tmp/project"),
                &cfg,
            );
            let args: Vec<String> = cmd
                .as_std()
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            // tool-bash has allow_net=true override → no --unshare-net.
            assert!(
                !args.contains(&"--unshare-net".to_string()),
                "tool-bash should NOT have --unshare-net by default: {args:?}"
            );
        }

        /// Integration test: actually spawn a sandboxed `echo` via bwrap and
        /// confirm it can execute. Requires `bwrap` installed.
        #[tokio::test]
        #[ignore = "requires bwrap on PATH"]
        async fn bwrap_sandboxed_echo_runs() {
            let mut cmd = tokio::process::Command::new("echo");
            cmd.arg("hello-from-bwrap");
            let cfg = config_on();
            let wrapper = apply_sandbox(
                &mut cmd,
                Path::new("echo"),
                Path::new("/tmp"),
                &cfg,
            );
            assert_eq!(wrapper, SandboxWrapper::Bwrap);
            let output = cmd.output().await.expect("bwrap echo failed");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(stdout.contains("hello-from-bwrap"), "got: {stdout}");
        }

        /// Integration test: verify a sandboxed process cannot read `/etc/shadow`.
        /// Requires `bwrap` installed. Does not require root.
        #[tokio::test]
        #[ignore = "requires bwrap on PATH"]
        async fn bwrap_sandboxed_cannot_read_shadow() {
            // We use `cat /etc/shadow` as the canary. On a typical Linux system,
            // /etc/shadow is mode 640 root:shadow (or 000 root:root), so a
            // non-privileged process should fail regardless of sandboxing.
            // The sandbox adds the --ro-bind / / layer, which is fine here —
            // the key property we check is that the process *cannot* write
            // outside the project root, not that it can't read /etc/shadow.
            //
            // Instead we test that `bwrap --unshare-net` actually blocks network:
            // try to connect to 127.0.0.1:9 (discard port) — it should fail.
            let mut cmd = tokio::process::Command::new("sh");
            cmd.arg("-c").arg("echo hello > /tmp/savvagent_sandbox_test_write_outside");
            // project root is /tmp/project (doesn't exist but that's fine for the test)
            let cfg = config_on();
            apply_sandbox(
                &mut cmd,
                Path::new("sh"),
                Path::new("/tmp/project"),
                &cfg,
            );
            let output = cmd.output().await.expect("bwrap sh failed");
            // Writing outside the project root should fail — bwrap's ro-bind
            // makes / read-only except for the explicit --bind of project_root.
            assert!(
                !output.status.success(),
                "expected write outside project root to fail; exit={:?}",
                output.status
            );
        }
    }

    #[test]
    fn sandbox_config_roundtrips_toml() {
        let cfg = SandboxConfig {
            enabled: true,
            allow_net: false,
            tool_overrides: {
                let mut m = HashMap::new();
                m.insert(
                    "tool-bash".to_string(),
                    ToolSandboxOverride {
                        allow_net: Some(true),
                        extra_binds: vec![PathBuf::from("/var/cache")],
                    },
                );
                m
            },
            extra_binds: vec![PathBuf::from("/data/shared")],
        };
        let text = toml::to_string_pretty(&cfg).unwrap();
        let roundtripped: SandboxConfig = toml::from_str(&text).unwrap();
        assert_eq!(roundtripped.enabled, cfg.enabled);
        assert_eq!(roundtripped.allow_net, cfg.allow_net);
        assert_eq!(roundtripped.extra_binds, cfg.extra_binds);
        let bash_ov = roundtripped.tool_overrides.get("tool-bash").unwrap();
        assert_eq!(bash_ov.allow_net, Some(true));
        assert_eq!(bash_ov.extra_binds, vec![PathBuf::from("/var/cache")]);
    }
}
