# NInferMonitor — Product Requirements Document

| | |
|---|---|
| **Version** | v0.2 |
| **Status** | Draft |
| **Date** | 2026-09-20 |
| **Stack** | Rust, Slint (UI), Plotters (charts), NVML API |
| **Platforms** | Windows, Linux, macOS |

---

## 1. Overview

NInferMonitor is a cross-platform desktop application that tails the NInfer server
request log (`server.requests.jsonl`, JSON Lines format) in real time — similar to
`tail -f` — and presents the core operational details of a running NInfer inference
server in a graphical dashboard: token throughput, request latency, TTFT, KV-cache
occupancy, scheduler state, and request-level details. This version builds
on v0.1 (see PRD-v0.1.md for more details).

## 2. Major changes in this version
- New "startup" screen to allow the user to enter the log file location (the
  main window showing the startup content; skipped when a valid file path is
  passed on the command line)
- Integration with NVML API and new widgets to display the hardware information
  from the NVIDIA GPU (the GPU section is hidden when NVML is unavailable)
- Improve all graph widgets to show (near) real-time data
- Cleanup in the main UI interface: the toolbar now contains only the window
  selector and the settings button (the Open file…, pause/play, and clear
  buttons are removed; file selection moves to the startup screen and the
  settings dialog)
- New 30m chart-window option (the chart time-window selector now offers
  1m / 5m / 15m / 30m / 1h)
- The server-info panel drops the GPU and Memory rows (and the GPU header
  line) that were present in v0.1
- New temperature unit selector in the settings dialog (°F / °C, default °F)
- The v0.1 settings "Poll interval" is renamed to "Log file poll interval"
  (it controls the log-file tailing frequency) and a new "GPU poll interval"
  setting is added (1 s .. 10 s, step 1 s, default 5 s) that controls the
  NVML polling frequency
- KPI values carry their unit next to the number instead of in the widget
  title: the latency widget values (TTFT, per-token latency) show an adaptive
  unit (ms / s / min by magnitude), the GPU temperature KPIs show the active
  unit (°F / °C), and the percentage KPIs (Cache hit, Spec. accept, GPU
  usage) show %
- New automatic update checks: the app checks the releases published at
  https://github.com/lsh123/ninfer-monitor for a newer version (on startup and
  at a configurable interval of 1 day .. 14 days, step 1 day, default 3 days;
  the check is enabled by default; both the check on/off and the interval are
  configured in the settings dialog, see §5); when a newer release is found,
  an update button with a new downward-arrow icon appears to the left of the
  settings button and opens a dialog with the new version and a link to the
  release notes

Note: the purge of the pause/play/clear code, the pause.svg / play.svg /
trash.svg icons, and the paused.png test snapshot is required by this version
but is still pending.

## 3. UX layout

### 3.1. Startup screen
The startup screen is shown when the log file location is not configured, or the
file doesn't exist or can't be read in the configured location. It is the main
window showing the startup content (the same window, not a separate dialog);
pressing Start switches the window to the main dashboard content. If a valid
log file path is passed as a command-line argument (the file exists and is
readable), the startup screen is skipped and the main dashboard is shown
directly:

```
┌─────────────────────────────────────────────┐
│                                             │
│            Welcome to NInferMonitor (v0.2)! │
│ [app icon] <Text message>                   │
│                                             │
│ ┌─────────────────────────┐                 │
│ │ <current file if any>   │ [Open File...]  │
│ └─────────────────────────┘                 │
│                                             │
│              [Start]  [Close]               │
└─────────────────────────────────────────────┘
```
- The app icon should be shown centered above the welcome text.
- The version (e.g. v0.2) in the welcome text should be generated at build time and
  NOT hardcoded. Increase the welcome screen's font sizes and bold the title.
- The text message should be different for no log file configured ("Please select
  NInfer log file (e.g. 'server.requests.jsonl') to start monitoring.") vs. when the
  file is configured but doesn't exist / can't be read / etc ("The currently selected
  file cannot be read, please ensure NInfer is configured correctly and is running;
  or select another file to start monitoring.")
- The text box should show the current file and allow the user to edit it (or
  enter a new filename).
- The Open File button should open the file selection dialog that populates the text
  box with the filename.
- The Start / Close buttons should be centered. The Start button should only be
  enabled when a valid file (one that exists and is readable) is selected. The
  file must exist and be readable at the time the Start button is pressed; the
  app does not wait for a file that appears later (no retry or auto-enable on
  the startup screen).
- When the Start button is pressed, the startup screen is dismissed and the main
  dashboard content is shown in the same window.
- When the Close button (or the escape key) is pressed, the startup screen is
  dismissed and the application exits.
- All buttons in the dialog should have the same height as the other buttons in the project.
- The dialog size should fit all inner elements but be at least 300x200 px.


### 3.2. Main screen

The biggest changes are:
- Cleaned up the button toolbar: removed the Open file…, pause/play, and clear
  buttons (file selection moves to the startup screen and the settings dialog)
  --> purge all the related code and icons! (Note: the purge of the
  pause/play/clear code, the pause.svg / play.svg / trash.svg icons, and the
  paused.png test snapshot is still pending as of this revision.)
- Added GPU usage / memory and GPU temperature widgets.
- Added an update button (a new downward-arrow icon) to the right side of the
  toolbar, immediately to the left of the settings button; it is shown only
  when a new release is available and opens the update dialog (see §5).

```
┌─────────────────────────────────────────────────────────────────────────────────────┐
│ [1m] [5m] [15m] [30m] [1h]                                                  [↓] [⚙] │
├─────────────────────────────────────────────────────────────────────────────────────┤
│ ┌ GPU0 (NVIDIA GeForce RTX 5090): usage / memory ─────┐ ┌ GPU0 temperature ───────┐ │
│ │ <GPU usage/memory widget>                           │ │ <GPU temperature widget>│ │
│ └─────────────────────────────────────────────────────┘ └─────────────────────────┘ │
│ ┌ Throughput (tok/s) ─────────────────┐ ┌ Latency ─────────────────────────────────┐ │
│ │ <throughput widget, see below>      │ │ <latency widget, see below>             │ │
│ └─────────────────────────────────────┘ └─────────────────────────────────────────┘ │
│ ┌ Cache ──────────────────────────────┐ ┌ Scheduler ──────────────────────────────┐ │
│ │ <cache widget, see below>           │ │ <scheduler widget, see below>           │ │
│ └─────────────────────────────────────┘ └─────────────────────────────────────────┘ │
├─────────────────────────────────────────────────────────────────────────────────────┤
│   #   Time          Model          Prompt Compl Think  TTFT   Total  Reason  Status │
│ > 66  14:02:58.124  qwen3.8-27b-…  42737  —     —      —      —      —       error  │
│ > 65  14:02:31.001  qwen3.8-27b-…  9589   115   —      310 ms 3.9 s  stop    done   │
│ > 64  14:02:12.556  qwen3.8-27b-…  645    155   —      422 ms 3.9 s  stop    done   │
├─────────────────────────────────────────────────────────────────────────────────────┤
│ ┌ Server info ────────────────────────────────────────────────────────────────────┐ │
│ │ Model    qwen3_8_27b (nvfp4) · 22.1 GiB · load 9.7 s                            │ │
│ │ Engine   KV fp8-e4m3-row256 · capacity 240 000 · max context 240 000 · …        │ │
│ │ Server   0.0.0.0:8080 · serve-27872-… · log d:/NInfer/logs/…                    │ │
│ └─────────────────────────────────────────────────────────────────────────────────┘ │
├─────────────────────────────────────────────────────────────────────────────────────┤
│ ● Live  14:02:58.124  Log file: d:/NInfer/logs/….requests.jsonl Errors: 0 Skipped 0 │
└─────────────────────────────────────────────────────────────────────────────────────┘
```

### GPU usage / memory widget

The GPU usage / memory widget has the current GPU % usage and % memory usage; plus
the historical values (same as other widgets). The widget title carries the GPU name
(e.g. "GPU0 (NVIDIA GeForce RTX 5090): usage / memory"), so there is no separate GPU
header row above the widgets. The usage KPI value shows a % unit label next to the
number. Note that memory usage should be reported as both an
absolute value (e.g. 512 MB) and as a percentage of total memory (e.g. 95%). However,
the historical line graph in the middle should only show the percentage. Refreshed
based on the GPU poll interval. GPU KPIs show no trend arrows (no up/down indicators).

```
┌───GPU0 (NVIDIA GeForce RTX 5090): usage / memory ────────────────┐
│                   ┌─────────────────────────┐                    │
│   GPU0 Usage  100 │  <dual-axis line graph> │ 100  GPU Memory    │
│    98 %         50 │                        │ 50  512MB (95%)    │
│                   └─────────────────────────┘                    │
└──────────────────────────────────────────────────────────────────┘
```

### GPU temperature widget

The GPU temperature widget has the current GPU temperature; plus the historical values
(same as other widgets). The temperature unit is selectable in the settings dialog
(°F / °C, default °F) and the active unit is shown as a label next to the Core and
VRAM KPI values (the widget title is e.g. "GPU0 temperature"); when a value is NA
there is no unit label; the y-axis scale is automatic. The VRAM KPI uses the
NVML_TEMPERATURE_MEMORY sensor and the Core KPI uses the NVML_TEMPERATURE_GPU sensor.
When the driver does not report a memory temperature (e.g. some GeForce cards), the
VRAM KPI shows NA and the chart omits the VRAM series. Refreshed based on the GPU
poll interval. GPU KPIs show no trend arrows (no up/down indicators).

```
┌───GPU0 temperature ───────────────────────────────────────────┐
│                   ┌─────────────────────────┐                 │
│   Core        100 │  <dual-axis line graph> │ 100  VRAM       │
│    98 °F       50 │                         │ 50    95 °F     │
│                   └─────────────────────────┘                 │
└───────────────────────────────────────────────────────────────┘
```

### Multiple NVIDIA GPUs

If there are multiple NVIDIA GPUs available, then the main window will have rows of
widgets per GPU for ALL detected NVIDIA GPUs. All widget rows in the charts section
(each GPU row and the two metric rows) have the same height. If NVML is unavailable
(no NVIDIA GPU, no NVIDIA driver, or a platform without NVML support such as macOS),
the entire GPU section (all GPU rows and widgets) is hidden and the rest of
the dashboard works normally:

```
│ ┌ GPU0 (NVIDIA GeForce RTX 5090): usage / memory ─────┐ ┌ GPU0 temperature ───────┐ │
│ │ <GPU usage/memory widget>                           │ │ <GPU temperature widget>│ │
│ └─────────────────────────────────────────────────────┘ └─────────────────────────┘ │
│ ┌ GPU1 (NVIDIA GeForce RTX 5090): usage / memory ─────┐ ┌ GPU1 temperature ───────┐ │
│ │ <GPU usage/memory widget>                           │ │ <GPU temperature widget>│ │
│ └─────────────────────────────────────────────────────┘ └─────────────────────────┘ │

...
```

### Throughput widget

No changes from v0.1


```
┌───Throughput (tok/s) ────────────────────────────────────────────┐
│                   ┌─────────────────────────┐                    │
│   Prefill      2k │  <dual-axis line graph> │ 200     Decode     │
│    1461 ▲      1k │                         │ 100     153.7 ▲    │
│                   └─────────────────────────┘                    │
└──────────────────────────────────────────────────────────────────┘
```

### Latency widget


The TTFT and per-token latency KPI values show an adaptive unit label next to the
number, chosen by magnitude: milliseconds (one decimal below 10 ms, whole below
1 s), seconds (one decimal, below 1 min), or minutes (one decimal, from 1 min up);
the widget title no longer hardcodes the unit (it is "Latency", not "Latency
(ms)"). When there is no data, no unit label is shown.


```
┌────Latency ──────────────────────────────────────────────────────┐
│                   ┌─────────────────────────┐                    │
│   TTFT        500 │  <dual-axis line graph> │ 10   Per-Token lat │
│   422 ms ▼    250 │                         │ 5     8.9 ms ▼     │
│                   └─────────────────────────┘                    │
└──────────────────────────────────────────────────────────────────┘
```

### Cache widget


The Cache hit and Spec. accept KPI values show a % unit label next to the number;
the widget title is "Cache" (the unit is no longer in the title).

```
┌───Cache ─────────────────────────────────────────────────────────┐
│                   ┌─────────────────────────┐                    │
│   Cache hit   50% │  <dual-axis line graph> │ 100%  Spec. accept │
│   22.7% ▲     25% │                         │ 50%     77% ▲      │
│                   └─────────────────────────┘                    │
└──────────────────────────────────────────────────────────────────┘
```

### Scheduler widget

No changes from v0.1

```
┌───Scheduler ─────────────────────────────────────────────────────┐
│                   ┌─────────────────────────┐                    │
│   Active        5 │  <dual-axis line graph> │ 5     Waiting      │
│    1 ▼          2 │                         │ 2        2 ▲       │
│                   └─────────────────────────┘                    │
└──────────────────────────────────────────────────────────────────┘
```

### Status bar


No changes from v0.1, except the Paused (yellow) state is removed (the pause
button is removed in v0.2).


The status bar is located at the bottom of the window and has a fixed height.

```
│ ● Live  14:02:58.124  Log file: d:/NInfer/logs/….requests.jsonl Errors: 0 Skipped 0 │
```

### Settings dialog

The modal settings dialog should be opened when the Settings button is clicked. The
file selector is a new element in v0.2; the log file poll interval and max requests
sliders are unchanged from v0.1 (the v0.1 "Poll interval" is renamed to "Log file
poll interval"); a new GPU poll interval slider is added; a new "Check for
updates" checkbox and a "Check interval" slider are added for the automatic
update checks (see §5).

```
┌────────────────────────────────────────────┐
│ Settings                                 x │
├────────────────────────────────────────────┤
│ ┌─────────────────────────┐                │
│ │ <current file if any>   │ [Open File...] │
│ └─────────────────────────┘                │
├────────────────────────────────────────────┤
│ Log file poll interval: 500 ms             │
│ 100 ms ----*-------------- 5 s             │
│ GPU poll interval: 5 s                     │
│ 1 s -------*------------ 10 s              │
│ Max requests: 1000                         │
│ 10 -------*-------------- 1000             │
│ Temperature unit: °F                       │
│                                            │
│ [x] Check for updates                      │
│ Check interval: 3 days                     │
│ 1 day ----*------------ 14 days            │
│                                            │
│ _Disable NVIDIA popup_                     │
│ _About_                                    │
├────────────────────────────────────────────┤
│                [OK] [Cancel]               │
└────────────────────────────────────────────┘
```

The log file text box should show the current file and allow the user to edit it (or
enter a new filename). The Open File button should open the file selection dialog that
populates the text box with the filename. On OK, the app switches the tail to the
selected file: it reopens the file, backfills its contents, and resets the in-memory
state (KPIs, charts, request table). If the selected file does not exist or cannot be
read, the app still switches to it and shows the Disconnected state, retrying until
the file becomes readable.

Below the log file poll interval there is a GPU poll interval slider (1 s .. 10 s,
step 1 s, default 5 s) that controls the frequency of the NVML polling (see §4.1).
It is persisted to the config as `gpu_poll_interval_ms` and applied to the running
GPU poller immediately on OK (no tail restart).

Below the max requests setting there is a temperature unit selector (°F / °C,
default °F) used by the GPU temperature widget (the selected unit is shown as a
label next to the Core and VRAM KPI values).

Below the temperature unit selector there is a "Check for updates" checkbox
(default on) that enables or disables the automatic update checks, and a
"Check interval" slider (1 day .. 14 days, step 1 day, default 3 days) that
sets how often the app checks for a new release; the slider is disabled while
the check is off. Both are persisted to the config as `update_check_enabled`
and `update_check_period_days` and applied on OK (see §5).

The new _Disable NVIDIA popup_ link is shown only on Windows (hidden on Linux and
macOS). It should trigger the profile installation to disable the NVIDIA game popup
(see below). On success the exact string "NVIDIA game popup disabled." is displayed
right under this link; any error from this process is displayed there as well.


No changes from v0.1 for the rest of the settings dialog.


### About dialog

No changes from v0.1

```
┌──────────────────────────────────┐
│ About                          x │
├──────────────────────────────────┤
│                                  │
│ NInferMonitor v0.2               │
│ Copyright (C) 2026 Aleksey Sanin │
│                                  │
├──────────────────────────────────┤
│               [OK]               │
└──────────────────────────────────┘
```

## 4. NVIDIA GPU integration

### 4.1. Getting real-time data
Use the NVML API (via the `nvml-wrapper` crate) to get all the required data from the NVIDIA GPU.
Use the GPU poll interval parameter (settings dialog, 1 s .. 10 s, step 1 s,
default 5 s; config `gpu_poll_interval_ms`) to control the frequency. The GPU
poll interval is independent of the log file poll interval.

### 4.2. Disabling NVIDIA game popup (Windows only)
On Windows, add two new command-line options: `--add-nvidia-app-profile` and
`--delete-nvidia-app-profile` that would add/remove the app profile to disable the
NVIDIA game popup and then exit immediately. On success the command prints the
status ("added" / "removed") to stdout and exits with code 0; on failure it prints
the error to stderr and exits with a non-zero code. Update the installer to add the
profile per machine (system-wide, for all users) at the end of the installation and
delete the profile before uninstallation.

Sample code:

```
[dependencies]
nvapi = "0.3" # Check for the latest version on crates.io
use nvapi::{DriverSettings, Nvapi};

fn disable_nvidia_overlay_for_app() -> Result<(), String> {
    // 1. Initialize NVAPI
    let nvapi = Nvapi::initialize().map_err(|e| format!("Failed to init NVAPI: {:?}", e))?;
 
    // 2. Access the DRS (Driver Session) API
    let mut session = nvapi.create_drs_session()
        .map_err(|e| format!("Failed to create DRS session: {:?}", e))?;
 
    session.load_settings().map_err(|e| format!("Failed to load driver settings: {:?}", e))?;

    // 3. Define your application's profile name
    let profile_name = "<App Name>Profile";
    let exe_name = "<Must match your built executable name>";

    // 4. Create or find the profile
    let mut profile = match session.find_profile_by_name(profile_name) {
        Ok(p) => p,
        Err(_) => {
            let p = session.create_profile(profile_name)
                .map_err(|e| format!("Failed to create profile: {:?}", e))?;
            // Associate the executable name with this driver profile
            session.add_application_to_profile(&p, exe_name)
                .map_err(|e| format!("Failed to link exe: {:?}", e))?;
            p
        }
    };

    // 5. Inject the magic setting to turn off ShadowPlay/Overlay hooks
    // Setting ID: 0x809D5F60, Value: 0x10000000
    let setting_id = 0x809D5F60;
    let disable_value = 0x10000000;

    session.set_profile_setting_uint32(&profile, setting_id, disable_value)
        .map_err(|e| format!("Failed to set overlay block setting: {:?}", e))?;

    // 6. Save the settings back to the system driver registry
    session.save_settings().map_err(|e| format!("Failed to save settings: {:?}", e))?;

    Ok(())
}

```

## 5. Automatic update checks

The app checks for new releases of itself and notifies the user when a newer
version is available. The releases are those published at
https://github.com/lsh123/ninfer-monitor; the check queries the GitHub
releases API for the latest release:

```
GET https://api.github.com/repos/lsh123/ninfer-monitor/releases/latest
```

The latest release's version (its tag with a leading `v` stripped, e.g.
`v0.3.0` → `0.3.0`) is compared with the running app's version (semantic
version comparison); only a strictly newer release is an available update.
The `releases/latest` endpoint excludes drafts and pre-releases.

### 5.1. Check settings and scheduling

- Two new settings in the settings dialog (see §3.2):
  - **Check for updates** — a checkbox, on by default. When off, no checks
    are run and the update button is hidden (a previously found update is
    cleared).
  - **Check interval** — a slider, 1 day .. 14 days, step 1 day, default 3
    days; the slider is disabled while the check is off.
- Both settings are persisted to the config as `update_check_enabled` and
  `update_check_period_days` (an out-of-range interval is clamped to 1..=14)
  and applied on OK, like the other settings.
- A check runs at startup and then at most once per check interval (also
  while the app is running, not only at startup). The time of the last
  successful check is persisted to the config (`last_update_check_unix_ms`),
  so the interval is measured across app restarts: a check runs when the
  last successful check is older than the interval, or has never run.
- The check runs on a background thread and never blocks the UI (NFR-2). A
  failed check (no network, HTTP error, API rate limit, unexpected response)
  is silent: no crash, no dialog, no status-bar entry; the next scheduled
  check retries.
- The app is never auto-updated: the check only discovers new releases. The
  user installs a new release manually — the "Release notes" link in the
  update dialog points to the release page with the installer downloads.

### 5.2. Update button (main window)

When a check finds a newer release, an update button appears in the main
window's toolbar, immediately to the left of the settings button (the only
additional toolbar element; the window selector buttons and the settings
button are unchanged). The button is hidden while no update is known. It
carries a new SVG icon — an arrow pointing down (a shaft with an arrowhead,
no tray line) in the same style as the existing toolbar icons (24×24,
`#a5d8ff` stroke, round caps and joins) — and the accessible text "Update
available". Pressing it opens the update dialog (§5.3). The button stays
visible after the dialog is closed, so the dialog can be reopened.

### 5.3. Update dialog

Pressing the update button opens the modal "New version available" dialog
with the question "Do you want to install new release version N?" (N is the
new release version, e.g. `0.3.0`) and a "Release notes" link that opens the
system browser at the new release's GitHub release page
(`https://github.com/lsh123/ninfer-monitor/releases/tag/<tag>`). The dialog
has OK and Cancel buttons (both close it) and follows the project's dialog
style (backdrop, panel chrome, focus scope, Escape closes):

```
┌──────────────────────────────────────────────┐
│ New version available                      x │
├──────────────────────────────────────────────┤
│ Do you want to install new release version   │
│ 0.3.0?                                       │
│                                              │
│ _Release notes_                              │
├──────────────────────────────────────────────┤
│                   [OK] [Cancel]              │
└──────────────────────────────────────────────┘
```

## 6. Graph improvements

For every chart series, when no new data is available and a last received value
exists, the series is extended with a dashed horizontal line from the last received
point to the right edge of the chart (the current time). When a new value arrives,
the dashed segment is completely removed and the actual data is used. This applies
to all charts, including the GPU usage/memory and temperature charts.

Note: because the dashed endpoint tracks the current time, the frame changes as time
passes; charts are re-rendered on every UI tick in which the frame could change (new
data, a panel size or chart window change, or the right edge advancing a full second),
so a frame that would render identically is not re-rendered.

## 7. Requests table improvements

The requests tabale columns widths should be resizable and the widths should be saved
in the config file and restored on startup IF AND ONLY IF the window width is 
the same. If for any reasons the window width changes, then the columnns widths 
should be reset to defauls. 

## 8. Non-functional requirements

The v0.1 NFRs carry over, with the following updates:

- **NFR-1 (Cross-platform).** Builds and runs on Windows 10+, Linux (glibc), and
  macOS 12+. Platform-specific code is confined to the file-tailing,
  window-state, and GPU (NVML/NVAPI) layers. The GPU layer is Windows/Linux only;
  on platforms without NVML support (e.g. macOS) or without an NVIDIA GPU/driver,
  the GPU section is hidden and the rest of the app is fully functional.
- **NFR-2 (Performance).** Steady-state CPU usage < 5% of one core and RAM <
  300 MB while tailing an actively written log. UI updates are batched (max
  ~4 Hz) and never block the UI thread; parsing runs on a background thread.
  Charts are re-rendered only when the frame could change (see §6).
- **NFR-3 (Latency).** A line written to the log is visible in the UI within
  1 second (log file poll interval + parse + render).
- **NFR-4 (Robustness).** The app must not crash on: malformed JSON, unknown
  events/fields, empty file, deleted file, truncated/rotated file, extremely
  long lines (up to 10 MB), a log written faster than it can be read, or NVML
  initialization/polling failures.
- **NFR-5 (Correctness).** Metrics must be computed exactly from the log fields
  defined in the v0.1 PRD §7.2/§9; no estimation. All timestamps are rendered in
  local time with millisecond precision.
- **NFR-6 (Usability).** Single-window app, usable at 1280×800; sensible
  defaults so the app is useful with zero configuration.
- **NFR-7 (Quality).** Rust edition 2024, `cargo clippy` and `cargo fmt` clean,
  unit tests for the parser and metric computations. No `unsafe` code except the
  contained Windows FFI in `src/window_state.rs` (monitor enumeration) and the
  GPU layer, which uses the safe bindings of the `nvml-wrapper` and `nvapi`
  crates; any direct FFI is isolated in the GPU module and documented with
  `SAFETY` comments.

## 9. Milestones

Milestones are incremental and buildable: at the end of each milestone the app
compiles, runs, and the milestone's "Done when" criteria are met. They build on
the completed v0.1 milestones.

### M1 — Startup screen

Implement the startup screen per §3.1 as the main window showing the startup
content (the same window, not a separate dialog).

- Show the startup screen when the log file location is not configured, or the
  file doesn't exist / can't be read in the configured location; skip it and
  open the dashboard directly when a valid file path is passed on the command
  line.
- The text box shows the current file (the last used path from the config) and
  is editable; the Open File button opens the file selection dialog and
  populates the text box.
- The two text messages (no file configured vs. file not readable) per §3.1.
- Start is enabled only for a valid file (exists and is readable, checked at
  click time); Start switches the window to the dashboard content; Close or
  Escape exits the application.
- The version in the welcome text is generated at build time; increased font
  sizes and a bold title; the app icon is shown centered above the welcome
  text; button heights match the rest of the project; the content fits the
  window (at least 300x200 px).

**Done when:** with no configured file the startup screen shows the "no file
configured" message and Start is disabled; selecting a valid file enables
Start and opens the dashboard in the same window; a valid CLI path skips the
startup screen; Close/Escape exits the app; `cargo clippy` and
`cargo fmt --check` are clean.

### M2 — Main UI cleanup

Implement the v0.2 main-screen cleanup per §3.2.

- Remove the Open file…, pause/play, and clear buttons from the toolbar (the
  toolbar contains only the window selector and the settings button); purge
  all the related code, the pause.svg / play.svg / trash.svg icons, and the
  paused.png test snapshot.
- Remove the Paused (yellow) state from the status bar (states: Live, "—",
  Disconnected).
- Drop the GPU and Memory rows (and the GPU header line) from the server-info
  panel; the panel shows Model, Engine, and Server (host:port, instance ID,
  log path).
- Update the affected tests and screenshots.

**Done when:** the toolbar shows only the window selector and the settings
button; no pause/play/clear code or icons remain in the tree; the status bar
has no Paused state; the server-info panel shows only Model/Engine/Server; the
full test suite passes.

### M3 — Settings dialog: file selector

Implement the settings-dialog file selector per §3.2.

- Add the log file text box (current file, editable) and the Open File button
  to the settings dialog.
- On OK, switch the tail to the selected file: reopen the file, backfill its
  contents, and reset the in-memory state (KPIs, charts, request table);
  persist the new path to the config.
- If the selected file doesn't exist or can't be read, still switch to it and
  show the Disconnected state, retrying until the file becomes readable.

**Done when:** changing the file in the settings dialog and pressing OK
re-tails the new file (backfill verified against the file contents) and resets
the KPIs/charts/table; the new path survives an app restart; selecting a
non-existent file shows Disconnected and recovers to Live when the file
appears; the full test suite passes.

### M4 — NVML integration and GPU widgets

Implement the NVIDIA GPU integration per §4.1 and the GPU widgets per §3.2.

- Add the `nvml-wrapper` dependency (Windows/Linux, cfg-gated; no NVML code
  compiled on macOS).
- Poll NVML at the GPU poll interval (settings dialog, 1 s .. 10 s, step 1 s,
  default 5 s; config `gpu_poll_interval_ms`): device list and names, GPU
  utilization %, memory used/total, NVML_TEMPERATURE_GPU and
  NVML_TEMPERATURE_MEMORY (VRAM).
- GPU section in the charts area: one row per detected GPU (usage/memory widget +
  temperature widget); the usage/memory widget title carries the GPU name (e.g.
  "GPU0 (NVIDIA GeForce RTX 5090): usage / memory"), so there is no separate
  header row; all widget rows (GPU rows and the two metric rows) have the same
  height.
  - Usage/memory widget: left KPI usage (with a % unit label next to the value),
    right KPI memory as an absolute value plus a percentage (e.g. "512MB (95%)");
    the historical graph shows percentages only; no trend arrows on GPU KPIs.
- Temperature widget: left KPI Core (NVML_TEMPERATURE_GPU), right KPI VRAM
  (NVML_TEMPERATURE_MEMORY); no trend arrows. The VRAM KPI shows NA when
  the driver does not report the memory-temperature sensor.
- Add the temperature unit selector (°F / °C, default °F) to the settings
  dialog; persist it to the config; the Core and VRAM KPI values show the
  selected unit (°F / °C) as a label next to the number; the y-axis scale is
  automatic.
- If NVML is unavailable (no NVIDIA GPU, no driver, or macOS), hide the entire
  GPU section; the rest of the dashboard works normally.

**Done when:** on a machine with an NVIDIA GPU and driver, the GPU section
shows one row per GPU with live values refreshed at the GPU poll interval
(values spot-checked against `nvidia-smi`); the temperature KPI values show the
active unit (default °F) and follow the settings selector; on a machine without
NVML the GPU section is hidden and the rest of the dashboard works; `cargo
clippy` and `cargo fmt --check` are clean.

### M5 — Graph improvements

Implement the dashed extrapolation per §6.

- For every chart series (including the GPU charts), when no new data is
  available and a last received value exists, extend the series with a dashed
  horizontal line from the last received point to the right edge of the chart
  (the current time).
- When a new value arrives, remove the dashed segment completely and use the
  actual data.
- Re-render the charts on every UI tick in which the frame could change (the
  dashed endpoint tracks the current time to within a second; a frame that
  would render identically is skipped).

**Done when:** stopping the log (or the GPU polling) extends every series with
a dashed line at the last received value; appending a new value removes the
dashed segment; the steady-state CPU usage stays within NFR-2.

### M6 — NVIDIA game popup (Windows)

Implement the NVIDIA game popup disabling per §4.2.

- Add the `nvapi` dependency (Windows only).
- `--add-nvidia-app-profile` / `--delete-nvidia-app-profile` command-line
  options: add/remove the app profile, print the status ("added" / "removed")
  to stdout, print errors to stderr, exit with code 0 on success and a
  non-zero code on failure, and exit immediately.
- The "Disable NVIDIA popup" link in the settings dialog (Windows only, hidden
  on Linux/macOS): runs the same profile installation and displays the exact
  success string "NVIDIA game popup disabled." or the error right under the
  link.
- Update the installer to add the profile per machine (system-wide) at the end
  of the installation and delete the profile before uninstallation.

**Done when:** on a machine with the NVIDIA driver, the CLI options add and
remove the profile with the correct stdout/stderr output and exit codes; the
settings link shows the success string; after installation the profile is
present and after uninstallation it is removed.

### M7 — Requests table improvements

Implement the resizable column widths in the requests table per §7.

- Add sliders to the table header row and allow user to drag the sliders 
  left and right.
- Resize table columns as sliders are dragged. Bot the columns on the left and right
  side of the slider change size. The total size of all columns doesn't change.
- Ensure min width for all columns and do not allow sliders to be dragged
  past the limit (for both left and right columns from the slider).
- Save columns withs and total window width to the config file. On restart, 
  check if the new window width is same as before and restore column widths
  in this case. Otherwise, use default widths.

**Done when:** columns are resizable, unit and screenshot tests updated.

### M8 — Automatic update checks

Implement the automatic update checks per §5.

- Add a minimal HTTPS client dependency (e.g. `ureq`) and a release-check
  module that queries the GitHub releases API
  (`https://api.github.com/repos/lsh123/ninfer-monitor/releases/latest`),
  parses the latest release's tag and `html_url`, normalizes the version
  (strip a leading `v`), and compares it with the current app version
  (`CARGO_PKG_VERSION`); only a strictly newer version reports an update.
- Schedule the check per §5.1: at startup and at most once per check
  interval (also while the app is running), on a background thread; persist
  the last-successful-check time in the config
  (`last_update_check_unix_ms`) so the interval spans restarts; when the
  check is disabled, run no checks and clear the update-available state;
  fail silently on network/API errors (the next scheduled check retries).
- Config: `update_check_enabled` (default `true`) and
  `update_check_period_days` (clamped to 1..=14, default 3), loaded,
  clamped, and saved like the other config fields (independent defaults,
  atomic save).
- Settings dialog: a "Check for updates" checkbox (default on) and a
  "Check interval" slider (1 .. 14 days, step 1 day, default 3 days,
  disabled while the check is off), saved on OK with the other settings.
- Generate the new SVG icon for the update button (an arrow pointing down:
  a shaft with an arrowhead, no tray line; 24×24, `#a5d8ff` stroke, round
  caps and joins, same style as `gear.svg`) in `src/ui/icons/`, and add the
  update button immediately to the left of the settings button, visible
  only when an update is available.
- Update dialog per §5.3: the "New version available" dialog with the
  question text and the "Release notes" link that opens the release page in
  the system browser (a small platform helper: `cmd /c start` on Windows,
  `xdg-open` on Linux, `open` on macOS); the project's dialog style
  (backdrop, focus scope, Escape closes).
- Unit-test the version comparison, the interval scheduling (including the
  persisted last-check time), and the API response parsing without hitting
  the network; update the affected tests and screenshots.

**Done when:** with a newer release published, a successful check shows the
update button (downward-arrow icon) immediately to the left of the settings
button; pressing it opens the "New version available" dialog with the new
version number, and the "Release notes" link opens the release page in the
system browser; the check on/off and the interval (1 .. 14 days, step 1 day,
default 3 days) persist across restarts, and the next check is due only
after the interval has elapsed (measured across restarts); with the check
disabled no checks run and the button is hidden; a failed check (e.g. no
network) is silent and the app keeps working; `cargo clippy` and `cargo fmt
--check` are clean.

### M9 — Hardening & v0.2 acceptance

Close out the v0.2 NFRs (§7) and run the full acceptance suite per §9.

- Robustness passes: NVML init/polling failures, no-GPU machines, macOS build
  (NFR-1/NFR-4).
- Performance validation: steady-state CPU < 5% of one core and RAM < 300 MB
  with the per-tick chart re-rendering (NFR-2); < 1 s end-to-end latency
  (NFR-3).
- Cross-platform: release builds on Windows, Linux, and macOS (NFR-1).
- `cargo clippy`, `cargo fmt --check`, full test suite green (NFR-7).

**Done when:** all 12 acceptance criteria in §9 pass on all three platforms;
v0.2 tagged.

## 10. Acceptance criteria (v0.2)

1. `cargo build --release` succeeds on Windows, Linux, and macOS.
2. With no configured log file, the app opens the startup screen (the main
   window showing the startup content) with the "no file configured" message;
   Start is disabled until the text box points to an existing, readable file;
   Close or Escape exits the app.
3. Running `ninfer-monitor <path>` with an existing, readable file skips the
   startup screen and opens the dashboard directly.
4. Changing the log file in the settings dialog and pressing OK switches the
   tail to the new file (backfill from the start) and resets the KPIs, charts,
   and request table.
5. On a machine with an NVIDIA GPU and driver, the GPU section shows one row
    per GPU (usage/memory and temperature widgets; the usage/memory widget title
    carries the GPU name), refreshed at the GPU poll interval; the temperature
    KPI values show the active unit (°F / °C, default °F) next to the number and
    follow the settings selector; GPU KPIs show no trend arrows.
6. On a machine without NVML (no NVIDIA GPU, no driver, or macOS), the GPU
   section is hidden and the rest of the dashboard works normally.
7. All widget rows in the charts section (GPU rows and the two metric rows)
   have the same height.
8. When no new data is available, each chart series is extended with a dashed
   line at the last received value; when a new value arrives, the dashed
   segment is removed.
9. The toolbar contains only the window selector (1m / 5m / 15m / 30m / 1h)
    and the settings button (plus the update button, shown only when a new
    release is available); the status bar has no Paused state; the request
   table keeps the Think and Status columns; the server-info panel has no
   GPU/Memory rows and its Server row shows host:port, instance ID, and log
   path.
10. On Windows, `--add-nvidia-app-profile` prints the added status to stdout
    and exits with code 0; `--delete-nvidia-app-profile` prints the removed
    status and exits with code 0; errors are printed to stderr with a non-zero
    exit code. The installer adds the profile per machine at the end of the
    installation and deletes it before uninstallation.
11. `cargo clippy` and `cargo fmt --check` report no issues and the full test
    suite passes.
12. Automatic update checks are enabled by default and check the releases
    published at https://github.com/lsh123/ninfer-monitor; when a newer
    release is found, an update button (downward-arrow icon) appears to the
    left of the settings button and opens the "New version available" dialog
    ("Do you want to install new release version N?") whose "Release notes"
    link opens the release page in the system browser; the check on/off and
    the interval (1 day .. 14 days, step 1 day, default 3 days) are
    configurable in the settings dialog and persisted; a failed check is
    silent.