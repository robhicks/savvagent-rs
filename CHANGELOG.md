# Changelog

All notable changes to savvagent are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
(pre-1.0: `0.MINOR.PATCH`, where MINOR captures features + breaking
boundary changes and PATCH captures fixes).

## v0.10.0 — Localize TUI (2026-05-13)

Internationalization for the TUI. The savvagent crate now ships
`rust-i18n` catalogs for English, Spanish, Portuguese, and Hindi. A
new `internal:language` built-in plugin contributes a `/language`
slash command and a centered-modal picker (mirroring the existing
`internal:themes` plugin).

### New features

- `/language` — open the language picker. Arrow-key navigation,
  type-to-filter, Enter to apply + persist, Esc to cancel.
- `/language <code>` — directly switch to a supported locale
  (`en`, `es`, `pt`, `hi`).
- Boot-time locale detection: `~/.savvagent/language.toml` > `LC_ALL`
  > `LC_MESSAGES` > `LANG` > `en`.
- Live preview during picker navigation; Esc reverts.

### Plugin SDK changes

- `Effect::SetActiveLocale { code, persist }` — additive variant on
  the `#[non_exhaustive]` Effect enum.
- `ScreenArgs::LanguagePicker { current_code }` — additive variant on
  the `#[non_exhaustive]` ScreenArgs enum.
- `savvagent-plugin` crate version: 0.9.0 → 0.10.0.

### Known limitations

- Slash-command summaries in the command palette are captured at the
  boot locale; changing language mid-session requires a restart to
  refresh the summary column. Modal titles, picker rows, status-bar
  text, and pushed notes all re-resolve every frame.
- Hindi rendering depends on a terminal font that includes Devanagari
  glyphs. Without one, rows fall back to replacement boxes. The
  language code column (`hi`) is always ASCII, so the user can still
  filter and select.
- `LANGUAGE=` (glibc compound-locale env var) is not honored — only
  POSIX `LC_ALL` / `LC_MESSAGES` / `LANG`.
- A small number of user-facing literals were deferred during PR 6's
  string sweep (ui.rs header, plugin manifest `name` fields, a handful
  of `app.rs` notes). The catalog parity test continues to enforce
  structural correctness on what IS in the catalog; these will be
  cleaned up in a follow-up.

### Migration notes

No external API changes for non-plugin consumers. Plugin authors:
`SetActiveLocale` and `LanguagePicker` are additive on
`#[non_exhaustive]` enums; existing match arms continue to compile
unchanged. The runtime applies `SetActiveLocale` automatically; no
plugin code needs to call `rust_i18n::set_locale` directly.

## [0.9.0] - 2026-05-12

### v0.9.0 — Plugin system

The TUI and host are now routed through a typed `Plugin` trait. Eighteen
built-in plugins compose the entire UI surface — chrome (footer, tips),
splash, command palette, modal screens (themes, plugins manager, connect,
resume, file viewer/editor), slash commands (`/clear`, `/save`, `/model`,
`/connect`, `/resume`, `/quit`), and provider plugins for Anthropic,
OpenAI, Gemini, and the local Ollama backend. A new screen-stack runtime
replaces the v0.8 `InputMode` state machine with consistent open / close /
back semantics across every modal. Plugin enable / disable state persists
to `~/.savvagent/plugins.toml`. The trait surface is intentionally
WIT-portable so the same plugin contract can drive a future WASM
Component-Model loader without churn.

### Plugin runtime (`savvagent-plugin` crate)

- **New leaf crate `savvagent-plugin`** carrying owned types + trait
  definitions only. No `&str` returns, no callbacks, no host references
  — every method takes / returns owned data so the same contract can
  cross a WASM Component-Model boundary verbatim.
- **`Plugin` trait surface:** `manifest()`, `handle_slash()`,
  `on_event()`, `render_slot()`, and `create_screen()`. A plugin
  implements only the methods relevant to its contributions; defaults
  return `Vec::new()` / `None`.
- **Effect enum** (`Effect`) describes every action a plugin can request:
  `PushNote`, `OpenScreen`, `CloseScreen`, `Stack`, `RunSlash`,
  `SetActiveTheme`, `RegisterProvider`, `SaveTranscript`, `ClearLog`,
  `TogglePlugin`, `Quit`, `PrefillInput`. The host applies effects in
  the order returned; `Stack` composes effects without per-plugin
  recursion.
- **Concrete enums for everything that crosses the boundary** —
  `PluginKind`, `Slot`, `ScreenLayout`, `ThemeColor`, `HostEvent`,
  `Effect` — instead of `Box<dyn Trait>` or string tags. WIT export is
  mechanical: no design work pending.

### Screen stack replaces `InputMode`

- The v0.8 `InputMode` enum (Normal, SelectingTheme, ConnectPicker, …)
  is gone. A single `screen_stack: Vec<ActiveScreen>` field on `App`
  now holds whichever screens are open; the textarea is the focus
  target when the stack is empty.
- **`ScreenLayout` variants** — `CenteredModal { width, height }`,
  `Fullscreen`, `BottomSheet { height }` — clear and repaint their
  region every frame so modals never bleed through onto each other.
- **Open / close / back semantics are uniform.** Esc pops the top
  screen, Enter commits, Ctrl-C closes everything. Splash, command
  palette, themes picker, plugins manager, connect picker, resume
  picker, and the file viewer / editor all participate.

### 18 built-in plugins

Grouped by category. Every entry is shipped enabled by default unless
noted; Core plugins cannot be disabled.

- **Chrome (Core):**
  - `internal:home-footer` — three-segment status bar (provider
    badges left, turn state center, `working_dir · ~N ctx · $0.00 · vX.Y.Z`
    right).
  - `internal:home-tips` — bottom-of-screen muted hint line.
- **Splash (Core):** `internal:splash` — startup splash screen.
- **Modals (Core):**
  - `internal:command-palette` — Ctrl-P / `/` palette.
  - `internal:themes` — `/theme` picker (replaces the v0.8 dedicated
    modal; now a `Screen` plugin like everything else).
  - `internal:plugins-manager` — `/plugins` enable / disable manager.
- **File screens (Optional):**
  - `internal:view-file` — `/view` centered modal.
  - `internal:edit-file` — `/edit` centered modal.
- **Slash-command plugins:**
  - `internal:clear` (Core), `internal:save` (Core), `internal:model`
    (Core), `internal:connect` (Core), `internal:resume` (Core),
    `internal:quit` (Core).
- **Provider plugins (Optional):**
  - `internal:provider-anthropic`, `internal:provider-openai`,
    `internal:provider-gemini`, `internal:provider-local`.

### Plugin manager + persistence

- **`/plugins` opens the manager modal** listing every plugin with its
  kind, version, contribution summary (which slash commands / slots /
  screens it owns), and an on / off toggle. Core plugins render greyed
  out — selecting them is a no-op.
- **Toggles persist atomically** to `~/.savvagent/plugins.toml`
  (schema `version = 1`). Writes go through a tempfile + rename so a
  crash mid-write never leaves a half-baked file.
- **File permissions:** 0o600 on the file, 0o700 on the
  `~/.savvagent/` directory (Unix). Missing file = all defaults
  (additive — never a hard failure on first launch).
- **Startup re-applies persisted overrides** before building the slash
  / slot / screen indexes, so a disabled plugin contributes nothing
  from frame one.

### Theme system (v0.8 work preserved + extended)

- **All v0.8 themes still present** — the 3 hand-rolled built-ins
  (`default`, `dark-mono`, `pastel`) plus the 15 upstream
  `ratatui-themes` slugs (`dracula`, `nord`, `tokyo-night`,
  `catppuccin-mocha`, `catppuccin-latte`, `gruvbox-dark`,
  `gruvbox-light`, `solarized-dark`, `solarized-light`,
  `one-dark-pro`, `monokai-pro`, `rose-pine`, `kanagawa`,
  `everforest`, `cyberpunk`).
- **Picker is now a `Screen` plugin** (`internal:themes`) — same
  open / close semantics as every other modal. `/theme` opens the
  picker; `/theme <slug>` still applies + persists directly without
  opening the modal.
- **Semantic `ThemeColor` variants** — `Fg`, `Bg`, `Accent`, `Muted`,
  `Error`, `Warning`, `Success`, `Secondary`, `Border` — so plugin
  chrome resolves through the active theme rather than hard-coding
  Crossterm colors. Switch themes and every plugin's rendering
  follows.

### Event-hook dispatch

- **`HostEvent` lifecycle events** flow through a `HookDispatcher`:
  `HostStarting`, `Connect`, `Disconnect`, `TurnStart`, `TurnEnd`,
  `ToolCallStart`, `ToolCallEnd`, `PromptSubmitted`, `TranscriptSaved`,
  `ProviderRegistered`, `ContextSizeChanged`.
- **Per-subscriber error isolation.** A panicking or errant plugin
  doesn't take down the dispatcher or other subscribers; its error is
  logged + skipped.
- **Shared `MAX_DISPATCH_DEPTH` cap** with `RunSlash` re-entry — a
  plugin that emits `RunSlash` in response to an event still respects
  the same recursion limit as plugin-emitted slash dispatch, so
  feedback loops fail fast.

### Multi-region home layout

- **Three-segment footer** below the textarea:
  - **Left:** provider badges, contributed by whichever provider
    plugins are enabled + connected.
  - **Center:** turn state ("ready", "thinking…", tool-call summary),
    contributed by `home-footer`.
  - **Right:** `working_dir · ~N ctx · $0.00 · vX.Y.Z`, contributed by
    `home-footer`.
- **1-row vertical / 2-col horizontal frame margin** around the content
  area for breathing room.
- **`$0.00` is a placeholder** — see _Out of scope_ below for the
  deferred cost-tracking work.

### UX polish (v0.9 hotfix, shipped pre-tag)

These landed on master between PR 8 and the release branch to fix
issues caught during manual smoke-testing:

- **Command palette is driven by the live slash index.** Disabled
  plugins' slashes don't appear; newly added plugins show up without
  touching a static list anywhere.
- **`/view` and `/edit` open as centered modals** (popup, not
  full-bleed) and strip the `@` file-picker prefix from path args so
  `/view @src/main.rs` works as expected.
- **`/edit` and `/view` from the palette prefill the textarea**
  (`/view ` / `/edit ` with a trailing space) so the user can finish
  the path via the `@` file picker before submitting.
- **Ctrl-P opens the palette** (matching v0.8 muscle memory).
- **`/quit` is a plugin again** (`internal:quit`, Core) — restored as
  a first-class plugin contribution rather than an `App::handle_command`
  arm.
- **Disabling a plugin actually disables its slashes.** Legacy
  `App::handle_command` arms for `/clear`, `/save`, `/view`, `/edit`,
  and `/quit` are removed — those commands now route exclusively
  through the slash index, so toggling the owning plugin off removes
  the command from the surface.

### Behavior changes (potentially breaking)

- **New config file `~/.savvagent/plugins.toml`** (schema v1).
  Additive: missing-file = all defaults; existing installs upgrade
  without touching anything on disk until the user toggles something.
- **Splash + theme persistence files unchanged** (`splash.toml`,
  `theme.toml`).
- **`InputMode::SelectingTheme` (and the rest of `InputMode`) deleted.**
  Any out-of-tree consumer reading `App` internals would notice — no
  public API impact otherwise.

### Internal architecture

- **New crate:** `crates/savvagent-plugin/` (leaf, no host deps; only
  WIT-portable types + the `Plugin` / `Screen` traits).
- **Consolidation:** the v0.8 `crates/savvagent/src/{splash, palette,
  theme, providers}.rs` modules collapsed into
  `crates/savvagent/src/plugin/builtin/` per-plugin directories.
- **`App::handle_command` slimmed** to just the legacy `/connect` arm
  (every other slash routes through the slash index now).
- **New runtime modules under `crates/savvagent/src/plugin/`:**
  `registry`, `manifests` (slash / slot / screen indexes), `effects`
  (`apply_effects` + `dispatch_host_event`), `hooks`
  (`HookDispatcher`), `slash`, `keybindings`, `screen_stack`, `slots`,
  `convert`.

### Out of scope (deferred)

These are deliberately not in v0.9 and have follow-up issues:

- WASM Component-Model loader / `.wit` file (the trait surface is
  WIT-portable; no loader yet).
- Third-party plugin discovery + signing.
- Sidebar UI.
- Streaming-delta hooks (per-token `on_text_delta` etc.).
- Hot-reload of disabled plugins (toggle takes effect on next launch
  for some surfaces).
- Real session-wide token-usage tracking + cost. The `$0.00` in the
  status line is a placeholder until `TurnOutcome.usage` accumulation
  and a per-model pricing table land.
- `HostEvent::Disconnect` emission — the variant exists and dispatches
  correctly, but no current code path fires it.
- Ollama health-check before `/connect local` — currently builds the
  client unconditionally; PR 7 punted the check to a follow-up.

[0.9.0]: https://github.com/robhicks/savvagent-rs/releases/tag/v0.9.0
