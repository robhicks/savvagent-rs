# Security model

Savvagent runs language-model-driven tool spawns. The sandbox layer is
designed to contain a compromised or misbehaving tool — including
third-party MCP servers — without crippling normal use.

## What the sandbox covers

As of v0.7, when `enabled = true` (the default on Linux and macOS):

- **Writes** outside the project root are denied.
- **Network** is denied unless the per-tool override sets `allow_net = true`.
  As of v0.7, `tool-bash` defaults to network-denied with a permission
  prompt; see PR 15 in the v0.7 roadmap (#17) for details.
- **Reads** of well-known sensitive paths under `$HOME` are denied:
  `~/.ssh`, `~/.aws`, `~/.gnupg`, `~/.netrc`, `~/.config/gh`, `~/.mozilla`,
  `~/.config/google-chrome`, and (on macOS) the Firefox / Chrome profile
  directories under `~/Library/Application Support`. The canonical list
  lives in `crates/savvagent-host/src/sensitive_paths.rs`
  (`SENSITIVE_HOME_STEMS`).

## What the sandbox does not cover

- **Windows** has no sandbox layer yet. Tools run unwrapped with a
  one-time warning. AppContainer + Job Objects support is on the v0.8
  roadmap.
- **Reads outside the sensitive list.** Tools can still read arbitrary
  files under `$HOME` that are not on the sensitive-path list. The list
  is conservative; open an issue if a path you treat as secret isn't
  covered.
- **Domain-level network policy.** `allow_net = true` opens the full
  network. A bundled allowlist (e.g. `crates.io`, `registry.npmjs.org`)
  is on the v0.8 roadmap.
- **In-process exfiltration via the host itself.** A compromised provider
  binary can read its own process memory and the data the host has
  already loaded. Provider trust is out of scope for the sandbox layer.

## How to disable

Interactive:

```
/sandbox off
```

Or in `~/.savvagent/sandbox.toml`:

```toml
enabled = false
```

A user who explicitly sets `enabled = false` keeps that value across
upgrade.

## Reporting

Security reports: please use GitHub's Security Advisories feature on the
repository, or contact the maintainer listed in the workspace
`Cargo.toml`. Do not file security issues as public GitHub issues.
