//! `/update` apply path for `internal:self-update`.
//!
//! v0.11.0 PR 4 wires the `/update` slash command. When the plugin's
//! state is [`UpdateState::Available`], `handle_slash` invokes
//! [`apply_update`], which downloads the matching GitHub Release asset
//! for the running binary's target triple and atomically replaces the
//! running executable using the [`self_update`] crate. On success the
//! plugin transitions to [`UpdateState::Updated`] so PR 3's banner row
//! flips to "Updated to vX, restart to apply" and PR 5's exit hook can
//! emit a stderr hint.
//!
//! The [`BinarySwapper`] trait keeps `self_update`'s blocking,
//! platform-specific call out of the plugin's unit tests; production
//! uses [`SelfUpdateBinarySwapper`] and tests substitute a stub.

use async_trait::async_trait;
use semver::Version;

use super::UpdateState;

/// GitHub repo owner — must match `RELEASES_API_URL` in `check.rs`.
const REPO_OWNER: &str = "robhicks";
/// GitHub repo name.
const REPO_NAME: &str = "savvagent-rs";
/// Name of the binary asset to install. cargo-dist appends `.exe` on
/// Windows automatically inside `self_update::backends::github`.
const BIN_NAME: &str = "savvagent";
/// Path of the binary inside the release archive. cargo-dist (≥0.20)
/// nests every binary under a top-level `savvagent-{target}/` directory
/// in the Linux/macOS tarball, but ships the Windows zip flat with the
/// binaries at the archive root. `self_update`'s default of `{{ bin }}`
/// looks at the root, which works for Windows but fails on Unix with
/// `Could not find the required path in the archive: "savvagent"`.
///
/// `{{ target }}` is substituted by `self_update` with the resolved
/// target triple (e.g. `x86_64-unknown-linux-gnu`); `{{ bin }}` with the
/// `bin_name` value. Compile-time `cfg` is safe here because the running
/// binary always fetches the archive matching its own build target.
#[cfg(target_os = "windows")]
const BIN_PATH_IN_ARCHIVE: &str = "{{ bin }}";
#[cfg(not(target_os = "windows"))]
const BIN_PATH_IN_ARCHIVE: &str = "savvagent-{{ target }}/{{ bin }}";

/// Abstraction over the actual binary swap. The production impl drives
/// [`self_update::backends::github::Update`]; tests substitute a stub
/// that records the requested upgrade and returns a canned result.
#[async_trait]
pub trait BinarySwapper: Send + Sync {
    /// Download the release asset for `latest` and atomically replace
    /// the running binary. `current` is included so the swapper can
    /// short-circuit a no-op upgrade. Returns an error on any download,
    /// extraction, or filesystem failure — the plugin keeps the banner
    /// in `Available` and pushes a note with the error message.
    async fn swap(&self, current: &Version, latest: &Version) -> anyhow::Result<()>;
}

/// Production [`BinarySwapper`] backed by the [`self_update`] crate's
/// GitHub backend. Wraps the synchronous `.update()` call in
/// [`tokio::task::spawn_blocking`] so the tokio runtime can keep
/// servicing the TUI render loop while the download proceeds.
#[derive(Debug, Default)]
pub struct SelfUpdateBinarySwapper;

#[async_trait]
impl BinarySwapper for SelfUpdateBinarySwapper {
    async fn swap(&self, current: &Version, latest: &Version) -> anyhow::Result<()> {
        let current = current.to_string();
        let latest_target = format!("v{latest}");
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let status = self_update::backends::github::Update::configure()
                .repo_owner(REPO_OWNER)
                .repo_name(REPO_NAME)
                .bin_name(BIN_NAME)
                .bin_path_in_archive(BIN_PATH_IN_ARCHIVE)
                .show_download_progress(false)
                .show_output(false)
                .no_confirm(true)
                .current_version(&current)
                .target_version_tag(&latest_target)
                .build()?
                .update()?;
            tracing::debug!(status = ?status, "self-update: swap complete");
            Ok(())
        })
        .await?
    }
}

/// Drive the apply path: call the swapper and, on success, return the
/// new [`UpdateState::Updated`]. On failure the caller leaves the state
/// in `Available` and surfaces the error to the user.
pub async fn apply_update<S: BinarySwapper + ?Sized>(
    swapper: &S,
    current: Version,
    latest: Version,
) -> anyhow::Result<UpdateState> {
    swapper.swap(&current, &latest).await?;
    Ok(UpdateState::Updated {
        from: current,
        to: latest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Records the most recent `(current, latest)` pair and returns the
    /// configured result. Used by both success and failure tests.
    struct StubSwapper {
        result: Mutex<Result<(), String>>,
        last_call: Mutex<Option<(Version, Version)>>,
    }

    impl StubSwapper {
        fn ok() -> Self {
            Self {
                result: Mutex::new(Ok(())),
                last_call: Mutex::new(None),
            }
        }
        fn err(msg: &str) -> Self {
            Self {
                result: Mutex::new(Err(msg.into())),
                last_call: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl BinarySwapper for StubSwapper {
        async fn swap(&self, current: &Version, latest: &Version) -> anyhow::Result<()> {
            *self.last_call.lock().unwrap() = Some((current.clone(), latest.clone()));
            match &*self.result.lock().unwrap() {
                Ok(()) => Ok(()),
                Err(msg) => Err(anyhow::anyhow!(msg.clone())),
            }
        }
    }

    #[tokio::test]
    async fn apply_returns_updated_state_on_success() {
        let swapper = StubSwapper::ok();
        let current = Version::parse("0.10.0").unwrap();
        let latest = Version::parse("0.11.0").unwrap();
        let state = apply_update(&swapper, current.clone(), latest.clone())
            .await
            .unwrap();
        match state {
            UpdateState::Updated { from, to } => {
                assert_eq!(from, current);
                assert_eq!(to, latest);
            }
            other => panic!("expected Updated, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn apply_propagates_swapper_error() {
        let swapper = StubSwapper::err("disk full");
        let current = Version::parse("0.10.0").unwrap();
        let latest = Version::parse("0.11.0").unwrap();
        let err = apply_update(&swapper, current, latest).await.unwrap_err();
        assert!(err.to_string().contains("disk full"), "got: {err}");
    }

    #[tokio::test]
    async fn apply_forwards_versions_to_swapper() {
        let swapper = StubSwapper::ok();
        let current = Version::parse("0.10.0").unwrap();
        let latest = Version::parse("0.11.2").unwrap();
        apply_update(&swapper, current.clone(), latest.clone())
            .await
            .unwrap();
        let last = swapper.last_call.lock().unwrap().clone().unwrap();
        assert_eq!(last.0, current);
        assert_eq!(last.1, latest);
    }
}
