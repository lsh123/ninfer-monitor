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
    pipeline, process_control, config, config_import, configs, chart,
    columns, kpi, requests, server_info, split, startup, window_state, gpu,
    nvidia_popup, update_check), the binary (`src/main.rs`) is the Slint GUI
    entry point.

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
│   ├── PRD-v0.2.md               # v0.2 PRD
│   └── PRD-v0.3.md               # v0.3 PRD
├── packaging/
│   └── nsis/
│       └── installer.nsi         # (M6) vendored cargo-packager NSIS template + NVIDIA profile install/uninstall hooks
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
    ├── process_control.rs        # (v0.3 M4) NInfer process control: command-line tokenization, program path, log-file determination
    ├── config.rs                 # (M11) JSON config load/save + defaults
    ├── config_import.rs          # (v0.3 M6) launcher-script config import: dialect-aware `ninfer-serve` invocation parsing (batch `.bat` / shell `.sh`), folder scan + name dedup
    ├── configs.rs                # (v0.3 M3) Control-tab domain logic: default configuration names, name/command-line validation
    ├── chart.rs                  # (M6) Plotters chart rendering
    ├── columns.rs                # (v0.2 M7) request-table column widths (drag dividers, persistence)
    ├── kpi.rs                    # (M7) KPI card formatting
    ├── requests.rs               # (M8) request table + detail formatting
    ├── server_info.rs            # (M9) server info panel formatting
    ├── split.rs                  # (FR-7.5) draggable split-panel fractions
    ├── startup.rs                # (v0.3 M2) startup-screen state (install-folder validity, monitored log path)
    ├── window_state.rs           # main-window geometry persistence/restore
    ├── gpu.rs                    # (v0.2 M4) NVML GPU polling + widget data (°F/°C)
    ├── nvidia_popup.rs           # (v0.2 M6) NVIDIA game-popup DRS app profile (NVAPI, Windows)
    ├── update_check.rs           # (v0.2 M8) update check: GitHub release lookup, semver comparison, due schedule, installer download + launch
    └── ui/
        ├── mod.rs
        ├── main_window.slint     # top-level Slint UI
        ├── about_panel.slint
        ├── confirm_delete_panel.slint # (v0.3 M3) delete-configuration confirmation dialog
        ├── control_panel.slint   # (v0.3 M3) Control tab (configurations list, Start/Restart/Stop/Add/Delete buttons, edit panel, status line)
        ├── dark_button.slint
        ├── gpu_row.slint         # (v0.2 M4) per-GPU row (2 metric widgets; usage title carries the GPU name)
        ├── metric_widget.slint
        ├── request_table.slint   # (v0.2 M7) request table (draggable column dividers in the header)
        ├── server_info_panel.slint
        ├── settings_panel.slint
        ├── splitter.slint
        ├── status_bar.slint
        ├── tab_button.slint      # (v0.3 M1) left tab-bar tab (icon + horizontal label, active highlight)
        ├── update_panel.slint    # (v0.2 M8) "New version available" dialog (OK downloads the installer and launches it, Windows only; Cancel also cancels a running download)
        └── icons/                # SVG icons (chevron-down/right, gear, open-folder, pause, play, check, update, power, pulse, info, add, delete, restart, stop) + app icon (app_icon.ico/.png/.svg)
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
`tests/snapshots/`. One snapshot per UI state (initial, live,
disconnected, cleared, server-restart, chart-window-1h, settings, about,
request-detail, gpu, gpu-celsius, table-columns, update, update-dialog,
control-tab, control-tab-error, control-tab-scrollbars,
control-delete-confirm, startup-invalid-folder).
The GPU snapshots inject
synthetic GPU data (the live NVML poll is not used, so they stay
deterministic).

- Compare mode (default): pixel-exact; on mismatch the test writes
  `tests/snapshots/<name>.actual.png` (gitignored) for visual diffing and
  reports the differing pixel count.
- Record mode: set `UPDATE_SNAPSHOTS` (any value) to (re)write the snapshots.
  Re-record whenever the UI is intentionally changed.
- Rendering is deterministic on a given machine (software rasterizer, mock
  time, system fonts), so snapshots are machine-specific — record them on the
  machine that runs the tests.

## Dialog styling (about / update)

The about and update dialogs share one style; follow it for any new dialog.

- **Panel chrome**: `background: #252526`, `border-radius: 6px`,
  `border-width: 1px`, `border-color: #3c3c3c`.
- **Backdrop**: full-window `Rectangle` with `background: #000000aa` and a
  `TouchArea` that closes the dialog on click; declared in `main_window.slint`
  before the panel.
- **Placement**: fixed width set in `main_window.slint` (about 380px,
  update 420px), centered with
  `x: (parent.width - self.width) / 2; y: (parent.height - self.height) / 2;`.
- **Height**: about and update hug their content — the panel root binds
  `height: <content-layout>.preferred-height;`. Do NOT bind to the child's
  actual `height` (binding loop) and do NOT set `cross-axis-alignment: start`
  on the content layout (children shrink to content width; controls must fill
  the dialog width).
- **Content layout**: `padding: 48px`, `spacing: 12px`.
- **Title**: plain `Text`, `font-size: 18px`, `font-weight: 700`,
  `color: #e0e0e0`. No close (×) button in the title — dialogs close via
  OK/Cancel, Escape, or the backdrop.
- **Group spacers**: `Rectangle { height: 16px; }` rows between logical
  groups: title → content, content → footer buttons.
- **Buttons**: `DarkButton` with `height: 24px`; footer buttons (OK/Cancel)
  `width: 90px`.
- **Focus & Escape**:
  - `click-blocker := TouchArea { width: parent.width; height: parent.height; }`
    as the panel's first child: catches clicks on empty space (focusing the
    scope) and stops them leaking through to the backdrop (which would close
    the dialog).
  - `scope := FocusScope` with a `KeyBinding` for `@keys(Escape)`.
  - A 1 ms `focus-timer` focuses the scope when the dialog opens (about,
    update); the Settings tab carries the same timer.
  - Settings' Escape is routed in `main_window.slint`: while the about dialog
    is open, Escape closes about first, then settings.
- **Tests** (`tests/ui.rs`): `ElementHandle::mock_single_click` hit-tests at
  the element's center — the topmost `TouchArea` there receives the event, and
  focus is only assigned when a `TouchArea` receives the press. Escape tests
  therefore click a non-interactive element inside the dialog (e.g.
  `SettingsPanel::log-poll-label`, `AboutPanel::about-texts`) so the click lands
  on the click-blocker, then `send_key(&window, '\u{1b}')`. Dimension tests
  assert the fixed width and a content-based height range
  (`about_panel_hugs_content_vertically`,
  `update_dialog_hugs_content_vertically`); the Settings tab is asserted to
  fill the tab content (`settings_panel_fills_tab_content`). Re-record
  screenshots after any intentional dialog change.

The **startup screen** is NOT a dialog: it is a full-window state (no
backdrop) with the same panel chrome and the same `FocusScope`/Escape/
`focus-timer` pattern — 560px wide, centered, height
`Math.max(content-layout.preferred-height, 200px)`, the app icon centered
above the welcome text, `Open Folder...` 110×24 with icon, and
`Start`/`Close` 90×24 footer buttons.

The **Settings tab** is NOT a dialog: it is a tab content area (it fills the
tab content — no backdrop, no fixed width) that reuses the same visual
language — `#252526` panel chrome, `Save`/`Discard` 90×24 footer buttons, and
Escape = Discard.

## Config & CLI (M11)

- Config file: `$HOME/.ninfer-monitor/config.json` (Windows: `%USERPROFILE%`),
  created on first run. Missing or malformed file falls back to defaults —
  the app never crashes on config errors; a malformed file is repaired
  (rewritten with defaults) at startup.
- Fields: `install_folder` (the NInfer installation folder, v0.3 M2;
  `None` until set — a blank stored value is normalized to `None` on load),
  `log_file_folder` (the base folder of the monitored log file
  `<log_file_folder>/ninfer-monitor/server.requests.jsonl`, default the OS
  temp dir, v0.3 M2 — a blank stored value is normalized to the default on
  load), `log_poll_interval_ms` (clamped to 100..=5000, default 500 — the
  log-file tailing interval; renamed from `poll_interval_ms` in v0.2, the
  old key is ignored), `gpu_poll_interval_ms` (clamped to 1000..=10000,
  default 5000 — the NVML poll interval, v0.2 M4), `chart_window_ms`
  (clamped to 60_000..=3_600_000, default 300_000), `max_requests` (clamped
  to 10..=1000, default 1000 — the request-table row cap, FR-5.5),
  `temp_unit` (`"F"`/`"C"`, default `"F"` — the GPU temperature display
   unit, v0.2 M4), `split_panels` (draggable split-panel fractions,
    FR-7.5), `control_split` (the Control-tab split: the configurations
    list's share of the middle area, clamped to 0.15..=0.7, default 0.5 —
    the edit panel takes the rest, v0.3 M3), `table_columns`
    (request-table column widths + the window width they were saved at,
    v0.2 M7), `window_state` (main-window geometry,
    optional), `update_check_enabled` (default `true` — the automatic update
    check, v0.2 M8), `update_check_period_days` (clamped to 1..=14, default
    3 — the update-check period), `last_update_check_unix_ms` (the time of
    the last successful check, recorded by the background checker; `None`
     until the first success), `configs` (the Control-tab configurations,
      v0.3 M3 — a JSON array of `{name, command_line}` objects; a missing or
      malformed array falls back to an empty list, and malformed entries are
      skipped), `running_config` (the name of the configuration the app
      started — persisted by a successful Start/Restart, cleared by Stop, a
      detected process exit, and deleting the running configuration; a blank
      or wrong-typed value falls back to `None`, v0.3 M4). Out-of-range
      stored values are clamped and
    written back at startup. A v0.2 `last_path` (last opened log) is
    migrated once at load: when it names `server.requests.jsonl` inside a
    `logs` directory, its grandparent directory becomes `install_folder`
    (only when `install_folder` is absent). `install_folder` is saved on
    startup Start and settings Save; `log_file_folder`,
    `log_poll_interval_ms`, `gpu_poll_interval_ms`, `max_requests`,
    `temp_unit`, `update_check_enabled`, and `update_check_period_days` on
     settings Save; `chart_window_ms` on window change; `table_columns` on
     column-divider release; `control_split` on Control-tab splitter
     release; `last_update_check_unix_ms` by the background
      update checker after a successful check; `configs` on Control-tab Add,
      Delete, and edit-panel apply; `running_config` by the process control
      (set on a successful Start/Restart, cleared on Stop, a detected
      process exit, and deleting the running configuration, and updated on a
      rename of the running configuration) — the only config writer that
    runs off the event-loop thread, so every config writer holds
    `config::write_lock()` across its load-modify-save to keep concurrent
    saves from losing each other's field changes. Saves are atomic (temp
    file + rename); a missing or wrong-typed field falls back to its
    default independently of the others.
- CLI: `--config <path>` / `--config=<path>` (overrides the config
  location); a positional argument is the NInfer installation folder
  (v0.3 M2) — it takes precedence over the configured `install_folder`, is
  not persisted unless Start/Save is used, and a valid folder skips the
  startup screen. Unknown flags are ignored.
- All tests use temp config locations (`tempfile`); no test may touch the
  user's real config.
- The startup Start and the Settings tab's Save replace the pipeline; the
  old pipeline is stopped on a background thread so its thread joins don't
  block the UI (NFR-2), and a re-click guard (`AtomicBool`) prevents
   stacked folder dialogs. The control handlers
   (`handle_startup_start`/`handle_startup_close`/`handle_settings_save`)
   are free functions so the tick/dialog callbacks stay thin and the logic
   is unit-testable.

## Process control (v0.3 M4)

`src/process_control.rs` (pure, no UI) holds the launch command-line logic:
`tokenize` (whitespace splitting with double-quoted groups and
backslash-escaped quotes), `program_path` (`<install_folder>/ninfer-serve`
with the OS-specific extension, reusing `startup::ninfer_serve_exe_name`),
and `determine_log_file` — scans the tokenized command line for
`--request-log-jsonl` (the `--request-log-jsonl <path>` and
`--request-log-jsonl=<path>` forms; the last occurrence with a value wins;
a dangling option or an empty `--request-log-jsonl=` is treated as absent,
and its tokens are dropped). If present, the path is resolved (a relative
path against the install folder, an absolute path as-is) and the arguments
are returned unchanged (not app-managed); if absent, `--request-log-jsonl
<log_file_folder>/ninfer-monitor/server.requests.jsonl` is appended
(app-managed).

The binary (`main.rs`) tracks the launched child processes per
configuration in `App::processes` (`HashMap<config id, TrackedProcess>`),
where `TrackedProcess` wraps the `std::process::Child`, a bounded combined
stdout/stderr buffer (the last 64 KiB), and the background reader threads:
Start spawns the program with the determined arguments (`current_dir` = the
install folder; stdout/stderr piped and drained in the background so the
child never blocks on a full pipe, PRD §4.4), creates the app-managed log
folder, deletes the previous app-managed log file (a failure does not block
the start), and switches the Monitor tab to the determined log file via
`switch_log_file`. A failed start (no install folder, an empty command
line, a missing executable, or a spawn error) sets the configuration's
`error` field and the Control-tab status line, and the row is marked red.
Stop kills and reaps the child and joins its readers; Restart = Stop +
Start. `poll_processes` runs on every UI tick (`on_tick`): a child that has
exited (crash, external kill, or Stop) is reaped and its configuration's
`running` flag cleared (the running state is per app instance, not
persisted); a failed exit sets the configuration's `error` field and the
status line to the exit code plus the captured console tail, and the row is
marked red; a `try_wait` error is treated as a failed exit. Deleting a
running configuration stops its process first. The name of the
configuration the app started is persisted as `running_config` (set on a
successful Start/Restart, cleared on Stop, a detected process exit, and
deleting the running configuration, and updated on a rename of the running
configuration); at startup (a valid configured or CLI installation folder)
and after the startup screen's Start, `restart_running_config` starts the
persisted configuration when it still exists (a case-insensitive name
match) and is not already running. On exit,
`stop_all_config_processes` kills and reaps every tracked server process
(releasing its GPU memory) without clearing `running_config`; on Windows
the child is spawned with `CREATE_NO_WINDOW`, so starting a configuration
does not open a console window (PRD §4.4).

## NVIDIA game popup (v0.2 M6, Windows)

`src/nvidia_popup.rs` disables the NVIDIA "game popup" (the overlay NVIDIA
shows the first time the driver sees a new executable) by registering a
per-executable DRS (driver settings) app profile via the NVAPI DRS interface.
A magic setting (DWORD `0x809D5F60 = 0x10000000`) in the profile suppresses
the popup for `ninfer-monitor.exe`.

- `nvapi64.dll` is loaded at run time only when a profile operation runs
  (NFR-1/NFR-4: the app must start on machines without the NVIDIA driver), so
  the executable has no load-time dependency on the driver. The `nvapi` crate
  contributes only its pure-Rust `Status` enum (used to name errors).
- **CLI** (exits before any GUI work, PRD v0.2 §4.2): `--add-nvidia-app-profile`
  prints `added` and exits 0 on success; `--delete-nvidia-app-profile` prints
  `removed` and exits 0 (a missing profile is a success, so it is idempotent).
  Errors print to stderr with a non-zero exit. The process attaches to the
  parent console so the output is visible in a terminal / the installer's
  `ExecWait`.
- **Settings link** ("Disable NVIDIA popup", Windows only): the callback runs
  on a background thread (the op can block on a UAC prompt) and calls
  `add_profile_interactive()`, which re-launches the app elevated (a UAC
  prompt) when this process is not elevated — the machine-wide (per-machine)
  DRS store requires administrator rights to write. The parent waits for the
  elevated copy's exit code and maps it to the success string ("NVIDIA game
  popup disabled.") or an error shown under the link.
- **Installer** (NSIS): the vendored template `packaging/nsis/installer.nsi`
  (a copy of cargo-packager's default with two hooks) runs
  `--add-nvidia-app-profile` at the end of install and
  `--delete-nvidia-app-profile` before uninstall. The per-machine installer
  runs elevated, so both write the system-wide (HKLM) profile for all users.
  The custom template is a full Handlebars-rendered replacement; plain NSIS
  lines (no `{{…}}`) pass through verbatim, and `!include "FileAssociation.nsh"`
  still resolves because cargo-packager runs makensis from the intermediates
  dir it wrote that file into.

### NVAPI quirk (learned the hard way)
The NVIDIA driver withholds `NvAPI_DRS_SaveSettings` (`nvapi_QueryInterface`
returns NULL for it) when the interface ID reaches `nvapi_QueryInterface` as a
**compile-time constant** (the compiler inlines the `const` id into the caller
as `mov ecx, imm32`). Resolving the id through a **run-time value** the
compiler cannot fold (parse the id table from a hex string into a heap
`Vec<u32>` and call `query(ids[i])`) makes SaveSettings resolve. The 12 DRS
interface ids are therefore stored as `INTERFACE_IDS_HEX` and parsed at run
time in `Nvapi::load()`. All `unsafe` FFI is isolated in this module with an
adjacent `// SAFETY:` comment per block (NFR-7).

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
- NVML (v0.2 M4): `nvml-wrapper` 0.13 + `nvml-wrapper-sys` 0.10 (cfg-gated
  Windows/Linux). `MemoryInfo` has no `used` field (compute `total - free`);
  `TemperatureSensor` only exposes `Gpu` (the `MEMORY` sensor is read via the
  isolated FFI block in `gpu.rs`); `TemperatureSensor` lives at
  `nvml_wrapper::enum_wrappers::device`.
- `windows` 0.62 (v0.2 M6 elevation): `SHELLEXECUTEINFOW` and `ShellExecuteExW`
  are gated behind the `Win32_System_Registry` feature (not `Win32_UI_Shell`);
  `GetTokenInformation` takes `Option<*mut c_void>` for the info buffer, and
  `OpenProcessToken`/`GetTokenInformation`/`CloseHandle` take a `HANDLE`.
  `windows::core::w!` accepts only string literals — encode runtime strings
  with `encode_utf16()` + a NUL and `PCWSTR(ptr)`.

## Log format (schema_version 21)

`tests/fixtures/server.requests.jsonl`: 120 lines, ~265 KB. One JSON object
per line. Common envelope: `artifact_type: "ninfer_serve_request_log"`,
`schema_version`, `server_instance_id`, `timestamp_unix_ms` (unix
milliseconds), `event`.

| Event | Count in sample | Carries |
|---|---|---|
| `server_start` | 1 | server/engine/GPU/memory config (once per server instance); the `artifact` object carries `name` (empty string when the artifact has no stored name), `architecture`, `formats`, `prefill_signature`, `device_object_count`, `host_object_count` (schema 20's `target`/`weights_id`/`tensor_count` are gone, and `engine.context_cost` swapped `model_id`/`weights_id` for `prefill_signature`) |
| `request_start` | 34 | `request_id`, model, sampling params, `preparation_seconds` |
| `request_done` | 32 | `result` (tokens, finish_reason, prefix hits), `timings_seconds` (ttft/prefill/decode/total), `engine_timing`, `speculative`, `materialization` (search fields such as `search_stop_phase`) |
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

- Rust edition 2024; `unsafe` is confined to isolated FFI areas, each with an
  adjacent `// SAFETY:` comment per block: `gpu.rs`
  (`gpu_memory_temperature`) reads the `NVML_TEMPERATURE_MEMORY` sensor (not
  exposed by the safe `nvml-wrapper` API) and reuses the already-loaded NVML
  library, returning `None` on any non-success code; `window_state.rs`
  enumerates Windows monitors (multi-monitor geometry restore);
  `nvidia_popup.rs` (per NFR-7) isolates all NVAPI/DRS FFI; and `main.rs`
  (`attach_parent_console`) attaches the NVIDIA profile CLI path to the
  parent console.
- `cargo clippy` and `cargo fmt --check` must stay clean.
- No code comments unless explicitly requested.
- Parsing must be tolerant: unknown events/fields ignored, malformed lines
  skipped and counted (PRD FR-2.3/FR-2.4).
- UI updates batched at ≤ 4 Hz; the charts (including the GPU charts)
  re-render on every UI tick in which the frame could change — new data, a
  panel size / chart window / temperature unit change, or the right edge (the
  current time) advancing a full second — so a pixel-identical frame is not
  re-rendered. A stale series is extended with a dashed horizontal tail from
  its last received point to the right edge (the current time), which a new
  value removes (PRD v0.2 M5).
- File tailing is polling-based (no `notify` dependency) for identical
  cross-platform behavior (PRD §11).
