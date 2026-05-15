# Self-update periodic re-check — design

Date: 2026-05-15
Status: pending review
Issue: #78 ("self-update: TUI only checks for new releases at startup —
no periodic re-check")
Ships as: v0.14.3 (PATCH)

## Problem

The `internal:self-update` plugin only checks for new releases when the
`HostStarting` hook fires (at TUI launch). A long-lived TUI session
never notices a release published mid-session, even if it stays open
for days. The 24h on-disk cache further suppresses re-checks across
restarts within the TTL, so a user who relaunches inside the cache
window also sees nothing new.

The user only learns about a new release on next launch *and* only when
either the cache has expired or the running version exceeds the cached
tag (the fix in #75 / commit f61924f).

## Approach

Extend the existing `on_event(HostStarting)` spawned task. Today it
runs the check-and-maybe-install body once and exits; instead wrap that
body in a `tokio::time::interval` loop:

- The first tick fires immediately, preserving today's startup
  behavior.
- Subsequent ticks fire every `PERIODIC_INTERVAL` (2 hours).
- Each tick runs the same `check_for_update` → `run_install` pipeline
  used by the startup path, so periodic detection auto-installs in
  exactly the same way (settled in brainstorming).

No new plugin hook, no host-level scheduler, no plugin-trait change —
the timer lives inside the existing tokio task.

## Per-tick skip rules

Evaluated at the top of each iteration:

| State at tick                 | Action                                  |
|-------------------------------|-----------------------------------------|
| `Disabled` (opt-out detected) | break loop                              |
| `InstallMethod::Dev`          | never starts the loop (existing check)  |
| `Installing { .. }`           | skip this tick (no double-installer)    |
| `Updated { .. }`              | break loop (awaiting user restart)      |
| `Unknown`                     | run check + maybe install               |
| `UpToDate`                    | run check + maybe install               |
| `Available { .. }`            | run check + maybe install (retry)       |
| `CheckFailed`                 | run check + maybe install (retry)       |
| `InstallFailed { .. }`        | run check + maybe install (retry)       |

The `Installing` skip prevents the timer from preempting an in-flight
install if one happens to span a tick boundary. The `Updated` break
stops the loop because the binary on disk has already been swapped;
the running process is now waiting for the user to relaunch, so
further checks add no value.

## Cache interaction

The 24h on-disk cache (`~/.savvagent/update-check.json`) is a
**startup-skip** mechanism — it answers "should this process even hit
the network at launch?" It is **not** an in-process throttle.

Periodic ticks therefore:

- **Bypass the cache read.** A tick always means "go ask GitHub now."
- **Continue to write the cache** on successful fetch, so the next
  process launch benefits from the freshest tag the running session
  observed.

This keeps cache semantics simple: the cache file always reflects the
latest tag the most recent run-of-savvagent confirmed with GitHub,
regardless of which tick produced it.

## Interval

```rust
const PERIODIC_INTERVAL: Duration = Duration::from_secs(2 * 60 * 60);
```

Fixed at 2 hours. Not configurable via env var — YAGNI; can be added
later if users ask. The existing `SAVVAGENT_NO_UPDATE_CHECK` env var
and `--no-update-check` CLI flag continue to short-circuit the plugin
at construction, so the loop never starts when the user has opted out.

For tests, a private `Duration` field on the plugin overrides the
const. The field defaults to `PERIODIC_INTERVAL`; a `#[cfg(test)]`
builder method (`with_periodic_interval`) lets tests drop it to
something small (e.g. `Duration::from_millis(50)`) so `tokio::time::pause()`
+ `advance()` can drive multiple ticks without real sleeping.

## Module changes

All edits land in `crates/savvagent/src/plugin/builtin/self_update/mod.rs`:

1. Add `PERIODIC_INTERVAL` const.
2. Add `periodic_interval: Duration` field on `SelfUpdatePlugin`,
   defaulted in `with_fetcher_and_installer`.
3. Add `#[cfg(test)] fn with_periodic_interval(self, d: Duration) -> Self`.
4. Refactor the body of the spawned task in `on_event` into a private
   `async fn run_check_once(...)` so the loop body is a single call.
5. Replace the one-shot spawn with a `loop { interval.tick().await;
   ... }` that consults the skip rules above before calling
   `run_check_once`. First `interval.tick()` resolves immediately, so
   the existing startup-check timing is preserved.

`check.rs`, `apply.rs`, `cache.rs`, `install_method.rs` are unchanged.
No public API change on `SelfUpdatePlugin` except the test-only
builder.

## Tests added

In the existing `mod tests` block in `mod.rs`. All four new tests run
under `#[tokio::test(start_paused = true)]` so virtual time can be
advanced deterministically. A small per-test interval (e.g.
`Duration::from_millis(50)`) is wired in via `with_periodic_interval`,
and a small helper that interleaves `tokio::time::advance(...)` with
`yield_now().await` polls the plugin state without burning a real
2-hour clock.

Existing tests at `mod.rs:948` and `mod.rs:1003` use a `wait_for_state`
helper that only yields — it does not advance virtual time and so is
not reusable for the new tests. The four tests below define their own
`advance_then_yield` helper.

1. **`periodic_check_runs_multiple_ticks`** — build the plugin with
   `FixedFetcher("vX")` (where X is below `CARGO_PKG_VERSION` so the
   classification lands at `UpToDate`, keeping the loop alive),
   `StubInstaller::ok()`, `with_install_method(InstallMethod::Installed)`,
   and a 50ms periodic interval. Drive `on_event(HostStarting)`,
   yield to let the first tick run, then advance 2× the interval. Assert
   `fetcher.invocation_count() >= 3` (one startup tick + at least two
   periodic ticks).
2. **`periodic_check_breaks_after_updated`** — `FixedFetcher("v99.99.99")`
   + `StubInstaller::ok()`, `InstallMethod::Installed`, 50ms interval.
   First tick lands the plugin in `Updated`. Advance 5× the interval
   and assert `installer.invocation_count() == 1` and
   `fetcher.invocation_count() == 1` — the loop must have broken.
3. **`periodic_check_skips_during_installing`** — `FixedFetcher("v99.99.99")`
   + a new `BlockingStubInstaller` whose `install()` awaits a
   `tokio::sync::Notify::notified()` held by the test. Drive
   `on_event`; the first tick classifies `Available`, calls
   `run_install`, sets state to `Installing`, and parks on the
   notify. Advance 3× the interval, yielding between advances. Assert
   `installer.invocation_count() == 1` (no second installer
   invocation), then release the notify, yield, and assert state
   transitions to `Updated`.
4. **`periodic_check_does_not_start_in_dev`** — explicitly call
   `with_install_method(InstallMethod::Dev)`. Drive `on_event`,
   advance several intervals. Assert `installer.invocation_count() == 0`
   and that the fetcher was never invoked. State must end at
   `Disabled`.
5. **Existing tests stay green.** The four tests that exercise
   `on_event` (`host_starting_auto_installs_on_available_then_writes_cache`,
   `host_starting_install_failure_transitions_to_install_failed`,
   `other_events_are_ignored`,
   `host_starting_bypasses_cache_when_current_version_is_ahead_of_cached_tag`)
   now observe the first tick of the interval but otherwise see the
   same behavior they assert today.

All new tests hold `HOME_LOCK` and reset the locale to `en` inside the
guard, per `feedback_test_locale_isolation` in project memory. The
`ReleasesFetcher` trait does not currently expose an invocation
counter on its production impl; tests 1 and 2 need a new
`CountingFetcher` test double that wraps `FixedFetcher` and increments
an `AtomicUsize` per `latest_tag()` call.

## Concurrency notes

- The spawned task continues to be a `tokio::spawn` future on the host
  runtime. It clones `Arc<Mutex<UpdateState>>`, `Arc<dyn ReleasesFetcher>`,
  `Arc<dyn Installer>` at spawn time — no new locks.
- `state.lock()` is never held across an `.await` (the existing code
  already enforces this; the loop preserves it by lifting the
  classification body into a non-locking helper).
- The `rmcp ProgressDispatcher` gotcha (see `project_rmcp_progress_gotcha`
  memory) does not apply here — this plugin's network call goes through
  `reqwest` in `GithubReleasesFetcher`, not through an MCP RPC.

## Versioning, changelog, docs

- Bump workspace version `0.14.2` → `0.14.3` in the root `Cargo.toml`
  `[workspace.package]` block and mirror the bump in the
  `[workspace.dependencies]` literals for each in-workspace crate
  (per `feedback_semver` in project memory).
- Add a CHANGELOG entry under `## [0.14.3] - 2026-05-15` referencing
  issue #78 and naming the new 2h periodic re-check behavior.
- Update the README's self-update paragraph to mention that the check
  also re-runs every 2 hours while the TUI is open, alongside the
  existing opt-out flag documentation (per `feedback_release_docs`).

## Out of scope

- Configurable interval via env var (deferred until requested).
- A separate "periodic check" plugin hook on the trait surface (the
  in-task `tokio::time::interval` is sufficient).
- Adjusting the 24h cache TTL — it remains a startup-only concern.
- Notify-only mode for periodic detection — auto-install was settled
  in brainstorming.

## Ship plan

One PR — localized fix in a single file plus version bump, CHANGELOG,
README:

1. Bump workspace version to 0.14.3 (Cargo.toml + workspace.dependencies
   literals).
2. Implement the loop + tests in `self_update/mod.rs`.
3. CHANGELOG entry.
4. README paragraph update.
5. Verify `cargo build`, `cargo test --workspace`, `rustup run stable
   cargo fmt`, `rustup run stable cargo clippy --workspace --all-targets
   -- -D warnings` all clean (per `feedback_match_ci_toolchain_locally`).
6. Open PR, wait for CI green (per `feedback_verify_ci_after_push`),
   merge.
7. Tag `v0.14.3` — cargo-dist owns the release lifecycle, do not run
   `gh release create` manually (per `feedback_cargo_dist_release`).
8. Post a comment on issue #78 closing it (per
   `feedback_keep_issue_updated`).
