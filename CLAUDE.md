# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Dredge is an ear-first practice looper for Linux: load a song, loop sections,
slow them down pitch-preserving, and drill passages with a live tempo trainer.
It ships
as a Tauri desktop app (Svelte 5 frontend, Rust host) and a headless daemon that
share the same Rust core. See `README.md` for the feature-level "what it does".

## Commands

Use `just` (the task runner) for everything; `just` alone lists recipes.

- `just dev` — desktop app in dev mode (vite hot-reload + debug Rust host)
- `just build` — release build of the desktop app (`target/release/dredge`) + headless daemon (`target/release/dredged`)
- `just run` / `just daemon` — run the release UI / headless daemon
- `just test` — full suite: `cargo test --workspace` + `pnpm vitest run`
- `just lint` — clippy (`-D warnings`), `cargo fmt --check`, `svelte-check`
- `just check` — pre-commit gate (test + lint)
- `just cmd '{"id":1,"cmd":"song.list"}'` — send a raw JSON command to a running instance over its Unix socket

Targeted runs (no recipe — invoke directly):
- Single Rust test: `cargo test -p practice store::tests::name_of_test`
- Single Rust crate: `cargo test -p engine`
- Single frontend test: `cd apps/desktop && pnpm vitest run lib/waveform-math.test.ts`

Frontend tooling lives under `apps/desktop` and uses **pnpm** (not npm).

## Architecture

### One dispatch surface, many clients

The entire backend is reached through a single command dispatcher,
`server::app::App` (`crates/server/src/app.rs`). Requests are
`{id, cmd, params}` JSON; responses are `{id, ok, data?, error?}`; pushed
state is `{event, data}`. Two transports wrap the *same* dispatcher:

- **Unix socket** (`crates/server/src/socket.rs`) — JSON-lines at
  `$XDG_RUNTIME_DIR/dredge.sock`. Used by the headless daemon (`dredged`)
  and by `just cmd` / shell scripts.
- **Tauri webview** (`apps/desktop/src-tauri/src/host.rs`) — one `dispatch`
  command in, one `dredge://event` channel out. The UI is "just another
  client".

There is exactly **one tick pump** (`server::socket::serve`, ~50 ms), regardless
of how many clients attach. The desktop passes an `on_events` hook so the same
pump that broadcasts to socket subscribers also mirrors ticks into the webview.

**Lock phasing:** known-heavy commands (`song.open`, `song.import`,
`capture.grab`) run their decode/hash/IO phase *outside* the `App` mutex via
`dispatch_shared`, so multi-second decodes never block the pump or other
clients. When adding a heavy command, follow the `*_phased` pattern in `app.rs`.

### The three crates

- **`engine`** (`crates/engine`) — real-time audio. Decode (symphonia),
  pitch-preserving stretch (Rubber Band R3 via `ffi.rs` + `stretch.rs`;
  the library is vendored in `crates/engine/vendor/rubberband` and compiled
  in by `build.rs` — see `VENDOR.md` there),
  sample-accurate crossfaded looping, PipeWire output and capture, filters
  (bass focus), waveform peaks. Audio thread talks to control thread over
  lock-free ring buffers (`ring.rs`, rtrb); commands/events flow through
  `pipeline.rs` (`EngineCmd`/`EngineEvent`).
- **`practice`** (`crates/practice`) — domain + persistence. `model.rs` holds
  the wire types (Song, Section, LoopRegion, Analysis…), `naming.rs` derives
  dynamic loop names from sections, `bundle.rs` defines the on-disk song bundle
  (`BundleManifest` + manifest/scan I/O), `library.rs` is the in-memory index
  over the bundle library (the source of truth for song data), and `store.rs`
  owns the small settings/profiles SQLite DB.
- **`server`** (`crates/server`) — the dispatcher + transports above, plus
  the bridges to external work: `analysis.rs`, `stems.rs`, `capture_control.rs`.

The desktop app (`apps/desktop/src-tauri`, binary name `dredge`) depends on all
three and embeds the built Svelte frontend.

### Persistence — song bundles are canonical

Each song is a **directory bundle** and the bundle is the source of truth.
Default library root is `<music dir>/dredge` (`dirs::audio_dir()/dredge`, e.g.
`~/Music/dredge`), overridable by the `library_root` setting. A bundle holds:

```
<library root>/<Artist — Title>/
  dredge.json      # BundleManifest: song, sections, loops, notes, analysis
  audio.<ext>      # the imported audio, copied in once on import
  stems/{vocals,drums,bass,other}.wav
```

Bundles are self-contained and portable: copy the folder to another machine and
dredge there loads the song with its stems, analysis, sections, loops, and notes
— no recomputation. `library.rs` scans the library into an in-memory index at
startup; every edit rewrites the affected `dredge.json` atomically (no save
button). On load, each manifest's audio path is rebased onto the actual bundle
dir, so a copied bundle resolves regardless of the origin machine's paths. IDs
are assigned at import and stored in the manifest. `song.import` copies the
source audio into a new bundle (the original is never touched again); dedup is by
content hash.

A small **SQLite DB** (rusqlite, bundled) at `~/.local/share/dredge/dredge.db`
holds *only* the `settings` and `profiles` tables — no song data. Schema is
embedded in `store.rs` (single `user_version` 1). App settings live in the
`settings` table as JSON; there are no TOML/JSON config files. Override the DB
path with `--db` (daemon) or `DREDGE_DB` (desktop).

### Frontend (Svelte 5 + Tauri)

`apps/desktop/src`. **All UI state derives from dispatch responses + events —
no second source of truth** (`lib/stores.ts` mirrors the wire shapes of
`server::app::App`). `lib/ipc.ts` is the main Tauri seam: `cmd()` sends a
request, `onEvent()` subscribes to the event channel; `lib/zoom.ts`,
`lib/window.ts`, `lib/file-picker.ts` and `lib/trace.ts` are the only other
(thin, intentional) Tauri wrappers — components never import `@tauri-apps/*`
directly. Pure logic (waveform math/hit-testing, fader math, zoom, colors,
time formatting, error normalization) lives in `lib/*.ts` with colocated
`*.test.ts` vitest files; `components/` are the Svelte views; `lib/ui/` is the
shared widget kit (`Box`, `Button`, `Fader`, `Modal`, `Group`, `Toolbar`,
`HoverActions`, `EmptyState`, `NumberField`).

**UI vocabulary** — names used in conversation and in code/CSS, so a spoken
term maps to one thing. The three columns are **panes**:

- **Library** (left, `aside.library`) — the song list.
- **Stage** (center, `main.stage`) — the work surface. Down it sit the
  **waveform**, the **transport control box** (`Transport.svelte`), then a flowing row
  of **control boxes** built on the `Box` widget (`lib/ui/Box.svelte`, a label header
  + body): the **isolation control box** (`Isolation.svelte`) — ways to hear less of
  the mix, from always-available **bass focus** (a low-pass) up to the separated
  **stem channels** (vocals/drums/bass/other) once analysis has run; before
  separation it carries the analyze CTA inline. Then the **notes control box**
  (`Notes.svelte`) — per-section notes (free text + inline `TabBlock`
  tablature), keyed by the section's occurrence label ("verse 2") and shown once
  a song has sections; the **tuner control box** (`Tuner.svelte`, always present); and
  the **drill control box** (`Drill.svelte`) last, only while a drill span is active.
  Call them *control boxes*, never "containers", "panels", or bare "boxes".
- **Dock** (right, `aside.panels`) — a vertical stack of resizable **panels**,
  each holding multiple **tabs** (structure, loops, routines, export, profile,
  devices, settings, guide), wired through the `TAB_VIEWS` registry in
  `App.svelte`. A tab can be dragged to reorder within its panel, joined into
  another panel (drop on its tab bar), or split into a new panel (drop on a panel
  body, top/bottom half); splitters between panels resize them. The whole
  arrangement is the **dock layout** — a `DockLayout` (`Panel[]`, each
  `{tabs, active, weight}`) whose pure transforms live in `lib/dock.ts`; the
  component only renders it and reads drag gestures. It persists as the
  `panel_layout` setting (which superseded the old flat `tab_order`) and is
  reconciled against the known tabs on load. The default is one panel holding
  every tab. The **structure tab** (`Sections.svelte`) owns song structure;
  there is no longer a center "structure box".

Some stage state is purely client-side and mirrored by no store — the
waveform's zoom (`view`) and clicked active span live as local `$state` in
`Waveform.svelte`. The **reset workspace** control (⟲ in the transport control box)
refits the zoom and clears that local state plus the selection, active loop, and
playhead; because the action (`resetWorkspace` in `stores.ts`) can't reach the
local state directly, it bumps the `workspaceReset` nonce store that the
waveform watches.

### External analysis & stems (Python)

Beat/downbeat/section analysis and Demucs stem separation run as out-of-process
Python, invoked through repo-shipped wrappers in `scripts/` (`analyze`,
`analyze_impl.py`, `songformer_impl.py`). The `analyze` wrapper **bootstraps its
own `uv` venv on first run** (downloads torch etc.) at
`~/.local/share/dredge/analyze-venv` (override `$DREDGE_ANALYZE_VENV`).
Contract: **stdout carries exactly one JSON object; all diagnostics go to
stderr.** Swapping models stays entirely in Python — the Rust side
(`server::analysis`, `server::stems`) only parses the JSON, so never let
non-JSON leak to stdout.

## Conventions

- The `time` crate is pinned `>=0.3, <0.3.48` (a regression breaks tauri-utils);
  don't bump past it.
- Errors crossing the protocol boundary collapse to the `error: String` channel
  via the `ErrStr`/`err_str` helper in `app.rs`.
