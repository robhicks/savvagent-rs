# Savvagent

A fast, MCP-first terminal coding agent written end-to-end in Rust.

The product vision and rationale live in [`PRD.md`](PRD.md). This README is
for developers working on the repo: how to build it, how it's laid out, and
how to extend it. End users wanting to install Savvagent can skip to
[Install](#install) below.

## Install

Precompiled binaries for Linux (x86_64 / aarch64), macOS (Apple Silicon),
and Windows (x86_64) are published to GitHub Releases on every tag. Each
release ships one archive per platform containing four binaries — the
TUI plus the bundled `savvagent-tool-fs`, `savvagent-anthropic`, and
`savvagent-gemini` MCP servers — installed to your Cargo bin directory.

**Linux / macOS** (one-liner):

```bash
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/robhicks/savvagent-rs/releases/latest/download/savvagent-installer.sh | sh
```

**Windows** (PowerShell):

```powershell
powershell -ExecutionPolicy ByPass -c "irm https://github.com/robhicks/savvagent-rs/releases/latest/download/savvagent-installer.ps1 | iex"
```

**Manual:** download the matching `savvagent-<target>.tar.xz` (or `.zip`
on Windows) from the [Releases page](https://github.com/robhicks/savvagent-rs/releases),
unpack it, and put the binaries on your `$PATH`. Each archive ships with a
`.sha256` next to it for verification.

After installing, run `savvagent` in your project, `/connect` once to store
an API key in the OS keyring, and you're done.

## Repository layout

The workspace is a small set of focused crates:

| Crate | Purpose |
|---|---|
| [`crates/savvagent`](crates/savvagent) | All four shipping binaries (`savvagent` TUI plus the `savvagent-tool-fs`, `savvagent-anthropic`, `savvagent-gemini` shims). Owns `/connect`, file picker, transcript persistence. |
| [`crates/savvagent-host`](crates/savvagent-host) | Agent engine consumed as a library. Drives the tool-use loop, manages provider/tool sessions, exposes `Host::run_turn` and `run_turn_streaming`. |
| [`crates/savvagent-protocol`](crates/savvagent-protocol) | Pure-types crate: `CompleteRequest`, `CompleteResponse`, `StreamEvent`, content blocks. SPP wire spec in [`SPEC.md`](crates/savvagent-protocol/SPEC.md). |
| [`crates/savvagent-mcp`](crates/savvagent-mcp) | The `ProviderClient` / `ProviderHandler` traits and the `InProcessProviderClient` bridge that makes provider crates linkable as libraries. |
| [`crates/provider-anthropic`](crates/provider-anthropic) | Anthropic Messages API as a `ProviderHandler` library plus `provider_anthropic::run` (the entry point the `savvagent-anthropic` shim calls). |
| [`crates/provider-gemini`](crates/provider-gemini) | Same shape, for Google Gemini. |
| [`crates/tool-fs`](crates/tool-fs) | `read_file`/`write_file`/`list_dir`/`glob` library plus `tool_fs::run` (the entry point the `savvagent-tool-fs` shim calls). |

Every provider and every tool is "just" an MCP-shaped library that *can* be
wrapped in a binary. The TUI links providers in-process by default and only
spawns `savvagent-tool-fs` (because tools have to be a separate process).

## Prerequisites

- Rust 1.85+ (workspace pins `rust-version = "1.85"`).
- Linux: a running freedesktop Secret Service for the keyring (GNOME Keyring,
  KeePassXC, or KWallet — any of them works). The crate falls back to a
  no-op when none is present, but `/connect` will fail to persist keys.
- macOS / Windows: nothing extra; the keyring uses the platform store.

## Quick start

```bash
# Build everything once. Important: the TUI doesn't depend on the tool-fs
# crate at compile time, but it spawns `savvagent-tool-fs` at runtime — a
# workspace build is the easy way to make that binary exist.
cargo build

# Run the TUI. With nothing configured, it boots disconnected.
cargo run -p savvagent
```

If `savvagent-tool-fs` isn't on `$PATH` and isn't sitting next to the TUI
binary, the TUI still boots — it just shows a note that tools are
disabled. Re-run `cargo build` (or set `SAVVAGENT_TOOL_FS_BIN`) to enable
them.

Inside the TUI:

1. Press <kbd>Ctrl-P</kbd> to open the command palette, choose `/connect`
   (or just type `/connect` and press <kbd>Enter</kbd>).
2. Pick a provider with <kbd>↑</kbd>/<kbd>↓</kbd>, hit <kbd>Enter</kbd>.
3. Paste your API key (input is masked) and <kbd>Enter</kbd>.

The key is stashed in the OS keyring under service `savvagent`, account
`<provider id>`. On the next launch the TUI auto-connects to whichever
provider has a key on file.

### Other slash commands

| Command | What it does |
|---|---|
| `/connect` | Pick a provider, set its API key, swap the active host. |
| `/clear` | Reset the conversation history (and the visible log). |
| `/save` | Write the current transcript to `~/.savvagent/transcripts/<unix>.json`. |
| `/view <path>` | Open a file in the read-only popup editor. |
| `/edit <path>` | Open a file for editing (Esc saves and closes). |
| `/quit` | Exit. |

`@` opens a file picker that inserts `@path` into the prompt.

## Development workflow

```bash
# Continuous type-checking on save.
bacon                # default job is `cargo check`
bacon clippy-all     # clippy across the workspace

# Tests.
cargo test --workspace

# Specific crate.
cargo test -p savvagent-host

# The headless host smoke-test (needs a running provider — see below).
cargo run -p savvagent-host --example headless -- "list my Cargo.toml"
```

`bacon.toml` defines several jobs (`check`, `check-all`, `clippy`,
`clippy-all`, `test`, `doc`, `run`); pick whichever matches what you're
iterating on.

### Running providers as standalone MCP servers

The default in-process path is the easy one. Sometimes you want the binary
form — e.g., when iterating on the wire format or running the `headless`
example. The standalone provider servers ship as bins on the `savvagent`
crate; each takes its API key via env and listens on loopback:

```bash
# Anthropic — defaults to 127.0.0.1:8787
ANTHROPIC_API_KEY=sk-ant-… cargo run -p savvagent --bin savvagent-anthropic

# Gemini — defaults to 127.0.0.1:8788
GEMINI_API_KEY=…           cargo run -p savvagent --bin savvagent-gemini
```

Then point the TUI (or `savvagent-host` example) at it:

```bash
SAVVAGENT_PROVIDER_URL=http://127.0.0.1:8787/mcp cargo run -p savvagent
```

When `SAVVAGENT_PROVIDER_URL` is set the TUI uses the MCP client path
instead of the in-process bridge — useful for debugging the wire protocol
or pointing at a third-party MCP provider.

## Architecture in five sentences

`Host` (in `savvagent-host`) holds a `Box<dyn ProviderClient>` and a
`ToolRegistry`. The `ProviderClient` is either an in-process bridge over a
`ProviderHandler` (default) or an `rmcp` Streamable HTTP client connected
to a remote provider binary (opt-in). Each user turn runs through
`Host::run_turn_streaming`, which loops `provider.complete` →
`tool_registry.call` until the model emits `end_turn`, forwarding stream
events to the TUI as it goes. The TUI keeps the active host in
`Arc<RwLock<Option<Arc<Host>>>>` so per-turn tasks can snapshot it without
holding a lock across awaits, and `/connect` swaps the slot atomically.
Tool servers are stdio children, owned by the registry and reaped on
shutdown.

## Adding a new provider

1. Create `crates/provider-foo/` with the standard layout (mirror
   `provider-gemini`).
2. Implement `savvagent_mcp::ProviderHandler` for your `FooProvider` —
   translate to/from the upstream API, deal with streaming via
   `StreamEmitter`. The Anthropic and Gemini crates are the reference.
3. Expose a `FooProvider::builder().api_key(...).build()` constructor.
4. Append one entry to [`crates/savvagent/src/providers.rs::PROVIDERS`]:
   ```rust
   ProviderSpec {
       id: "foo",
       display_name: "Foo Models",
       api_key_env: "FOO_API_KEY",
       default_model: "foo-latest",
       build: build_foo,
   }
   ```
   …and a `build_foo` factory next to the existing two.
5. Wire `provider-foo` into `Cargo.toml` (workspace deps + savvagent crate
   deps).
6. Optional: ship a standalone `savvagent-foo` MCP server. Add a
   `pub async fn run()` to `provider_foo`'s lib (mirror `provider_anthropic::run`),
   then add a `[[bin]]` entry in `crates/savvagent/Cargo.toml` pointing at a
   3-line shim in `crates/savvagent/src/bin/savvagent-foo.rs` that just calls
   `provider_foo::run().await`. The release archive picks it up automatically.

That's the whole touch surface. There's no provider registry to update in
the host, no tool dispatch table — the host doesn't know about providers
beyond the `ProviderClient` trait.

## Adding a new tool

Tools are stdio MCP servers. Mirror `crates/tool-fs`:

1. Implement your tool methods using `rmcp`'s server primitives.
2. Build a binary that calls `serve_server` on stdin/stdout.
3. The host config takes a `ToolEndpoint::Stdio { command, args }` —
   `savvagent` currently bakes one in (`SAVVAGENT_TOOL_FS_BIN`); for
   additional tools you can extend `HostConfig::with_tool` calls in
   `crates/savvagent/src/main.rs`.

## Environment variables

| Var | Where read | Default | Notes |
|---|---|---|---|
| `SAVVAGENT_PROVIDER_URL` | `savvagent` | (unset) | When set, skips in-process bridge; uses MCP HTTP. |
| `SAVVAGENT_MODEL` | `savvagent` | per-provider | Overrides `ProviderSpec::default_model`. |
| `SAVVAGENT_TOOL_FS_BIN` | `savvagent` | `savvagent-tool-fs` (PATH) | Path to the fs tool binary. |
| `ANTHROPIC_API_KEY` | `savvagent-anthropic` | — | Read at server start. In-process flow gets it from `/connect`. |
| `ANTHROPIC_BASE_URL` | `savvagent-anthropic` | `https://api.anthropic.com` | For local mocks. |
| `SAVVAGENT_ANTHROPIC_LISTEN` | `savvagent-anthropic` | `127.0.0.1:8787` | Bind address. |
| `GEMINI_API_KEY` / `GOOGLE_API_KEY` | `savvagent-gemini` | — | Same idea. |
| `GEMINI_BASE_URL` | `savvagent-gemini` | `https://generativelanguage.googleapis.com` | |
| `SAVVAGENT_GEMINI_LISTEN` | `savvagent-gemini` | `127.0.0.1:8788` | |
| `RUST_LOG` | all binaries | `warn` (TUI), `info` (providers) | Standard `tracing-subscriber` env filter. |

`.env` and `.env.local` at the repo root are auto-loaded on startup.

## Persistence on disk

| Path | Owner | Contents |
|---|---|---|
| `~/.savvagent/transcripts/<unix_secs>.json` | TUI | One pretty-printed `Vec<spp::Message>` per save (auto on `TurnComplete`, manual on `/save`). |
| `~/.savvagent/` (other) | reserved | Future config / per-project state. |
| OS keyring (`service=savvagent`, `account=<provider id>`) | `/connect` | Provider API keys. Never written to disk in plaintext. |

## Project context: `SAVVAGENT.md`

If a `SAVVAGENT.md` file exists at the project root the host reads it as
the system prompt. See `crates/savvagent-host/src/project.rs` and the
"Project context" section of the PRD.

## Reference docs

- [`PRD.md`](PRD.md) — vision, scope, milestones.
- [`crates/savvagent-protocol/SPEC.md`](crates/savvagent-protocol/SPEC.md) —
  Savvagent Provider Protocol (SPP) wire format.
- [`docs/`](docs) — architecture diagrams and design notes.

## License

Dual-licensed under Apache-2.0 OR MIT, at your option.
