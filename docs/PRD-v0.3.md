# NInferMonitor — Product Requirements Document

| | |
|---|---|
| **Version** | v0.3 |
| **Status** | Draft |
| **Date** | 2026-09-27 |
| **Stack** | Rust, Slint (UI), Plotters (charts), NVML API |
| **Platforms** | Windows, Linux, macOS |

---

## 1. Overview

NInferMonitor is a cross-platform desktop application that monitors and manages
the NInfer server. The monitoring is performed by "tailing" the request log
(`server.requests.jsonl`, JSON Lines format) in real time — similar to `tail -f` —
and the app presents the core operational details of a running NInfer inference
server in a graphical dashboard. This version builds on v0.2 (see PRD-v0.2.md
for more details) and adds the ability to control the NInfer server itself: the
app registers NInfer configurations (each one a list of command line options),
and starts, restarts, and stops the server process from the new Control tab.

## 2. Major changes in this version

- **NInfer process control (new).** The app can start, restart, and stop the
  NInfer server. A server is described by a *configuration* — a name plus a
  list of command line options (e.g. `--model qwen3.8-27b-nvfp4 --port 8080`;
  the program is always `ninfer-serve` from the installation folder, with the
  OS-specific extension, §4.2). Configurations are registered in the app (persisted to
  the config file) and managed in the new Control tab: start / restart / stop
  the selected configuration, add a new one, delete one, and edit its command line
   options.
- **The running configuration is persisted and restarted on startup.** The
  name of the configuration started by the app is stored in the config file as
  `running_config` (§6). A successful Start/Restart persists it; Stop, a
  detected process exit, and deleting the running configuration clear it. When
  the app opens directly to the dashboard (a valid configured or CLI
  installation folder) or after the startup screen's Start, the app restarts
  the persisted configuration if it still exists. Closing the app stops the
  tracked processes but leaves `running_config` set, so a configuration that
  was still running when the app closed is restarted on the next start; a
  configuration that was stopped or exited before the app closed is not.
- **New window layout: a vertical tab bar on the left.** The main window now has
  a fixed-width vertical tab bar on the left side. At the top of the bar are
  three tabs — **Control**, **Monitor**, and **Settings** — and at the bottom
  of the bar are the **Update** button (shown only when a new release is
   available) and the **About** button. The Control tab is shown by default.
- **The Monitor tab is the v0.2 dashboard.** It has the same UX as the v0.2 main
  screen (window selector, GPU/metric widgets, request table, server-info panel,
  status bar), except the settings button and the update button are no longer in
  the toolbar — the settings button becomes the Settings tab and the update
  button moves to the bottom of the left tab bar. The toolbar contains only the
  chart-window selector (1m / 5m / 15m / 30m / 1h).
- **The startup screen and the Settings tab select the NInfer installation
  folder instead of the log file.** A folder is valid only if it is an existing
  directory that contains the `ninfer-serve` executable (with the OS-specific
  extension, e.g. `ninfer-serve.exe` on Windows); the startup screen's Start
  button, the Settings tab's Save button, and the CLI positional argument
  accept only a valid folder (§3.2). The text box shows the current
  installation folder and is editable; the "Open File…" button becomes
  "Open Folder…" (a folder selection dialog).
- **The request log is determined by the last-started configuration.** The app
  scans the configuration's command line for `--request-log-jsonl`: if
  present, the Monitor tab tails that file; if absent, the app appends
  `--request-log-jsonl <log-file-folder>/ninfer-monitor/server.requests.jsonl`
  to the launched command line (the `ninfer-monitor` folder is created if
  missing) and tails that file (§3.2, §4.2). Before start (after stop on a
  restart), the app deletes the previous app-managed log file so each run
  starts with a fresh log.
- **The Settings tab gains a "Log file folder" field** — the base folder for
  the app-managed log file, defaulting to the OS temp folder (§3.5).
- **The status bar shows the NInfer installation folder** instead of the log
  file path.
- **The Settings tab drops the About link** (replaced by the About button at
  the bottom of the left tab bar).
- **The Settings tab gains a "Check for updates" button** — right below the
  update-check settings, it runs the update check immediately (bypassing the
  period schedule and the failed-attempt throttle), opens the "New version
  available" dialog when a newer release is found, and shows an "Up to date"
  dialog (the current version, an OK button) when it is not (§3.5, §5).
- **Configurations can be imported from launcher scripts (new).** When the
  installation folder is selected (the startup screen's Start, the Settings
  tab's Save, or a valid CLI folder) and the app has no configurations yet,
  the app scans the folder for launcher scripts — `.bat` files on Windows,
  `.sh` files on Linux — and pre-creates a configuration from each script that
  invokes `ninfer-serve` directly (named after the file name, the command line
  is the script's arguments after the program token, §4.1). The Settings tab
  also gains an **Import Configurations** button that imports from the
  currently selected folder at any time, skipping the names that already exist
  (case-insensitive) (§3.5).
- **Config file changes:** a new `install_folder` field replaces `last_path`
  (the legacy `last_path` is ignored, except a one-time migration, §6), a new
  `configs` field holds the registered NInfer configurations, and a new
  `running_config` field holds the name of the configuration to restart on
  startup (§6).
- **CLI:** the positional argument is now the NInfer installation folder
  (`ninfer-monitor <install-folder>`), not a log file; a valid folder skips the
  startup screen.

## 3. UX layout

### 3.1. Window layout: the left tab bar

The main window is divided into a fixed-width vertical tab bar on the left
(96 px) and the tab content area on the right. The tab bar spans the full
height of the window, above the status bar.

- **Tabs (top of the bar):** three tabs stacked vertically — **Control**,
  **Monitor**, and **Settings** — each with an icon and a horizontal text
  label. The active tab is highlighted. Clicking a tab switches the content
   area; the tab state is not persisted (the app always opens on the **Control**
   tab).
- **Buttons (bottom of the bar):** two buttons stacked vertically, styled the
  same as the top tab buttons (a 24 px icon above a text label, full tab-bar
  width, 48 px tall), anchored to the bottom of the bar; the icons use the
  existing toolbar-icon style (`#a5d8ff` stroke, round caps and joins):
  - **Update** — the v0.2 downward-arrow icon with an "Update" label; shown
    only when a new release is available (same behavior as the v0.2 toolbar
    button, §5); opens the update dialog.
  - **About** — a new "info" icon (a circle with an "i") with an "About"
    label; opens the About dialog (§3.6).
```
┌─────────┬───────────────────────────────────────────────────────────────────┐
│Control  │ Tab content, one of:                                              │
│Monitor  │   * the Monitor tab (§3.3)                                        │
│Settings │   * the Control tab (§3.4)                                        │
│         │   * the Settings tab (§3.5)                                       │
│         │                                                                   │
│         │                                                                   │
│ Update   ├───────────────────────────────────────────────────────────────────┤
│ About    │ Status bar content (§3.7)                                         │
└─────────┴───────────────────────────────────────────────────────────────────┘
```

- The **status bar** is at the bottom of the window, below the tab content
  area, and is visible on all tabs (it reports the log-tail state, which is
  global). Its content is unchanged from v0.2, except the Log file field shows
  the NInfer installation folder (§3.7).
- New icons (in `src/ui/icons/`, same style as the existing ones): the Control
  tab icon (a power symbol), the Monitor tab icon (a pulse/activity line), and
  the About "info" icon; the Settings tab uses the existing gear icon.

### 3.2. NInfer installation folder and the monitored log file

The single source of truth for "where NInfer is" is the **NInfer installation
folder** — the directory that contains the `ninfer-serve` executable.

- **Folder validity:** a folder is valid only if it is an existing directory
  that contains the `ninfer-serve` executable (with the OS-specific extension,
  e.g. `ninfer-serve.exe` on Windows). The startup screen's Start button
  (§3.2.1), the Settings tab's Save button (§3.5), and the CLI positional
  argument accept only a valid folder.
- **The monitored log file** is determined by the **last-started
  configuration** (§4.2): the app scans the configuration's command line for
  `--request-log-jsonl`; if present, the Monitor tab tails that file (relative
  paths resolve against the installation folder); if absent, the app appends
  `--request-log-jsonl <log-file-folder>/ninfer-monitor/server.requests.jsonl`
  to the launched command line and tails that file. `<log-file-folder>` is the
  **Log file folder** from the Settings tab (§3.5); the default is the OS temp
  folder (`std::env::temp_dir()`; e.g. `C:\Users\<user>\AppData\Local\Temp` on
  Windows, `/tmp` on Linux). The `ninfer-monitor` subfolder is created if it
  does not exist. The Monitor tab switches to the new log file when a
  configuration is started or restarted, keeps the current one until another
  configuration is started, and shows the Disconnected state before the first
  start. A log file that does not exist yet is retried until it appears (the
  existing missing-file retry behavior).
- **Log file reset:** when the app appends the log file parameter (the
  configuration has no explicit `--request-log-jsonl`), the app deletes the
  previous log file before start — on a restart, after the old process has
  stopped and exited — so each run starts with a fresh log. A configuration's
  explicit `--request-log-jsonl` file is never deleted by the app. A deletion
  failure does not block the start (§4.2).
- The installation folder is set on the startup screen (§3.2.1) or in the
  Settings tab (§3.5), and persisted to the config as `install_folder` (§6).
- The installation folder (not the log file path) is shown in the status bar
  (§3.7).

#### 3.2.1. Startup screen

The startup screen is shown when the installation folder is not configured, or
the configured folder is invalid (it does not exist, is not a directory, or
does not contain the `ninfer-serve` executable, §3.2). It is the main window
showing the startup content (the same window, not a separate dialog); pressing
Start switches the window to the main content (the Control tab). If a valid
installation folder is passed as a command-line argument
(`ninfer-monitor <install-folder>`), the startup screen is skipped and the main
content is shown directly:

```
┌─────────────────────────────────────────────┐
│                                             │
│                  [app icon]                 │
│            Welcome to NInferMonitor (v0.3)! │
│ <Text message>                              │
│                                             │
│ ┌─────────────────────────┐                 │
│ │ <current folder if any> │ [Open Folder...]│
│ └─────────────────────────┘                 │
│                                             │
│              [Start]  [Close]               │
└─────────────────────────────────────────────┘
```

- The app icon is shown centered above the welcome text; the version in the
  welcome text is generated at build time (not hardcoded); the title is bold
  and the font sizes match v0.2.
- The text message is different for no folder configured ("Please select the
  NInfer installation folder to start.") vs. when the folder is configured but
  invalid ("The currently selected folder is not a valid NInfer installation
  folder (it must contain the ninfer-serve executable), please select the
  NInfer installation folder to start monitoring.").
- The text box shows the current installation folder (the last used folder from
  the config) and allows the user to edit it (or enter a new path).
- The **Open Folder…** button opens the folder selection dialog that
  populates the text box with the selected folder.
- The Start / Close buttons are centered. **Start is enabled only when the
  text box points to a valid folder** (an existing directory that contains the
  `ninfer-serve` executable, §3.2; checked at click time). The log file does
  NOT have to exist for Start to be enabled — the Monitor tab shows
  Disconnected until a configuration is started from the Control tab (the
  typical flow is: select the installation folder → start a configuration from
  the Control tab → the log appears → the Monitor tab goes Live).
- When Start is pressed, the startup screen is dismissed, the installation
  folder is persisted, and the main content (the Control tab; the Monitor tab
  stays Disconnected until a configuration is started) is shown in the same
  window. If a persisted running configuration exists, it is started after the
  main content is shown (§4.4).
- When Close is pressed (or the escape key), the application exits.
- All buttons have the same height as the other buttons in the project; the
  dialog size fits all inner elements but is at least 300×200 px.

### 3.3. Monitor tab

The Monitor tab has the same UX as the v0.2 main screen (§3.2 of PRD-v0.2.md):
the toolbar with the chart-window selector (1m / 5m / 15m / 30m / 1h), the
charts section (one row per GPU — usage/memory and temperature widgets — plus
the Throughput, Latency, Cache, and Scheduler widgets), the request table, and
the server-info panel, separated by the two draggable splitters. The widget
behavior (KPI values, units, trend arrows, dashed extrapolation, re-render
rules) is unchanged from v0.2.

The only differences from v0.2:

- The toolbar contains **only** the window selector — the settings button
  becomes the Settings tab and the update button moves to the bottom of the
  left tab bar (§3.1).
- The tab content area is to the right of the left tab bar (96 px narrower than
  the v0.2 window content).


The Monitor tab only:
```
┌───────────────────────────────────────────────────────────────────┐
│ [1m] [5m] [15m] [30m] [1h]                                        │
├───────────────────────────────────────────────────────────────────┤
│ ┌ GPU0 (NVIDIA GeForce RTX 5090): usage / memory ─┐ ┌ GPU0 temp ┐ │
│ │ <GPU usage/memory widget>                       │ │ <GPU temp>│ │
│ └─────────────────────────────────────────────────┘ └───────────┘ │
│ ┌ Throughput (tok/s) ──────────────┐ ┌ Latency ─────────────────┐ │
│ │ <throughput widget>              │ │ <latency widget>         │ │
│ └──────────────────────────────────┘ └──────────────────────────┘ │
│ ┌ Cache ───────────────────────────┐ ┌ Scheduler ───────────────┐ │
│ │ <cache widget>                   │ │ <scheduler widget>       │ │
│ └──────────────────────────────────┘ └──────────────────────────┘ │
├───────────────────────────────────────────────────────────────────┤
│ #   Time          Model          Prompt Compl Think   TTFT ...    │
│ > 66  14:02:58.124  qwen3.8-27b-…  42737  —     —      —  error   │
├───────────────────────────────────────────────────────────────────┤
│ ┌ Server info ──────────────────────────────────────────────────┐ │
│ │ Model    qwen3_8_27b (nvfp4) · 22.1 GiB · load 9.7 s          │ │
│ │ Engine   KV fp8-e4m3-row256 · capacity 240 000 · …            │ │
│ │ Server   0.0.0.0:8080 · serve-27872-… · log d:/NInfer/logs/…  │ │
│ └───────────────────────────────────────────────────────────────┘ │
└───────────────────────────────────────────────────────────────────┘
```

### 3.4. Control tab

The Control tab manages the NInfer server: the registered configurations and
the start / restart / stop controls for the selected one.


Control tab only:

```
┌───────────────────────────────────────────────────────────────────┐
│  [Start] [Restart] [Stop]                        [Add] [Delete]   │
├───────────────────────────────────────────────────────────────────┤
│ ┌ Configurations ───────────────────────────────────────────────┐ │
│ │ > ● my-server                                                 │ │
│ │   bench-run                                                   │ │
│ │   Config 3                                                    │ │
│ └───────────────────────────────────────────────────────────────┘ │
├───────────────────────────────────────────────────────────────────┤
│ ┌ Edit configuration ───────────────────────────────────────────┐ │
│ │ Name:         [my-server_______________________________]      │ │
│ │ Command line options:                                         │ │
│ │ [--model qwen3.8-27b-nvfp4 --port 8080 ...___             ]   │ │
│ └───────────────────────────────────────────────────────────────┘ │
│ <status line: "Started." / "Stopped." / "Error: …">               │
└───────────────────────────────────────────────────────────────────┘
```

- **Button row (top):** two groups of buttons, all with the same height as the
  other buttons in the project and light pastel-blue SVG icons beside the
  labels (new icons in `src/ui/icons/`, same style as the existing ones):
  - **Start** (a play triangle) — starts the selected configuration (§4.3).
  - **Restart** (a circular arrow) — restarts the selected configuration.
  - **Stop** (a filled square) — stops the selected configuration.
  - **Add** (a plus) — adds a new configuration.
  - **Delete** (a trash) — opens a confirmation dialog for the selected
    configuration; accepting the dialog deletes it.
  The Start / Restart / Stop group is left-aligned; the Add / Delete group is
  right-aligned.
- **Configurations list:** the registered configurations, one row each, showing
  the configuration name. The selected row is highlighted; clicking a row
  selects it and loads it into the edit panel. A configuration started by this
  app instance shows a green dot before its name (the "running" indicator);
  configurations not started by this instance show no indicator. A configuration
  whose last start or exit failed is marked red (a red row and a red dot). The
  rows are top-aligned (any unused vertical space appears below the last row),
  and the list scrolls when it overflows.
- **Edit configuration panel:** shows the selected configuration:
  - **Name** — a single-line text box.
    - **Command line options** — a multi-line text box with all the options
      (the program is always `ninfer-serve` from the installation folder with
      the OS-specific extension, §4.2; the app treats the text as an opaque
      string except for the `--request-log-jsonl` option, which it scans for to
      determine the monitored log file, §4.2). It scrolls vertically when the
      text overflows. The user can lay the options out across multiple lines;
      the line breaks (newlines) are treated as spaces when the command line is
      tokenized for execution (§4.2).
   When no configuration is selected (e.g. the list is empty), the panel is
   disabled and shows a placeholder ("No configuration selected").
- **List / edit divider:** the configurations list and the edit panel are
  divided by a draggable horizontal splitter. The list's share of the middle
  area (the space between the button row and the status line) is persisted as
  `control_split` (§6); dragging the splitter resizes the list (the edit panel
  takes the rest) and the position is saved on release.
- **Status line (bottom of the tab content):** shows the result of the last
  operation — "Started.", "Stopped.", "Restarted.", "Saved.", "Added.",
  "Deleted.", "Imported N configurations." (the automatic import from
  launcher scripts, §4.1), or an error ("Error: the name must be unique",
  "Error: the command line is empty",   "Error: failed to start: …", including the captured console output when the
  server process exits abnormally). Errors are shown in red. It is empty
  initially.

Behavior:

- **Add** creates a new configuration with a unique default name ("Config 1",
  "Config 2", … — the next unused number) and an empty command line, selects
  it, and focuses the edit panel. The new configuration is persisted to the
  config file immediately.
- **Delete** first shows a confirmation dialog naming the selected
  configuration. Accepting the dialog removes the selected configuration; if
  the configuration is running (started by this instance), its process is
  stopped first. Cancel, `Escape`, or a click on the backdrop closes the dialog
  without deleting anything. The list and the config file are updated
  immediately after acceptance; if the deleted configuration was selected, the
  edit panel shows the placeholder (no other row is auto-selected).
- The edits of the selected configuration are applied immediately as the user
  pauses typing or when focus changes. The basic checks are done first:
  - the name must be non-empty (after trimming) and unique (case-insensitive)
    among the other configurations;
  - the command line must be non-empty (after trimming).
   On success the configuration is updated and persisted to the config file and
   the status line shows "Saved."; renaming the persisted configuration also
   updates `running_config` to the new name; on failure the status line shows
   the error and nothing is changed.
- **Start / Restart / Stop** act on the selected configuration (§4.3). Start
  is enabled when no configuration is running (started by this instance) and a
  configuration is selected. Restart is enabled when at least one configuration
  is running (started by this instance) and a configuration is selected. Stop
  is enabled when the selected configuration is running (started by this
  instance). The name of the running configuration is persisted as
  `running_config` (§6): a successful Start/Restart saves it, Stop clears it,
  and deleting the running configuration clears it (§4.3/§4.4).
- The Monitor tab tails the log file of the last-started configuration (§3.2,
  §4.2): the app scans the configuration's command line for
  `--request-log-jsonl` (if present, the Monitor tab tails that file); if the
  option is absent, the app appends
  `--request-log-jsonl <log-file-folder>/ninfer-monitor/server.requests.jsonl`
  to the launched command line and tails that file.

### 3.5. Settings tab

The v0.2 modal settings dialog is converted to a tab. The Settings tab is shown
when the Settings tab is clicked. The v0.2 log-file selector is replaced by an
installation folder selector; the About link is removed (the About button at the
bottom of the left tab bar replaces it); the OK/Cancel buttons are renamed
Save/Discard (see below). Everything else is unchanged from v0.2:

```
┌────────────────────────────────────────────────────────────────────┐
│ ┌─────────────────────────┐                                        │
│ │ <current folder if any> │ [Open Folder..] [Import Configurations]│
│ └─────────────────────────┘                                        │
│ <import status line, e.g. "Imported 3 configurations.">            │
│ Log file folder:                                                   │
│ ┌─────────────────────────┐                                        │
│ │ <log file folder>       │ [Open Folder..]                        │
│ └─────────────────────────┘                                        │
├────────────────────────────────────────────────────────────────────┤
│ Log file poll interval: 500 ms                                     │
│ 100 ms ----*-------------- 5 s                                     │
│ GPU poll interval: 5 s                                             │
│ 1 s -------*------------ 10 s                                      │
│ Max requests: 1000                                                 │
│ 10 -------*-------------- 1000                                     │
│ Temperature unit: °F                                               │
│                                                                    │
│ [x] Check for updates                                              │
│ Check interval: 3 days                                             │
│ 1 day ----*------------ 14 days                                    │
│ [Check for updates]                                                │
│                                                                    │
│ _Disable NVIDIA popup_                                             │
├────────────────────────────────────────────────────────────────────┤
│                          [Save] [Discard]                          │
└────────────────────────────────────────────────────────────────────┘
```

- The folder text box shows the current installation folder and allows the user
  to edit it (or enter a new path). The **Open Folder…** button opens the
  folder selection dialog that populates the text box.
- The **Import Configurations** button (right of the installation folder's
  Open Folder… button) imports configurations from the currently selected
  installation folder (§4.1): it scans the folder for launcher scripts
  (`.bat` on Windows, `.sh` on Linux), adds a configuration for each script
  that invokes `ninfer-serve` directly, and skips the names that already exist
  (case-insensitive). The button is enabled when the installation folder text
  box points to an existing directory. The outcome is shown in the status line
  below the folder row — "Imported N configurations.", "No new configurations
  found.", or an error ("Error: the installation folder is not a valid
  directory") — and the imported configurations are persisted to the config
  file. The status line is cleared when the tab is (re)opened or on Discard.
- The **Log file folder** text box shows the base folder for the app-managed
  log file (the default is the OS temp folder, §3.2) and allows the user to
  edit it (or enter a new path); its **Open Folder…** button opens the folder
  selection dialog that populates the text box.
- **Save is enabled only when both text boxes point to valid folders**: the
  installation folder must contain the `ninfer-serve` executable (§3.2) and
  the log file folder must be an existing directory.
- On Save, the app switches to the selected installation folder: it
  re-determines the monitored log file for the last-started configuration
  (§3.2), reopens the tail, backfills its contents, and resets the in-memory
  state (KPIs, charts, request table); the new folder is persisted to the
  config. Save applies all the changed settings (the installation folder, the
  log file folder, the poll intervals, max requests, the temperature unit, and
  the update-check settings) and persists them to the config.
- Discard (and the Escape key) discards the edits made in the tab: all the
  fields revert to the saved values and nothing is persisted.
- The _Disable NVIDIA popup_ link (Windows only) and the update-check settings
  are unchanged from v0.2.
- The **Check for updates** button (right below the update-check settings)
  runs the update check immediately: it bypasses the period schedule and the
  failed-attempt throttle (the user explicitly asked for it) and works even
  when the "Check for updates" checkbox is off. On success it records the
  check time (the automatic schedule restarts from it) and, when a newer
  release is found, opens the "New version available" dialog (the same dialog
  the tab-bar Update button opens, §5); when no newer release is found, it
  shows the "Up to date" dialog (§5); a failed check is silent.

### 3.6. About dialog

No changes from v0.2, except it is opened from the About button at the bottom
of the left tab bar (not from a link in the Settings tab):

```
┌──────────────────────────────────┐
│ About                          x │
├──────────────────────────────────┤
│                                  │
│ NInferMonitor v0.3               │
│ Copyright (C) 2026 Aleksey Sanin │
│                                  │
 ├──────────────────────────────────┤
 │               [OK]               │
 └──────────────────────────────────┘
 ```

The About dialog also carries Slint's `AboutSlint` attribution widget (the
Slint Royalty-Free License requires either that widget or a public Slint
badge; NInferMonitor uses both). The app forces Slint's dark `Palette`, so the
badge renders the dark `#MadeWithSlint` logo variant.

### 3.7. Status bar

The status bar is unchanged from v0.2, except the Log file field shows the
NInfer installation folder (the label is "NInfer folder:").

```
  ● Live  14:02:58.124  NInfer folder: d:/NInfer  Errors: 0 Skipped 0
```

## 4. NInfer process control

### 4.1. Configurations

A **configuration** is a registered way to start the NInfer server: a name plus
a command line with all the options. Example:

| Name | Command line |
|---|---|
| `my-server` | `--model qwen3.8-27b-nvfp4 --port 8080 --request-log-jsonl logs/server.requests.jsonl` |

- Configurations are stored in the config file as the `configs` field (§6) and
  are managed in the Control tab (§3.4): Add, Delete, and the edit panel
  (name + command line, validated as the user edits, §3.4).
- The command line is an **opaque string** to the app: the app does not parse
  its options; it tokenizes it into an argument vector (§4.2) and executes it.
  The only exception is `--request-log-jsonl`, which the app scans for to
  determine the monitored log file (§4.2).
- The running state is tied to the configuration entry itself (not its name or
  list position): renaming or reordering the list does not lose it.
- The name of the configuration started by the app is persisted as
  `running_config` (§6) so the app can restart it on startup; the in-memory
  running indicator remains per app instance.
- Multiple configurations may be running at the same time (each is an
  independent process).
- **Import from launcher scripts (new):** configurations can be pre-created
  from the launcher scripts that sit in the NInfer installation folder —
  `.bat` files on Windows, `.sh` files on Linux (e.g. the `qwen3_*.bat`
  launchers shipped with NInfer):
  - **Automatic import:** when the installation folder is selected — the
    startup screen's Start, the Settings tab's Save, or a valid CLI folder —
    and the app has **no configurations yet**, the app scans the folder for
    launcher scripts and pre-creates a configuration from each one (and
    persists them; the Control tab's status line shows "Imported N
    configurations."). When the app already has configurations, nothing is
    imported automatically (the user's configurations are never touched).
  - **Manual import:** the Settings tab's **Import Configurations** button
    (§3.5) imports from the currently selected folder at any time,
    deduplicating against the existing configuration names.
  - **Parsing rules:** a script yields a configuration when one of its
    logical lines (the physical lines joined across the line-continuation
    marker — `^` in batch, `\` in shell) invokes `ninfer-serve` directly as
    its first token (the file name of the token, after stripping surrounding
    quotes, equals `ninfer-serve.exe` on Windows / `ninfer-serve` on Linux; a
    path prefix such as `.\` or `./` is allowed). Comment lines (a `rem` or
    `::` line in batch, a `#` line in shell) and blank lines are skipped, so a
    commented-out invocation is not imported; scripts that do not invoke
    `ninfer-serve` directly (e.g. a downloader helper) yield nothing. The
    configuration's **name** is the script file name without the extension
    (e.g. `qwen3_8_27b_nvfp4-main.bat` → `qwen3_8_27b_nvfp4-main`) and its
    **command line** is the rest of the invocation line after the program
    token, with the whitespace runs collapsed to single spaces (the program
    token itself is not part of it, §4.2). An invocation without arguments
    yields nothing.
  - **Dedup:** a script whose name (case-insensitive) collides with an
    existing configuration name — or with an earlier script in the same
    folder — is skipped.
  - **Robustness:** an unreadable folder or script is skipped silently; the
    import never fails the start/save that triggered it.

### 4.2. Command line and launch

- **Tokenization:** the command line (the options only; the program is not part
  of it, see below) is split into an argument vector on whitespace. Newlines
  and carriage returns are replaced with spaces before tokenization, so the
  options can be laid out across multiple lines (the user-added line breaks are
  treated as spaces in the launched command line). Double quotes group
  characters into a single argument (the quotes are removed); a backslash
  before a double quote produces a literal quote inside the argument. There is
  no shell interpretation: no pipes, redirects, environment-variable expansion,
  or globbing. The tokenization is a pure, unit-testable function.
- **Program resolution:** the program is always `ninfer-serve` from the
  selected NInfer installation folder, with the OS-specific extension (e.g. on
  Windows: `<NInfer-path>\ninfer-serve.exe`). The app verifies that the program
  file exists and shows an error otherwise.
- **Working directory:** the child process's working directory is the NInfer
  installation folder, so relative paths in the command line (e.g.
  `--request-log-jsonl logs/server.requests.jsonl`) resolve against it.
- **Log file determination:** the app scans the tokenized command line for
  `--request-log-jsonl` (both the `--request-log-jsonl <path>` and
  `--request-log-jsonl=<path>` forms; if the option appears multiple times, the
  last occurrence wins). If present, the Monitor tab tails that file (relative
  paths resolve against the installation folder). If absent, the app appends
  `--request-log-jsonl <log-file-folder>/ninfer-monitor/server.requests.jsonl`
  to the argument vector before launching, where `<log-file-folder>` is the
  Log file folder from the Settings tab (§3.5; the default is the OS temp
  folder, `std::env::temp_dir()`); the `ninfer-monitor` subfolder is created if
  it does not exist. The Monitor tab tails the determined file of the
  last-started configuration and switches to the new file on Start/Restart of
  another configuration (§3.2).
- **Log file reset:** when the app appends the log file parameter (the
  configuration has no explicit `--request-log-jsonl`), the app deletes the
  previous log file before start: on Start, before spawning; on Restart, after
  the old process has stopped and exited (and before the new process starts).
  A configuration's explicit `--request-log-jsonl` file is never deleted by
  the app. A deletion failure (e.g. the file is locked) does not block the
  start; the error is shown in the status line.
- **stdout/stderr:** piped and drained in the background so the child never
  blocks on a full pipe buffer. The last 64 KiB of the combined output is kept
  per configuration and is appended to the error message when a start fails or
  the process exits abnormally; otherwise the output is discarded.
- **Preconditions:** Start requires a configured installation folder (for the
  program resolution and the working directory) and a non-empty command line;
  otherwise the status line shows the error and nothing is started.

### 4.3. Start / Restart / Stop

- **Start** spawns `ninfer-serve` from the installation folder with the
  configuration's options as arguments (§4.2); if the app appends the log file
  parameter, the previous log file is deleted first (§4.2). On success the
  configuration shows the running indicator, the status line shows
   "Started.", the configuration's error state is cleared, and the
  configuration's name is persisted as `running_config` (§6); on failure (e.g.
   the executable does not exist) the status line shows "Error: failed to
   start: …", the configuration is not marked running, and the configuration is
   marked red in the list.
- **Restart** stops the configuration's process if it is running (started by
  this instance) and waits for its exit; if the app appends the log file
  parameter, the previous log file is deleted after the exit (§4.2); then it
  starts a new process and persists the configuration's name as
  `running_config` on success (§6). If the configuration is not running,
  Restart simply starts it.
- **Stop** terminates the configuration's process (a direct kill; no graceful
  shutdown signal in v0.3) and waits for its exit. The running indicator is
  cleared, the status line shows "Stopped.", and the persisted
  `running_config` is cleared when it named the stopped configuration.

### 4.4. Process lifecycle

- The app tracks the child processes it started, per configuration. On every
  UI tick (≤ 4 Hz) it polls each tracked child with a non-blocking wait; when a
  process has exited (crash, external kill, or Stop), the running indicator is
  cleared, the Start / Restart / Stop enablement is refreshed (§3.4), and the
  persisted `running_config` is cleared when it named the exited
  configuration. The app never crashes on a process exit. A failed exit (a
  non-zero exit code, or an exit that cannot be read) marks the configuration
  red and shows an error in the status line with the exit code and the captured
  console tail (§4.2); a successful exit clears no error (the error state is
  set only by failures).
- When the app exits, it stops every server process it started (a direct kill
  and a wait for the exit), so the processes release their resources (in
  particular their GPU memory). The in-memory running state is per app
  instance and is not persisted; a server started by a previous instance is
  not tracked and cannot be stopped from the app. The persisted
  `running_config` name is **not** cleared by the app exit: when the app opens
  directly to the dashboard (a valid configured or CLI installation folder) or
  when the startup screen's Start is pressed, it restarts the persisted
  configuration if it still exists. A configuration that was still running when
  the app closed is therefore restarted on the next start; a configuration that
  was stopped, exited, or deleted before the app closed is not.

## 5. Automatic update checks

No functional changes from v0.2 (§5 of PRD-v0.2.md): the check settings and
scheduling, the silent-failure behavior, and the "New version available"
dialog are unchanged. The only change is the **location of the update button**:
it is no longer in the Monitor tab's toolbar but at the bottom of the left tab
bar (§3.1), above the About button. It is shown only when a new
release is available, carries the same downward-arrow icon with an "Update"
label (styled like the top tab buttons, §3.1), and opens the same update
dialog.

- **Manual check (new in v0.3):** the Settings tab's **Check for updates**
  button (§3.5) runs the check immediately, bypassing the due condition (the
  period schedule and the failed-attempt throttle) and the enabled flag; a
  successful check records `last_check_unix_ms` (the automatic schedule
  restarts from it) and a found release opens the update dialog.
- **Latest-version dialog (new in v0.3):** when the manual check (§3.5)
  succeeds and finds no newer release, the app shows a dialog with the
  message "You are currently running the latest available version
  <version>." (`<version>` is the current app version) and an OK button that
  closes it; Escape and a backdrop click also close it.

## 6. Config file

The config file location and behavior are unchanged from v0.2
(`$HOME/.ninfer-monitor/config.json`, `--config <path>` override, tolerant
parse, independent field defaults, atomic save, `write_lock()`). Field changes
in this version:

- **`install_folder`** (new, replaces `last_path`) — the NInfer installation
  folder (a string). Saved on the startup screen's Start and on the Settings
  tab's Save. A missing or wrong-typed field falls back to `None`.
  **Migration:** on load, when `install_folder` is absent and the legacy
  `last_path` (v0.2) ends with `logs/server.requests.jsonl`, the folder part of
  that path is used as `install_folder` (one-time migration); otherwise the
  legacy `last_path` is ignored.
- **`configs`** (new) — the registered NInfer configurations: a JSON array of
  objects with `name` (string) and `command_line` (string). A missing or
  malformed entry is skipped independently of the others; a missing or
  wrong-typed field inside an entry falls back to an empty string. Saved on
  Add, Delete, when the edit panel's changes are applied (Control tab), and on
  import from launcher scripts (the automatic import and the Settings tab's
  Import Configurations button, §4.1).
- **`running_config`** (new) — the name of the configuration to restart on
  startup (a string, or `null` when none). Saved on a successful Start/Restart;
  updated when the persisted configuration is renamed; cleared on Stop, when
  the tracked process exits, and when the running configuration is deleted.
  Closing the app does not clear it. A missing or wrong-typed field falls back
  to `null`; a value that does not match a registered configuration name
  (case-insensitive) is normalized to `null` at startup.
- **`log_file_folder`** (new) — the base folder for the app-managed log file
  (a string). The default is the OS temp folder (`std::env::temp_dir()`).
  Saved on the Settings tab's Save. A missing or wrong-typed field falls back
  to the default.
- **`control_split`** (new) — the Control-tab split: the share of the middle
  area given to the configurations list (a number in `0.15..=0.7`, default
  `0.5`); the edit panel takes the rest. Saved when the Control-tab splitter
  is released. A missing or wrong-typed field falls back to the default; an
  out-of-range value is clamped and written back at startup.

All other fields (`log_poll_interval_ms`, `gpu_poll_interval_ms`,
`chart_window_ms`, `max_requests`, `temp_unit`, `split_panels`,
`table_columns`, `window_state`, `update_check_enabled`,
`update_check_period_days`, `last_update_check_unix_ms`) are unchanged from
v0.2.

Example:

```json
{
  "install_folder": "d:/NInfer",
  "log_file_folder": "C:/Users/aleksey/AppData/Local/Temp",
  "log_poll_interval_ms": 500,
  "gpu_poll_interval_ms": 5000,
  "chart_window_ms": 300000,
  "max_requests": 1000,
  "temp_unit": "F",
  "split_panels": { "charts": 0.5, "table": 0.5 },
  "control_split": 0.5,
  "configs": [
    {
      "name": "my-server",
      "command_line": "--model qwen3.8-27b-nvfp4 --port 8080"
    }
  ],
  "running_config": "my-server"
}
```

## 7. Non-functional requirements

The v0.2 NFRs carry over, with the following updates:

- **NFR-1 (Cross-platform).** Builds and runs on Windows 10+, Linux (glibc),
  and macOS 12+. Platform-specific code is confined to the file-tailing,
  window-state, GPU (NVML/NVAPI), and process-control layers. The GPU layer is
  Windows/Linux only; on platforms without NVML support (e.g. macOS) or without
  an NVIDIA GPU/driver, the GPU section is hidden and the rest of the app is
  fully functional. Process control uses the safe `std::process` API; the
  platform differences are confined to program resolution (the `.exe` suffix)
  and process termination.
- **NFR-2 (Performance).** Steady-state CPU usage < 5% of one core and RAM <
  300 MB while tailing an actively written log. UI updates are batched (max
  ~4 Hz) and never block the UI thread; parsing runs on a background thread.
  Charts are re-rendered only when the frame could change. Polling the tracked
  child processes (a non-blocking wait per process, at most a handful of
  processes) happens on the UI tick and must not measurably increase the
  steady-state CPU usage.
- **NFR-3 (Latency).** A line written to the log is visible in the UI within
  1 second (log file poll interval + parse + render).
- **NFR-4 (Robustness).** The app must not crash on: malformed JSON, unknown
  events/fields, empty file, deleted file, truncated/rotated file, extremely
  long lines (up to 10 MB), a log written faster than it can be read, NVML
  initialization/polling failures, a failed process launch (missing
  executable, invalid command line), a server process that exits unexpectedly,
  a missing or invalid installation folder, a missing or invalid log file
  folder, a failed log file deletion, or an empty configuration list.
- **NFR-5 (Correctness).** Metrics must be computed exactly from the log fields
  defined in the v0.1 PRD §7.2/§9; no estimation. All timestamps are rendered
  in local time with millisecond precision.
- **NFR-6 (Usability).** Single-window app, usable at 1280×800 including the
  left tab bar; sensible defaults so the app is useful with zero configuration.
- **NFR-7 (Quality).** Rust edition 2024, `cargo clippy` and `cargo fmt` clean,
  unit tests for the parser and metric computations. No `unsafe` code except
  the contained Windows FFI in `src/window_state.rs` (monitor enumeration), the
  GPU layer (NVML/NVAPI), and `src/nvidia_popup.rs`; process management uses
  the safe `std::process` API and introduces no new `unsafe`.

## 8. Milestones

Milestones are incremental and buildable: at the end of each milestone the app
compiles, runs, and the milestone's "Done when" criteria are met. They build on
the completed v0.2 milestones.

### M1 — Left tab bar and window re-layout

Implement the new window layout per §3.1.

- Add the fixed-width (96 px) vertical tab bar on the left: the Control,
  Monitor, and Settings tabs at the top (icon + label, the active tab
  highlighted) and the Update (visible only when an update is available) and
  About buttons at the bottom (icon + label, styled the same as the top tab
  buttons, stacked vertically, the same size as the top tab buttons).
- Convert the v0.2 settings dialog to the Settings tab (the folder selector,
  the sliders, the update-check settings, and the _Disable NVIDIA popup_ link,
  with the Save/Discard buttons per §3.5); move the settings and update
  buttons out of the Monitor tab's toolbar (the toolbar keeps only the window
  selector); wire the left-bar tabs and buttons to the Settings tab, the
  update dialog, and the About dialog.
- The status bar spans the bottom of the window and is visible on all tabs.
- The Control tab is the default on startup; the tab state is not persisted.
- New icons in `src/ui/icons/` (same style as the existing ones): the Control
  tab icon (power symbol), the Monitor tab icon (pulse line), and the About
  "info" icon; the Settings tab uses the existing gear icon.
- Remove the About link from the Settings tab.
- Update the affected tests and screenshots.

**Done when:** the window shows the left tab bar with the three tabs at the
top and the two buttons at the bottom; the Control tab opens by default; the
Monitor tab is the v0.2 dashboard with a toolbar that contains only the window
selector; the Settings tab shows the v0.2 settings content with the folder
selector and the Save/Discard buttons; the Update button appears in the left
bar only when a new release is available and opens the update dialog; the
About button opens the About dialog; the Settings tab has no About link;
`cargo clippy` and `cargo fmt --check` are clean.

### M2 — NInfer installation folder

Implement the installation-folder selection per §3.2.

- Folder validity: a folder is valid only if it is an existing directory that
  contains the `ninfer-serve` executable (with the OS-specific extension, e.g.
  `ninfer-serve.exe` on Windows); the check is a pure, unit-testable function.
- Startup screen per §3.2.1: the folder text box (current folder, editable)
  and the Open Folder… button (folder selection dialog); the two text messages
  (no folder configured vs. invalid folder); Start enabled only for a valid
  folder; Start persists the folder and switches to the Control tab (the
  Monitor tab stays Disconnected until a configuration is started);
  Close/Escape exits.
- CLI: the positional argument is the installation folder
  (`ninfer-monitor <install-folder>`); a valid folder skips the startup screen.
- Settings tab per §3.5: the folder selector replaces the file selector; the
  new **Log file folder** field (a text box + Open Folder…, the default is the
  OS temp folder); Save is enabled only when the installation folder contains
  the executable and the log file folder is an existing directory; on Save,
  re-determine the monitored log file, reopen the tail, backfill, reset the
  in-memory state, and persist the folders.
- Config: the `install_folder` field (replacing `last_path`) with the one-time
  migration rule (§6); the legacy `last_path` is otherwise ignored.
- The status bar shows the NInfer installation folder (the label is
  "NInfer folder:").
- Update the affected tests and screenshots.

**Done when:** with no configured folder the startup screen shows the "no
folder configured" message and Start is disabled; selecting a valid folder
(an existing directory containing the `ninfer-serve` executable) enables Start
and opens the Control tab (the Monitor tab stays Disconnected until a
configuration is started); a folder without the executable shows the
invalid-folder message and Start stays
disabled; a valid CLI folder skips the startup screen; changing the folder in
the Settings tab and pressing Save re-tails the re-determined log file
(backfill verified against the file contents) and resets the KPIs/charts/table;
the new folder survives an app restart; the Log file folder field defaults to
the OS temp folder, can be changed, and survives an app restart; the status bar
shows the NInfer installation folder; `cargo clippy` and `cargo fmt --check`
are clean.

### M3 — Configurations (Control tab)

Implement the Control tab per §3.4 and the `configs` config field per §6.

- The Control tab layout: the button row (Start / Restart / Stop on the left,
  Add / Delete on the right, with the new icons), the configurations list
   (selection highlight, the green running dot, the red error row/dot,
   scrolling), the draggable splitter between the list and the edit panel (the
   list's share of the middle area persisted as `control_split`), the edit
   panel (single-line name text
   box, multi-line command-line text box, the "No configuration selected"
   placeholder), the status line (errors shown in red), and the delete
   confirmation dialog (the same dialog style as the about / update dialogs).
- Config: the `configs` field (a list of `{name, command_line}`) with tolerant
  parsing (a missing or malformed entry is skipped independently of the
  others), and the `control_split` field (the list's share of the middle area,
  clamped to `0.15..=0.7`, default `0.5`).
- Add: creates a configuration with a unique default name ("Config N") and an
  empty command line, selects it, and persists it.
- Delete: shows a confirmation dialog naming the selected configuration;
  accepting it removes the configuration (stopping its process first if it is
  running, M4), updates the list and the config file, and shows the placeholder
  when the selected row is gone; Cancel / `Escape` / a backdrop click closes
  the dialog without deleting anything.
- Edits: applied immediately (when the user pauses typing or when focus
  changes) after the basic checks (name non-empty and unique
  case-insensitively; command line non-empty); on success the configuration is
  updated and persisted and the status line shows "Saved."; on failure the
  status line shows the error and nothing is changed.
- Selecting a row loads it into the edit panel.
- The Start / Restart / Stop buttons are present with the enablement rules of
  §3.4 (the process actions are wired in M4).
- Unit tests for the config round-trip and the name validation; update the
  affected tests and screenshots.

**Done when:** Add creates a configuration with a unique default name and an
empty command line; Delete shows a confirmation dialog and, when accepted,
removes the selected configuration (Cancel / `Escape` / a backdrop click closes
the dialog without deleting anything); the edit panel
edits the name and command line and the changes are applied and persisted
immediately (on pause of typing / on focus change; an empty or duplicate name
or an empty command line shows the error and changes nothing);
the configurations survive an app restart (including a config file with a
malformed entry, which is skipped without losing the others); `cargo clippy`
and `cargo fmt --check` are clean.

### M4 — Process control (Start / Restart / Stop)

Implement the process control per §4.

- The command-line tokenization (whitespace split, newlines and carriage
  returns replaced with spaces, double quotes, escaped quotes; no shell
  interpretation) as a pure, unit-tested function; the program path
  construction (the program is always `ninfer-serve` in the
  installation folder, with the OS-specific extension, e.g. `ninfer-serve.exe`
  on Windows) and the existence check (an error is shown when the file does
  not exist).
- Log file determination per §4.2: scan the tokenized command line for
  `--request-log-jsonl` (space and `=` forms, last occurrence wins; relative
  paths resolve against the installation folder); if present, the Monitor tab
  tails that file; if absent, append
  `--request-log-jsonl <log-file-folder>/ninfer-monitor/server.requests.jsonl`
  to the argument vector (the `ninfer-monitor` subfolder is created if
  missing) and tail that file; the Monitor tab switches to the new log file on
  Start/Restart and keeps it until another configuration starts (Disconnected
  before the first start).
- Log file reset per §4.2: when the app appends the log file parameter, delete
  the previous log file before start (on Restart, after the old process has
  exited); a configuration's explicit `--request-log-jsonl` file is never
  deleted; a deletion failure does not block the start (the error is shown in
  the status line).
- Start: spawn `ninfer-serve` from the installation folder with the
  configuration's options as arguments, with the working directory set to the
   installation folder and stdout/stderr piped and drained in the background;
   on success mark the configuration running (the green dot), clear its error
   state, and show "Started."; on failure show "Error: failed to start: …" in
   the status line and mark the configuration red.
- Restart: stop the tracked process (if any) and wait for its exit, then start
  a new process.
- Stop: terminate the tracked process (a direct kill) and wait for its exit;
  clear the running state and show "Stopped.".
- Exit detection: on every UI tick, poll each tracked child with a
  non-blocking wait; when a process has exited (crash, external kill, or
  Stop), clear the running indicator and re-enable Start. A failed exit marks
  the configuration red and shows an error in the status line with the exit
  code and the captured console tail (§4.2).
- Preconditions: a configured installation folder and a non-empty command
  line; otherwise the status line shows the error and nothing is started.
- The in-memory running state is per app instance (not persisted); on exit the
  app stops every tracked server process (a direct kill and a wait for the
  exit), so the processes release their resources (in particular their GPU
  memory). The persisted `running_config` name is saved on a successful
  Start/Restart, cleared on Stop, on a detected process exit, and when the
  running configuration is deleted, and is not cleared by the app exit; on
  startup (a valid configured or CLI installation folder) and after the
  startup screen's Start, the app restarts the persisted configuration if it
  still exists.
- Unit tests for the tokenization and the program resolution (platform cases);
  integration tests for the launch/exit tracking using a short-lived test
  command (e.g. `timeout` / `sleep`); update the affected tests and
  screenshots.

**Done when:** Start launches `ninfer-serve` from the installation folder with
the configuration's options (working directory = the installation folder) and
the configuration shows the running indicator; a configuration with
`--request-log-jsonl` in its command line is tailed at that path, and a
configuration without it is launched with the appended
`--request-log-jsonl <log-file-folder>/ninfer-monitor/server.requests.jsonl`
and tailed at that path; the previous app-managed log file is deleted before
start (after stop on a restart) and a configuration's explicit log file is
never deleted; Stop
terminates the process and clears the indicator; Restart stops and starts; a
server process that exits on its own is detected within one UI tick (the
indicator is cleared and the Start / Restart / Stop enablement is refreshed,
§3.4); a failed launch (missing executable) shows the error in the status line
and marks the configuration red without crashing; a process that exits with a non-zero exit code marks the
configuration red and shows the exit code and the captured console tail in the
status line; closing the app stops every tracked server process (releasing its
GPU memory); the in-memory running state is not persisted across app restarts,
but a configuration that was still running when the app closed is restarted on
the next start (a configuration that was stopped, exited, or deleted before the
app closed is not); `cargo clippy` and `cargo fmt --check` are clean.

### M5 — Hardening & v0.3 acceptance

Close out the v0.3 NFRs (§7) and run the full acceptance suite per §9.

- Robustness passes: failed process launch, a process that exits unexpectedly,
  a missing/invalid installation folder, an empty configuration list, NVML
  init/polling failures, no-GPU machines, macOS build (NFR-1/NFR-4).
- Performance validation: steady-state CPU < 5% of one core and RAM < 300 MB
  with the per-tick child-process polling (NFR-2); < 1 s end-to-end latency
  (NFR-3).
- Cross-platform: release builds on Windows, Linux, and macOS (NFR-1).
- `cargo clippy`, `cargo fmt --check`, full test suite green (NFR-7).

**Done when:** all the acceptance criteria in §9 pass on all three platforms;
v0.3 tagged.

### M6 — Configuration import from launcher scripts

Implement the configuration import per §4.1 and the Settings tab's
**Import Configurations** button per §3.5.

- The launcher-script parser (`src/config_import.rs`, pure, no UI): the dialect
  (batch on Windows, shell on Linux), the logical-line joining across the
  line-continuation marker (`^` in batch, `\` in shell), the comment skipping
  (`rem`/`::` lines in batch, `#` lines in shell), the direct-invocation
  detection (the first token names `ninfer-serve` with the OS-specific
  extension, a path prefix allowed), and the command-line extraction (the
  arguments after the program token, whitespace-normalized; a bare invocation
  yields nothing).
- The folder scan: one configuration per launcher script (`.bat` on Windows,
  `.sh` on Linux) that invokes `ninfer-serve` directly, named after the file
  name (without the extension), sorted by name; the case-insensitive dedup
  against the existing configuration names and within the folder; an
  unreadable folder or script is skipped silently.
- Automatic import: when the installation folder is selected (the startup
  screen's Start, the Settings tab's Save, or a valid CLI folder) and the app
  has no configurations yet, pre-create the configurations from the folder's
  launcher scripts and persist them (the Control tab's status line shows
  "Imported N configurations."); when the app already has configurations,
  import nothing automatically.
- Manual import: the Settings tab's **Import Configurations** button (right of
  the installation folder's Open Folder… button, enabled when the folder text
  box points to an existing directory) imports from the currently selected
  folder, deduplicating against the existing names; the outcome is shown in
  the status line below the folder row ("Imported N configurations." / "No new
  configurations found." / the error) and the imported configurations are
  persisted.
- Unit tests for the parser (single-line and multi-line scripts with
  continuations and comments, a script without an invocation, a commented-out
  invocation, a bare invocation, a quoted program path, an indirect
  invocation) and the folder scan (the platform scripts, the dedup, the
  missing folder).
- Update the affected tests and screenshots.

**Done when:** with no configurations, selecting an installation folder that
contains launcher scripts (e.g. `C:\NInfer`) pre-creates a configuration per
launcher script that invokes `ninfer-serve` directly (named after the file,
the command line the script's arguments after the program token) and persists
them, while a folder without launcher scripts (or an app that already has
configurations) imports nothing; the Settings tab's Import Configurations
button imports from the currently selected folder, skips the names that
already exist (case-insensitive), and shows the outcome in the status line; a
script that does not invoke `ninfer-serve` directly (or a commented-out
invocation) yields no configuration; `cargo clippy` and `cargo fmt --check`
are clean.

## 9. Acceptance criteria (v0.3)

1. `cargo build --release` succeeds on Windows, Linux, and macOS.
2. The window shows the left vertical tab bar (96 px): the Control, Monitor,
   and Settings tabs at the top and the Update (shown only when a new release
    is available) and About buttons at the bottom; the Control tab is the
    default on startup; the Monitor tab is the v0.2 dashboard and its toolbar
    contains only the window selector (1m / 5m / 15m / 30m / 1h).
3. The startup screen shows the NInfer installation folder (an editable text
   box + Open Folder…); Start is enabled only for a valid folder (an existing
   directory containing the `ninfer-serve` executable); running
   `ninfer-monitor <folder>` with a valid folder skips the startup screen;
   Close or Escape exits the app.
4. The status bar shows the NInfer installation folder; the Monitor tab tails
   the log file of the last-started configuration: a configuration with
   `--request-log-jsonl` in its command line is tailed at that path, and a
   configuration without it is launched with
   `--request-log-jsonl <log-file-folder>/ninfer-monitor/server.requests.jsonl`
   appended (the `ninfer-monitor` folder is created if missing) and tailed at
   that path; the previous app-managed log file is deleted before start (after
   stop on a restart) and a configuration's explicit log file is never
   deleted; when the log does not exist yet, the Monitor tab shows
   Disconnected and goes Live when the file appears.
5. Changing the installation folder or the log file folder in the Settings tab
   and pressing Save switches the tail to the re-determined log file (backfill
   from the start) and resets the KPIs, charts, and request table; the new
   folders survive an app restart.
6. The Control tab lists the registered configurations; Add creates a new
   configuration (a unique default name, an empty command line) and selects
    it; Delete shows a confirmation dialog and, when accepted, removes the
    selected configuration (Cancel / `Escape` / a backdrop click closes the
    dialog without deleting anything); the edit panel edits the
   name and command line and the changes are applied and persisted immediately
   (on pause of typing / on focus change; an empty or duplicate name or an
   empty command line shows the error and changes nothing); the configurations
   survive an app restart.
7. Start launches `ninfer-serve` from the installation folder with the
    configuration's options (the working directory is the installation folder)
    and the configuration shows the running indicator; a successful Start or
    Restart persists the configuration's name as `running_config`; Stop
    terminates the process and clears `running_config` when it named the
    stopped configuration; Restart stops (if running) and starts a new
    process.
8. A server process that exits on its own is detected within one UI tick: the
   running indicator is cleared, the Start / Restart / Stop enablement is
   refreshed (§3.4), and `running_config` is cleared when it named the exited
   configuration.
9. When the app opens directly to the dashboard (a valid configured or CLI
   installation folder) or after the startup screen's Start, the persisted
   `running_config` configuration is restarted if it still exists; closing the
   app leaves `running_config` set, while Stop, a detected process exit, and
   deletion of the running configuration clear it.
10. A failed launch (e.g. a missing executable) shows the error in the Control
    tab's status line and marks the configuration red; a process that exits
    with a non-zero exit code marks the configuration red and shows the exit
    code and the captured console tail in the status line; the app does not
    crash.
11. The About button in the left tab bar opens the About dialog; the Update
    button (when available) opens the "New version available" dialog; the
    Settings tab has no About link; the Settings tab's "Check for updates"
    button runs an immediate check, opens the dialog when a newer release is
    found, and shows the "Up to date" dialog (with the current version and an
    OK button) when no newer release is found.
12. With no configurations, selecting an installation folder that contains
    launcher scripts (`.bat` on Windows, `.sh` on Linux) pre-creates a
    configuration per script that invokes `ninfer-serve` directly (named after
    the file name, the command line the script's arguments after the program
    token) and persists them; an app that already has configurations imports
    nothing automatically; the Settings tab's Import Configurations button
    imports from the currently selected folder, skipping the names that
    already exist (case-insensitive), and shows the outcome in the status line
    (§4.1).
13. `cargo clippy` and `cargo fmt --check` report no issues and the full test
    suite passes.

## 10. Risks & open questions

| # | Risk / Question | Mitigation |
|---|---|---|
| R1 | The app-managed log location (`<log-file-folder>/ninfer-monitor/server.requests.jsonl`) is shared across all configurations without an explicit `--request-log-jsonl`; two such configurations started at the same time would write to the same file. | The Monitor tab tails the log of the last-started configuration; the user can give each configuration an explicit `--request-log-jsonl` path to separate them. |
| R2 | A configuration's `--request-log-jsonl` may point to a file the app cannot read (permissions, a path that never appears). | The tail layer already handles a missing/unreadable file with the Disconnected state and retry; the state is visible in the status bar. |
| R3 | Stop is a hard kill (no graceful shutdown signal); the server has no chance to flush in-flight state. | The log is JSON Lines (one line per event, written as it completes), so at most an incomplete trailing line is lost — the tail layer already buffers incomplete lines (FR-1.4). |
| R4 | The program is always `ninfer-serve` from the installation folder; a configuration cannot launch a different program. | Documented in §4.2; the existence check shows an error when the executable does not exist. Launching an arbitrary program is out of scope for v0.3. |
| R5 | Deleting the previous app-managed log file before start loses the history of the previous run. | Only the app-managed path is deleted; a configuration's explicit `--request-log-jsonl` file is never touched, so the user can keep a history by setting an explicit log path. |
| OQ1 | Should the app display the server's stdout/stderr (a console pane in the Control tab)? | Deferred; the output is piped and drained in v0.3. |
| OQ2 | Should the running state be persisted and the app stop the servers on exit? | The app stops the servers it started on exit (§4.4). The in-memory running state remains per app instance, but the name of the running configuration is persisted as `running_config` and is used to restart it on the next start unless it was stopped, exited, or deleted before the app closed. |
