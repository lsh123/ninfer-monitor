# AGENTS.md

Guidance for AI coding agents (and humans) working in this repository.

## Project

NInferMonitor — a cross-platform desktop app (Rust + Slint + Plotters) that tails
the NInfer server request log (`server.requests.jsonl`, JSON Lines) in real time
(`tail -f` semantics) and displays core metrics — token throughput, TTFT,
latency, KV-cache occupancy, scheduler state, per-request details — in a GUI
dashboard.

- Single-package layout: the crate (package name `ninfer-monitor`, Rust
  edition 2024) lives at the repo root as a lib + binary pair: the library
  (`src/lib.rs`) holds the testable modules (tail, parser, store, metrics,
  pipeline, config, chart, kpi, requests, server_info, split, window_state),
  the binary (`src/main.rs`) is the Slint GUI entry point.

## Repo layout

```
NInferMonitor/
├── .gitignore
├── AGENTS.md
├── Cargo.toml                    # package `ninfer-monitor` (lib + binary) + [package.metadata.packager] installer config
├── Cargo.lock
├── LICENSE.md                    # copyright / license text
├── README.md
├── build.rs                      # slint-build compiles src/ui/*.slint; winres embeds the app icon + version on Windows
├── vs-cargo.bat                  # MSVC env wrapper: cmd /c vs-cargo.bat build
├── docs/
│   ├── PRD-v0.1.md               # v0.1 PRD (FR/NFR, milestones M1–M12)
│   └── PRD-v0.2.md               # v0.2 PRD
├── tests/                        # integration tests (public API)
│   ├── fixtures/
│   │   └── server.requests.jsonl # sample log (test fixture)
│   ├── snapshots/                # recorded screenshot PNGs
│   ├── chart.rs
│   ├── kpi.rs
│   ├── parser.rs
│   ├── pipeline.rs
│   ├── screenshot.rs             # headless UI snapshot tests
│   ├── status_bar.rs
│   ├── store.rs
│   ├── tail.rs
│   └── ui.rs
└── src/
    ├── main.rs                   # binary entry point, include_modules!, event loop
    ├── lib.rs                    # library root
    ├── tail.rs                   # (M2) file tailing
    ├── parser.rs                 # (M3) JSONL -> typed events
    ├── store.rs                  # (M4) in-memory state
    ├── metrics.rs                # (M4) derived metrics
    ├── pipeline.rs               # (M5) tail -> parse -> store, 4 Hz snapshots
    ├── config.rs                 # (M11) JSON config load/save + defaults
    ├── chart.rs                  # (M6) Plotters chart rendering
    ├── kpi.rs                    # (M7) KPI card formatting
    ├── requests.rs               # (M8) request table + detail formatting
    ├── server_info.rs            # (M9) server info panel formatting
    ├── split.rs                  # (FR-7.5) draggable split-panel fractions
    ├── window_state.rs           # main-window geometry persistence/restore
    └── ui/
        ├── mod.rs
        ├── main_window.slint     # top-level Slint UI
        ├── about_panel.slint
        ├── dark_button.slint
        ├── metric_widget.slint
        ├── request_table.slint
        ├── server_info_panel.slint
        ├── settings_panel.slint
        ├── splitter.slint
        ├── status_bar.slint
        └── icons/                # SVG icons (chevron-down/right, gear, open_folder, pause, play, trash) + app icon (app_icon.ico/.png/.svg)
```

## Copyright notice

Every source file (`.rs`, `.slint`, and the `vs-cargo.bat` wrapper) begins with
the copyright notice below. **Add it to every new file you create.** Use the
comment syntax for the file's language — the text is identical in each:

- Rust (`.rs`): `//` line comments.
- Slint (`.slint`): `//` line comments.

Do **not** use the C-style `/** … */` form: it breaks `main.rs` (a doc comment
cannot precede the `#![windows_subsystem]` inner attribute) and is invalid in
`.bat`.

```
// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.
```

Do not change LICENSE.md or the existing Copyright notices in the source code
unless explicitly instructed by the user.


## Build environment (Windows)

- Rust: stable 1.95.0 (`cargo`/`rustc` on PATH).
- **Slint compiles a C++ core, so MSVC is required on Windows.** `cl.exe` is
  NOT on PATH by default. MSVC 14.44.35207 is installed via Visual Studio 2022
  Community.
- Run all cargo commands that compile the crate inside the VS developer
  environment:

```bat
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"
cargo build
```

- Reusable wrapper (drop a `vs-cargo.bat` next to the repo or in a temp dir):

```bat
@echo off
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" >nul
cargo %*
```

  then invoke as `cmd /c vs-cargo.bat build`.
- `cargo fmt` / `cargo fmt --check` do not need MSVC.

## Commands

| Task | Command (inside vcvars64 env) |
|---|---|
| Build | `cargo build` |
| Run | `cargo run` |
| Tests | `cargo test` |
| Screenshot tests | `cargo test --test screenshot` |
| Record screenshots | `$env:UPDATE_SNAPSHOTS=1; cargo test --test screenshot` |
| Lint | `cargo clippy --all-targets` |
| Format check | `cargo fmt --check` |
| Installer (NSIS) | `cmd /c vs-cargo.bat packager --release` |

## Installers (cargo-packager)

Installers are built with [cargo-packager](https://crates.io/crates/cargo-packager)
(a cargo subcommand, `cargo install cargo-packager --locked` one-time setup).
Configuration lives in the `[package.metadata.packager]` table in `Cargo.toml`
(product name, identifier, `before-packaging-command` build hook,
`binaries-dir = "target/release"`, `out-dir = "target/packager"`).

- Build: `cmd /c vs-cargo.bat packager --release` (Windows). The
  `before-packaging-command` hook runs `cargo build --release` first, so the
  MSVC env must be inherited — use the `vs-cargo.bat` wrapper (or a VS
  developer prompt). On other platforms: `cargo packager --release`.
- Output: `target/packager/ninfer-monitor_<version>_x64-setup.exe` — an NSIS
  installer (NSIS is the platform default format on Windows).
- **NSIS is NOT taken from the system.** cargo-packager downloads its own
  NSIS 3.09 toolchain + plugins into `%LOCALAPPDATA%\.cargo-packager\NSIS`
  (Windows) on first run — network access is required for the first
  packaging. A system NSIS install (e.g. `C:\Program Files (x86)\NSIS`) is
  not used by cargo-packager.
- Other formats: set `formats` in the packager config, e.g.
  `formats = ["wix"]` for an MSI (the WiX 3.11 toolset is auto-downloaded to
  the same cache dir). Full config reference:
  https://docs.rs/cargo-packager/latest/cargo_packager/config/struct.Config.html

## Screenshot tests

`tests/screenshot.rs` renders `MainWindow` headlessly (Slint testing backend
with the software rasterizer, mock time, fixed 1280x800) via
`Window::take_snapshot()` and compares the pixels against recorded PNGs in
`tests/snapshots/`. One snapshot per UI state (initial, live, paused,
disconnected, cleared, server-restart, chart-window-1h, settings, about,
request-detail).

- Compare mode (default): pixel-exact; on mismatch the test writes
  `tests/snapshots/<name>.actual.png` (gitignored) for visual diffing and
  reports the differing pixel count.
- Record mode: set `UPDATE_SNAPSHOTS` (any value) to (re)write the snapshots.
  Re-record whenever the UI is intentionally changed.
- Rendering is deterministic on a given machine (software rasterizer, mock
  time, system fonts), so snapshots are machine-specific — record them on the
  machine that runs the tests.

## Config & CLI (M11)

- Config file: `$HOME/.ninfer-monitor/config.json` (Windows: `%USERPROFILE%`),
  created on first run. Missing or malformed file falls back to defaults —
  the app never crashes on config errors; a malformed file is repaired
  (rewritten with defaults) at startup.
- Fields: `last_path` (last opened log), `poll_interval_ms` (clamped to
  100..=5000, default 500), `chart_window_ms` (clamped to 60_000..=3_600_000,
  default 300_000), `max_requests` (clamped to 10..=1000, default 1000 — the
  request-table row cap, FR-5.5), `split_panels` (draggable split-panel
  fractions, FR-7.5), `window_state` (main-window geometry, optional).
  Out-of-range stored values are clamped and
  written back at startup; CLI overrides (positional path, `--poll`) are
  one-shot and not persisted. `last_path` is saved on "Open file…";
  `poll_interval_ms` and `max_requests` on settings "OK"; `chart_window_ms`
  on window change. Saves are atomic (temp file + rename); a missing or
  wrong-typed field falls back to its default independently of the others.
- CLI: positional log path, `--poll <ms>` / `--poll=<ms>` (clamped; a
  missing or non-numeric value warns on stderr and is ignored),
  `--config <path>` / `--config=<path>` (overrides the config location).
  Precedence: CLI path > config `last_path`, CLI `--poll` > config
  `poll_interval_ms`.
- All tests use temp config locations (`tempfile`); no test may touch the
  user's real config.
- "Clear" resets the parse-thread store via a command channel
  (`Pipeline::clear`); the resulting snapshot carries a bumped `clear_count`
  so the UI can skip pre-clear snapshots (no flicker; the decision lives in
  the testable `pipeline::clear_gate`). "Pause" freezes the UI only — the
  tail thread keeps reading, and the tick keeps draining the update channel
  to bound memory; the last tail status is remembered while paused and
  restored on resume.
- "Open file…" replaces the pipeline; the old pipeline is stopped on a
  background thread so its thread joins don't block the UI (NFR-2), and a
  re-click guard (`AtomicBool`) prevents stacked file dialogs. The control
  handlers (`handle_clear`/`handle_open_file`/`handle_window_changed`) are
  free functions so the tick/dialog callbacks stay thin and the logic is
  unit-testable.

## Toolchain quirks (learned the hard way)

- `cargo add <crate> --build` adds a build-dependency on cargo 1.95
  (`--build-dependency` is rejected as an unknown flag).
- Slint 1.17.x: UI is `.slint` files compiled by `slint-build` in `build.rs`
  (`slint_build::compile_with_config("src/ui/main_window.slint", config)` —
  single path, debug info enabled in debug builds, returns
  `Result<(), slint_build::CompileError>`); on Windows it also embeds the app
  icon and version metadata via `winres`. The generated component is pulled
  in with `slint::include_modules!();` in `main.rs` and run via
  `MainWindow::new()?.run()?` (`run()` = show + event loop).
- The `slint!` macro takes **inline Slint code, not a file path** — passing a
  string literal produces "Parse error: expected a top-level item".
- `slint::EventLoop` does not exist in 1.17; use `ComponentHandle::run()` or
  `slint::run_event_loop()`.
- On Rust 1.88+, paths inside the `slint!` macro (e.g. `@image-url`, `.slint`
  imports) resolve relative to the `.rs` file containing the macro, not the
  crate root.
- `#![windows_subsystem = "windows"]` in `main.rs` is required so Windows does not
  allocate a console window next to the GUI (no stdout/stderr console once
  set — use a log file or `OutputDebugString` for diagnostics).
- `std::os::windows::fs::MetadataExt::file_index()` / `volume_serial_number()`
  are unstable (feature `windows_by_handle`). File identity for
  rotation detection uses `Metadata::created()` on Windows and `(dev, ino)`
  on Unix.
- `std::sync::mpsc::Sender` has no `is_closed()`; stop a channel-producing
  thread with an `Arc<AtomicBool>` flag.
- First build compiles ~600 crates including Slint's C++ core — allow several
  minutes.

## Log format (schema_version 20)

`tests/fixtures/server.requests.jsonl`: 120 lines, ~259 KB. One JSON object
per line. Common envelope: `artifact_type: "ninfer_serve_request_log"`,
`schema_version`, `server_instance_id`, `timestamp_unix_ms` (unix
milliseconds), `event`.

| Event | Count in sample | Carries |
|---|---|---|
| `server_start` | 1 | server/engine/GPU/memory config (once per server instance) |
| `request_start` | 34 | `request_id`, model, sampling params, `preparation_seconds` |
| `request_done` | 32 | `result` (tokens, finish_reason, prefix hits), `timings_seconds` (ttft/prefill/decode/total), `engine_timing`, `speculative` |
| `throughput` | 51 | periodic engine stats every ~5 s (`engine.log_stats_interval_ms`): `throughput_tokens_per_second.{decode,prefill}`, `scheduler`, `context_cache.occupancy`, `host_work` |
| `request_error` | 2 | `error.message` |

- `finish_reason` values seen: `stop_token`.
- `error.message` values seen: `inference request expired while waiting for
  admission`, `client disconnected`.
- Key metric fields: `throughput_tokens_per_second.{decode,prefill}`,
  `timings_seconds.ttft`,
  `result.{prompt_tokens,completion_tokens,model_thinking_tokens,prefix_cache_hit_tokens}`,
  `speculative.{drafted_tokens,accepted_tokens}`, `context_cache.occupancy.*`,
  `scheduler.{running,prefilling,waiting}`.
- Reference values for tests (request_id 1): `prompt_tokens=645`,
  `completion_tokens=155`, `ttft=0.4219088`, `speculative.accepted_tokens=109`,
  `speculative.drafted_tokens=141`. First `throughput` event in the sample:
  decode 53.71620426468579 tok/s, prefill 2461.725450667429 tok/s. Last
  `throughput` event: decode 123.89240590071061 tok/s, prefill 0.0 tok/s.

## Conventions

- Rust edition 2024; no `unsafe` code.
- `cargo clippy` and `cargo fmt --check` must stay clean.
- No code comments unless explicitly requested.
- Parsing must be tolerant: unknown events/fields ignored, malformed lines
  skipped and counted (PRD FR-2.3/FR-2.4).
- UI updates batched at ≤ 4 Hz; charts re-render at 2 Hz (PRD §11).
- File tailing is polling-based (no `notify` dependency) for identical
  cross-platform behavior (PRD §11).
