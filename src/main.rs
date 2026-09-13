// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

#![windows_subsystem = "windows"]

mod ui;

slint::include_modules!();

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ninfer_monitor::chart;
use ninfer_monitor::config::{self, Config};
use ninfer_monitor::kpi;
use ninfer_monitor::pipeline::{
    ClearGate, Pipeline, StoreUpdate, clear_gate, format_last_event, shorten_file_name,
};
use ninfer_monitor::requests;
use ninfer_monitor::server_info;
use ninfer_monitor::split::SplitPanels;
use ninfer_monitor::store::Store;
use ninfer_monitor::tail::TailStatus;
use ninfer_monitor::window_state::{self, RestorePlan, WindowState};
use slint::ComponentHandle;
use slint::ModelRc;
use slint::{CloseRequestResponse, LogicalPosition, LogicalSize, WindowPosition, WindowSize};

const CHART_REFRESH: Duration = Duration::from_millis(500);

const COLOR_LIVE: slint::Color = slint::Color::from_rgb_u8(0x4c, 0xaf, 0x50);
const COLOR_PAUSED: slint::Color = slint::Color::from_rgb_u8(0xff, 0xc1, 0x07);
const COLOR_DISCONNECTED: slint::Color = slint::Color::from_rgb_u8(0xf4, 0x43, 0x36);
const COLOR_NONE: slint::Color = slint::Color::from_rgb_u8(0x9e, 0x9e, 0x9e);

fn status_color(status: TailStatus) -> slint::Color {
    match status {
        TailStatus::Live => COLOR_LIVE,
        TailStatus::Paused => COLOR_PAUSED,
        TailStatus::Disconnected => COLOR_DISCONNECTED,
    }
}

/// The tail/parse pipeline and the file it tails, shared between the event
/// loop and the control handlers (FR-7.1).
struct App {
    pipeline: Option<Pipeline>,
    path: Option<PathBuf>,
}

/// Chart rendering state shared by the tick and window-change handlers.
/// `window_ms` is the source of truth for the chart window; the slint
/// `chart-window-ms` property is a read-only mirror of it.
struct ChartState {
    window_ms: u64,
    fingerprint: u64,
    last_render: Option<Instant>,
    store: Store,
    table_fingerprint: u64,
    /// Request ids whose inline detail is expanded in the table (FR-5.4).
    expanded_ids: HashSet<u64>,
    /// The `server_start_count` last seen; a higher value means the server
    /// restarted and the (now stale) expanded ids must be cleared.
    server_start_count: usize,
    server_info: Option<server_info::ServerInfoSnapshot>,
    clear_count: u64,
    clear_pending: bool,
    last_status: Option<TailStatus>,
    /// Set when the chart window changes while paused: the charts stay
    /// frozen until resume, which then re-renders them with the new window.
    charts_stale: bool,
    /// The sizes the charts were last rendered at; a change forces a
    /// re-render so each plot matches its panel aspect (no distortion).
    render_sizes: [(u32, u32); 4],
}

fn new_chart_state(window_ms: u64, max_requests: u64) -> ChartState {
    ChartState {
        window_ms,
        fingerprint: 0,
        last_render: None,
        store: Store::with_max_requests(max_requests as usize),
        table_fingerprint: 0,
        expanded_ids: HashSet::new(),
        server_start_count: 0,
        server_info: None,
        clear_count: 0,
        clear_pending: false,
        last_status: None,
        charts_stale: false,
        render_sizes: [(chart::CHART_WIDTH, chart::CHART_HEIGHT); 4],
    }
}

/// The actual on-screen size of each chart panel, read from the widget out
/// properties. Falls back to the default render size when the layout has not
/// run yet (e.g. in tests), so charts are never rendered at size 0.
fn chart_panel_sizes(window: &MainWindow) -> [(u32, u32); 4] {
    fn one(w: f32, h: f32) -> (u32, u32) {
        if w <= 0.0 || h <= 0.0 {
            (chart::CHART_WIDTH, chart::CHART_HEIGHT)
        } else {
            (w as u32, h as u32)
        }
    }
    [
        one(
            window.get_widget_throughput_w(),
            window.get_widget_throughput_h(),
        ),
        one(window.get_widget_latency_w(), window.get_widget_latency_h()),
        one(window.get_widget_cache_w(), window.get_widget_cache_h()),
        one(
            window.get_widget_scheduler_w(),
            window.get_widget_scheduler_h(),
        ),
    ]
}

fn table_fingerprint(rows: &[requests::TableRowData]) -> u64 {
    // Fingerprint of every cell the table can display. It must change for any
    // column change (not just id/status/total/color) so `apply_table`
    // refreshes the model whenever a displayed value changes — including
    // in-place updates to a row (e.g. a running request gaining a ttft) and
    // expansion toggles.
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    rows.len().hash(&mut h);
    for r in rows {
        r.id.hash(&mut h);
        r.time.hash(&mut h);
        r.model.hash(&mut h);
        r.prompt.hash(&mut h);
        r.compl.hash(&mut h);
        r.thinking.hash(&mut h);
        r.ttft.hash(&mut h);
        r.total.hash(&mut h);
        r.reason.hash(&mut h);
        r.status.hash(&mut h);
        r.color.hash(&mut h);
        r.expanded.hash(&mut h);
    }
    h.finish()
}

fn apply_table(window: &MainWindow, state: &mut ChartState, store: &Store, mut force: bool) {
    // A server restart clears the store's requests (FR-6.2); the expanded ids
    // now reference stale request ids (which may be reused), so drop them.
    let start_count = store.server_start_count();
    if start_count > state.server_start_count {
        state.server_start_count = start_count;
        if !state.expanded_ids.is_empty() {
            state.expanded_ids.clear();
            force = true;
        }
    }
    let rows = requests::table_rows(store, &state.expanded_ids);
    let fp = table_fingerprint(&rows);
    if !force && fp == state.table_fingerprint {
        return;
    }
    state.table_fingerprint = fp;
    let slint_rows: Vec<RequestRow> = rows
        .iter()
        .map(|r| RequestRow {
            id: r.id.clone().into(),
            time: r.time.clone().into(),
            model: r.model.clone().into(),
            prompt: r.prompt.clone().into(),
            compl: r.compl.clone().into(),
            thinking: r.thinking.clone().into(),
            ttft: r.ttft.clone().into(),
            total: r.total.clone().into(),
            reason: r.reason.clone().into(),
            status: r.status.clone().into(),
            color: slint::Color::from_rgb_u8(r.color[0], r.color[1], r.color[2]),
            expanded: r.expanded,
            detail_sampling: r.detail.sampling.clone().into(),
            detail_engine: r.detail.engine.clone().into(),
            detail_speculative: r.detail.speculative.clone().into(),
            detail_tool_call: r.detail.tool_call.clone().into(),
            detail_prefix: r.detail.prefix.clone().into(),
            detail_error: r.detail.error.clone().into(),
        })
        .collect();
    let model = slint::ModelRc::from(Rc::new(slint::VecModel::from(slint_rows)));
    window.set_request_rows(model);
    window.set_table_content_height(requests::table_content_height(&rows) as f32);
    if window.get_table_follow() {
        window.set_table_scroll_y(0.0);
    }
}

fn pick_log_file() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("JSON Lines", &["jsonl"])
        .add_filter("All files", &["*"])
        .pick_file()
}

struct CliArgs {
    path: Option<PathBuf>,
    poll_interval: Option<Duration>,
    config_path: Option<PathBuf>,
}

fn parse_cli_args(args: &[String]) -> CliArgs {
    let mut path = None;
    let mut poll_interval = None;
    let mut config_path = None;
    let mut i = 0;
    while i < args.len() {
        // A flag's value must not itself be a flag, so `--poll --config x`
        // leaves `--config` for its own handling. Both `--flag value` and
        // `--flag=value` forms are accepted; a missing or non-numeric value
        // warns on stderr and is ignored.
        let arg = args[i].as_str();
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag == "--poll" || flag == "--config" => (flag, Some(value)),
            _ => (arg, None),
        };
        match flag {
            "--poll" => {
                let value = match inline {
                    Some(value) => Some(value),
                    None if i + 1 < args.len() && !args[i + 1].starts_with('-') => {
                        i += 1;
                        Some(args[i].as_str())
                    }
                    None => None,
                };
                match value {
                    Some(value) => match value.parse::<u64>() {
                        Ok(value) => {
                            poll_interval = Some(Duration::from_millis(
                                value.clamp(config::MIN_POLL_MS, config::MAX_POLL_MS),
                            ));
                        }
                        Err(_) => {
                            eprintln!("warning: ignoring non-numeric --poll value {value:?}");
                        }
                    },
                    None => eprintln!("warning: --poll expects a value in milliseconds"),
                }
            }
            "--config" => match inline {
                Some(value) => config_path = Some(PathBuf::from(value)),
                None if i + 1 < args.len() && !args[i + 1].starts_with('-') => {
                    i += 1;
                    config_path = Some(PathBuf::from(args[i].as_str()));
                }
                None => eprintln!("warning: --config expects a path value"),
            },
            other if !other.starts_with('-') => path = Some(PathBuf::from(other)),
            _ => {}
        }
        i += 1;
    }
    CliArgs {
        path,
        poll_interval,
        config_path,
    }
}

/// Effective startup settings after applying the CLI-over-config precedence
/// (FR-1.1/FR-7.2/FR-7.3): CLI path > config last path, CLI `--poll` >
/// config poll interval.
struct Resolved {
    path: Option<PathBuf>,
    poll_interval: Duration,
    chart_window_ms: u64,
    max_requests: u64,
}

fn resolve(cli: &CliArgs, cfg: &Config) -> Resolved {
    Resolved {
        path: cli
            .path
            .clone()
            .or_else(|| cfg.last_path.clone().map(PathBuf::from)),
        poll_interval: cli.poll_interval.unwrap_or_else(|| cfg.poll_interval()),
        chart_window_ms: cfg.chart_window_ms(),
        max_requests: cfg.max_requests(),
    }
}

/// Load the config from `cli_config_path` (or the default location,
/// FR-7.2), clamp out-of-range stored values, and persist the normalized
/// config when the file is missing (created on first run), malformed
/// (repaired), or its stored values were out of range. The file is read
/// once. CLI overrides (positional path, `--poll`) are one-shot and are
/// not persisted (FR-7.3).
fn load_startup_config(cli_config_path: Option<&Path>) -> (Option<PathBuf>, Config) {
    let config_path = cli_config_path
        .map(PathBuf::from)
        .or_else(config::default_config_path);
    if let Some(cp) = &config_path {
        config::cleanup_temp_files(cp);
    }
    let (mut cfg, state) = config_path
        .as_deref()
        .map(config::load_with_state)
        .unwrap_or_else(|| (Config::default(), config::ConfigFileState::Missing));
    let clamped_poll = cfg
        .poll_interval_ms
        .clamp(config::MIN_POLL_MS, config::MAX_POLL_MS);
    let clamped_window = cfg
        .chart_window_ms
        .clamp(config::MIN_WINDOW_MS, config::MAX_WINDOW_MS);
    let clamped_max_requests = cfg
        .max_requests
        .clamp(config::MIN_MAX_REQUESTS, config::MAX_MAX_REQUESTS);
    let clamped_split = cfg.split_panels.clamped();
    let changed = clamped_poll != cfg.poll_interval_ms
        || clamped_window != cfg.chart_window_ms
        || clamped_max_requests != cfg.max_requests
        || clamped_split != cfg.split_panels;
    cfg.poll_interval_ms = clamped_poll;
    cfg.chart_window_ms = clamped_window;
    cfg.max_requests = clamped_max_requests;
    cfg.split_panels = clamped_split;
    if let Some(cp) = &config_path
        && (changed
            || matches!(
                state,
                config::ConfigFileState::Missing | config::ConfigFileState::Malformed
            ))
    {
        let _ = config::save(cp, &cfg);
    }
    (config_path, cfg)
}

/// Set the status-bar file name (PRD §10): the full name for width
/// measurement, plus the display name — the full name when it fits the
/// available space, otherwise the shortened form.
fn set_file_name(window: &MainWindow, path: &Path) {
    let name = path.display().to_string();
    window.set_file_name_full(name.clone().into());
    window.set_file_name(pick_file_name(window, &name).into());
}

/// Re-pick the display name from the UI's last layout pass (PRD §10): the
/// available width changes with the window size, and the measured widths
/// only update after a layout pass. Called on every tick so a resize while
/// the log is idle is picked up within one tick.
fn refresh_file_name(window: &MainWindow, app: &Arc<Mutex<App>>) {
    let Some(path) = app.lock().unwrap().path.clone() else {
        return;
    };
    let name = path.display().to_string();
    let chosen = pick_file_name(window, &name);
    if window.get_file_name().as_str() != chosen {
        window.set_file_name(chosen.into());
    }
}

/// The display name for `name` (PRD §10): the full name when it fits the
/// space measured by the UI, otherwise the shortened form. A zero width
/// (no layout pass yet) optimistically shows the full name; the next tick
/// re-picks once the widths are known.
fn pick_file_name(window: &MainWindow, name: &str) -> String {
    let full_width = window.get_file_full_width();
    let available = window.get_file_available();
    if full_width > 0.0 && available > 0.0 && full_width > available {
        shorten_file_name(name)
    } else {
        name.to_string()
    }
}

/// Apply the startup UI state (FR-7.1): show the file and enable the
/// controls when a log is opened, otherwise disable them.
fn apply_startup_state(window: &MainWindow, path: Option<&Path>) {
    match path {
        Some(path) => {
            set_file_name(window, path);
            window.set_controls_enabled(true);
        }
        None => {
            window.set_controls_enabled(false);
        }
    }
}

/// Refresh the status bar fields (PRD §10): the log-file name, the last
/// event time (FR-1.7), and the error/skipped counters (FR-2.3/FR-3.1).
fn apply_status_bar(window: &MainWindow, path: &Path, update: &StoreUpdate) {
    set_file_name(window, path);
    window.set_last_event(
        update
            .last_event_time
            .map(format_last_event)
            .unwrap_or_else(|| "—".to_string())
            .into(),
    );
    window.set_errors_text(format!("Errors: {}", update.store.total_error()).into());
    window.set_skipped_text(format!("Skipped {}", update.store.skipped_lines()).into());
}

/// Poll interval and max request rows for a pipeline started by "Open
/// file…" (FR-7.1/FR-7.3): a CLI `--poll` override applies to the whole
/// session; otherwise the (possibly externally edited) config values are
/// re-read.
fn settings_for_new_file(
    cli_poll: Option<Duration>,
    config_path: Option<&Path>,
) -> (Duration, u64) {
    let cfg = config_path.map(config::load).unwrap_or_default();
    (
        cli_poll.unwrap_or_else(|| cfg.poll_interval()),
        cfg.max_requests(),
    )
}

/// Persist the newly opened log path as `last_path` (FR-7.2), keeping the
/// other settings.
fn persist_last_path(config_path: Option<&Path>, path: &Path) {
    let Some(cp) = config_path else {
        return;
    };
    let new_path = path.display().to_string();
    let mut cfg = config::load(cp);
    if cfg.last_path.as_deref() == Some(new_path.as_str()) {
        return;
    }
    cfg.last_path = Some(new_path);
    let _ = config::save(cp, &cfg);
}

/// Restore the main-window geometry on startup (FR-7.4): match the saved
/// window to a connected monitor and apply its position, size, and maximized
/// state. A missing saved state, or a saved monitor that no longer exists,
/// leaves the default size/position in place.
fn apply_window_state(window: &MainWindow, saved: Option<WindowState>) {
    let monitors = window_state::current_monitors();
    let RestorePlan::Restore {
        x,
        y,
        width,
        height,
        maximized,
    } = window_state::resolve_restore(saved.as_ref(), &monitors)
    else {
        return;
    };
    let win = window.window();
    win.set_position(WindowPosition::Logical(LogicalPosition::new(x, y)));
    win.set_size(WindowSize::Logical(LogicalSize::new(width, height)));
    win.set_maximized(maximized);
}

/// The window geometry to persist on close (FR-7.4): the last normal
/// (non-maximized, non-minimized) geometry, carrying the current maximized
/// flag. Falls back to the current geometry when no normal geometry was
/// tracked (e.g. the window was maximized immediately on startup).
fn window_state_to_save(window: &MainWindow, tracked: Option<WindowState>) -> WindowState {
    let win = window.window();
    let maximized = win.is_maximized();
    if let Some(normal) = tracked {
        return WindowState {
            maximized,
            ..normal
        };
    }
    let scale = win.scale_factor();
    let pos = win.position();
    let size = win.size();
    WindowState {
        x: pos.x as f32 / scale,
        y: pos.y as f32 / scale,
        width: size.width as f32 / scale,
        height: size.height as f32 / scale,
        maximized,
    }
}

/// Track the current normal (non-maximized, non-minimized) window geometry so
/// it can be restored after a maximize (FR-7.4).
fn track_window_geometry(window: &MainWindow, tracked: &Mutex<Option<WindowState>>) {
    let win = window.window();
    if win.is_maximized() || win.is_minimized() {
        return;
    }
    let scale = win.scale_factor();
    let pos = win.position();
    let size = win.size();
    *tracked.lock().unwrap() = Some(WindowState {
        x: pos.x as f32 / scale,
        y: pos.y as f32 / scale,
        width: size.width as f32 / scale,
        height: size.height as f32 / scale,
        maximized: false,
    });
}

/// Persist the main-window geometry as `window_state` (FR-7.4), keeping the
/// other settings.
fn persist_window_state(config_path: Option<&Path>, state: WindowState) {
    let Some(cp) = config_path else {
        return;
    };
    let mut cfg = config::load(cp);
    cfg.window_state = Some(state);
    let _ = config::save(cp, &cfg);
}

/// Reset the dashboard to the empty state (FR-7.1 "Clear"): KPIs to "—",
/// charts to "no data", table cleared, server info reset.
///
/// Does not touch `clear_count`/`clear_pending`: the caller sets them
/// depending on whether the pipeline continues (Clear) or is replaced
/// (Open file…).
fn reset_view(window: &MainWindow, state: &mut ChartState) {
    let empty = Store::with_max_requests(state.store.max_requests());
    let k = kpi::snapshot(&empty);
    ninfer_monitor::apply_kpis!(window, &k);
    let images = chart::render_all_sized(&empty, state.window_ms, &state.render_sizes);
    state.fingerprint = chart::fingerprint(&empty);
    state.last_render = Some(Instant::now());
    state.charts_stale = false;
    ninfer_monitor::apply_chart_images!(window, &images);
    window.set_request_rows(ModelRc::from(Rc::new(slint::VecModel::<RequestRow>::from(
        Vec::new(),
    ))));
    state.table_fingerprint = table_fingerprint(&[]);
    state.expanded_ids.clear();
    state.server_start_count = 0;
    window.set_table_content_height(0.0);
    window.set_schema_warning(slint::SharedString::from(""));
    let info = server_info::snapshot(&empty);
    ninfer_monitor::apply_server_info!(window, &info);
    state.server_info = Some(info);
    state.store = empty;
    window.set_last_event("—".into());
    window.set_errors_text("Errors: 0".into());
    window.set_skipped_text("Skipped 0".into());
}

/// Non-blocking warning when the log's schema version is newer than the
/// supported one (FR-2.4).
fn schema_warning(store: &Store) -> String {
    match store.max_schema_version() {
        Some(v) if v > ninfer_monitor::parser::SUPPORTED_SCHEMA_VERSION => {
            format!(
                "Warning: log schema version {v} is newer than supported version {supported}",
                supported = ninfer_monitor::parser::SUPPORTED_SCHEMA_VERSION
            )
        }
        _ => String::new(),
    }
}

/// Apply the live view (KPIs, server info, charts, table) from
/// `store`. Charts re-render at most every `CHART_REFRESH` and only when
/// the fingerprint changed (PRD §11).
fn apply_view(window: &MainWindow, state: &mut ChartState, store: &Store) {
    let k = kpi::snapshot(store);
    ninfer_monitor::apply_kpis!(window, &k);
    window.set_schema_warning(schema_warning(store).into());
    let info = server_info::snapshot(store);
    if server_info::needs_update(&state.server_info, &info) {
        ninfer_monitor::apply_server_info!(window, &info);
        state.server_info = Some(info);
    }
    // Re-render if a panel size changed (window resized), even when the
    // data fingerprint is unchanged, so each plot matches its panel aspect.
    let sizes = chart_panel_sizes(window);
    if sizes != state.render_sizes {
        state.render_sizes = sizes;
        state.last_render = None;
        state.charts_stale = true;
    }
    let fp = chart::fingerprint(store);
    let now = Instant::now();
    let due = state
        .last_render
        .map(|t| now.duration_since(t) >= CHART_REFRESH)
        .unwrap_or(true);
    if (fp != state.fingerprint || state.charts_stale) && due {
        let images = chart::render_all_sized(store, state.window_ms, &state.render_sizes);
        state.fingerprint = fp;
        state.last_render = Some(now);
        state.charts_stale = false;
        ninfer_monitor::apply_chart_images!(window, &images);
    }
    apply_table(window, state, store, false);
}

/// Apply the newest pending snapshot to the UI.
///
/// Called from the ≤ 4 Hz tick, immediately on resume, and on a
/// chart-window change (when not paused) so the re-render uses the
/// freshest store. The last tail status is remembered on every update
/// (even while paused) so the resume handler can restore it, and the
/// connection status/color are refreshed on every update (even a skipped
/// pre-clear one, when not paused) since they track the tail status, which
/// is independent of the in-memory data. While paused
/// the live view is frozen (FR-7.1): snapshots are drained (to bound
/// channel memory, NFR-2) and only the status bar is refreshed;
/// `state.store` is kept current so the view can catch up on resume even
/// when no new snapshot is pending (the log went idle while paused). A
/// Clear issued while paused already reset the view in `on_clear`, so the
/// post-clear snapshot only needs the status bar.
///
/// Returns `true` when the live view was applied (the `Apply` path), so a
/// caller (the resume handler) can skip a redundant re-apply.
fn apply_latest(
    window: &MainWindow,
    app: &Arc<Mutex<App>>,
    charts: &Arc<Mutex<ChartState>>,
) -> bool {
    let (update, path) = {
        let app = app.lock().unwrap();
        let update = app.pipeline.as_ref().and_then(|p| p.take_latest());
        (update, app.path.clone())
    };
    let Some(update) = update else {
        return false;
    };
    let Some(path) = path else {
        return false;
    };
    let paused = window.get_paused();
    let mut state = charts.lock().unwrap();
    state.last_status = Some(update.status);
    match clear_gate(paused, state.clear_pending, state.clear_count, &update) {
        ClearGate::Skip => {
            // The connection status tracks the tail status, which is
            // independent of the in-memory data, so refresh it even for a
            // skipped pre-clear snapshot (but not while paused, where it
            // shows "Paused").
            if !paused {
                window.set_connection_status(update.status.to_string().into());
                window.set_status_color(status_color(update.status));
            }
            return false;
        }
        ClearGate::StatusOnly => {
            state.clear_pending = false;
            state.clear_count = update.clear_count;
            apply_status_bar(window, &path, &update);
            state.store = update.store;
            return false;
        }
        ClearGate::Apply => {
            state.clear_pending = false;
        }
    }
    apply_status_bar(window, &path, &update);
    window.set_connection_status(update.status.to_string().into());
    window.set_status_color(status_color(update.status));
    apply_view(window, &mut state, &update.store);
    state.store = update.store;
    state.clear_count = update.clear_count;
    true
}

/// Resume the live view after a pause (FR-7.1).
///
/// Restores the last tail status seen (remembered even while paused) instead
/// of assuming Live, then applies any pending snapshot. If no new snapshot
/// was pending (the log went idle while paused), re-apply the view from the
/// internal store kept current while paused so the view catches up — this
/// also re-renders the charts if the window changed while paused
/// (`charts_stale`).
fn resume_view(window: &MainWindow, app: &Arc<Mutex<App>>, charts: &Arc<Mutex<ChartState>>) {
    match charts.lock().unwrap().last_status {
        Some(status) => {
            window.set_connection_status(status.to_string().into());
            window.set_status_color(status_color(status));
        }
        None => {
            window.set_connection_status("—".into());
            window.set_status_color(COLOR_NONE);
        }
    }
    let applied = apply_latest(window, app, charts);
    {
        let mut state = charts.lock().unwrap();
        // Catch up if no snapshot was applied (the log went idle while
        // paused), or if the charts are still stale (the window changed
        // while paused and the apply above was throttled by `CHART_REFRESH`).
        if !applied || state.charts_stale {
            state.last_render = None;
            let store = std::mem::take(&mut state.store);
            apply_view(window, &mut state, &store);
            state.store = store;
        }
    }
}

/// Clear handler (FR-7.1): reset the view and ask the parse thread to clear
/// the store. The post-clear snapshot (bumped `clear_count`) is recognized by
/// `clear_gate`, which skips pre-clear snapshots (no flicker).
fn handle_clear(window: &MainWindow, app: &Arc<Mutex<App>>, charts: &Arc<Mutex<ChartState>>) {
    let has_pipeline = {
        let app = app.lock().unwrap();
        match app.pipeline.as_ref() {
            Some(p) => {
                p.clear();
                true
            }
            None => false,
        }
    };
    let mut state = charts.lock().unwrap();
    reset_view(window, &mut state);
    if has_pipeline {
        state.clear_pending = true;
    }
}

/// Chart-window handler (FR-7.1): update the window, re-render (or mark the
/// charts stale while paused), and persist the setting. When not paused,
/// the newest pending snapshot is applied first so the re-render uses the
/// freshest store, not one up to a tick old.
fn handle_window_changed(
    window: &MainWindow,
    app: &Arc<Mutex<App>>,
    charts: &Arc<Mutex<ChartState>>,
    config_path: Option<&Path>,
    ms: i32,
) {
    let window_ms = (ms as u64).clamp(config::MIN_WINDOW_MS, config::MAX_WINDOW_MS);
    window.set_chart_window_ms(window_ms as i32);
    let paused = window.get_paused();
    // Set the new window before draining a snapshot so that any chart render
    // (including one inside `apply_latest`) uses the new window, not the old
    // one. While paused the window selection is a setting and is remembered,
    // but the charts stay frozen until resume (FR-7.1); mark them stale so
    // resume re-renders with the new window.
    {
        let mut state = charts.lock().unwrap();
        state.window_ms = window_ms;
        if paused {
            state.charts_stale = true;
        }
    }
    if !paused {
        // Drain the newest snapshot so the re-render uses the freshest store,
        // not one up to a tick old. Remember whether `apply_latest` already
        // re-rendered the charts (it bumps `last_render`) to avoid a
        // redundant second render.
        let rendered = {
            let before = charts.lock().unwrap().last_render;
            apply_latest(window, app, charts);
            charts.lock().unwrap().last_render != before
        };
        if !rendered {
            let mut state = charts.lock().unwrap();
            if state.store.max_timestamp_ms().is_some() {
                let images = chart::render_all_sized(&state.store, window_ms, &state.render_sizes);
                state.fingerprint = chart::fingerprint(&state.store);
                state.last_render = Some(Instant::now());
                ninfer_monitor::apply_chart_images!(window, &images);
            }
        }
    }
    if let Some(cp) = config_path {
        let mut cfg = config::load(cp);
        if cfg.chart_window_ms != window_ms {
            cfg.chart_window_ms = window_ms;
            let _ = config::save(cp, &cfg);
        }
    }
}

/// Split-panel drag handler (FR-7.5): update the in-memory split fractions
/// (clamped) and push them to the UI so the section heights update live.
/// `which` is 0 for the charts/table splitter and 1 for the table/server
/// splitter. Persistence happens on release (`handle_split_released`), not on
/// every move, so a drag does not hit the disk on each mouse event.
fn handle_split_changed(window: &MainWindow, split: &Mutex<SplitPanels>, which: i32, frac: f32) {
    let updated = {
        let mut s = split.lock().unwrap();
        let new = match which {
            0 => s.with_charts(frac),
            _ => s.with_table(frac),
        };
        *s = new;
        new
    };
    window.set_charts_frac(updated.charts);
    window.set_table_frac(updated.table);
}

/// Split-panel release handler (FR-7.5): persist the current split fractions
/// as `split_panels`, keeping the other settings.
fn handle_split_released(config_path: Option<&Path>, split: &Mutex<SplitPanels>) {
    let s = *split.lock().unwrap();
    if let Some(cp) = config_path {
        let mut cfg = config::load(cp);
        if cfg.split_panels != s {
            cfg.split_panels = s;
            let _ = config::save(cp, &cfg);
        }
    }
}

/// Settings-open handler (FR-7.1): show the settings dialog pre-filled with
/// the current poll interval and max request rows.
fn handle_settings_open(window: &MainWindow, config_path: Option<&Path>) {
    let cfg = config_path.map(config::load).unwrap_or_default();
    window.set_settings_poll_ms(cfg.poll_interval_ms as i32);
    window.set_settings_max_requests(cfg.max_requests() as i32);
    window.set_settings_visible(true);
}

/// Settings-save handler (FR-7.1): persist the poll interval and the max
/// request rows and close the dialog. Both take effect for the next opened
/// file.
fn handle_settings_save(
    window: &MainWindow,
    config_path: Option<&Path>,
    poll_ms: i32,
    max_requests: i32,
) {
    let poll_ms = (poll_ms as u64).clamp(config::MIN_POLL_MS, config::MAX_POLL_MS);
    let max_requests =
        (max_requests as u64).clamp(config::MIN_MAX_REQUESTS, config::MAX_MAX_REQUESTS);
    if let Some(cp) = config_path {
        let mut cfg = config::load(cp);
        cfg.poll_interval_ms = poll_ms;
        cfg.max_requests = max_requests;
        let _ = config::save(cp, &cfg);
    }
    window.set_about_visible(false);
    window.set_settings_visible(false);
}

/// Settings-close handler (FR-7.1): hide the panel without persisting. The
/// About dialog (opened from the settings) is hidden too so it is never left
/// open on its own.
fn handle_settings_close(window: &MainWindow) {
    window.set_about_visible(false);
    window.set_settings_visible(false);
}

/// About-open handler: show the About dialog over the settings dialog.
fn handle_about_open(window: &MainWindow) {
    window.set_about_visible(true);
}

/// About-close handler: hide the About dialog, returning to the settings
/// dialog behind it.
fn handle_about_close(window: &MainWindow) {
    window.set_about_visible(false);
}

/// Open-file handler (FR-7.1): replace the pipeline with one tailing `path`,
/// reset the view, and persist the new path as `last_path`. The old pipeline
/// is stopped on a background thread so the UI thread is not blocked by the
/// thread joins (NFR-2).
fn handle_open_file(
    window: &MainWindow,
    app: &Arc<Mutex<App>>,
    charts: &Arc<Mutex<ChartState>>,
    config_path: Option<&Path>,
    cli_poll: Option<Duration>,
    path: PathBuf,
) {
    let (poll_interval, max_requests) = settings_for_new_file(cli_poll, config_path);
    let old_pipeline = {
        let mut app = app.lock().unwrap();
        let old = app.pipeline.take();
        app.pipeline = Some(Pipeline::start(&path, poll_interval, max_requests as usize));
        app.path = Some(path.clone());
        old
    };
    if let Some(old) = old_pipeline {
        std::thread::spawn(move || old.stop());
    }
    set_file_name(window, &path);
    window.set_controls_enabled(true);
    window.set_connection_status("—".into());
    window.set_status_color(COLOR_NONE);
    let mut state = charts.lock().unwrap();
    reset_view(window, &mut state);
    state.clear_count = 0;
    state.clear_pending = false;
    state.last_status = None;
    persist_last_path(config_path, &path);
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = parse_cli_args(&args);

    let (config_path, cfg) = load_startup_config(cli.config_path.as_deref());
    let resolved = resolve(&cli, &cfg);

    let window = MainWindow::new()?;
    window.set_about_version(env!("CARGO_PKG_VERSION").into());
    window.set_chart_window_ms(resolved.chart_window_ms as i32);
    apply_window_state(&window, cfg.window_state);
    // Restore the split-panel fractions (FR-7.5); the UI derives the section
    // heights from them.
    window.set_charts_frac(cfg.split_panels.charts);
    window.set_table_frac(cfg.split_panels.table);
    let split = Arc::new(Mutex::new(cfg.split_panels));
    let charts = Arc::new(Mutex::new(new_chart_state(
        resolved.chart_window_ms,
        resolved.max_requests,
    )));
    let app = Arc::new(Mutex::new(App {
        pipeline: resolved
            .path
            .as_ref()
            .map(|p| Pipeline::start(p, resolved.poll_interval, resolved.max_requests as usize)),
        path: resolved.path.clone(),
    }));
    apply_startup_state(&window, resolved.path.as_deref());
    let tracked_state = Arc::new(Mutex::new(None::<WindowState>));

    window.on_window_changed({
        let weak = window.as_weak();
        let charts = charts.clone();
        let app = app.clone();
        let config_path = config_path.clone();
        move |ms: i32| {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_window_changed(&ui_window, &app, &charts, config_path.as_deref(), ms);
        }
    });
    window.on_split_changed({
        let weak = window.as_weak();
        let split = split.clone();
        move |which: i32, frac: f32| {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_split_changed(&ui_window, &split, which, frac);
        }
    });
    window.on_split_released({
        let config_path = config_path.clone();
        let split = split.clone();
        move |_which: i32, _frac: f32| {
            handle_split_released(config_path.as_deref(), &split);
        }
    });
    window.on_clear({
        let weak = window.as_weak();
        let charts = charts.clone();
        let app = app.clone();
        move || {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_clear(&ui_window, &app, &charts);
        }
    });
    window.on_row_clicked({
        let weak = window.as_weak();
        let charts = charts.clone();
        move |id: slint::SharedString| {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            let Ok(request_id) = id.as_str().parse::<u64>() else {
                return;
            };
            let mut state = charts.lock().unwrap();
            requests::toggle_expanded(&mut state.expanded_ids, request_id);
            let store = std::mem::take(&mut state.store);
            apply_table(&ui_window, &mut state, &store, true);
            state.store = store;
        }
    });
    window.on_open_file({
        let weak = window.as_weak();
        let charts = charts.clone();
        let app = app.clone();
        let config_path = config_path.clone();
        let cli_poll = cli.poll_interval;
        let dialog_open = Arc::new(AtomicBool::new(false));
        move || {
            // Ignore re-clicks while a file dialog is already open.
            if dialog_open
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return;
            }
            let weak = weak.clone();
            let app = app.clone();
            let charts = charts.clone();
            let config_path = config_path.clone();
            let cli_poll = cli_poll;
            let dialog_open = dialog_open.clone();
            std::thread::spawn(move || {
                let path = pick_log_file();
                let guard = dialog_open.clone();
                let invoked = slint::invoke_from_event_loop(move || {
                    // Release the re-click guard only once we are back on the
                    // event loop, so a re-click cannot stack a second dialog
                    // while the result is still being applied.
                    guard.store(false, Ordering::SeqCst);
                    let Some(w) = weak.upgrade() else {
                        return;
                    };
                    let Some(path) = path else {
                        return;
                    };
                    handle_open_file(&w, &app, &charts, config_path.as_deref(), cli_poll, path);
                });
                if invoked.is_err() {
                    // The event loop is gone; release the guard so it isn't
                    // stuck.
                    dialog_open.store(false, Ordering::SeqCst);
                }
            });
        }
    });
    window.on_pause_resume({
        let weak = window.as_weak();
        let charts = charts.clone();
        let app = app.clone();
        move || {
            let Some(w) = weak.upgrade() else {
                return;
            };
            let paused = !w.get_paused();
            w.set_paused(paused);
            if paused {
                w.set_connection_status("Paused".into());
                w.set_status_color(COLOR_PAUSED);
            } else {
                resume_view(&w, &app, &charts);
            }
        }
    });
    window.on_settings({
        let weak = window.as_weak();
        let config_path = config_path.clone();
        move || {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_settings_open(&ui_window, config_path.as_deref());
        }
    });
    window.on_settings_save({
        let weak = window.as_weak();
        let config_path = config_path.clone();
        move |poll_ms: i32, max_requests: i32| {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_settings_save(&ui_window, config_path.as_deref(), poll_ms, max_requests);
        }
    });
    window.on_settings_close({
        let weak = window.as_weak();
        move || {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_settings_close(&ui_window);
        }
    });
    window.on_about({
        let weak = window.as_weak();
        move || {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_about_open(&ui_window);
        }
    });
    window.on_about_close({
        let weak = window.as_weak();
        move || {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_about_close(&ui_window);
        }
    });
    window.on_tick({
        let weak = window.as_weak();
        let charts = charts.clone();
        let app = app.clone();
        let tracked_state = tracked_state.clone();
        move || {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            apply_latest(&ui_window, &app, &charts);
            refresh_file_name(&ui_window, &app);
            track_window_geometry(&ui_window, &tracked_state);
        }
    });
    // Hiding the window on close is deliberate, not a zombie/leak: this is a
    // single-window app, and Slint's `Window::hide()` releases its keepalive
    // (`context::release_keepalive`), which quits `run_event_loop()` once no
    // window keeps it alive. So `window.run()` below returns, `pipeline.stop()`
    // runs, and the process exits. (Verified against Slint 1.17 source.)
    window.window().on_close_requested({
        let weak = window.as_weak();
        let config_path = config_path.clone();
        let tracked_state = tracked_state.clone();
        move || {
            if let Some(ui_window) = weak.upgrade() {
                let state = window_state_to_save(&ui_window, *tracked_state.lock().unwrap());
                persist_window_state(config_path.as_deref(), state);
            }
            CloseRequestResponse::HideWindow
        }
    });

    window.run()?;

    if let Some(pipeline) = app.lock().unwrap().pipeline.take() {
        pipeline.stop();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ninfer_monitor::split;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    fn make_config(
        last_path: Option<&str>,
        poll_ms: u64,
        window_ms: u64,
        max_requests: u64,
    ) -> Config {
        Config {
            last_path: last_path.map(str::to_owned),
            poll_interval_ms: poll_ms,
            chart_window_ms: window_ms,
            max_requests,
            split_panels: SplitPanels::default(),
            window_state: None,
        }
    }

    #[test]
    fn positional_path_is_returned() {
        let cli = parse_cli_args(&args(&["logs/server.requests.jsonl"]));
        assert_eq!(cli.path, Some(PathBuf::from("logs/server.requests.jsonl")));
        assert_eq!(cli.poll_interval, None);
        assert_eq!(cli.config_path, None);
    }

    #[test]
    fn poll_flag_value_is_not_treated_as_path() {
        let cli = parse_cli_args(&args(&["--poll", "100", "logs/x.jsonl"]));
        assert_eq!(cli.path, Some(PathBuf::from("logs/x.jsonl")));
        assert_eq!(cli.poll_interval, Some(Duration::from_millis(100)));
    }

    #[test]
    fn path_before_poll_flag() {
        let cli = parse_cli_args(&args(&["logs/x.jsonl", "--poll", "100"]));
        assert_eq!(cli.path, Some(PathBuf::from("logs/x.jsonl")));
        assert_eq!(cli.poll_interval, Some(Duration::from_millis(100)));
    }

    #[test]
    fn no_args_yields_defaults() {
        let cli = parse_cli_args(&args(&[]));
        assert_eq!(cli.path, None);
        assert_eq!(cli.poll_interval, None);
        assert_eq!(cli.config_path, None);
    }

    #[test]
    fn poll_flag_without_path_yields_none_path() {
        let cli = parse_cli_args(&args(&["--poll", "100"]));
        assert_eq!(cli.path, None);
        assert_eq!(cli.poll_interval, Some(Duration::from_millis(100)));
    }

    #[test]
    fn unknown_flags_are_skipped() {
        let cli = parse_cli_args(&args(&["--verbose", "logs/x.jsonl"]));
        assert_eq!(cli.path, Some(PathBuf::from("logs/x.jsonl")));
    }

    #[test]
    fn poll_out_of_range_is_clamped() {
        let cli = parse_cli_args(&args(&["--poll", "1"]));
        assert_eq!(
            cli.poll_interval,
            Some(Duration::from_millis(config::MIN_POLL_MS))
        );
        let cli = parse_cli_args(&args(&["--poll", "99999"]));
        assert_eq!(
            cli.poll_interval,
            Some(Duration::from_millis(config::MAX_POLL_MS))
        );
    }

    #[test]
    fn poll_non_numeric_value_is_ignored() {
        let cli = parse_cli_args(&args(&["--poll", "fast"]));
        assert_eq!(cli.poll_interval, None);
    }

    #[test]
    fn poll_flag_without_value_is_ignored() {
        let cli = parse_cli_args(&args(&["--poll"]));
        assert_eq!(cli.poll_interval, None);
        assert_eq!(cli.path, None);
    }

    #[test]
    fn poll_non_numeric_value_is_consumed() {
        let cli = parse_cli_args(&args(&["--poll", "fast", "logs/x.jsonl"]));
        assert_eq!(cli.poll_interval, None);
        assert_eq!(
            cli.path,
            Some(PathBuf::from("logs/x.jsonl")),
            "the non-numeric value is consumed as the --poll value, \
             the following argument is still parsed as the path"
        );
    }

    #[test]
    fn second_positional_path_wins() {
        let cli = parse_cli_args(&args(&["a.jsonl", "b.jsonl"]));
        assert_eq!(cli.path, Some(PathBuf::from("b.jsonl")));
    }

    #[test]
    fn config_flag_value_is_not_treated_as_path() {
        let cli = parse_cli_args(&args(&["--config", "cfg.json", "logs/x.jsonl"]));
        assert_eq!(cli.config_path, Some(PathBuf::from("cfg.json")));
        assert_eq!(cli.path, Some(PathBuf::from("logs/x.jsonl")));
    }

    #[test]
    fn config_flag_without_value_is_ignored() {
        let cli = parse_cli_args(&args(&["--config"]));
        assert_eq!(cli.config_path, None);
    }

    #[test]
    fn poll_flag_does_not_consume_another_flag() {
        let cli = parse_cli_args(&args(&["--poll", "--config", "cfg.json"]));
        assert_eq!(cli.poll_interval, None);
        assert_eq!(cli.config_path, Some(PathBuf::from("cfg.json")));
    }

    #[test]
    fn config_flag_does_not_consume_another_flag() {
        let cli = parse_cli_args(&args(&["--config", "--poll", "100"]));
        assert_eq!(cli.config_path, None);
        assert_eq!(cli.poll_interval, Some(Duration::from_millis(100)));
    }

    #[test]
    fn poll_equals_form_is_parsed() {
        let cli = parse_cli_args(&args(&["--poll=100", "logs/x.jsonl"]));
        assert_eq!(cli.poll_interval, Some(Duration::from_millis(100)));
        assert_eq!(cli.path, Some(PathBuf::from("logs/x.jsonl")));
    }

    #[test]
    fn config_equals_form_is_parsed() {
        let cli = parse_cli_args(&args(&["--config=cfg.json", "logs/x.jsonl"]));
        assert_eq!(cli.config_path, Some(PathBuf::from("cfg.json")));
        assert_eq!(cli.path, Some(PathBuf::from("logs/x.jsonl")));
    }

    #[test]
    fn poll_equals_form_non_numeric_is_ignored() {
        let cli = parse_cli_args(&args(&["--poll=fast", "logs/x.jsonl"]));
        assert_eq!(cli.poll_interval, None);
        assert_eq!(cli.path, Some(PathBuf::from("logs/x.jsonl")));
    }

    #[test]
    fn startup_config_created_on_first_run() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        let (path, cfg) = load_startup_config(Some(&cp));
        assert_eq!(path, Some(cp.clone()));
        assert_eq!(cfg, Config::default());
        assert!(cp.exists(), "config file is created on first run");
    }

    #[test]
    fn startup_config_creates_missing_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("nested").join("deeper").join("config.json");
        let (path, cfg) = load_startup_config(Some(&cp));
        assert_eq!(path, Some(cp.clone()));
        assert_eq!(cfg, Config::default());
        assert!(cp.exists(), "config file is created with its parent dirs");
    }

    #[test]
    fn startup_config_clamps_and_persists_out_of_range_values() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(
            &cp,
            r#"{"poll_interval_ms":1,"chart_window_ms":999999999,"max_requests":999999}"#,
        )
        .unwrap();
        let (_, cfg) = load_startup_config(Some(&cp));
        assert_eq!(cfg.poll_interval_ms, config::MIN_POLL_MS);
        assert_eq!(cfg.chart_window_ms, config::MAX_WINDOW_MS);
        assert_eq!(cfg.max_requests, config::MAX_MAX_REQUESTS);
        let saved = config::load(&cp);
        assert_eq!(saved.poll_interval_ms, config::MIN_POLL_MS);
        assert_eq!(saved.chart_window_ms, config::MAX_WINDOW_MS);
        assert_eq!(saved.max_requests, config::MAX_MAX_REQUESTS);
    }

    #[test]
    fn startup_config_clamps_single_out_of_range_field() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(&cp, r#"{"poll_interval_ms":1,"chart_window_ms":300000}"#).unwrap();
        let (_, cfg) = load_startup_config(Some(&cp));
        assert_eq!(cfg.poll_interval_ms, config::MIN_POLL_MS);
        assert_eq!(cfg.chart_window_ms, 300_000);
        assert_eq!(cfg.max_requests, config::DEFAULT_MAX_REQUESTS);
        let saved = config::load(&cp);
        assert_eq!(saved.poll_interval_ms, config::MIN_POLL_MS);
        assert_eq!(saved.chart_window_ms, 300_000);
        assert_eq!(saved.max_requests, config::DEFAULT_MAX_REQUESTS);
    }

    #[test]
    fn startup_config_in_range_file_is_not_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(
            &cp,
            r#"{"poll_interval_ms":200,"chart_window_ms":60000,"sentinel":1}"#,
        )
        .unwrap();
        let (_, cfg) = load_startup_config(Some(&cp));
        assert_eq!(cfg.poll_interval_ms, 200);
        assert_eq!(cfg.chart_window_ms, 60_000);
        let raw = std::fs::read_to_string(&cp).unwrap();
        assert!(
            raw.contains("sentinel"),
            "an in-range config must not be rewritten: {raw}"
        );
    }

    #[test]
    fn open_file_settings_use_cli_poll_override() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(&cp, r#"{"poll_interval_ms":200,"max_requests":250}"#).unwrap();
        let (poll, max_requests) =
            settings_for_new_file(Some(Duration::from_millis(100)), Some(&cp));
        assert_eq!(poll, Duration::from_millis(100));
        assert_eq!(max_requests, 250);
    }

    #[test]
    fn open_file_settings_fall_back_to_config() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(&cp, r#"{"poll_interval_ms":200,"max_requests":250}"#).unwrap();
        let (poll, max_requests) = settings_for_new_file(None, Some(&cp));
        assert_eq!(poll, Duration::from_millis(200));
        assert_eq!(max_requests, 250);
    }

    #[test]
    fn open_file_settings_defaults_without_config() {
        let (poll, max_requests) = settings_for_new_file(None, None);
        assert_eq!(poll, Config::default().poll_interval());
        assert_eq!(max_requests, config::DEFAULT_MAX_REQUESTS);
    }

    #[test]
    fn persist_last_path_keeps_other_settings() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(&cp, r#"{"poll_interval_ms":200,"chart_window_ms":60000}"#).unwrap();
        persist_last_path(Some(&cp), Path::new("logs/x.jsonl"));
        let cfg = config::load(&cp);
        assert_eq!(cfg.last_path, Some("logs/x.jsonl".to_owned()));
        assert_eq!(cfg.poll_interval_ms, 200);
        assert_eq!(cfg.chart_window_ms, 60_000);
    }

    #[test]
    fn persist_last_path_without_config_is_a_noop() {
        persist_last_path(None, Path::new("logs/x.jsonl"));
    }

    #[test]
    fn startup_state_disables_controls_without_file() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        apply_startup_state(&window, None);
        assert!(!window.get_controls_enabled());
        assert_eq!(window.get_file_name(), "");

        apply_startup_state(&window, Some(Path::new("logs/x.jsonl")));
        assert!(window.get_controls_enabled());
        assert_eq!(window.get_file_name_full(), "logs/x.jsonl");
        assert_eq!(window.get_file_name(), "logs/x.jsonl");
    }

    #[test]
    fn about_open_shows_dialog() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        assert!(!window.get_about_visible());
        handle_about_open(&window);
        assert!(window.get_about_visible());
    }

    #[test]
    fn about_close_hides_dialog() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        window.set_about_visible(true);
        handle_about_close(&window);
        assert!(!window.get_about_visible());
    }

    #[test]
    fn settings_close_also_hides_about() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        window.set_settings_visible(true);
        window.set_about_visible(true);
        handle_settings_close(&window);
        assert!(!window.get_settings_visible());
        assert!(!window.get_about_visible());
    }

    #[test]
    fn settings_save_also_hides_about() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        window.set_settings_visible(true);
        window.set_about_visible(true);
        handle_settings_save(&window, None, 500, 1000);
        assert!(!window.get_settings_visible());
        assert!(!window.get_about_visible());
    }

    #[test]
    fn about_version_defaults_to_empty() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        assert_eq!(window.get_about_version(), "");
    }

    #[test]
    fn resolve_cli_path_wins_over_config() {
        let cli = parse_cli_args(&args(&["cli.jsonl"]));
        let cfg = make_config(Some("saved.jsonl"), 200, 60_000, 1_000);
        let r = resolve(&cli, &cfg);
        assert_eq!(r.path, Some(PathBuf::from("cli.jsonl")));
    }

    #[test]
    fn resolve_falls_back_to_config_path() {
        let cli = parse_cli_args(&args(&[]));
        let cfg = make_config(Some("saved.jsonl"), 200, 60_000, 1_000);
        let r = resolve(&cli, &cfg);
        assert_eq!(r.path, Some(PathBuf::from("saved.jsonl")));
    }

    #[test]
    fn resolve_no_sources_yields_no_path() {
        let cli = parse_cli_args(&args(&[]));
        let r = resolve(&cli, &Config::default());
        assert_eq!(r.path, None);
    }

    #[test]
    fn resolve_cli_poll_wins_over_config() {
        let cli = parse_cli_args(&args(&["--poll", "100"]));
        let cfg = make_config(None, 200, 60_000, 1_000);
        let r = resolve(&cli, &cfg);
        assert_eq!(r.poll_interval, Duration::from_millis(100));
    }

    #[test]
    fn resolve_falls_back_to_config_poll() {
        let cli = parse_cli_args(&args(&[]));
        let cfg = make_config(None, 200, 60_000, 1_000);
        let r = resolve(&cli, &cfg);
        assert_eq!(r.poll_interval, Duration::from_millis(200));
    }

    #[test]
    fn resolve_config_poll_is_clamped() {
        let cli = parse_cli_args(&args(&[]));
        let cfg = make_config(None, 999_999, 60_000, 1_000);
        let r = resolve(&cli, &cfg);
        assert_eq!(r.poll_interval, Duration::from_millis(config::MAX_POLL_MS));
    }

    #[test]
    fn resolve_window_comes_from_config_clamped() {
        let cli = parse_cli_args(&args(&[]));
        let cfg = make_config(None, 200, 999_999_999, 1_000);
        let r = resolve(&cli, &cfg);
        assert_eq!(r.chart_window_ms, config::MAX_WINDOW_MS);
    }

    #[test]
    fn resolve_max_requests_comes_from_config_clamped() {
        let cli = parse_cli_args(&args(&[]));
        let cfg = make_config(None, 200, 60_000, 999_999);
        let r = resolve(&cli, &cfg);
        assert_eq!(r.max_requests, config::MAX_MAX_REQUESTS);
    }

    fn append_throughput_line(path: &Path, ts: u64, decode: f64) {
        use std::io::Write;
        let line = format!(
            r#"{{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":{ts},"event":"throughput","throughput_tokens_per_second":{{"decode":{decode},"prefill":2.5}}}}"#
        );
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(format!("{line}\n").as_bytes()).unwrap();
    }

    fn wait_until(mut cond: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !cond() {
            if Instant::now() >= deadline {
                panic!("timed out waiting for condition");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn pause_freezes_view_and_catches_up_on_resume() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        std::fs::File::create(&path).unwrap();

        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: Some(Pipeline::start(
                &path,
                Duration::from_millis(50),
                config::DEFAULT_MAX_REQUESTS as usize,
            )),
            path: Some(path.clone()),
        }));
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));

        append_throughput_line(&path, 1_000, 1.5);
        wait_until(|| {
            apply_latest(&window, &app, &charts);
            window.get_kpi_decode_value() != "—"
        });
        let value_before = window.get_kpi_decode_value().to_string();
        assert_eq!(value_before, "1.5");

        window.set_paused(true);
        let last_event_before = window.get_last_event().to_string();
        append_throughput_line(&path, 2_000, 9.9);
        wait_until(|| {
            apply_latest(&window, &app, &charts);
            window.get_last_event() != last_event_before.as_str()
        });
        assert_eq!(
            window.get_kpi_decode_value(),
            value_before.as_str(),
            "the live view must stay frozen while paused"
        );

        window.set_paused(false);
        resume_view(&window, &app, &charts);
        assert_eq!(window.get_kpi_decode_value(), "9.9");
    }

    #[test]
    fn resume_without_seen_status_shows_none() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: None,
            path: None,
        }));
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));
        window.set_paused(true);

        resume_view(&window, &app, &charts);

        assert_eq!(window.get_connection_status(), "—");
        assert_eq!(window.get_status_color(), COLOR_NONE);
    }

    #[test]
    fn clear_resets_view_and_applies_post_clear_snapshot() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        std::fs::File::create(&path).unwrap();

        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: Some(Pipeline::start(
                &path,
                Duration::from_millis(50),
                config::DEFAULT_MAX_REQUESTS as usize,
            )),
            path: Some(path.clone()),
        }));
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));

        append_throughput_line(&path, 1_000, 1.5);
        wait_until(|| {
            apply_latest(&window, &app, &charts);
            window.get_kpi_decode_value() != "—"
        });
        assert_eq!(window.get_kpi_decode_value(), "1.5");

        handle_clear(&window, &app, &charts);
        assert_eq!(window.get_kpi_decode_value(), "—", "clear resets the KPIs");
        assert!(charts.lock().unwrap().clear_pending, "the clear is pending");

        wait_until(|| {
            apply_latest(&window, &app, &charts);
            !charts.lock().unwrap().clear_pending
        });
        assert_eq!(charts.lock().unwrap().clear_count, 1);
        assert_eq!(
            window.get_kpi_decode_value(),
            "—",
            "the view stays empty after the post-clear snapshot"
        );

        append_throughput_line(&path, 2_000, 9.9);
        wait_until(|| {
            apply_latest(&window, &app, &charts);
            window.get_kpi_decode_value() != "—"
        });
        assert_eq!(window.get_kpi_decode_value(), "9.9");
    }

    #[test]
    fn clear_while_paused_resets_view_and_freezes_until_resume() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        std::fs::File::create(&path).unwrap();

        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: Some(Pipeline::start(
                &path,
                Duration::from_millis(50),
                config::DEFAULT_MAX_REQUESTS as usize,
            )),
            path: Some(path.clone()),
        }));
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));

        append_throughput_line(&path, 1_000, 1.5);
        wait_until(|| {
            apply_latest(&window, &app, &charts);
            window.get_kpi_decode_value() != "—"
        });
        assert_eq!(window.get_kpi_decode_value(), "1.5");

        // Pause, then Clear: the view resets immediately even while paused.
        window.set_paused(true);
        handle_clear(&window, &app, &charts);
        assert_eq!(
            window.get_kpi_decode_value(),
            "—",
            "clear resets the KPIs even while paused"
        );
        assert!(charts.lock().unwrap().clear_pending, "the clear is pending");

        // The post-clear snapshot arrives while paused: status bar only, the
        // view stays empty (no flicker back to pre-clear data).
        wait_until(|| {
            apply_latest(&window, &app, &charts);
            !charts.lock().unwrap().clear_pending
        });
        assert_eq!(charts.lock().unwrap().clear_count, 1);
        assert_eq!(
            window.get_kpi_decode_value(),
            "—",
            "the view stays empty after the post-clear snapshot while paused"
        );

        // New data while paused: the live view stays frozen (status bar only).
        append_throughput_line(&path, 2_000, 9.9);
        wait_until(|| {
            apply_latest(&window, &app, &charts);
            charts.lock().unwrap().store.latest_throughput().is_some()
        });
        assert_eq!(
            window.get_kpi_decode_value(),
            "—",
            "the live view stays frozen while paused"
        );

        // Resume: catch up to the data that arrived while paused.
        window.set_paused(false);
        resume_view(&window, &app, &charts);
        assert_eq!(window.get_kpi_decode_value(), "9.9");
    }

    #[test]
    fn open_file_replaces_pipeline_and_persists_path() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let path_a = dir.path().join("a.jsonl");
        let path_b = dir.path().join("b.jsonl");
        let cp = dir.path().join("config.json");
        std::fs::File::create(&path_a).unwrap();
        std::fs::File::create(&path_b).unwrap();

        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: Some(Pipeline::start(
                &path_a,
                Duration::from_millis(50),
                config::DEFAULT_MAX_REQUESTS as usize,
            )),
            path: Some(path_a.clone()),
        }));
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));

        append_throughput_line(&path_a, 1_000, 1.5);
        wait_until(|| {
            apply_latest(&window, &app, &charts);
            window.get_kpi_decode_value() != "—"
        });
        assert_eq!(window.get_kpi_decode_value(), "1.5");

        handle_open_file(&window, &app, &charts, Some(&cp), None, path_b.clone());
        assert_eq!(window.get_file_name_full(), path_b.display().to_string());
        assert_eq!(window.get_kpi_decode_value(), "—", "the view is reset");
        assert!(!charts.lock().unwrap().clear_pending);
        assert_eq!(charts.lock().unwrap().clear_count, 0);
        assert_eq!(
            config::load(&cp).last_path,
            Some(path_b.display().to_string())
        );

        append_throughput_line(&path_b, 2_000, 9.9);
        wait_until(|| {
            apply_latest(&window, &app, &charts);
            window.get_kpi_decode_value() != "—"
        });
        assert_eq!(window.get_kpi_decode_value(), "9.9");
    }

    #[test]
    fn open_file_without_config_replaces_pipeline() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let path_a = dir.path().join("a.jsonl");
        let path_b = dir.path().join("b.jsonl");
        std::fs::File::create(&path_a).unwrap();
        std::fs::File::create(&path_b).unwrap();

        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: Some(Pipeline::start(
                &path_a,
                Duration::from_millis(50),
                config::DEFAULT_MAX_REQUESTS as usize,
            )),
            path: Some(path_a.clone()),
        }));
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));

        handle_open_file(&window, &app, &charts, None, None, path_b.clone());
        assert_eq!(window.get_file_name_full(), path_b.display().to_string());
        assert_eq!(window.get_kpi_decode_value(), "—", "the view is reset");
        assert!(!charts.lock().unwrap().clear_pending);
        assert_eq!(charts.lock().unwrap().clear_count, 0);
    }

    #[test]
    fn window_changed_saves_config_and_marks_stale_while_paused() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(&cp, r#"{"chart_window_ms":300000}"#).unwrap();

        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: None,
            path: None,
        }));
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));

        handle_window_changed(&window, &app, &charts, Some(&cp), 900_000);
        assert_eq!(charts.lock().unwrap().window_ms, 900_000);
        assert!(!charts.lock().unwrap().charts_stale);
        assert_eq!(config::load(&cp).chart_window_ms, 900_000);

        window.set_paused(true);
        handle_window_changed(&window, &app, &charts, Some(&cp), 60_000);
        assert_eq!(charts.lock().unwrap().window_ms, 60_000);
        assert!(
            charts.lock().unwrap().charts_stale,
            "charts are stale while paused"
        );
        assert_eq!(config::load(&cp).chart_window_ms, 60_000);
    }

    #[test]
    fn window_changed_without_config_updates_state_only() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: None,
            path: None,
        }));
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));

        handle_window_changed(&window, &app, &charts, None, 900_000);
        assert_eq!(charts.lock().unwrap().window_ms, 900_000);
        assert!(!charts.lock().unwrap().charts_stale);
    }

    #[test]
    fn split_changed_updates_charts_fraction() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let split = Arc::new(Mutex::new(SplitPanels::default()));

        handle_split_changed(&window, &split, 0, 0.7);
        assert_eq!(window.get_charts_frac(), 0.7);
        assert_eq!(
            window.get_table_frac(),
            0.5,
            "the table fraction is untouched"
        );
        assert_eq!(
            *split.lock().unwrap(),
            SplitPanels {
                charts: 0.7,
                table: 0.5
            }
        );
    }

    #[test]
    fn split_changed_updates_table_fraction() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let split = Arc::new(Mutex::new(SplitPanels {
            charts: 0.6,
            table: 0.5,
        }));

        handle_split_changed(&window, &split, 1, 0.3);
        assert_eq!(
            window.get_charts_frac(),
            0.6,
            "the charts fraction is untouched"
        );
        assert_eq!(window.get_table_frac(), 0.3);
        assert_eq!(
            *split.lock().unwrap(),
            SplitPanels {
                charts: 0.6,
                table: 0.3
            }
        );
    }

    #[test]
    fn split_changed_clamps_out_of_range() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let split = Arc::new(Mutex::new(SplitPanels::default()));

        handle_split_changed(&window, &split, 0, 0.99);
        assert_eq!(window.get_charts_frac(), split::MAX_CHARTS_FRAC);
        handle_split_changed(&window, &split, 0, 0.01);
        assert_eq!(window.get_charts_frac(), split::MIN_CHARTS_FRAC);
        handle_split_changed(&window, &split, 1, 0.99);
        assert_eq!(window.get_table_frac(), split::MAX_TABLE_FRAC);
        handle_split_changed(&window, &split, 1, 0.01);
        assert_eq!(window.get_table_frac(), split::MIN_TABLE_FRAC);
    }

    #[test]
    fn split_released_persists_and_keeps_other_settings() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(&cp, r#"{"poll_interval_ms":200,"chart_window_ms":60000}"#).unwrap();
        let window = MainWindow::new().unwrap();
        let split = Arc::new(Mutex::new(SplitPanels::default()));

        handle_split_changed(&window, &split, 0, 0.7);
        handle_split_changed(&window, &split, 1, 0.3);
        handle_split_released(Some(&cp), &split);

        let cfg = config::load(&cp);
        assert_eq!(
            cfg.split_panels,
            SplitPanels {
                charts: 0.7,
                table: 0.3
            }
        );
        assert_eq!(cfg.poll_interval_ms, 200, "the other settings are kept");
        assert_eq!(cfg.chart_window_ms, 60_000);
    }

    #[test]
    fn split_released_without_config_is_a_noop() {
        let split = Arc::new(Mutex::new(SplitPanels {
            charts: 0.7,
            table: 0.3,
        }));
        handle_split_released(None, &split);
        assert_eq!(
            *split.lock().unwrap(),
            SplitPanels {
                charts: 0.7,
                table: 0.3
            }
        );
    }

    #[test]
    fn startup_config_clamps_out_of_range_split_panels() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(&cp, r#"{"split_panels":{"charts":0.95,"table":0.05}}"#).unwrap();
        let (_, cfg) = load_startup_config(Some(&cp));
        assert_eq!(
            cfg.split_panels,
            SplitPanels {
                charts: split::MAX_CHARTS_FRAC,
                table: split::MIN_TABLE_FRAC
            }
        );
        let saved = config::load(&cp);
        assert_eq!(
            saved.split_panels,
            SplitPanels {
                charts: split::MAX_CHARTS_FRAC,
                table: split::MIN_TABLE_FRAC
            }
        );
    }

    #[test]
    fn settings_open_prefills_from_config() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(
            &cp,
            r#"{"poll_interval_ms":200,"chart_window_ms":60000,"max_requests":250}"#,
        )
        .unwrap();

        let window = MainWindow::new().unwrap();
        handle_settings_open(&window, Some(&cp));
        assert!(window.get_settings_visible());
        assert_eq!(window.get_settings_poll_ms(), 200);
        assert_eq!(window.get_settings_max_requests(), 250);
    }

    #[test]
    fn settings_open_without_config_uses_defaults() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        handle_settings_open(&window, None);
        assert!(window.get_settings_visible());
        assert_eq!(
            window.get_settings_poll_ms(),
            Config::default().poll_interval_ms as i32
        );
        assert_eq!(
            window.get_settings_max_requests(),
            config::DEFAULT_MAX_REQUESTS as i32
        );
    }

    #[test]
    fn settings_save_persists_poll_and_max_requests() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(&cp, r#"{"poll_interval_ms":500,"chart_window_ms":300000}"#).unwrap();

        let window = MainWindow::new().unwrap();

        handle_settings_save(&window, Some(&cp), 200, 250);
        assert!(!window.get_settings_visible());
        let cfg = config::load(&cp);
        assert_eq!(cfg.poll_interval_ms, 200);
        assert_eq!(cfg.max_requests, 250);
        assert_eq!(cfg.chart_window_ms, 300_000);
    }

    #[test]
    fn settings_save_clamps_out_of_range() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(&cp, r#"{"poll_interval_ms":500,"chart_window_ms":300000}"#).unwrap();

        let window = MainWindow::new().unwrap();

        handle_settings_save(&window, Some(&cp), 1, 999_999);
        let cfg = config::load(&cp);
        assert_eq!(cfg.poll_interval_ms, config::MIN_POLL_MS);
        assert_eq!(cfg.max_requests, config::MAX_MAX_REQUESTS);
    }

    #[test]
    fn settings_close_hides_panel() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        window.set_settings_visible(true);
        handle_settings_close(&window);
        assert!(!window.get_settings_visible());
    }

    #[test]
    fn window_change_while_paused_renders_on_resume() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        std::fs::File::create(&path).unwrap();

        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: Some(Pipeline::start(
                &path,
                Duration::from_millis(50),
                config::DEFAULT_MAX_REQUESTS as usize,
            )),
            path: Some(path.clone()),
        }));
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));

        append_throughput_line(&path, 1_000, 1.5);
        wait_until(|| {
            apply_latest(&window, &app, &charts);
            window.get_kpi_decode_value() != "—"
        });

        // Change the window while running: re-renders immediately, not stale.
        handle_window_changed(&window, &app, &charts, None, 60_000);
        assert_eq!(charts.lock().unwrap().window_ms, 60_000);
        assert!(!charts.lock().unwrap().charts_stale);

        // Pause, then change the window: the charts stay frozen (stale).
        window.set_paused(true);
        handle_window_changed(&window, &app, &charts, None, 900_000);
        assert_eq!(charts.lock().unwrap().window_ms, 900_000);
        assert!(
            charts.lock().unwrap().charts_stale,
            "the charts are stale while paused"
        );

        // Resume: the stale charts are re-rendered with the new window.
        window.set_paused(false);
        resume_view(&window, &app, &charts);
        assert_eq!(charts.lock().unwrap().window_ms, 900_000);
        assert!(
            !charts.lock().unwrap().charts_stale,
            "the charts are re-rendered (stale cleared) on resume"
        );
    }

    #[test]
    fn resume_applies_new_snapshot_and_renders_stale_charts() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        std::fs::File::create(&path).unwrap();

        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: Some(Pipeline::start(
                &path,
                Duration::from_millis(50),
                config::DEFAULT_MAX_REQUESTS as usize,
            )),
            path: Some(path.clone()),
        }));
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));

        append_throughput_line(&path, 1_000, 1.5);
        wait_until(|| {
            apply_latest(&window, &app, &charts);
            window.get_kpi_decode_value() != "—"
        });

        // Pause, change the window (charts become stale), and append new data.
        window.set_paused(true);
        handle_window_changed(&window, &app, &charts, None, 900_000);
        assert!(
            charts.lock().unwrap().charts_stale,
            "charts are stale while paused"
        );
        append_throughput_line(&path, 2_000, 9.9);

        // While paused the view is frozen but the internal store is kept
        // current; wait until the 9.9 snapshot is drained.
        wait_until(|| {
            apply_latest(&window, &app, &charts);
            charts
                .lock()
                .unwrap()
                .store
                .latest_throughput()
                .is_some_and(|t| t.timestamp_ms == 2_000)
        });

        // Resume: the new snapshot is applied AND the stale charts are
        // re-rendered with the new window.
        window.set_paused(false);
        resume_view(&window, &app, &charts);
        assert_eq!(window.get_kpi_decode_value(), "9.9");
        assert_eq!(charts.lock().unwrap().window_ms, 900_000);
        assert!(
            !charts.lock().unwrap().charts_stale,
            "the stale charts are re-rendered on resume"
        );
    }

    #[test]
    fn startup_config_repairs_malformed_file() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(&cp, "{not json").unwrap();
        let (_, cfg) = load_startup_config(Some(&cp));
        assert_eq!(cfg, Config::default());
        assert_eq!(
            config::load(&cp),
            Config::default(),
            "the malformed file is repaired with defaults"
        );
    }
}
