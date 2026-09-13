# NInferMonitor — Product Requirements Document

| | |
|---|---|
| **Version** | v0.1 |
| **Status** | Draft |
| **Date** | 2026-09-13 |
| **Stack** | Rust, Slint (UI), Plotters (charts) |
| **Platforms** | Windows, Linux, macOS |

---

## 1. Overview

NInferMonitor is a cross-platform desktop application that tails the NInfer server
request log (`server.requests.jsonl`, JSON Lines format) in real time — similar to
`tail -f` — and presents the core operational details of a running NInfer inference
server in a graphical dashboard: token throughput, request latency, TTFT, KV-cache
occupancy, scheduler state, and request-level details.

The log is produced by `ninfer-serve` via the `--request-log-jsonl` flag.

## 2. Background & Problem

NInfer operators currently inspect server health by reading a raw JSONL log file
with a terminal (`tail -f`, `jq`). This is slow, error-prone, and makes it hard to
see trends (throughput over time, latency spikes, cache pressure) at a glance.
There is no graphical tool that turns this log into live, actionable metrics.

## 3. Goals

- **G1.** Tail a local NInfer request log file in real time and render new events
  within ~1 second of them being written.
- **G2.** Show core server metrics live: decode/prefill token throughput (tok/s),
  TTFT, request latency, tokens per request, prefix-cache hits, speculative
  decoding acceptance, KV-cache occupancy, scheduler state.
- **G3.** Show per-request details in a table (id, tokens, timings, finish reason,
  errors).
- **G4.** Show server/engine context: GPU, model, engine configuration, memory
  layout (from the `server_start` event).
- **G5.** Be cross-platform (Windows, Linux, macOS) with a single codebase.
- **G6.** Be robust: tolerate malformed lines, unknown fields/events, log
  rotation/truncation, and a missing or locked file without crashing.

## 4. Non-Goals (v0.1)

- No remote log sources (SSH, HTTP, network streams) — local file only.
- No log writing, filtering, or modification of the NInfer server.
- No multi-file / multi-server aggregation (one file per app instance).
- No persistence of history (no database); in-memory only, bounded.
- No authentication, no i18n (English UI only).
- No mobile targets.

## 5. Target Users

- **Primary:** ML engineers / developers running `ninfer-serve` locally or on a
  workstation, who want a live "vitals" view of their inference server while
  benchmarking or developing.
- **Secondary:** Operators debugging throughput regressions, cache pressure, or
  request errors during a session.

## 6. User Stories

1. As a developer, I open NInferMonitor, point it at my `server.requests.jsonl`,
   and immediately see the current decode/prefill throughput and active requests.
2. As a developer, I watch the throughput chart update live as my benchmark runs,
   so I can spot stalls or regressions without leaving the GUI.
3. As a developer, I see each completed request in a table with prompt/completion
   token counts, TTFT, total time, and finish reason, so I can verify correctness
   and performance per request.
4. As a developer, I see request errors (e.g. "inference request expired while
   waiting for admission", "client disconnected") highlighted, so I know when the
   server dropped work.
5. As a developer, I see KV-cache occupancy and scheduler queue depth, so I can
   tell whether the server is saturated.
6. As a developer, I can pause/resume the live view and scroll through recent
   requests without the view jumping.
7. As a developer, I can open a historical log file and see its full contents
   rendered (backfill), not just new lines.
8. As a developer, I see the server's GPU, model, and engine configuration once
   at the top of the dashboard for context.

## 7. Functional Requirements

### 7.1 Log file monitoring (tail)

- **FR-1.1** The user selects a log file via a file picker dialog; the path is
  also accepted as a command-line argument (`ninfer-monitor [path]`) and as the
  last-used path (persisted in settings).
- **FR-1.2** The app opens the file and follows it like `tail -f`: it polls for
  appended data (default interval 500 ms, configurable 100–5000 ms) and parses
  newly appended complete lines.
- **FR-1.3** On first open, the app backfills the existing file contents (parses
  the whole file) so historical data is shown, then continues tailing. Backfill of
  the sample file (~6 MB, ~2600 lines) must complete in under 2 seconds.
- **FR-1.4** Incomplete trailing lines (no terminating newline yet) are buffered
  and parsed once the line completes.
- **FR-1.5** File truncation/rotation is detected (file size < read offset, or
  inode/identity change) and the app reopens from the start of the new file.
- **FR-1.6** If the file is deleted or temporarily unreadable, the app shows a
  "disconnected" status and keeps retrying; it does not crash. On Windows the
  file must be opened with shared read/write/delete access so a running
  `ninfer-serve` can keep writing.
- **FR-1.7** The UI shows connection status: file path, Live / Paused /
  Disconnected, and the time of the last received event.

### 7.2 Parsing

- **FR-2.1** Each line is parsed as a JSON object (JSONL). All events carry
  `artifact_type: "ninfer_serve_request_log"`, `schema_version`,
  `server_instance_id`, and `timestamp_unix_ms`.
- **FR-2.2** Supported events (schema_version 20):
  - `server_start` — server/engine/environment/memory configuration (once per
    server instance).
  - `request_start` — a request was admitted; carries `request_id`, model,
    sampling params, `preparation_seconds`.
  - `request_done` — a request completed; carries `result` (tokens, finish
    reason, prefix-cache hits), `timings_seconds` (ttft, prefill, decode,
    total), `engine_timing`, `speculative` stats.
  - `throughput` — periodic engine stats (default every 5000 ms per
    `engine.log_stats_interval_ms`); carries
    `throughput_tokens_per_second.{decode,prefill}`, `tokens`, `decode_batch`,
    `scheduler`, `context_cache` occupancy/pressure, `host_work`.
  - `request_error` — a request failed; carries `error.message`.
- **FR-2.3** Malformed lines (invalid JSON, missing required fields) are skipped
  and counted; the app never crashes on bad input. A "skipped lines" counter is
  shown in the status bar.
- **FR-2.4** Unknown events and unknown fields are ignored (forward
  compatibility with newer schema versions). If `schema_version` is greater than
  the supported version, the app shows a non-blocking warning banner but still
  renders whatever fields it understands.
- **FR-2.5** Request lifecycle is tracked by `request_id`: `request_start` opens
  an in-flight entry; `request_done`/`request_error` closes it. Requests that
  never close (e.g. server killed) are marked "in-flight" and aged out after
  24 h.

### 7.3 Live metrics dashboard (KPI values)

- **FR-3.1** The dashboard shows live KPI values (two per metric widget, see
  §10), updated at least every 500 ms:
  - Decode throughput (tok/s) — latest `throughput_tokens_per_second.decode`.
  - Prefill throughput (tok/s) — latest `throughput_tokens_per_second.prefill`.
  - Active requests (Scheduler widget, left KPI) — count of open
    `request_id`s. This is distinct from the Scheduler chart's
    `scheduler.running` series (requests the engine is running); the KPI
    counts open `request_id`s tracked by the app.
  - Waiting requests (Scheduler widget, right KPI) — latest
    `scheduler.waiting` from the `throughput` event.
  - Avg TTFT (last 60 s) — mean of `timings_seconds.ttft` over completed
    requests in the window.
  - Avg decode latency per token (last 60 s) — mean of
    `timings_seconds.decode / result.completion_tokens`.
  - Prefix-cache hit rate (last 60 s) —
    `sum(prefix_cache_hit_tokens) / sum(prompt_tokens)`.
  - Speculative acceptance (last 60 s) —
    `sum(speculative.accepted_tokens) / sum(speculative.drafted_tokens)`.
  - Errors — count of `request_error` events since server start; shown in the
    status bar (the §10 widgets have no slot for it).
- **FR-3.2** Each KPI shows a label, the current value, and a small trend
  indicator (up/down vs. the previous 60 s window); the unit is shown in the
  widget title. The arrow is colored by
  whether the change is favorable for that KPI: green when moving in the
  "good" direction, red when moving in the "bad" direction, and gray when flat
  (no change or no previous-window data). "Up is good" for Decode, Prefill,
  Cache hit, and Speculative acceptance; "down is good" for TTFT, Decode
  latency/token, and Waiting requests. Active requests show no trend (always
  flat). (The "down is good" rule for Errors is moot: Errors is shown in the
  status bar with no trend indicator.)
- **FR-3.3** KPIs with no data yet show "—" (not 0 or NaN).

### 7.4 Time-series charts (Plotters)

- **FR-4.1** Charts are rendered with Plotters into image buffers and displayed
  in the Slint UI; they refresh at 2 Hz while live.
- **FR-4.2** Required charts, driven by a single global time-window selector
  (1 min / 5 min / 15 min / 30 min / 1 h, default 5 min) that applies to all
  charts. Four equal-sized widgets (see §10):
  - **Throughput:** decode tok/s and prefill tok/s over time (two series, from
    `throughput` events).
  - **Latency:** TTFT and per-token decode latency per completed request over
    time (two series, from `request_done.timings_seconds`).
  - **Cache (%):** prefix-cache hit rate and speculative acceptance over time
    (two series, from `request_done` results).
  - **Scheduler:** `running` and `waiting` request counts over time (two
    series, from `throughput.scheduler`).
  KV-cache occupancy (`context_cache.occupancy`) is deferred to v0.2 (see §14).
- **FR-4.3** Each chart is a dual-axis line chart: the first series is drawn
  against the left y-axis, the second against the right y-axis. Line colors
  are unified across all charts — the left-axis line is always yellow
  (`#ffb74d`), the right-axis line always blue (`#4fc3f7`). Y-axis tick
  values are shown on both sides, each colored to match its line; x-axis
  tick labels are hidden (the window selector defines the time range).
  Units are shown in the widget title; the series are identified by the KPI
  labels flanking the chart (no in-chart legend).
- **FR-4.4** Chart history is kept in bounded ring buffers (e.g. max 10 000
  points per series); memory usage stays bounded for multi-hour sessions.
- **FR-4.5** When the two series' maxima are within 2× of each other, both
  y-axes share the same scale (0 to the larger maximum, with 10% headroom)
  so the lines are directly comparable; otherwise each axis scales to its
  own series.

### 7.5 Request table

- **FR-5.1** A table lists requests, newest first, with columns: an
  expand/collapse symbol (first column, reflecting the row's expanded state),
  `request_id`, start time, model, prompt tokens, completion tokens, thinking
  tokens, TTFT, total time, finish reason, status.
- **FR-5.2** Status values: `in-flight` (started, not done), `done`,
  `error` (with the error message shown in the row / on hover).
- **FR-5.3** Rows are color-coded: normal (done), warning (slow: total time >
  p95 of the completed requests currently held in the table, i.e. the most
  recent N since the latest `server_start` (N is the "max requests" setting,
  FR-5.5), error (red).
- **FR-5.4** Clicking a row expands its detail inline, directly below the row
  in the table, with the full `request_done` payload: sampling parameters,
  `engine_timing` breakdown, `speculative` stats, `tool_call_parse`,
  prefix-reuse path (plus the error message for `request_error`). Clicking
  anywhere on an expanded row collapses it again. Multiple rows may be
  expanded at the same time; the expand/collapse symbol in the first column
  reflects each row's state.
- **FR-5.5** The table keeps the most recent N requests in memory (N is the
  "max requests" setting, 10–1 000, default 1 000); older rows are dropped.
- **FR-5.6** While live, the table auto-scrolls to new rows unless the user has
  scrolled up (standard "pause on scroll" behavior).

### 7.6 Server info panel

- **FR-6.1** A panel (always visible) shows data from the latest `server_start`:
  - Model: `artifact.target`, `artifact.weights_id`, `artifact.size_bytes`,
    load/upload seconds.
  - Engine: `engine.model_id`, `kv_cache`, `kv_capacity`,
    `max_context`, `max_concurrency`, `prefill_chunk`, `speculative_backend`,
    `cuda_graph`, `prefix_reuse`.
  - GPU: `environment.gpu_name`, compute capability, CUDA driver/runtime
    versions, `total_device_memory_bytes`.
  - Memory: weights / KV / workspace capacity and used bytes
    (`memory.weights`, `memory.sequence`, `memory.workspace`,
    `available_after_weights_bytes`).
  - Server: host:port, `server_instance_id`, `server.public_model_id`, log path.
- **FR-6.2** If a new `server_start` appears (server restart), the panel updates
  and a "server restarted" marker is drawn on the charts.

### 7.7 Controls & settings

- **FR-7.1** Controls: Open file…, Pause/Resume, Clear in-memory state,
  chart time-window selector.
- **FR-7.2** Settings (persisted to a JSON config file at
  `$HOME/.ninfer-monitor/config.json`, where `$HOME` is platform-specific):
  last log path, poll interval, chart window, max requests, the split-panel
  fractions (`split_panels`, FR-7.5), and the main-window geometry (FR-7.4).
- **FR-7.3** Command-line: `ninfer-monitor [path]` opens the given file on
  startup; `--poll <ms>` overrides the poll interval.
- **FR-7.4** The main window's geometry is persisted and restored across
  restarts. On close the app saves the window's position, size, and maximized
  state (in logical pixels) to the config file; on startup it restores them.
  The saved window is matched to a currently connected monitor by its center
  point; if that monitor no longer exists (e.g. an external display was
  disconnected), the window opens at the default size/position instead. The
  restored geometry is clamped so the window fits within the matched monitor.
  The minimized state is not persisted (the window always opens in its normal
  or maximized state).
- **FR-7.5** The dashboard's middle area is divided into three vertically
  stacked sections — the charts, the request table, and the server-info panel —
  by two draggable splitters. The division is described by two fractions:
  `charts` (the share of the section space given to the charts, default 0.5,
  clamped to 0.1–0.8) and `table` (the share of the *remaining* space given to
  the request table, default 0.5, clamped to 0.1–0.9); the server-info panel
  takes whatever is left. Dragging a splitter resizes the section above it
  live (the fraction is clamped to its range on every move); releasing
  persists the fractions to the config as `split_panels` (FR-7.2). The
  fractions are restored on startup.

## 8. Non-Functional Requirements

- **NFR-1 (Cross-platform).** Builds and runs on Windows 10+, Linux (glibc),
  and macOS 12+. No platform-specific code outside the file-tailing and
  window-state layers.
- **NFR-2 (Performance).** Steady-state CPU usage < 5% of one core and RAM <
  300 MB while tailing an actively written log. UI updates are batched (max
  ~4 Hz) and never block the UI thread; parsing runs on a background thread.
- **NFR-3 (Latency).** A line written to the log is visible in the UI within
  1 second (poll interval + parse + render).
- **NFR-4 (Robustness).** The app must not crash on: malformed JSON, unknown
  events/fields, empty file, deleted file, truncated/rotated file, extremely
  long lines (up to 10 MB), or a log written faster than it can be read (the
  reader must catch up without unbounded memory growth).
- **NFR-5 (Correctness).** Metrics must be computed exactly from the log fields
  defined in §7.2/§9; no estimation. All timestamps are rendered in local time
  with millisecond precision.
- **NFR-6 (Usability).** Single-window app, usable at 1280×800; sensible
  defaults so the app is useful with zero configuration (open file → see data).
- **NFR-7 (Quality).** Rust edition 2024, `cargo clippy` and `cargo fmt` clean,
  unit tests for the parser and metric computations (using the sample log as a
  fixture). No `unsafe` code except the contained Windows FFI in
  `src/window_state.rs` (monitor enumeration for FR-7.4), which is isolated in
  a single module and documented with `SAFETY` comments.

## 9. Data Model

Common envelope for every line:

```json
{
  "artifact_type": "ninfer_serve_request_log",
  "schema_version": 20,
  "server_instance_id": "serve-27872-1789281769048204",
  "timestamp_unix_ms": 1789281813959,
  "event": "<event name>",
  ...
}
```

| Event | Key fields consumed by the app |
|---|---|
| `server_start` | `argv`, `artifact.{target,weights_id,size_bytes,load_seconds,upload_seconds,tensor_count}`, `engine.{model_id,kv_cache,kv_capacity,max_context,max_concurrency,prefill_chunk,speculative_backend,cuda_graph,prefix_reuse,log_stats_interval_ms}`, `environment.{gpu_name,compute_capability_*,cuda_*_version,total_device_memory_bytes}`, `memory.{weights,sequence,workspace,available_after_weights_bytes,host_kv_capacity_bytes}`, `server.{host,port,public_model_id,request_log_jsonl}` |
| `request_start` | `request.{request_id,model,protocol,message_count,tool_count,stream,sampling.*}`, `preparation_seconds.{tokenize,total}` |
| `request_done` | `request.request_id`, `result.{prompt_tokens,completion_tokens,model_thinking_tokens,finish_reason,prefix_cache_hit_tokens,prefix_reuse_path,tool_call_count,tool_call_parse}`, `timings_seconds.{ttft,prefill,decode,prepare,total,vision}`, `engine_timing.{queue_wait_seconds,decode.rounds,units}`, `speculative.{backend,drafted_tokens,accepted_tokens,accepted_per_position,fallback_steps,rounds}` |
| `throughput` | `throughput_tokens_per_second.{decode,prefill}`, `tokens.{committed_decode,computed_prefill}`, `decode_batch.{rounds,average_size}`, `scheduler.{running,prefilling,waiting,materializing,decode_ready,capture_pending,terminal_pending}`, `context_cache.occupancy.{device_main_kv_pages,device_backend_kv_pages,host_kv_bytes,host_state_slots,device_state_slots}`, `context_cache.pressure.*`, `host_work.work_class_seconds.*`, `interval_seconds` |
| `request_error` | `request.request_id`, `error.message` |

Observed `finish_reason` values: `stop_token`, `cancelled`.
Observed `error.message` values: `inference request expired while waiting for
admission`, `client disconnected`.

Derived metrics (computed by the app):

| Metric | Formula |
|---|---|
| Decode throughput | `throughput_tokens_per_second.decode` (per `throughput` event) |
| Prefill throughput | `throughput_tokens_per_second.prefill` |
| TTFT | `timings_seconds.ttft` (per request) |
| Decode latency/token | `timings_seconds.decode / result.completion_tokens` |
| Prefix-cache hit rate | `Σ prefix_cache_hit_tokens / Σ prompt_tokens` (windowed) |
| Speculative acceptance | `Σ speculative.accepted_tokens / Σ speculative.drafted_tokens` (windowed) |
| Active requests | open `request_id`s; cross-checked with `scheduler.running` |

## 10. UI Layout

All the four main widgets should have exactly the same size and keep the same size if window
is resized.

```
┌─────────────────────────────────────────────────────────────────────────────────────┐
│ [Open ...] [Pause] [Clear]  [1m] [5m] [15m] [1h]                               [⚙] │
├─────────────────────────────────────────────────────────────────────────────────────┤
│ ┌ Throughput (tok/s) ─────────────────┐ ┌ Latency (ms) ───────────────────────────┐ │
│ │ <throughput widget, see below>      │ │ <latency widget, see below>             │ │
│ └─────────────────────────────────────┘ └─────────────────────────────────────────┘ │
│ ┌ Cache (%) ──────────────────────────┐ ┌ Scheduler ──────────────────────────────┐ │
│ │ <cache widget, see below>           │ │ <scheduler widget, see below>           │ │
│ └─────────────────────────────────────┘ └─────────────────────────────────────────┘ │
├─────────────────────────────────────────────────────────────────────────────────────┤
│ #  Time              Model            Prompt Compl Think TTFT   Total Reason Status │
│ 66 14:02:58.124      qwen3.8-27b-…    42737  —     —    —      —     —      error │
│ 65 14:02:31.001      qwen3.8-27b-…    9589   115   —    310 ms 3.9 s stop   done  │
│ 64 14:02:12.556      qwen3.8-27b-…    645    155   —    422 ms 3.9 s stop   done  │
├─────────────────────────────────────────────────────────────────────────────────────┤
│ ┌ Server info ────────────────────────────────────────────────────────────────────┐ │
│ │ RTX 5090 · qwen3.8-27b-nvfp4 · KV fp8 240k · 24 GB host RAM free              │ │
│ │ Model    qwen3_8_27b (nvfp4) · 22.1 GiB · load 9.7 s                            │ │
│ │ Engine   KV fp8-e4m3-row256 · capacity 240 000 · max context 240 000 · …        │ │
│ │ GPU      NVIDIA GeForce RTX 5090 · SM 12.0 · CUDA driver 13.4 · 31.8 GiB        │ │
│ │ Memory   weights 19.7/19.7 GiB · KV 8.5/8.5 GiB · workspace 0.0/0.3 GiB · …     │ │
│ │ Server   0.0.0.0:8080 · serve-27872-… · log d:/NInfer/logs/…                    │ │
│ └─────────────────────────────────────────────────────────────────────────────────┘ │
├─────────────────────────────────────────────────────────────────────────────────────┤
│ ● Live  14:02:58.124  Log file: d:/NInfer/logs/….requests.jsonl Errors: 0 Skipped 0 │
└─────────────────────────────────────────────────────────────────────────────────────┘
```

### Buttons bar
The buttons bar at the top of the window consists of three groups of buttons.
All buttons share the same height, and the bar has a fixed height that does not
change when the window is resized.

- **Control buttons** (left-aligned, same size): Open file…, Pause/Resume,
  Clear. Each carries a light pastel-blue SVG icon beside its label:
  open-folder (Open file…), pause/play (Pause/Resume — the icon and label
  toggle together with the paused state), and trash (Clear).
- **Window buttons** (1m / 5m / 15m / 30m / 1h): same size, grouped together
  and placed a fixed 2× window-button-width (72 px) to the right of the
  control buttons — not centered between the control and settings groups.
- **Settings button** (right-aligned, square): gear icon.

Icon and label are centered horizontally and vertically within each button.
Icons are 24×24 single-color (light pastel blue, `#a5d8ff`) SVGs in
`src/ui/icons/`.

```
│ [Open ...] [Pause] [Clear]  [1m] [5m] [15m] [30m] [1h]                       [⚙] │
```

### Throughput widget

The Throughput widget has the current / latest Decode throughput on the left side and Prefill
throughput on the right side. In the center, there is dual-axis line graph that features
history decode and prefill throughputs. The y-axis labels on each side use same color as 
the corresponding line in the graph.

```
┌───Throughput (tok/s) ────────────────────────────────────────────┐
│                   ┌─────────────────────────┐                    |
│   Decode         200 |  <dual-axis line graph> | 2k     Prefill  |
│    153.7 ▲      100 |                         | 1k     1461 ▲    |
│                   └─────────────────────────┘                    |
└──────────────────────────────────────────────────────────────────┘
```

### Latency widget

The Latency widget has the current / latest TTFT on the left side and Per-Token
decode latency on the right side. In the center, there is dual-axis line graph that features
history TTFT and Per-Token decode latency. The y-axis labels on each side use same color as 
the corresponding line in the graph.

```
┌────Latency (ms) ─────────────────────────────────────────────────┐
│                   ┌─────────────────────────┐                    |
│   TTFT        500 |  <dual-axis line graph> | 10   Per-Token lat |
│   422 ▼       250 |                         | 5      8.9  ▼      |
│                   └─────────────────────────┘                    |
└──────────────────────────────────────────────────────────────────┘
```

### Cache widget

The Cache widget has the current / latest Cache hit %  on the left side and Spec. 
accept % on the right side. In the center, there is dual-axis line graph that features
history Cache hit % and Spec. accept %. The y-axis labels on each side use same color as 
the corresponding line in the graph.

```
┌───Cache (%) ─────────────────────────────────────────────────────┐
│                   ┌─────────────────────────┐                    |
│   Cache hit   50% |  <dual-axis line graph> | 100%  Spec. accept |
│   22.7% ▲     25% |                         | 50%     77% ▲      |
│                   └─────────────────────────┘                    |
└──────────────────────────────────────────────────────────────────┘
```

### Scheduler widget

The Scheduler widget has the number of current / latest Active requests on the left side 
and Waiting request on the right side. In the center, there is dual-axis line graph that features
history Active and Waiting requests number. The y-axis labels on each side use same color as 
the corresponding line in the graph.

```
┌───Scheduler ─────────────────────────────────────────────────────┐
│                   ┌─────────────────────────┐                    |
│   Active        5 |  <dual-axis line graph> | 5     Waiting      |
│    1 ▼          2 |                         | 2        2 ▲       |
│                   └─────────────────────────┘                    |
└──────────────────────────────────────────────────────────────────┘
```

### Status bar

The status bar is located at the bottom of the window and has a fixed height.

```
│ ● Live  14:02:58.124  Log file: d:/NInfer/logs/….requests.jsonl Errors: 0 Skipped 0 │
```

The status bar has three sections:

- The connection indicator (colored dot + state) is aligned to the left,
  followed by the time of the last received event (FR-1.7). The state is
  Live (green) while tailing, Paused (yellow) when the pause button is
  pressed, "—" (gray) when no file has been selected yet, and
  Disconnected (red) when a file was selected but is no longer readable or
  present.
- The Log file field is placed at equal distance between the indicator and
  the counters. It shows the path of the current log file. The full name is
  shown when it fits the available space (the width is measured from the UI
  layout and re-checked on every tick, so a window resize is picked up);
  when it does not fit, the shortened form is used — names longer than 32
  characters keep 15 characters on each side with … in the middle. When no
  file is selected, the field shows "No file selected".
- The error/skipped counters ("Errors: N Skipped N") are right aligned.

Reserve space in both the Live indicator and the Errors / Skipped counters
sections to ensure that nothing jumps if live <-> paused change happens, or
counters go to 2 digits.

### Settings dialog

The modal settings dialog should be opened when Settings button is clicked:

```
┌──────────────────────────────────┐
│ Settings                       x │
├──────────────────────────────────┤
│ Poll interval: 500 ms            |
│ 100 ms ----*-------------- 10 s  |
│ Max requests: 1000               |
│ 10 -------*-------------- 1000   |
│                                  │
│                                  │
│                                  │
│ _About_                          │
├──────────────────────────────────┤
│           [OK] [Cancel]          │
└──────────────────────────────────┘
```

The poll interval should be a slider and current value printed above. As user changes the 
slider the current value should automatically update. The slider step should be 100ms
with min 100ms and max 10s.

Below the poll interval there is the max requests setting: a slider with the current
value printed above, step 10, min 10 and max 1 000. It bounds how many request rows
the table keeps in memory (FR-5.5); the new value applies to the next opened file.


Below the settings there should be an empty space at least 30% of the dialog's height.

OK/ Cancel buttons should be centered and have same height as buttons on the main screen.

The OK button should save the changed settings values while Cancel, escape key, or clicking
the close button in the top right corner should discard all changes.


### About dialog

The modal About dialog should be opened when "About" link in the Settings dialog is clicked:

```
┌──────────────────────────────────┐
│ About                          x │
├──────────────────────────────────┤
│                                  │
│ NInferMonitor v0.1               |
│ Copyright (C) 2026 Aleksey Sanin │
│                                  │
├──────────────────────────────────┤
│               [OK]               │
└──────────────────────────────────┘
```

OK button should be centered, its height should match the height of buttons
in the settings dialog. Same for x button.

The current version should not be hard coded but an actual version info during
the build should be used.

When OK button, x button, or esc key is pressed, the dialog should close.

## 11. Technical Approach

- **Language/toolchain:** Rust (stable, edition 2024).
- **UI:** Slint (single main window; components: `MainWindow`, `MetricWidget`,
  `RequestTable`, `ServerInfoPanel`, `SettingsPanel`,
  `StatusBar`, `DarkButton`, `Splitter`, `AboutPanel`).
- **UI icons:** button icons (open-folder, pause, play, trash, gear) are 24×24
  single-color (light pastel blue, `#a5d8ff`) SVGs in `src/ui/icons/`, rendered
  by Slint's built-in SVG support and shown in `DarkButton` via an `icon`
  property (icon + label centered horizontally and vertically).
- **Charts:** Plotters rendering into an in-memory image buffer, exposed to
  Slint via `Image` (re-rendered at 2 Hz, only when data changed).
- **Architecture (threads):**
  - **Tail thread:** polls the file (FR-1.2), reads appended bytes, splits into
    complete lines, sends `Vec<u8>` lines over an `std::sync::mpsc` channel.
    Handles truncation/rotation/reopen (FR-1.5/1.6).
  - **Parse thread (or same thread):** deserializes lines with `serde_json`
    into typed event structs (`#[serde(default)]` + `deny_unknown_fields` off),
    updates the **Store**, and pushes a `StoreUpdate` to the UI.
  - **Store:** in-memory state — server info, ring buffers of throughput
    samples, request map (`request_id → RequestState`), bounded request list,
    windowed counters. All access confined to the parse thread; the UI reads a
    snapshot via a channel (no shared mutable state across threads).
  - **UI thread:** Slint event loop; applies snapshots at ≤ 4 Hz; renders
    charts with Plotters.
- **Key crates:** `slint`, `plotters`, `serde`, `serde_json`, `chrono`
  (timestamps), `rfd` (file dialog), `windows` (Windows monitor enumeration,
  FR-7.4), `thiserror`/`anyhow` (errors). The
  config path is derived from the platform home directory directly (no
  `directories` dependency). File tailing is implemented directly (no
  `notify` dependency) to keep behavior identical across platforms; `notify`
  may be used later as an optimization.
- **Config file:** `$HOME/.ninfer-monitor/config.json` (FR-7.2). Fields:
  `last_path`, `poll_interval_ms`, `chart_window_ms`, `max_requests`,
  `split_panels` (the split-panel fractions, FR-7.5), and `window_state`
  (the main-window geometry, FR-7.4).
- **Window geometry (FR-7.4):** the main window's position/size/maximized
  state is persisted in logical pixels and restored on startup. The saved
  window is matched to a connected monitor by its center point; if that
  monitor is gone, the default size/position is used. Monitor enumeration uses
  the `windows` crate on Windows (`EnumDisplayMonitors`, `GetMonitorInfoW`,
  `GetDpiForMonitor`); the contained `unsafe` FFI is isolated in
  `src/window_state.rs`. On non-Windows platforms the saved geometry is
  restored as-is (no monitor matching).

## 12. Milestones

Milestones are incremental and buildable: at the end of each milestone the app
compiles, runs, and the milestone's "Done when" criteria are met.

### M1 — Project scaffolding

Create the basic Rust application with all necessary crates and a minimal
Slint window.

- Single-package layout: the binary crate (package name `ninfer-monitor`)
  lives at the repo root, Rust stable, edition 2024.
- Add dependencies: `slint`, `plotters`, `serde` (derive), `serde_json`,
  `chrono`, `rfd`, `windows`, `thiserror`, `anyhow`.
- Module layout: `src/main.rs`, `src/tail.rs`, `src/parser.rs`, `src/store.rs`,
  `src/ui/` (Slint components), `src/metrics.rs`.
- Minimal `MainWindow.slint` rendering the title "NInferMonitor" and an empty
  status bar.

**Done when:** `cargo run` opens a window titled "NInferMonitor";
`cargo clippy` / `cargo fmt --check` clean; all crates are in `Cargo.toml`
(even if not yet used).

### M2 — Log tailing

Implement the file-tail layer (`src/tail.rs`) per FR-1.

- Open a file by path (CLI arg `ninfer-monitor [path]`). On first open,
  backfill reads the existing contents from offset 0 (FR-1.3); on subsequent
  opens (or when not backfilling), seek to end and poll for appended bytes at
  a configurable interval (default 500 ms).
- Emit only complete lines (buffer partial trailing lines, FR-1.4).
- Detect truncation/rotation and reopen (FR-1.5); handle deleted/unreadable
  file with retry (FR-1.6); Windows shared-mode open.
- Expose a `TailStatus` (Live / Disconnected / Paused, last-event time).

**Done when:** unit + integration tests pass: appending lines to a temp file
yields them in order within one poll interval; a partial line is held back
until completed; truncation triggers reopen; deleting the file yields
Disconnected then Live on recreate.

### M3 — Parser

Implement JSONL parsing (`src/parser.rs`) per FR-2.

- Typed event structs for `server_start`, `request_start`, `request_done`,
  `throughput`, `request_error` (fields per §9), with `#[serde(default)]` and
  unknown fields ignored.
- Line → `Result<ParsedEvent, ParseError>`; malformed JSON and missing
  required fields produce `ParseError` (never panic).
- Unknown `event` names and unknown `schema_version` handled per FR-2.4.

**Done when:** every line of `logs/server.requests.jsonl` parses: 1
`server_start`, 653 `request_start`, 642 `request_done`, 1288 `throughput`,
11 `request_error`, 0 skipped; spot-check tests assert exact field values
(e.g. request 1: `prompt_tokens=645`, `completion_tokens=155`,
`ttft=0.4219088`, `speculative.accepted_tokens=109`); a malformed-line test
returns `ParseError`.

### M4 — Store & metrics

Implement the in-memory store and derived metrics (`src/store.rs`,
`src/metrics.rs`) per FR-3/FR-4/§9.

- Server info (latest `server_start`), request map keyed by `request_id`
  (in-flight → done/error), bounded request list (default 1 000 rows,
  configurable via the max-requests setting).
- Ring buffers for `throughput` samples (max 10 000 points per series).
- Windowed counters (60 s) for TTFT, decode latency/token, prefix-cache hit
  rate, speculative acceptance, errors.
- Derived-metric functions per the §9 table.

**Done when:** unit tests feed the sample log's events into the store and
assert: 642 closed requests, 11 errors, correct windowed metrics against
hand-computed values from the sample log; ring buffers and the request list
stay bounded when fed > 10 000 synthetic events.

### M5 — Pipeline (threading)

Wire tail → parse → store → UI per §11.

- Tail thread reads lines; parse thread deserializes and updates the store;
  `StoreUpdate` snapshots pushed over an mpsc channel; UI applies snapshots at
  ≤ 4 Hz.
- No shared mutable state across threads; UI thread never blocks on I/O.

**Done when:** running against a live-written log (a test script appending
lines) updates the store within 1 s; no data races under
`cargo test` with Miri (or `cargo clippy -W clippy::all` + thread-sanitizer
run on Linux); steady-state CPU < 5% of one core while tailing.

### M6 — UI shell

Build the Slint layout per §10 with placeholder content.

- `MainWindow` with buttons bar (Open/Pause/Clear, chart window selector,
  Settings), 2×2 metric-widget grid (each widget: two KPIs + dual-axis
  chart), request table, server-info strip, status bar (connection state,
  last event time, log file, error/skipped counters).
- File picker dialog (Open file…), Pause/Resume, Clear buttons wired to
  callbacks (behavior stubbed).
- Dark theme default; layout usable at 1280×800.

**Done when:** the window matches the §10 wireframe with placeholder values;
all controls are clickable and emit their callbacks; no layout overflow at
1280×800.

### M7 — KPI values

Implement live KPI values per FR-3.

- Remove all placeholders for KPI values.
- All 8 KPIs from FR-3.1 with trend indicators (FR-3.2), and "—" for
  missing data (FR-3.3); units are shown in the widget titles.

**Done when:** with the sample log loaded, the widgets show Decode 123.9,
Prefill 0.0 (last `throughput` event), the status bar shows `Errors: 11`,
and correct windowed TTFT / cache-hit / speculative values; appending a new
`throughput` line updates the widgets within 1 s.

### M8 — Charts

Implement the four Plotters charts per FR-4.

- Remove all placeholders for Plotters charts.
- Throughput (decode + prefill tok/s), Latency (TTFT + per-token), Cache (%)
  (cache hit + speculative acceptance), Scheduler (running + waiting);
  timestamp-driven x-axis (R4).
- Time-window selector (1 m / 5 m / 15 m / 30 m / 1 h, default 5 m); dual-axis
  layout with unified line colors and colored y-axis tick values (FR-4.3);
  shared y-axis scale when the series maxima are within 2× (FR-4.5);
  2 Hz re-render only on data change.
- "Server restarted" marker on new `server_start` (FR-6.2).

**Done when:** each chart renders the sample log's history correctly
(values verified against raw JSON at ≥ 3 points per chart); window switching
re-renders instantly; re-render cost measured < 50 ms per chart.

### M9 — Request table & detail

Implement the request table per FR-5.

- Remove all placeholders for request table & detail.
- Columns, status values, color coding (FR-5.1/5.2/5.3), configurable row cap
  (FR-5.5), pause-on-scroll auto-scroll (FR-5.6).
- Row click expands the detail inline in the table with the full
  `request_done` payload (FR-5.4); clicking the expanded row collapses it;
  multiple rows can be expanded at once.

**Done when:** the sample log shows 642 done + 11 error rows with correct
values (spot-checked against raw JSON); a live `request_start` appears as
in-flight and transitions to done/error on completion; scrolling up pauses
auto-scroll; the inline detail shows sampling params, `engine_timing`,
`speculative`, and `tool_call_parse` for an expanded row, and the
expand/collapse symbol in the first column reflects the row's state.

### M10 — Server info panel

Implement the server info panel per FR-6.

- Remove all placeholders for Server info.
- Always-visible panel showing model, engine, GPU, memory, and server fields
  from `server_start` (FR-6.1); updates on server restart (FR-6.2).

**Done when:** with the sample log, the panel shows RTX 5090,
qwen3.8-27b-nvfp4, KV fp8-e4m3 240 000, max context 240 000, concurrency 2,
and the memory breakdown; a synthetic second `server_start` updates the panel
and draws the restart marker on charts.

### M11 — Controls & settings

Implement controls and persistence per FR-7.

- Open file…, Pause/Resume, Clear state, chart window selector (FR-7.1).
- JSON config (by default in the $HOME/.ninfer-monitor folder, where $HOME is platform specific): last path, poll interval, chart
  window, max requests (FR-7.2); CLI `--poll <ms>` (FR-7.3).
- Add command line "--config" parameter to override and use a different config location
- Add Settings dialog including About dialog
- Update ALL tests (unit tests, screenshots, ...) to use custom temporary config location and ensure that user's config is not overwritten

**Done when:** settings survive an app restart; `--poll 100` changes the poll
interval; "--config" parameter overrides config location; all tests don't 
change the user's config; Clear resets KPIs/charts/table to empty state;
Pause freezes the live view while the tail thread keeps reading (catches up
on resume).

### M13 — Window state persistence

Persist and restore the main window's geometry (position, size, maximized
state) across restarts, including which monitor the window was on (FR-7.4).

- New `src/window_state.rs` module: `WindowState` (x, y, width, height,
  maximized — logical pixels), `Monitor`, and a pure `resolve_restore`
  (saved state + current monitors → restore plan) that is unit-testable
  without a display.
- Monitor enumeration on Windows via the `windows` crate
  (`EnumDisplayMonitors` + `GetMonitorInfoW` + `GetDpiForMonitor`); the
  contained `unsafe` FFI is isolated in this module. On non-Windows platforms
  the saved geometry is restored as-is (no monitor matching).
- Config: a `window_state` object field (tolerant parse; a missing or
  malformed entry falls back to the default geometry).
- On startup, apply the restored geometry (position/size before the window is
  shown, maximized after); on close, save the current geometry (using the last
  normal geometry when the window is maximized).

**Done when:** closing and reopening the app restores the window's position,
size, and maximized state on the same monitor; disconnecting the monitor the
window was on makes the app open at the default size/position; the pure
`resolve_restore` logic is covered by unit tests (existing monitor, gone
monitor, no monitors, clamping, maximized preserved); `cargo clippy` and
`cargo fmt --check` stay clean.

### M12 — Hardening & v0.1 acceptance

Close out NFRs and run the full acceptance suite per §13.

- Robustness passes: malformed lines, unknown events/fields, empty file,
  10 MB line, faster-than-read log (FR-2.3, NFR-4).
- Performance validation: 1 h tail of a busy log, RAM < 300 MB, CPU < 5%
  (NFR-2); < 1 s end-to-end latency (NFR-3).
- Cross-platform: release builds on Windows, Linux, macOS (NFR-1).
- `cargo clippy`, `cargo fmt --check`, full test suite green (NFR-7).

**Done when:** all 11 acceptance criteria in §13 pass on all three platforms;
v0.1 tagged.

## 13. Acceptance Criteria (v0.1)

1. `cargo build --release` succeeds on Windows, Linux, and macOS.
2. Opening test log file backfills the file in < 2 s and shows:
   server info (RTX 5090, qwen3.8-27b-nvfp4), 642 completed requests, 11
   errors, and the last throughput values.
3. Appending a new `throughput` line to a live log makes the KPI values and
   charts update within 1 s.
4. Appending a `request_start` followed by a `request_done` shows the request
   in the table with correct token counts, TTFT, and total time (verified
   against the raw JSON values).
5. A malformed line (e.g. `{"broken`) is skipped, the "skipped lines" counter
   increments, and the app keeps tailing.
6. Truncating the log file (simulating rotation) causes the app to reopen and
   continue without crashing.
7. Deleting the log file shows "Disconnected"; recreating the file restores
   "Live".
8. After 1 hour of tailing a busy log, RAM stays < 300 MB and CPU < 5% of one
   core (steady state, no active benchmark).
9. Unit tests pass: parser round-trips every event type from the sample log;
   derived-metric formulas match hand-computed values from the sample log.
10. `cargo clippy` and `cargo fmt --check` report no issues.
11. Closing and reopening the app restores the main window's position, size,
    and maximized state on the same monitor; if that monitor is disconnected,
    the window opens at the default size/position (FR-7.4).

## 14. Out of Scope / Future Work (v0.2+)

- Multiple log files / multiple servers in one window.
- Remote log sources (SSH/HTTP) and log upload.
- Alerting (thresholds, notifications, sound).
- Export of metrics (CSV/JSON), session recording.
- GPU utilization / VRAM sampling from the driver (nvidia-smi / NVML) as a
  complement to log data.
- `notify`-based (event-driven) tailing instead of polling.
- i18n, theming (dark/light).
- KV-cache occupancy chart (device KV pages used vs. capacity, host KV bytes)
  from `context_cache.occupancy`.

## 15. Risks & Open Questions

| # | Risk / Question | Mitigation |
|---|---|---|
| R1 | Log schema may evolve (new `schema_version`). | Tolerant parsing (FR-2.3/2.4); version warning banner; keep field extraction in one module. |
| R2 | Windows file locking while `ninfer-serve` holds the file open. | Open with `FILE_SHARE_READ \| FILE_SHARE_WRITE \| FILE_SHARE_DELETE`; verify against a live server during acceptance. |
| R3 | Very high event rates (many concurrent requests) could flood the UI. | Batched UI updates (≤ 4 Hz), bounded buffers, table capped at a configurable number of rows (default 1 000). |
| R4 | `throughput` event interval is server-configured (`log_stats_interval_ms`); charts must not assume 5 s. | Charts are timestamp-driven, not interval-driven. |
| R5 | Window-geometry restore (FR-7.4) depends on Windows monitor enumeration (FFI); DPI/monitor changes can misplace the window. | Geometry stored in logical pixels; matched to a monitor by center point and clamped to fit; falls back to the default size/position when the saved monitor is gone; contained `unsafe` isolated in `src/window_state.rs`. |
| OQ1 | Should the app also accept a directory and auto-pick the newest `*.jsonl`? | Deferred; v0.1 uses an explicit file path. |
| OQ2 | Is a system-tray icon / minimize-to-tray wanted? | Deferred to v0.2. |
